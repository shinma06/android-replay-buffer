//! Stdio JSON-RPC 2.0 Model Context Protocol (MCP) server for `arb-cli mcp`.
//!
//! Exposes Android Replay Buffer live capture, WORM bundle inspection, contact-sheet /
//! tap-crop AI visual framing, and strict integer-frame trimming (`share.mp4` / WORM bundle)
//! over newline-delimited JSON-RPC 2.0 on stdin/stdout (every revision in [`ProtocolVersion`]).
//!
//! Strict stdio discipline: **ZERO** logs or non-JSON-RPC bytes are ever written to stdout;
//! diagnostics go exclusively to stderr.

use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Write};

use arb_os::paths::ArbPaths;
use serde_json::{Map, Value, json};

use crate::CliError;
use crate::rpc::{AutoStartMode, RpcClient, RpcClientError};

mod args;
mod catalog;
mod errors;
mod output;
use output::ToolOutput;
mod project;
mod tools;
mod version;
pub(crate) use errors::Failure;
#[cfg(test)]
pub(crate) use tools::save::save_content;
pub(crate) use version::ProtocolVersion;
mod resources;
mod tasks;
mod validate;

use std::sync::Arc;
use tasks::{Activity, Session};

const JSONRPC_PARSE_ERROR: i32 = -32_700;
const JSONRPC_INVALID_REQUEST: i32 = -32_600;
const JSONRPC_METHOD_NOT_FOUND: i32 = -32_601;
const JSONRPC_INVALID_PARAMS: i32 = -32_602;

const SERVER_INSTRUCTIONS: &str = "直近の不具合は arb_save_and_inspect、保存済みなら arb_timeline_overview から開始。crashes[0] と process_events の frame_index_hint を arb_inspect_frames の single に渡し、arb_query_logs の start_frame・end_frame で前後を確認します。input_capture は観測範囲、taps は物理入力、injected_inputs は adbd の注入要求（対話シェル・別の注入元は観測外、実行成功は未確認）です。arb_screenshot は既定 include_image: false、必要時は include_image: true または resource_links・file_uri を開きます。フレームは arb://bundles/{id}/frame/{n} で閲覧できます。整理は arb_gc_bundles({}) で候補を確認後、dry_run: false で実行。失敗ジョブは arb_jobs の list で確認します。各出力スキーマは arb://schemas/{tool}。";

/// Runs the blocking stdio MCP JSON-RPC 2.0 loop until stdin reaches EOF.
///
/// # Errors
///
/// Returns [`CliError`] if writing to stdout fails irrecoverably.
pub(crate) fn run_mcp_stdio(paths: &ArbPaths) -> Result<(), CliError> {
    let stdin = io::stdin();
    let reader = BufReader::new(stdin.lock());
    let mut server = McpServer::new(paths.clone());
    let session = Arc::clone(&server.session);
    std::thread::scope(|scope| {
        let (stop, wake) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || {
            while wake.recv_timeout(std::time::Duration::from_secs(1))
                == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                session.reap();
            }
        });
        let result = server.serve_io(reader);
        drop(stop);
        result
    })
    .map_err(|err| CliError::general(format!("mcp stdio I/O error: {err}")))
}

struct McpServer {
    paths: ArbPaths,
    client: Option<RpcClient>,
    version: ProtocolVersion,
    session: Arc<Session>,
    activity: Activity,
    external_bundles: std::collections::BTreeMap<String, String>,
    screenshots: std::collections::VecDeque<(url::Url, String)>,
}

impl McpServer {
    fn new(paths: ArbPaths) -> Self {
        Self {
            paths,
            client: None,
            version: ProtocolVersion::default(),
            session: Arc::default(),
            activity: Activity::default(),
            external_bundles: std::collections::BTreeMap::new(),
            screenshots: std::collections::VecDeque::new(),
        }
    }

