//! Stdio JSON-RPC 2.0 Model Context Protocol (MCP) server for `arb-cli mcp`.
//!
//! Exposes Android Replay Buffer live capture, WORM bundle inspection, contact-sheet /
//! tap-crop AI visual framing, and strict integer-frame trimming (`share.mp4` / WORM bundle)
//! over newline-delimited JSON-RPC 2.0 on stdin/stdout (2024-11-05 through 2025-11-25).
//!
//! Strict stdio discipline: **ZERO** logs or non-JSON-RPC bytes are ever written to stdout;
//! diagnostics go exclusively to stderr.

use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use arb_os::paths::ArbPaths;
use serde_json::{Map, Value, json};

use crate::rpc::{AutoStartMode, RpcClient, RpcClientError};
use crate::{CliError, absolutize_bundle_target, absolutize_path};

mod contract;
mod resources;
mod schema;
mod tasks;

use std::sync::Arc;
use tasks::{Activity, Session};

const MCP_PROTOCOL_LATEST: &str = "2025-11-25";
const JSONRPC_PARSE_ERROR: i32 = -32_700;
const JSONRPC_INVALID_REQUEST: i32 = -32_600;
const JSONRPC_METHOD_NOT_FOUND: i32 = -32_601;
const JSONRPC_INVALID_PARAMS: i32 = -32_602;

const SERVER_INSTRUCTIONS: &str = "不具合の記録には arb_save_and_inspect を使います。arb_query_logs は limit と cursor でページ送りし、whole_word で単語を絞れます。画像は必要な resource_link のみ resources/read で取得してください。整理の前には arb_gc_bundles の dry_run: true で候補を確認してください。";

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
    version: String,
    session: Arc<Session>,
    activity: Activity,
    external_bundles: std::collections::BTreeMap<String, String>,
}

impl McpServer {
    fn new(paths: ArbPaths) -> Self {
        Self {
            paths,
            client: None,
            version: MCP_PROTOCOL_LATEST.into(),
            session: Arc::default(),
            activity: Activity::default(),
            external_bundles: std::collections::BTreeMap::new(),
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
            // JSON-RPC batch (required by MCP 2025-03-26): answer each element, omit notifications.
            if self.version.as_str() >= "2025-06-18" {
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
                let init = handle_initialize(&params);
                init["protocolVersion"]
                    .as_str()?
                    .clone_into(&mut self.version);
                Some(rpc_ok(&req_id, &init))
            }
            "ping" => Some(rpc_ok(&req_id, &json!({}))),
            "tools/list" => {
                let mut tools = contract::for_version(&self.version).to_vec();
                if self.version != MCP_PROTOCOL_LATEST {
                    for t in &mut tools {
                        t.as_object_mut()?.remove("execution");
                    }
                }
                Some(rpc_ok(&req_id, &json!({"tools":tools})))
            }
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
                if self.version == MCP_PROTOCOL_LATEST =>
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
        let canonical = match name {
            "arb_export_share_mp4" | "arb_trim_bundle" => {
                let format = if name == "arb_trim_bundle" {
                    "bundle"
                } else {
                    "share_mp4"
                };
                if let Some(obj) = args.as_object_mut() {
                    if obj.get("format").is_some_and(|v| v != format) {
                        return rpc_ok(
                            req_id,
                            &Failure::invalid("format がツール名と一致しません").result(),
                        );
                    }
                    obj.insert("format".into(), json!(format));
                }
                "arb_export_trim"
            }
            _ => name,
        };
        let Some(def) = contract::for_version(&self.version)
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
        let long = def.get("execution").is_some();
        let progress = long && (canonical != "arb_capture_start" || args["wait_connected"] == true);
        let activity = Activity::new(canonical, token.filter(|_| progress));
        if let Some(task) = params.get("task") {
            if self.version != MCP_PROTOCOL_LATEST || !long {
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

    fn run_tool(&mut self, def: &Value, mut args: Value) -> Value {
        self.activity.notify(&self.session, "開始");
        let name = def["name"].as_str().expect("tool name");
        schema::normalize_integers(&def["inputSchema"], &mut args);
        let output = schema::validate(&def["inputSchema"], &args, "arguments")
            .map_err(|message| {
                let mut err = Failure::invalid(message);
                err.hint = match name {
                    "arb_export_trim" if args.get("trash_original").is_some() => {
                        Some("元バンドルをごみ箱に移すには arb_trash_bundle を実行してください。")
                    }
                    "arb_screenshot" if args.get("overwrite").is_some() => {
                        Some("別名保存には新しい output_path を指定してください。")
                    }
                    _ => None,
                };
                err
            })
            .and_then(|()| self.dispatch_tool(name, args.as_object().expect("validated object")));
        self.activity.finish(&self.session, output.as_ref().err());
        match output {
            Ok(output) => output.result(&self.version),
            Err(failure) => failure.result(),
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

    fn dispatch_tool(
        &mut self,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        match name {
            "arb_status" => self.tool_status(),
            "arb_devices" => {
                let resp = self.call_daemon("device.list", &json!({}))?;
                Ok(ToolOutput::new(resp))
            }
            "arb_capture_start" => self.tool_capture_start(args),
            "arb_capture_stop" => {
                let mut resp = self.call_daemon("capture.stop", &json!({}))?;
                summarize_audio_reason(&mut resp);
                Ok(ToolOutput::new(resp))
            }
            "arb_config_get" => {
                let resp = self.call_daemon("config.get", &json!({}))?;
                Ok(ToolOutput::new(resp))
            }
            "arb_config_update" => self.tool_config_update(args),
            "arb_save_and_inspect" => self.save_and_inspect(args),
            "arb_save" => self.tool_save(args),
            "arb_screenshot" => self.tool_screenshot(args),
            "arb_save_logs" => self.tool_save_logs(args),
            "arb_list_bundles" => {
                let state = args.get("state").cloned().unwrap_or(Value::Null);
                let mut resp = self.call_daemon("bundle.list", &json!({ "state": state }))?;
                for row in resp["bundles"].as_array_mut().into_iter().flatten() {
                    self.catalog_row(row);
                }
                Ok(ToolOutput::new(resp))
            }
            "arb_trash_bundle" => {
                let bundle_id = require_str(args, "bundle_id")?;
                let mut resp =
                    self.call_daemon("bundle.trash", &json!({ "bundle_id": bundle_id }))?;
                self.catalog_row(&mut resp);
                Ok(ToolOutput::new(resp))
            }
            "arb_restore_bundle" => {
                let bundle_id = require_str(args, "bundle_id")?;
                let mut resp =
                    self.call_daemon("bundle.restore", &json!({ "bundle_id": bundle_id }))?;
                self.catalog_row(&mut resp);
                Ok(ToolOutput::new(resp))
            }
            "arb_gc_bundles" => {
                let resp = self.call_daemon("bundle.gc", &Value::Object(args.clone()))?;
                Ok(ToolOutput::new(resp))
            }
            "arb_forget_bundle" => {
                let resp = self.call_daemon("bundle.forget", &Value::Object(args.clone()))?;
                Ok(ToolOutput::new(resp))
            }
            "arb_timeline_overview" => self.tool_timeline_overview(args),
            "arb_query_logs" => self.tool_query_logs(args),
            "arb_inspect_frames" => self.tool_inspect_frames(args),
            "arb_export_trim" => self.tool_export_trim(args),
            "arb_export_frame_png" => self.tool_export_frame_png(args),
            other => Err(Failure::invalid(format!("unknown tool `{other}`"))),
        }
    }

    fn catalog_row(&self, row: &mut Value) {
        fill_missing(
            row,
            &[
                "bundle_kind",
                "duration_ms",
                "frame_count",
                "has_video",
                "has_audio",
                "log_count",
                "crash_marker_count",
                "memo",
                "disk_state",
            ],
        );
        if self.version == MCP_PROTOCOL_LATEST {
            if let Some(obj) = row.as_object_mut() {
                obj.remove("created_utc");
            }
        } else {
            fill_missing(row, &["created_utc"]);
        }
    }

    fn tool_status(&mut self) -> Result<ToolOutput, Failure> {
        let mut hello = self.call_daemon("system.hello", &json!({}))?;
        if hello.get("gc_dry_run").is_none() {
            hello["gc_dry_run"] = json!(false);
        }
        let mut status = self.call_daemon("system.status", &json!({}))?;
        summarize_audio_reason(&mut status);
        Ok(ToolOutput::new(json!({"hello":hello,"status":status})))
    }

    fn tool_capture_start(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let mut params = args.clone();
        for key in ["companion_jar", "scrcpy_jar"] {
            absolutize_optional_path_field(&mut params, key)?;
        }
        let mut resp = self.call_daemon("capture.start", &Value::Object(params))?;
        if resp.get("connected").is_none() {
            let status = self.call_daemon("system.status", &json!({}))?;
            let capture = &status["capture"];
            let current = resp["started"] == true && capture["serial"] == resp["serial"];
            resp["connected"] = json!(current && capture["connected"] == true);
            resp["audio_state"] = if current {
                capture["audio_state"].clone()
            } else {
                json!("unknown")
            };
            resp["audio_cause"] = if current {
                capture["audio_cause"].clone()
            } else {
                Value::Null
            };
            resp["wait_timed_out"] = json!(false);
        }
        Ok(ToolOutput::new(resp))
    }

    fn tool_config_update(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let mut patch = args.clone();
        for key in ["output_dir_override", "screenshot_dir_override"] {
            if let Some(val) = patch.get(key).and_then(Value::as_str)
                && !val.is_empty()
            {
                let abs = absolutize_path(Path::new(val)).map_err(|e| e.message)?;
                patch.insert(key.to_owned(), json!(abs));
            }
        }
        let resp = self.call_daemon("config.update", &Value::Object(patch))?;
        Ok(ToolOutput::new(resp))
    }

    fn tool_save(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let mut params = Map::new();
        if let Some(v) = args
            .get("duration_secs")
            .or_else(|| args.get("window_secs"))
            .or_else(|| args.get("duration_sec"))
        {
            params.insert("duration_sec".to_owned(), v.clone());
        }
        if let Some(dest) = optional_abs_path(args, "dest_dir")? {
            params.insert("dest_dir".to_owned(), json!(dest));
        }
        if let Some(drain) = args.get("drain_timeout_ms") {
            params.insert("drain_timeout_ms".to_owned(), drain.clone());
        }
        if let Some(note) = args.get("note") {
            params.insert("note".into(), note.clone());
        }
        let resp = self.call_daemon("save.trigger", &Value::Object(params))?;
        save_content(resp)
    }

    fn tool_screenshot(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let include_image = args
            .get("include_image")
            .and_then(Value::as_bool)
            .unwrap_or(self.version != MCP_PROTOCOL_LATEST);
        let mut params = Map::new();
        if let Some(serial) = args.get("serial") {
            params.insert("serial".to_owned(), serial.clone());
        }
        if let Some(out) = optional_abs_path(args, "output_path")? {
            params.insert("output_path".to_owned(), json!(out));
        }
        if let Some(out_dir) = optional_abs_path(args, "output_dir")? {
            params.insert("output_dir".to_owned(), json!(out_dir));
        }
        if let Some(overwrite) = args.get("overwrite") {
            params.insert("overwrite".to_owned(), overwrite.clone());
        }
        let mut resp = self.call_daemon("save.screenshot", &Value::Object(params))?;
        if self.version == MCP_PROTOCOL_LATEST
            && let Some(uri) = resp["path"]
                .as_str()
                .and_then(|path| url::Url::from_file_path(path).ok())
        {
            resp["file_uri"] = json!(uri.as_str());
        }
        let mut content = Vec::with_capacity(2);
        if include_image
            && let Some(path_str) = resp.get("path").and_then(Value::as_str)
            && let Ok(bytes) = fs::read(path_str)
        {
            content.push(image_block(&base64_encode(&bytes)));
        }
        Ok(ToolOutput {
            body: resp,
            blocks: content,
        })
    }

    fn tool_save_logs(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let mut params = Map::new();
        if let Some(v) = args
            .get("duration_secs")
            .or_else(|| args.get("window_secs"))
            .or_else(|| args.get("duration_sec"))
        {
            params.insert("window_secs".to_owned(), v.clone());
        }
        if let Some(dest) = optional_abs_path(args, "dest_dir")? {
            params.insert("dest_dir".to_owned(), json!(dest));
        }
        if let Some(drain) = args.get("drain_timeout_ms") {
            params.insert("drain_timeout_ms".to_owned(), drain.clone());
        }
        if let Some(note) = args.get("note").or_else(|| args.get("memo")) {
            params.insert("note".into(), note.clone());
        }
        let resp = self.call_daemon("save.logs", &Value::Object(params))?;
        save_content(resp)
    }

    fn tool_timeline_overview(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = Map::new();
        params.insert("bundle_id".to_owned(), Value::String(bundle_id));
        for key in ["max_error_logs", "package", "tag", "exclude_tags"] {
            if let Some(value) = args.get(key) {
                params.insert(key.to_owned(), value.clone());
            }
        }
        let mut resp = self.call_daemon("timeline.overview", &Value::Object(params))?;
        if self.version == MCP_PROTOCOL_LATEST
            && let Some(obj) = resp.as_object_mut()
            && let Some(detail) = obj.remove("logcat_completeness_detail")
        {
            obj.insert("logcat_completeness".into(), detail);
        }
        Ok(ToolOutput::new(resp))
    }

    fn tool_query_logs(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id_or_path".to_owned(), Value::String(bundle_id));
        params.remove("bundle_id");
        // The existing MCP schema advertises Android's legacy ASSERT abbreviation.
        if params
            .get("level")
            .and_then(Value::as_str)
            .is_some_and(|v| v.eq_ignore_ascii_case("A"))
        {
            params.insert("level".into(), Value::String("F".into()));
        }
        params.entry("limit").or_insert(json!(100));
        params.entry("bucket").or_insert(json!("main"));
        let resp = self.call_daemon("bundle.query_logs", &Value::Object(params))?;
        Ok(ToolOutput::new(resp))
    }

    fn tool_inspect_frames(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id".to_owned(), json!(bundle_id));
        params.remove("bundle_id_or_path");
        let inline = params.remove("include_images") == Some(Value::Bool(true));
        params.insert("metadata_only".into(), json!(!inline));
        let mut resp = self.call_daemon("media.inspect_frames", &Value::Object(params))?;

        let mut blocks = Vec::new();
        if inline {
            blocks.push(image_block(&take_png(&mut resp)?));
        } else {
            if !resp["frames"].is_array() {
                return Err(Failure::new(
                    -32601,
                    "DAEMON_UNAVAILABLE",
                    "フレームの遅延取得には arbd の更新が必要です",
                ));
            }
            let resource_id = self.frame_resource_id(&bundle_id, &resp["bundle_id"])?;
            for index in resp["frame_indices_rendered"]
                .as_array()
                .into_iter()
                .flatten()
            {
                blocks.push(resources::bundle_link(
                    &resource_id,
                    &format!("frame/{index}"),
                ));
            }
        }
        Ok(ToolOutput { body: resp, blocks })
    }

    fn save_and_inspect(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let mut save_args = Map::new();
        if let Some(v) = args.get("duration_secs") {
            save_args.insert("window_secs".into(), v.clone());
        }
        if let Some(memo) = args.get("memo") {
            save_args.insert("note".into(), memo.clone());
        }
        let mut result = if args.get("logs_only") == Some(&Value::Bool(true)) {
            self.tool_save_logs(&save_args)?
        } else {
            self.tool_save(&save_args)?
        };
        self.activity.notify(&self.session, "保存完了・証跡を確認");
        if let Err(mut error) = self.inspect_saved(args, &mut result) {
            error.detail = Some(json!({"save":result.body}));
            return Err(error);
        }
        Ok(result)
    }

    fn inspect_saved(
        &mut self,
        args: &Map<String, Value>,
        result: &mut ToolOutput,
    ) -> Result<(), Failure> {
        let id = result.body["bundle_id"].clone();
        let target = json!({"bundle_id_or_path":id});
        let summary = self.call_daemon("timeline.summary", &target)?;
        let max_errors = args.get("max_errors").cloned().unwrap_or(json!(20));
        let (screens, crashes, error_logs) = if self.version == MCP_PROTOCOL_LATEST {
            let mut overview = self.call_daemon(
                "timeline.overview",
                &json!({"bundle_id":id,"max_error_logs":max_errors}),
            )?;
            if !overview["unique_error_logs"].is_u64() {
                return Err(Failure::unavailable(
                    "エラーを集約するには arbd を更新してください",
                ));
            }
            let logs = overview["error_log_highlights"].take();
            let shown = logs.as_array().map_or(0, Vec::len);
            let errors = json!({"logs":logs,"total_matched":overview["total_error_logs"],
                "unique_count":overview["unique_error_logs"], "returned_count":shown,"has_more":overview["error_logs_truncated"]});
            (
                overview["screens"].take(),
                overview["crashes"].take(),
                errors,
            )
        } else {
            let screens = self.call_daemon("timeline.screens", &target)?["screens"].take();
            let crashes = self.call_daemon("timeline.crashes", &target)?["crashes"].take();
            let errors = self.call_daemon(
                "bundle.query_logs",
                &json!({"bundle_id_or_path":id,"level":"ERROR","tail":true,"limit":max_errors}),
            )?;
            (screens, crashes, errors)
        };
        result.blocks.push(resources::bundle_link(&id, "crashes"));
        if args.get("include_key_frame_links") != Some(&Value::Bool(false)) {
            for frame in screens
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["start_frame_index"].as_u64())
            {
                result
                    .blocks
                    .push(resources::bundle_link(&id, &format!("frame/{frame}")));
            }
        }
        result.body = json!({"save":result.body,"summary":summary,"screens":screens,"crashes":crashes,"error_logs":error_logs,"memo":args.get("memo")});
        Ok(())
    }

    fn tool_export_trim(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let format = args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("share_mp4");
        let trash_original = args
            .get("trash_original")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let burn_taps = args
            .get("burn_taps")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let include_share_mp4 = args
            .get("include_share_mp4")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        match format {
            "share_mp4" => {
                let resp = self.call_daemon(
                    "export.share_mp4",
                    &json!({
                        "bundle_id": bundle_id,
                        "in_frame_index": args.get("in_frame_index"),
                        "out_frame_index": args.get("out_frame_index"),
                        "in_time_ns": args.get("in_time_ns"),
                        "out_time_ns": args.get("out_time_ns"),
                        "output_path": optional_abs_path(args, "output_path")?,
                        "dest_dir": optional_abs_path(args, "dest_dir")?,
                        "keep_bundle": args.get("keep_bundle").and_then(Value::as_bool).unwrap_or(false),
                        "burn_taps": burn_taps,
                        "trash_original": trash_original,
                        "holder_pid": std::process::id(),
                    }),
                )?;
                Ok(ToolOutput::new(resp))
            }
            "bundle" => {
                let resp = self.call_daemon(
                    "export.trim",
                    &json!({
                        "bundle_id_or_path": bundle_id,
                        "in_frame_index": args.get("in_frame_index"),
                        "out_frame_index": args.get("out_frame_index"),
                        "in_time_ns": args.get("in_time_ns"),
                        "out_time_ns": args.get("out_time_ns"),
                        "include_share_mp4": include_share_mp4,
                        "share_output_path": optional_abs_path(args, "output_path")?,
                        "burn_taps": burn_taps,
                        "dest_dir": optional_abs_path(args, "dest_dir")?,
                        "trash_original": trash_original,
                        "holder_pid": std::process::id(),
                    }),
                )?;
                Ok(ToolOutput::new(resp))
            }
            other => Err(Failure::invalid(format!("未対応の出力形式: {other}"))),
        }
    }

    fn tool_export_frame_png(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let frame_index = args
            .get("frame_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| "arb_export_frame_png requires integer `frame_index`".to_owned())?;
        let out_raw = require_str(args, "output_path")?;
        let out_abs = absolutize_path(Path::new(out_raw)).map_err(|e| e.message)?;
        let resp = self.call_daemon(
            "export.png",
            &json!({
                "bundle_id_or_path": bundle_id,
                "frame_index": frame_index,
                "output_path": out_abs,
            }),
        )?;
        Ok(ToolOutput::new(resp))
    }
}

fn handle_initialize(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(MCP_PROTOCOL_LATEST);
    let negotiated = match requested {
        MCP_PROTOCOL_LATEST | "2025-06-18" | "2025-03-26" | "2024-11-05" => requested,
        _ => MCP_PROTOCOL_LATEST,
    };
    let mut capabilities = json!({"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false},"prompts":{"listChanged":false},"logging":{},"completions":{}});
    if negotiated == MCP_PROTOCOL_LATEST {
        capabilities["tasks"] = json!({"list":{},"cancel":{},"requests":{"tools":{"call":{}}}});
    }
    json!({
        "protocolVersion":negotiated, "capabilities":capabilities,
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

#[derive(Debug)]
pub(super) struct ToolOutput {
    pub(super) body: Value,
    blocks: Vec<Value>,
}

impl ToolOutput {
    const fn new(body: Value) -> Self {
        Self {
            body,
            blocks: Vec::new(),
        }
    }
    fn result(self, version: &str) -> Value {
        let (mut content, links): (Vec<Value>, Vec<Value>) =
            self.blocks.into_iter().partition(|v| v["type"] == "image");
        content.push(json!({"type":"text","text":self.body.to_string()}));
        for block in links {
            if version >= "2025-06-18" {
                content.push(block);
            } else {
                content.push(json!({"type":"text","text":block["uri"]}));
            }
        }
        json!({"isError":false,"structuredContent":self.body,"content":content})
    }
}

fn summarize_audio_reason(response: &mut Value) {
    if let Some(reason) = response.pointer_mut("/capture/audio_reason")
        && let Some(detail) = reason.as_str()
    {
        let message = detail.split("; scrcpy stderr:").next().unwrap_or(detail);
        *reason = Value::String(
            message
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(256)
                .collect(),
        );
    }
}

/// `warning_details` for a daemon that predates it: each code paired with the display text at
/// the same position (older daemons omitted the text of a trailing `window_shortfall`).
fn legacy_warning_details(obj: &serde_json::Map<String, Value>) -> Value {
    let codes = obj.get("warning_codes").and_then(Value::as_array);
    let messages = obj.get("warnings").and_then(Value::as_array);
    Value::Array(
        codes
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, code)| {
                let message = messages
                    .and_then(|m| m.get(i))
                    .filter(|m| m.is_string())
                    .unwrap_or(code);
                json!({"code": code, "message": message})
            })
            .collect(),
    )
}

fn fill_missing(body: &mut Value, names: &[&str]) {
    if let Some(obj) = body.as_object_mut() {
        for &key in names {
            obj.entry(key).or_insert(Value::Null);
        }
    }
}

pub(super) fn save_content(mut response: Value) -> Result<ToolOutput, Failure> {
    if let Some(obj) = response.as_object_mut() {
        obj.entry("warning_codes").or_insert_with(|| json!([]));
        if !obj.contains_key("warning_details") {
            let details = legacy_warning_details(obj);
            obj.insert("warning_details".into(), details);
        }
    }
    fill_missing(
        &mut response,
        &[
            "video_shortfall",
            "log_shortfall",
            "logcat_completeness",
            "requested_duration_secs",
            "actual_video_duration_ms",
            "actual_log_duration_ms",
            "video_frame_count",
            "log_count",
            "unanchored_log_count",
            "truncated_unanchored_count",
            "discontinuity_count",
            "window_shortfall",
            "warnings",
        ],
    );
    if super::save_outcome_failed(&response) {
        return Err(Failure {
            rpc_code: -32006,
            label: "FAILED_RETRYABLE",
            message: "保存に失敗しました。ジョブの状態を確認してください。".into(),
            detail: Some(response),
            hint: None,
        });
    }
    let blocks = ["manifest", "logs", "screens"]
        .map(|r| resources::bundle_link(&response["bundle_id"], r))
        .into();
    Ok(ToolOutput {
        body: response,
        blocks,
    })
}

#[derive(Debug)]
pub(super) struct Failure {
    rpc_code: i32,
    label: &'static str,
    message: String,
    detail: Option<Value>,
    hint: Option<&'static str>,
}
impl Failure {
    fn new(code: i32, label: &'static str, message: impl Into<String>) -> Self {
        Self {
            rpc_code: code,
            label,
            message: message.into(),
            detail: None,
            hint: None,
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new(JSONRPC_INVALID_PARAMS, "INVALID_PARAMS", message)
    }
    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(-32000, "DAEMON_UNAVAILABLE", message)
    }
    pub(super) fn from_rpc(e: RpcClientError) -> Self {
        match e {
            RpcClientError::Rpc {
                code,
                message,
                error_code,
            } => Self::new(
                code,
                match (code, error_code.as_deref()) {
                    (-32002, Some("ALREADY_ACTIVE")) => "ALREADY_ACTIVE",
                    (-32002, Some("ALREADY_TRASHED")) => "ALREADY_TRASHED",
                    _ => arb_os::rpc::error_label(code),
                },
                message,
            ),
            _ => Self::unavailable(e.to_string()),
        }
    }
    pub(super) fn result(&self) -> Value {
        let mut body = self.detail.clone().unwrap_or_else(|| json!({}));
        let (kind, hint, retryable) = arb_os::rpc::error_guidance(self.label);
        body["kind"] = json!(kind);
        body["hint"] = json!(self.hint.unwrap_or(hint));
        body["retryable"] = json!(retryable);
        body["error_code"] = json!(self.label);
        body["rpc_code"] = json!(self.rpc_code);
        body["message"] = json!(self.message);
        json!({"isError":true,"content":[{"type":"text","text":body.to_string()}],"structuredContent":body})
    }
}
impl From<String> for Failure {
    fn from(s: String) -> Self {
        Self::invalid(s)
    }
}
impl From<&str> for Failure {
    fn from(s: &str) -> Self {
        Self::invalid(s)
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
    json!({
        "type": "image",
        "data": base64_png,
        "mimeType": "image/png",
    })
}

fn require_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing required non-empty string `{key}`"))
}

fn require_bundle_target(args: &Map<String, Value>) -> Result<String, String> {
    let raw = args
        .get("bundle_id")
        .or_else(|| args.get("bundle_id_or_path"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing required string `bundle_id`".to_owned())?;
    absolutize_bundle_target(raw).map_err(|e| e.message)
}

fn optional_abs_path(args: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    let Some(val) = args.get(key) else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let Some(raw) = val.as_str() else {
        return Err(format!("`{key}` must be a string path"));
    };
    let abs = absolutize_path(Path::new(raw)).map_err(|e| e.message)?;
    Ok(Some(abs.to_string_lossy().into_owned()))
}

fn absolutize_optional_path_field(
    params: &mut Map<String, Value>,
    key: &str,
) -> Result<(), String> {
    if let Some(raw) = params.get(key).and_then(Value::as_str) {
        let abs = absolutize_path(Path::new(raw)).map_err(|e| e.message)?;
        params.insert(key.to_owned(), json!(abs));
    }
    Ok(())
}

/// Pure-Rust RFC 4648 standard Base64 encoder (zero dependencies).
use arb_core::base64_encode;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use std::io::Cursor;

    use super::{
        LineRead, McpServer, base64_encode, contract::TOOLS, is_retry_safe_method,
        read_bounded_line,
    };
    use arb_os::paths::ArbPaths;
    use serde_json::{Value, json};

    #[test]
    fn tool_registry_matches_known_tools_with_valid_schemas() {
        let defs = &*TOOLS;
        assert_eq!(defs.len(), 20);
        let mut names = BTreeSet::new();
        for def in defs {
            let name = def["name"].as_str().expect("tool must have string name");
            assert!(
                names.insert(name.to_owned()),
                "duplicate tool name `{name}`"
            );
            assert!(
                !def["description"].as_str().unwrap_or_default().is_empty(),
                "tool `{name}` must have non-empty description"
            );
            assert_eq!(
                def["inputSchema"]["type"], "object",
                "tool `{name}` inputSchema must be a JSON Schema object"
            );
        }
    }

    #[test]
    fn mcp_protocol_initialize_ping_and_notification_silence() {
        let paths = ArbPaths {
            data_dir: std::env::temp_dir().join("arb-mcp-unit-d"),
            cache_dir: std::env::temp_dir().join("arb-mcp-unit-c"),
            runtime_dir: std::env::temp_dir().join("arb-mcp-unit-r"),
        };
        let mut srv = McpServer::new(paths);

        let init = srv
            .handle_line(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": { "protocolVersion": "2024-11-05" }
                })
                .to_string(),
            )
            .expect("initialize must emit a response");
        assert_eq!(init["id"], 1, "echoed id");
        assert_eq!(
            init["result"]["protocolVersion"], "2024-11-05",
            "negotiated protocol version"
        );

        let notif = srv.handle_line(
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })
            .to_string(),
        );
        assert!(
            notif.is_none(),
            "notifications/initialized must emit zero bytes to stdout"
        );

        let ping = srv
            .handle_line(&json!({ "jsonrpc": "2.0", "id": "p1", "method": "ping" }).to_string())
            .expect("ping must respond");
        assert_eq!(ping["id"], "p1", "ping id");
        assert_eq!(ping["result"], json!({}), "ping empty result");

        let bad = srv
            .handle_line("not valid json")
            .expect("parse error must respond");
        assert_eq!(bad["error"]["code"], -32_700, "parse error code");

        let unknown_tool = srv
            .handle_line(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 9,
                    "method": "tools/call",
                    "params": { "name": "nonexistent_tool", "arguments": {} }
                })
                .to_string(),
            )
            .expect("unknown tool must respond");
        assert_eq!(
            unknown_tool["error"]["code"], -32_602,
            "unknown tool must return invalid params"
        );
    }

    #[test]
    fn mcp_audio_reason_omits_stack_without_losing_cause() {
        let mut response = json!({"capture":{"audio_cause":"encoder_fatal","audio_reason":format!("録音に失敗; scrcpy stderr: {}","スタック\n".repeat(500))}});
        super::summarize_audio_reason(&mut response);
        assert_eq!(response["capture"]["audio_reason"], "録音に失敗");
        assert_eq!(response["capture"]["audio_cause"], "encoder_fatal");
    }

    #[test]
    fn base64_encode_matches_standard_vectors() {
        assert_eq!(base64_encode(b""), "", "empty");
        assert_eq!(base64_encode(b"a"), "YQ==", "1 byte");
        assert_eq!(base64_encode(b"ab"), "YWI=", "2 bytes");
        assert_eq!(base64_encode(b"abc"), "YWJj", "3 bytes");
        let parsed: Value = json!({ "b64": base64_encode(b"\x89PNG\r\n\x1a\n") });
        assert_eq!(parsed["b64"], "iVBORw0KGgo=", "PNG signature base64");
    }

    #[test]
    fn mutating_methods_are_never_retry_safe() {
        for m in [
            "save.trigger",
            "export.trim",
            "export.share_mp4",
            "bundle.trash",
            "capture.start",
            "save.screenshot",
        ] {
            assert!(!is_retry_safe_method(m), "{m} must not be auto-retried");
        }
        assert!(is_retry_safe_method("timeline.overview"));
    }

    #[test]
    fn bounded_line_reader_survives_invalid_utf8_and_oversize() {
        let mut data: Vec<u8> = vec![0xFF, 0xFE, b'\n'];
        data.extend(std::iter::repeat_n(b'a', 64));
        data.extend_from_slice(b"\nok\n");
        let mut r = Cursor::new(data);
        let mut buf = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut r, &mut buf, 16).unwrap(),
            LineRead::Line
        ));
        assert!(std::str::from_utf8(&buf).is_err());
        buf.clear();
        assert!(matches!(
            read_bounded_line(&mut r, &mut buf, 16).unwrap(),
            LineRead::TooLong
        ));
        buf.clear();
        assert!(matches!(
            read_bounded_line(&mut r, &mut buf, 16).unwrap(),
            LineRead::Line
        ));
        assert_eq!(buf, b"ok");
        buf.clear();
        assert!(matches!(
            read_bounded_line(&mut r, &mut buf, 16).unwrap(),
            LineRead::Eof
        ));
    }

    #[test]
    fn json_rpc_batch_answers_each_request_and_omits_notifications() {
        let paths = ArbPaths {
            data_dir: std::env::temp_dir().join("arb-mcp-unit-bd"),
            cache_dir: std::env::temp_dir().join("arb-mcp-unit-bc"),
            runtime_dir: std::env::temp_dir().join("arb-mcp-unit-br"),
        };
        let mut srv = McpServer::new(paths);
        let batch = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "id": 2, "method": "tools/list" }
        ]);
        for version in ["2025-11-25", "2025-06-18", "2025-03-26"] {
            let init = srv.handle_message(&json!({
                "jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":version}
            })).unwrap();
            assert_eq!(init["result"]["protocolVersion"], version);
            if version != "2025-03-26" {
                assert_eq!(
                    srv.handle_line(&batch.to_string()).unwrap()["error"]["code"],
                    -32600
                );
            }
        }
        let resp = srv.handle_line(&batch.to_string()).expect("batch response");
        let arr = resp.as_array().expect("array response");
        assert_eq!(arr.len(), 2, "notification must not produce a response");
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[1]["id"], 2);
        let empty = srv.handle_line("[]").expect("empty batch error");
        assert_eq!(empty["error"]["code"], -32600);
    }
    #[test]
    fn previous_daemon_reports_safe_gc_capability_and_actual_capture_state() {
        use arb_os::ipc::{
            DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, read_frame_line, write_frame_line,
        };
        let tmp = tempfile::tempdir().unwrap();
        let paths = ArbPaths {
            data_dir: tmp.path().join("data"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: tmp.path().join("run"),
        };
        let endpoint = paths.ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).unwrap();
        let daemon = std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(listener.accept().unwrap());
            for (method, result) in [
                (
                    "system.hello",
                    json!({"protocol_version":arb_os::rpc::EXPECTED_IPC_PROTOCOL_VERSION}),
                ),
                ("system.status", json!({})),
                (
                    "capture.start",
                    json!({"started":true,"serial":"fixture","capture_mode":"video_and_logs"}),
                ),
                (
                    "system.status",
                    json!({"capture":{"serial":"fixture","connected":true,"audio_state":"available"}}),
                ),
            ] {
                let line = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                    .unwrap()
                    .unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(req["method"], method);
                let reply = json!({"jsonrpc":"2.0","id":req["id"],"result":result});
                write_frame_line(
                    reader.get_mut(),
                    &reply.to_string(),
                    DEFAULT_MAX_IPC_LINE_BYTES,
                )
                .unwrap();
            }
            reader.get_mut().drain().unwrap();
        });
        let mut mcp = McpServer::new(paths);
        mcp.client = Some(super::RpcClient::connect_endpoint(&endpoint).unwrap());
        assert_eq!(
            mcp.tool_status().unwrap().body["hello"]["gc_dry_run"],
            false
        );
        let capture = mcp
            .tool_capture_start(&serde_json::Map::new())
            .unwrap()
            .body;
        assert_eq!(capture["connected"], true);
        assert_eq!(capture["wait_timed_out"], false);
        assert_eq!(capture["audio_state"], "available");
        assert!(capture["audio_cause"].is_null());
        let schema = super::contract::for_version("2025-11-25")
            .iter()
            .find(|t| t["name"] == "arb_capture_start")
            .unwrap();
        super::schema::validate(&schema["outputSchema"], &capture, "capture.start").unwrap();
        drop(mcp);
        daemon.join().unwrap();
    }

    #[test]
    fn cancelling_during_hello_prevents_the_first_mutation() {
        use arb_os::ipc::{
            DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, read_frame_line, write_frame_line,
        };
        use std::sync::mpsc;
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();
        let paths = ArbPaths {
            data_dir: tmp.path().join("data"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: tmp.path().join("run"),
        };
        let listener = LocalIpcListener::bind(paths.ipc_endpoint()).unwrap();
        let (began, hello) = mpsc::channel();
        let (release, cancelled) = mpsc::channel();
        let daemon = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let line = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                .unwrap()
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "system.hello");
            began.send(()).unwrap();
            cancelled.recv_timeout(Duration::from_secs(5)).unwrap();
            let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocol_version":arb_os::rpc::EXPECTED_IPC_PROTOCOL_VERSION}});
            write_frame_line(
                reader.get_mut(),
                &response.to_string(),
                DEFAULT_MAX_IPC_LINE_BYTES,
            )
            .unwrap();
            assert!(
                read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                    .unwrap()
                    .is_none()
            );
        });
        let mut mcp = McpServer::new(paths);
        let created = mcp.handle_tools_call(
            &json!(1),
            &json!({"name":"arb_save","arguments":{},"task":{}}),
        );
        hello.recv_timeout(Duration::from_secs(5)).unwrap();
        let task = &created["result"]["task"];
        let result = mcp
            .session
            .request("tasks/cancel", &json!(2), &json!({"taskId":task["taskId"]}))
            .unwrap();
        assert_eq!(result["result"]["status"], "cancelled");
        release.send(()).unwrap();
        daemon.join().unwrap();
    }

    #[test]
    fn mutation_timeout_is_reported_without_connecting_again() {
        use arb_os::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, read_frame_line};
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();
        let paths = arb_os::paths::ArbPaths {
            data_dir: tmp.path().join("data"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: tmp.path().join("run"),
        };
        let endpoint = paths.ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).unwrap();
        let server = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let req = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&req).unwrap()["method"],
                "save.trigger"
            );
            // Keep the listener alive while withholding the response.
            assert!(
                read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                    .unwrap()
                    .is_none()
            );
        });
        let mut client = super::RpcClient::connect_endpoint(&endpoint).unwrap();
        client.set_default_timeout(Duration::from_millis(250));
        client.set_long_timeout(Duration::from_millis(250));
        let mut mcp = super::McpServer {
            client: Some(client),
            ..McpServer::new(paths)
        };
        let result = mcp
            .call_daemon("save.trigger", &serde_json::json!({}))
            .unwrap_err();
        assert!(result.message.contains("timed out"), "{result:?}");
        assert!(mcp.client.is_none());
        server.join().unwrap();
    }
    #[test]
    fn closed_client_reconnects_once_even_for_mutating_call() {
        use arb_os::ipc::{
            DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, read_frame_line, write_frame_line,
        };
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let paths = arb_os::paths::ArbPaths {
            data_dir: tmp.path().join("data"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: tmp.path().join("run"),
        };
        let endpoint = paths.ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).unwrap();
        let server = std::thread::spawn(move || {
            // Connection 1: withhold reply so the client times out and marks itself Closed.
            let first = listener.accept().unwrap();
            let mut r1 = std::io::BufReader::new(first);
            let _ = read_frame_line(&mut r1, DEFAULT_MAX_IPC_LINE_BYTES).unwrap();
            let _ = read_frame_line(&mut r1, DEFAULT_MAX_IPC_LINE_BYTES);
            drop(r1);

            // Connection 2: MCP reconnects -> system.hello, then save.trigger.
            let second = listener.accept().unwrap();
            let mut r2 = std::io::BufReader::new(second);
            for expected in ["system.hello", "save.trigger"] {
                let line = read_frame_line(&mut r2, DEFAULT_MAX_IPC_LINE_BYTES)
                    .unwrap()
                    .unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(req["method"], expected);
                let result = if expected == "system.hello" {
                    json!({ "protocol_version": arb_os::rpc::EXPECTED_IPC_PROTOCOL_VERSION })
                } else {
                    json!({ "bundle_id": "b-reconnected", "job": { "state": "completed" } })
                };
                let resp = json!({ "jsonrpc": "2.0", "id": req["id"], "result": result });
                write_frame_line(r2.get_mut(), &resp.to_string(), DEFAULT_MAX_IPC_LINE_BYTES)
                    .unwrap();
            }
            r2.get_mut().drain().unwrap();
        });

        let mut closed_client = super::RpcClient::connect_endpoint(&endpoint).unwrap();
        assert!(matches!(
            closed_client.call_with_timeout("system.status", json!({}), Duration::from_millis(120)),
            Err(super::RpcClientError::Timeout)
        ));
        let mut mcp = super::McpServer {
            client: Some(closed_client),
            ..McpServer::new(paths)
        };
        let resp = mcp
            .call_daemon("save.trigger", &json!({}))
            .expect("Closed client sent nothing so mutating call may reconnect once");
        assert_eq!(resp["bundle_id"], "b-reconnected");
        server.join().unwrap();
    }

    #[test]
    fn save_content_only_accepts_a_recovered_job_with_a_bundle() {
        let mut body = serde_json::json!({"job":{"job_id":"same-job","state":"failed_retryable","pinned_chunk_ids":["c1"]}, "bundle_id":"b1"});
        assert!(super::save_content(body.clone()).is_err());
        body["job"]["state"] = serde_json::json!("completed");
        let content = super::save_content(body.clone()).unwrap();
        assert_eq!(content.body["bundle_id"], body["bundle_id"]);
        body["bundle_id"] = serde_json::Value::Null;
        assert!(super::save_content(body.clone()).is_err());
    }

    #[test]
    fn save_content_keeps_or_derives_warning_details_one_to_one() {
        // Current daemon: the canonical details pass through unchanged.
        let current = serde_json::json!({
            "job": {"job_id": "j", "state": "completed", "pinned_chunk_ids": []},
            "bundle_id": "b",
            "warning_codes": ["log_shortfall", "window_shortfall"],
            "warnings": ["ログが要求した保存時間に達していません", "要求した保存時間に達していない記録があります"],
            "warning_details": [
                {"code": "log_shortfall", "message": "ログが要求した保存時間に達していません"},
                {"code": "window_shortfall", "message": "要求した保存時間に達していない記録があります"}
            ]
        });
        let out = super::save_content(current.clone()).unwrap().body;
        assert_eq!(out["warning_details"], current["warning_details"]);
        // Older daemon: codes and texts were not one to one (no text for window_shortfall).
        let legacy = serde_json::json!({
            "job": {"job_id": "j", "state": "completed", "pinned_chunk_ids": []},
            "bundle_id": "b",
            "warning_codes": ["log_shortfall", "window_shortfall"],
            "warnings": ["ログが要求した保存時間に達していません"]
        });
        let out = super::save_content(legacy).unwrap().body;
        let details = out["warning_details"].as_array().unwrap();
        assert_eq!(details.len(), 2);
        assert_eq!(
            details[1],
            serde_json::json!({"code": "window_shortfall", "message": "window_shortfall"})
        );
    }
}