    fn serve_io<R: BufRead>(&mut self, mut reader: R) -> io::Result<()> {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            let read = read_bounded_line(&mut reader, &mut buf, MAX_FRAME_BYTES)?;
            let resp = match read {
                LineRead::Eof => break,
                LineRead::TooLong => Some(rpc_error(
                    &Value::Null,
                    JSONRPC_PARSE_ERROR,
                    format!("frame exceeds {MAX_FRAME_BYTES} bytes"),
                )),
                LineRead::Line => match std::str::from_utf8(&buf) {
                    Ok(text) => {
                        let trimmed = text.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        self.handle_line(trimmed)
                    }
                    Err(_) => Some(rpc_error(
                        &Value::Null,
                        JSONRPC_PARSE_ERROR,
                        "frame is not valid UTF-8",
                    )),
                },
            };
            if let Some(resp) = resp {
                write_message(&resp)?;
            }
        }
        self.session.shutdown();
        Ok(())
    }

    fn handle_line(&mut self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(err) => {
                return Some(rpc_error(
                    &Value::Null,
                    JSONRPC_PARSE_ERROR,
                    format!("invalid JSON: {err}"),
                ));
            }
        };

        if let Some(batch) = msg.as_array() {
            // JSON-RPC batch (revisions that `supports_batch`): answer each element, omit notifications.
            if !self.version.supports_batch() {
                return Some(rpc_error(
                    &Value::Null,
                    JSONRPC_INVALID_REQUEST,
                    "ネゴシエーションした MCP バージョンはバッチに対応していません",
                ));
            }
            if batch.is_empty() {
                return Some(rpc_error(
                    &Value::Null,
                    JSONRPC_INVALID_REQUEST,
                    "JSON-RPC batch must not be empty",
                ));
            }
            let responses: Vec<Value> = batch
                .iter()
                .filter_map(|item| self.handle_message(item))
                .collect();
            return if responses.is_empty() {
                None
            } else {
                Some(Value::Array(responses))
            };
        }
        self.handle_message(&msg)
    }

    fn handle_message(&mut self, msg: &Value) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(rpc_error(
                &Value::Null,
                JSONRPC_INVALID_REQUEST,
                "JSON-RPC request must be a JSON object",
            ));
        };

        let id = obj.get("id").cloned();
        let is_notification = id.is_none();

        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return id.as_ref().map(|req_id| {
                rpc_error(
                    req_id,
                    JSONRPC_INVALID_REQUEST,
                    "jsonrpc field must equal \"2.0\"",
                )
            });
        }

        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return id.as_ref().map(|req_id| {
                rpc_error(
                    req_id,
                    JSONRPC_INVALID_REQUEST,
                    "missing string `method` field",
                )
            });
        };

        let params = obj
            .get("params")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));

        if is_notification {
            // Notifications (e.g. `notifications/initialized`, `notifications/cancelled`)
            // MUST NOT emit a response frame.
            return None;
        }
        let req_id = id.unwrap_or(Value::Null);

        match method {
            "initialize" => {
                self.version = params["protocolVersion"]
                    .as_str()
                    .and_then(ProtocolVersion::parse)
                    .unwrap_or_default();
                Some(rpc_ok(&req_id, &handle_initialize(self.version)))
            }
            "ping" => Some(rpc_ok(&req_id, &json!({}))),
            "tools/list" => Some(rpc_ok(
                &req_id,
                &json!({"tools":catalog::listed(self.version)}),
            )),
            "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "prompts/list"
            | "prompts/get"
            | "completion/complete" => Some(match self.resource_request(method, &params) {
                Ok(v) => rpc_ok(&req_id, &v),
                Err(e) => {
                    let mut response = rpc_error(&req_id, e.rpc_code, e.message);
                    if e.label == "RESOURCE_NOT_FOUND" {
                        response["error"]["data"] = json!({"uri": params["uri"]});
                    }
                    response
                }
            }),
            "logging/setLevel" => Some(self.session.set_level(&req_id, &params)),
            "tasks/get" | "tasks/list" | "tasks/cancel" | "tasks/result"
                if self.version.supports_tasks() =>
            {
                self.session.request(method, &req_id, &params)
            }
            "tools/call" => Some(self.handle_tools_call(&req_id, &params)),
            other => Some(rpc_error(
                &req_id,
                JSONRPC_METHOD_NOT_FOUND,
                format!("unknown MCP method `{other}`"),
            )),
        }
    }

    fn handle_tools_call(&mut self, req_id: &Value, params: &Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return rpc_error(
                req_id,
                JSONRPC_INVALID_PARAMS,
                "tools/call requires string `name`",
            );
        };
        let mut args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let canonical = match args::tool_name(name, &mut args) {
            Ok(name) => name,
            Err(e) => {
                return rpc_ok(
                    req_id,
                    &Failure::from_violation(name, e).result(self.version),
                );
            }
        };
        let Some(def) = catalog::for_version(self.version)
            .iter()
            .find(|t| t["name"] == canonical)
        else {
            return rpc_error(
                req_id,
                JSONRPC_INVALID_PARAMS,
                format!("unknown tool {name}"),
            );
        };
        let token = params
            .pointer("/_meta/progressToken")
            .filter(|t| t.is_string() || t.is_number())
            .cloned();
        let long = catalog::long_running(canonical);
        let progress = long && (canonical != "arb_capture_start" || args["wait_connected"] == true);
        let activity = Activity::new(canonical, token.filter(|_| progress));
        if let Some(task) = params.get("task") {
            if !self.version.supports_tasks() || !long {
                return rpc_error(
                    req_id,
                    JSONRPC_INVALID_PARAMS,
                    "このツールの Task 実行には対応していません",
                );
            }
            return self.start_task(req_id, task, args, activity);
        }
        self.activity = activity;
        let result = self.run_tool(def, args);
        self.activity = Activity::default();
        rpc_ok(req_id, &result)
    }

    fn run_tool(&mut self, def: &Value, args: Value) -> Value {
        self.activity.notify(&self.session, "開始");
        let name = def["name"].as_str().expect("tool name");
        let output = args::normalize(name, &def["inputSchema"], args)
            .map_err(|e| Failure::from_violation(name, e))
            .and_then(|args| self.dispatch_tool(name, &args));
        self.activity.finish(&self.session, output.as_ref().err());
        match output {
            Ok(output) => output.result(self.version),
            Err(failure) => failure.for_tool(name, self.version).result(self.version),
        }
    }

    fn call_daemon(&mut self, method: &str, params: &Value) -> Result<Value, Failure> {
        self.activity.check(&self.session)?;
        self.activity.notify(&self.session, method);
        // A request that may already have reached the daemon is only replayed when the method is
        // read-only; mutating methods (save/export/trash/capture) are never retried automatically.
        let retry_safe = is_retry_safe_method(method);
        for attempt in 0..2 {
            if self.client.is_none() {
                let connected = RpcClient::connect(&self.paths, AutoStartMode::Enabled)
                    .map_err(Failure::from_rpc)?;
                self.client = Some(connected);
            }
            // The task may have been cancelled while connecting or negotiating system.hello.
            self.activity.check(&self.session)?;
            let Some(client) = self.client.as_mut() else {
                return Err(Failure::unavailable("arbd client unavailable"));
            };
            match client.call(method, params.clone()) {
                Ok(val) => return Ok(val),
                Err(err) if err.is_transport() => {
                    self.client = None;
                    // A request rejected locally (too large / invalid) would fail identically again.
                    let interrupted = match &err {
                        RpcClientError::UnexpectedEof | RpcClientError::Io(_) => true,
                        RpcClientError::Os(os) => !matches!(
                            os,
                            arb_os::OsError::FrameTooLarge { .. }
                                | arb_os::OsError::InvalidArgument { .. }
                        ),
                        _ => false,
                    };
                    // `Closed` means nothing was sent, so even a mutating call may reconnect once.
                    let nothing_sent = matches!(err, RpcClientError::Closed);
                    if !((nothing_sent || (retry_safe && interrupted)) && attempt == 0) {
                        return Err(Failure::from_rpc(err));
                    }
                }
                Err(err) => {
                    if matches!(&err, RpcClientError::Rpc { code: -32601, .. }) {
                        self.client = None;
                    }
                    return Err(Failure::from_rpc(err));
                }
            }
        }
        Err(Failure::unavailable(
            "arbd IPC connection failed after retry",
        ))
    }
}

fn handle_initialize(negotiated: ProtocolVersion) -> Value {
    let mut capabilities = json!({"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false},"prompts":{"listChanged":false},"logging":{},"completions":{}});
    if negotiated.supports_tasks() {
        capabilities["tasks"] = json!({"list":{},"cancel":{},"requests":{"tools":{"call":{}}}});
    }
    json!({
        "protocolVersion":negotiated.as_str(), "capabilities":capabilities,
        "serverInfo":{"name":"android-replay-buffer","title":"Android Replay Buffer (ARB)","version":env!("CARGO_PKG_VERSION"),"description":"Android の画面・ログ・操作を保存して調査します。"},
        "instructions": SERVER_INSTRUCTIONS
    })
}

/// Methods with no side effects (or idempotent reads) that may be replayed after a dropped socket.
fn is_retry_safe_method(method: &str) -> bool {
    matches!(
        method,
        "system.hello"
            | "system.status"
            | "device.list"
            | "config.get"
            | "bundle.list"
            | "timeline.summary"
            | "timeline.overview"
            | "timeline.query_logs"
            | "bundle.query_logs"
            | "bundle.manifest"
            | "timeline.crashes"
            | "timeline.screens"
            | "media.inspect_frames"
    )
}

/// Maximum accepted size of one newline-delimited JSON-RPC frame.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

enum LineRead {
    Eof,
    Line,
    TooLong,
}

/// Reads one `\n`-terminated frame into `buf` without ever buffering more than `max` bytes.
/// An oversized frame is drained to its newline and reported as [`LineRead::TooLong`].
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<LineRead> {
    let mut overflow = false;
    let mut any = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(a) => a,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if available.is_empty() {
            return Ok(if !any {
                LineRead::Eof
            } else if overflow {
                LineRead::TooLong
            } else {
                LineRead::Line
            });
        }
        any = true;
        let (chunk, found) = match available.iter().position(|b| *b == b'\n') {
            Some(pos) => (&available[..pos], Some(pos + 1)),
            None => (available, None),
        };
        if !overflow {
            if buf.len() + chunk.len() > max {
                overflow = true;
                buf.clear();
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        let consumed = found.unwrap_or(chunk.len());
        reader.consume(consumed);
        if found.is_some() {
            return Ok(if overflow {
                LineRead::TooLong
            } else {
                LineRead::Line
            });
        }
    }
}

fn rpc_ok(id: &Value, result: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn rpc_error(id: &Value, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        }
    })
}

fn fill_missing(body: &mut Value, names: &[&str]) {
    if let Some(obj) = body.as_object_mut() {
        for &key in names {
            obj.entry(key).or_insert(Value::Null);
        }
    }
}

fn take_png(resp: &mut Value) -> Result<String, Failure> {
    if let Some(Value::String(data)) = resp.as_object_mut().and_then(|o| o.remove("png_base64")) {
        return Ok(data);
    }
    let path = resp["png_path"]
        .as_str()
        .ok_or_else(|| Failure::new(-32603, "OPERATION_FAILED", "PNG のパスがありません"))?;
    fs::read(path)
        .map(|b| base64_encode(&b))
        .map_err(|e| Failure::new(-32603, "OPERATION_FAILED", e.to_string()))
}

fn write_message(msg: &Value) -> io::Result<()> {
    let mut out = BufWriter::new(io::stdout().lock());
    serde_json::to_writer(&mut out, msg)?;
    out.write_all(b"\n")?;
    out.flush()
}

fn image_block(base64_png: &str) -> Value {
    typed_image_block(base64_png, "image/png")
}

fn typed_image_block(base64: &str, mime_type: &str) -> Value {
    json!({
        "type": "image",
        "data": base64,
        "mimeType": mime_type,
    })
}

/// Pure-Rust RFC 4648 standard Base64 encoder (zero dependencies).
use arb_core::base64_encode;

#[cfg(test)]
mod tests;
