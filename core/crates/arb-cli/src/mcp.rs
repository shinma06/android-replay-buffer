//! Stdio JSON-RPC 2.0 Model Context Protocol (MCP) server for `arb-cli mcp`.
//!
//! Exposes Android Replay Buffer live capture, WORM bundle inspection, contact-sheet /
//! tap-crop AI visual framing, and strict integer-frame trimming (`share.mp4` / WORM bundle)
//! over newline-delimited JSON-RPC 2.0 on stdin/stdout (`2025-03-26` & `2024-11-05` compatible).
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

const MCP_PROTOCOL_LATEST: &str = "2025-03-26";
const MCP_PROTOCOL_FALLBACK: &str = "2024-11-05";

const JSONRPC_PARSE_ERROR: i32 = -32_700;
const JSONRPC_INVALID_REQUEST: i32 = -32_600;
const JSONRPC_METHOD_NOT_FOUND: i32 = -32_601;
const JSONRPC_INVALID_PARAMS: i32 = -32_602;

const SERVER_INSTRUCTIONS: &str = "Android Replay Buffer (ARB) MCP server. \
Recommended AI triage & precision-trim workflow: \
(1) Call `arb_list_bundles` or `arb_save` to locate/create a WORM bundle. \
(2) Call `arb_timeline_overview` once to inspect screens, taps, crashes, gaps, and error logs. \
(3) Call `arb_inspect_frames` with `mode=\"contact_sheet\"` (up to 16 labeled tiles) to narrow the range, then `mode=\"single\"` or `mode=\"tap_crop\"` for exact boundaries. \
(4) Call `arb_export_trim` with exact integer `in_frame_index` and `out_frame_index` (`format=\"share_mp4\"` or `\"bundle\"`).";

/// Runs the blocking stdio MCP JSON-RPC 2.0 loop until stdin reaches EOF.
///
/// # Errors
///
/// Returns [`CliError`] if writing to stdout fails irrecoverably.
pub(crate) fn run_mcp_stdio(paths: &ArbPaths) -> Result<(), CliError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let reader = BufReader::new(stdin.lock());
    let writer = BufWriter::new(stdout.lock());
    let mut server = McpServer::new(paths.clone());
    server
        .serve_io(reader, writer)
        .map_err(|err| CliError::general(format!("mcp stdio I/O error: {err}")))
}

struct McpServer {
    paths: ArbPaths,
    client: Option<RpcClient>,
}

impl McpServer {
    const fn new(paths: ArbPaths) -> Self {
        Self {
            paths,
            client: None,
        }
    }

    fn serve_io<R: BufRead, W: Write>(&mut self, mut reader: R, mut writer: W) -> io::Result<()> {
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
                let encoded = serde_json::to_string(&resp)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                writer.write_all(encoded.as_bytes())?;
                writer.write_all(b"\n")?;
                writer.flush()?;
            }
        }
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
            "initialize" => Some(rpc_ok(&req_id, &handle_initialize(&params))),
            "ping" => Some(rpc_ok(&req_id, &json!({}))),
            "tools/list" => Some(rpc_ok(&req_id, &json!({ "tools": tool_definitions() }))),
            "resources/list" => Some(rpc_ok(&req_id, &json!({ "resources": [] }))),
            "prompts/list" => Some(rpc_ok(&req_id, &json!({ "prompts": [] }))),
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
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let Some(args_obj) = args.as_object() else {
            return rpc_error(
                req_id,
                JSONRPC_INVALID_PARAMS,
                "tools/call `arguments` must be a JSON object",
            );
        };

        if !is_known_tool(name) {
            return rpc_error(
                req_id,
                JSONRPC_INVALID_PARAMS,
                format!("unknown tool `{name}`"),
            );
        }

        match self.dispatch_tool(name, args_obj) {
            Ok(content) => rpc_ok(
                req_id,
                &json!({
                    "content": content,
                    "isError": false,
                }),
            ),
            Err(err_msg) => rpc_ok(
                req_id,
                &json!({
                    "content": [{ "type": "text", "text": err_msg }],
                    "isError": true,
                }),
            ),
        }
    }

    fn call_daemon(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        // A request that may already have reached the daemon is only replayed when the method is
        // read-only; mutating methods (save/export/trash/capture) are never retried automatically.
        let retry_safe = is_retry_safe_method(method);
        for attempt in 0..2 {
            if self.client.is_none() {
                let connected = RpcClient::connect(&self.paths, AutoStartMode::Enabled)
                    .map_err(|err| format!("failed to connect/auto-start arbd: {err}"))?;
                self.client = Some(connected);
            }
            let Some(client) = self.client.as_mut() else {
                return Err("arbd client unavailable".to_owned());
            };
            match client.call(method, params.clone()) {
                Ok(val) => return Ok(val),
                Err(
                    err @ (RpcClientError::UnexpectedEof
                    | RpcClientError::Os(_)
                    | RpcClientError::Io(_)),
                ) => {
                    self.client = None;
                    if !(retry_safe && attempt == 0) {
                        return Err(err.to_string());
                    }
                }
                Err(err) => return Err(err.to_string()),
            }
        }
        Err("arbd IPC connection failed after retry".to_owned())
    }

    fn dispatch_tool(
        &mut self,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<Vec<Value>, String> {
        match name {
            "arb_status" => self.tool_status(),
            "arb_devices" => {
                let resp = self.call_daemon("device.list", &json!({}))?;
                Ok(vec![text_block(&resp)])
            }
            "arb_capture_start" => self.tool_capture_start(args),
            "arb_capture_stop" => {
                let resp = self.call_daemon("capture.stop", &json!({}))?;
                Ok(vec![text_block(&resp)])
            }
            "arb_config_get" => {
                let resp = self.call_daemon("config.get", &json!({}))?;
                Ok(vec![text_block(&resp)])
            }
            "arb_config_update" => self.tool_config_update(args),
            "arb_save" => self.tool_save(args),
            "arb_screenshot" => self.tool_screenshot(args),
            "arb_save_logs" => self.tool_save_logs(args),
            "arb_list_bundles" => {
                let state = args.get("state").cloned().unwrap_or(Value::Null);
                let resp = self.call_daemon("bundle.list", &json!({ "state": state }))?;
                Ok(vec![text_block(&resp)])
            }
            "arb_trash_bundle" => {
                let bundle_id = require_str(args, "bundle_id")?;
                let resp = self.call_daemon("bundle.trash", &json!({ "bundle_id": bundle_id }))?;
                Ok(vec![text_block(&resp)])
            }
            "arb_restore_bundle" => {
                let bundle_id = require_str(args, "bundle_id")?;
                let resp =
                    self.call_daemon("bundle.restore", &json!({ "bundle_id": bundle_id }))?;
                Ok(vec![text_block(&resp)])
            }
            "arb_timeline_overview" => self.tool_timeline_overview(args),
            "arb_query_logs" => self.tool_query_logs(args),
            "arb_inspect_frames" => self.tool_inspect_frames(args),
            "arb_export_trim" => self.tool_export_trim(args),
            "arb_export_frame_png" => self.tool_export_frame_png(args),
            other => Err(format!("unknown tool `{other}`")),
        }
    }

    fn tool_status(&mut self) -> Result<Vec<Value>, String> {
        let hello = self.call_daemon("system.hello", &json!({}))?;
        let status = self.call_daemon("system.status", &json!({}))?;
        Ok(vec![text_block(
            &json!({ "hello": hello, "status": status }),
        )])
    }

    fn tool_capture_start(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let mut params = args.clone();
        for key in ["companion_jar", "scrcpy_jar"] {
            absolutize_optional_path_field(&mut params, key)?;
        }
        let resp = self.call_daemon("capture.start", &Value::Object(params))?;
        Ok(vec![text_block(&resp)])
    }

    fn tool_config_update(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
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
        Ok(vec![text_block(&resp)])
    }

    fn tool_save(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let mut params = Map::new();
        if let Some(v) = args.get("window_secs").or_else(|| args.get("duration_sec")) {
            params.insert("duration_sec".to_owned(), v.clone());
        }
        if let Some(dest) = optional_abs_path(args, "dest_dir")? {
            params.insert("dest_dir".to_owned(), json!(dest));
        }
        if let Some(drain) = args.get("drain_timeout_ms") {
            params.insert("drain_timeout_ms".to_owned(), drain.clone());
        }
        let mut resp = self.call_daemon("save.trigger", &Value::Object(params))?;
        if let Some(note) = args.get("note").and_then(Value::as_str)
            && let Some(obj) = resp.as_object_mut()
        {
            obj.insert("note".to_owned(), Value::String(note.to_owned()));
        }
        Ok(vec![text_block(&resp)])
    }

    fn tool_screenshot(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let include_image = args
            .get("include_image")
            .and_then(Value::as_bool)
            .unwrap_or(true);
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
        let resp = self.call_daemon("save.screenshot", &Value::Object(params))?;
        let mut content = Vec::with_capacity(2);
        if include_image
            && let Some(path_str) = resp.get("path").and_then(Value::as_str)
            && let Ok(bytes) = fs::read(path_str)
        {
            content.push(image_block(&base64_encode(&bytes)));
        }
        content.push(text_block(&resp));
        Ok(content)
    }

    fn tool_save_logs(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let mut params = Map::new();
        if let Some(v) = args.get("window_secs").or_else(|| args.get("duration_sec")) {
            params.insert("window_secs".to_owned(), v.clone());
        }
        if let Some(dest) = optional_abs_path(args, "dest_dir")? {
            params.insert("dest_dir".to_owned(), json!(dest));
        }
        if let Some(drain) = args.get("drain_timeout_ms") {
            params.insert("drain_timeout_ms".to_owned(), drain.clone());
        }
        let resp = self.call_daemon("save.logs", &Value::Object(params))?;
        Ok(vec![text_block(&resp)])
    }

    fn tool_timeline_overview(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = Map::new();
        params.insert("bundle_id".to_owned(), Value::String(bundle_id));
        if let Some(max_err) = args.get("max_error_logs") {
            params.insert("max_error_logs".to_owned(), max_err.clone());
        }
        let resp = self.call_daemon("timeline.overview", &Value::Object(params))?;
        Ok(vec![text_block(&resp)])
    }

    fn tool_query_logs(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id_or_path".to_owned(), Value::String(bundle_id));
        params.remove("bundle_id");
        let resp = self.call_daemon("timeline.query_logs", &Value::Object(params))?;
        Ok(vec![text_block(&resp)])
    }

    fn tool_inspect_frames(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id".to_owned(), Value::String(bundle_id));
        params.remove("bundle_id_or_path");
        let mut resp = self.call_daemon("media.inspect_frames", &Value::Object(params))?;

        let inline_b64 = resp
            .as_object_mut()
            .and_then(|obj| obj.remove("png_base64"))
            .and_then(|v| v.as_str().map(str::to_owned));

        let b64 = if let Some(encoded) = inline_b64 {
            encoded
        } else {
            let png_path = resp
                .get("png_path")
                .and_then(Value::as_str)
                .ok_or_else(|| "media.inspect_frames omitted png_path".to_owned())?;
            let bytes = fs::read(png_path)
                .map_err(|err| format!("failed to read rendered PNG `{png_path}`: {err}"))?;
            base64_encode(&bytes)
        };

        Ok(vec![image_block(&b64), text_block(&resp)])
    }

    fn tool_export_trim(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let bundle_id = require_bundle_target(args)?;
        let format = args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("share_mp4");
        let trash_original = args
            .get("trash_original")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        match format {
            "share_mp4" => {
                let mut params = Map::new();
                params.insert("bundle_id".to_owned(), Value::String(bundle_id));
                if let Some(v) = args.get("in_frame_index") {
                    params.insert("in_frame_index".to_owned(), v.clone());
                }
                if let Some(v) = args.get("out_frame_index") {
                    params.insert("out_frame_index".to_owned(), v.clone());
                }
                if let Some(out) = optional_abs_path(args, "output_path")? {
                    params.insert("output_path".to_owned(), json!(out));
                }
                if let Some(dest) = optional_abs_path(args, "dest_dir")? {
                    params.insert("dest_dir".to_owned(), json!(dest));
                }
                params.insert("trash_original".to_owned(), Value::Bool(trash_original));
                params.insert("holder_pid".to_owned(), json!(std::process::id()));
                let resp = self.call_daemon("export.share_mp4", &Value::Object(params))?;
                Ok(vec![text_block(&resp)])
            }
            "bundle" => {
                let in_f = args
                    .get("in_frame_index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        "format=\"bundle\" requires integer `in_frame_index`".to_owned()
                    })?;
                let out_f = args
                    .get("out_frame_index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        "format=\"bundle\" requires integer `out_frame_index`".to_owned()
                    })?;

                // Resolve the catalog id BEFORE trimming so a doomed trash never leaves an
                // untracked trimmed bundle behind.
                let mut trash_catalog_id: Option<String> = None;
                if trash_original {
                    let summary = self.call_daemon(
                        "timeline.summary",
                        &json!({ "bundle_id_or_path": bundle_id }),
                    )?;
                    if summary.get("is_legacy").and_then(Value::as_bool) == Some(true) {
                        return Err(format!(
                            "refusing trash_original on read-only legacy bundle `{bundle_id}`"
                        ));
                    }
                    trash_catalog_id = Some(self.resolve_active_catalog_id(&bundle_id, &summary)?);
                }

                let mut params = Map::new();
                params.insert(
                    "bundle_id_or_path".to_owned(),
                    Value::String(bundle_id.clone()),
                );
                params.insert("in_frame_index".to_owned(), json!(in_f));
                params.insert("out_frame_index".to_owned(), json!(out_f));
                if let Some(dest) = optional_abs_path(args, "dest_dir")? {
                    params.insert("dest_dir".to_owned(), json!(dest));
                }
                params.insert("holder_pid".to_owned(), json!(std::process::id()));

                let mut resp = self.call_daemon("export.trim", &Value::Object(params))?;
                let mut original_trashed = false;
                let mut trash_warning: Option<String> = None;
                if let Some(catalog_id) = trash_catalog_id {
                    match self.call_daemon("bundle.trash", &json!({ "bundle_id": catalog_id })) {
                        Ok(_) => original_trashed = true,
                        Err(err) => {
                            trash_warning = Some(format!(
                                "trimmed bundle was created but trashing the original failed: {err}"
                            ));
                        }
                    }
                }
                if let Some(obj) = resp.as_object_mut() {
                    obj.insert("original_trashed".to_owned(), Value::Bool(original_trashed));
                    if let Some(warning) = trash_warning {
                        obj.insert("trash_warning".to_owned(), Value::String(warning));
                    }
                }
                Ok(vec![text_block(&resp)])
            }
            other => Err(format!(
                "unsupported format `{other}`; expected \"share_mp4\" or \"bundle\""
            )),
        }
    }

    /// Maps a user-supplied bundle target (catalog id or path) to an active, non-legacy
    /// catalog id, or fails when the bundle is not cataloged.
    fn resolve_active_catalog_id(
        &mut self,
        target: &str,
        summary: &Value,
    ) -> Result<String, String> {
        let list = self.call_daemon("bundle.list", &json!({ "state": "active" }))?;
        let summary_path = summary.get("path").and_then(Value::as_str);
        let found = list
            .get("bundles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| {
                let id = entry.get("bundle_id").and_then(Value::as_str);
                let path = entry.get("path").and_then(Value::as_str);
                id == Some(target) || path.is_some_and(|p| p == target || Some(p) == summary_path)
            })
            .and_then(|entry| entry.get("bundle_id").and_then(Value::as_str));
        found.map(str::to_owned).ok_or_else(|| {
            format!(
                "refusing trash_original on uncataloged or non-active bundle `{target}`; open or register it first"
            )
        })
    }

    fn tool_export_frame_png(&mut self, args: &Map<String, Value>) -> Result<Vec<Value>, String> {
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
        Ok(vec![text_block(&resp)])
    }
}

fn handle_initialize(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(MCP_PROTOCOL_LATEST);
    let negotiated = match requested {
        MCP_PROTOCOL_LATEST | MCP_PROTOCOL_FALLBACK => requested,
        _ => MCP_PROTOCOL_LATEST,
    };
    json!({
        "protocolVersion": negotiated,
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "arb-mcp",
            "version": env!("CARGO_PKG_VERSION")
        },
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

fn text_block(value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({
        "type": "text",
        "text": text,
    })
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
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = usize::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, usize::from);
        let b2 = chunk.get(2).copied().map_or(0, usize::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(TABLE[(n >> 18) & 0x3F]));
        out.push(char::from(TABLE[(n >> 12) & 0x3F]));
        if chunk.len() > 1 {
            out.push(char::from(TABLE[(n >> 6) & 0x3F]));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(TABLE[n & 0x3F]));
        } else {
            out.push('=');
        }
    }
    out
}

const KNOWN_TOOLS: &[&str] = &[
    "arb_status",
    "arb_devices",
    "arb_capture_start",
    "arb_capture_stop",
    "arb_config_get",
    "arb_config_update",
    "arb_save",
    "arb_screenshot",
    "arb_save_logs",
    "arb_list_bundles",
    "arb_trash_bundle",
    "arb_restore_bundle",
    "arb_timeline_overview",
    "arb_query_logs",
    "arb_inspect_frames",
    "arb_export_trim",
    "arb_export_frame_png",
];

fn is_known_tool(name: &str) -> bool {
    KNOWN_TOOLS.contains(&name)
}

fn tool_def(name: &str, description: &str, input_schema: &Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "single declarative JSON Schema registry for all 17 MCP tools"
)]
fn tool_definitions() -> Vec<Value> {
    vec![
        tool_def(
            "arb_status",
            "Return arbd daemon hello metadata, active live capture telemetry, Zone-1 ring usage, storage pressure state, and crash auto-save status.",
            &json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_devices",
            "List connected Android devices and emulators visible via ADB.",
            &json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_capture_start",
            "Start live rolling screen/audio/logcat/input replay capture (or lightweight `logs_only` capture without scrcpy-server) on an Android device.",
            &json!({
                "type": "object",
                "properties": {
                    "serial": { "type": "string", "description": "Optional ADB device serial (auto-selects sole online device if omitted; never auto-selects when 2+ devices are online)." },
                    "capture_mode": { "type": "string", "enum": ["video_and_logs", "logs_only"], "description": "Capture mode (`video_and_logs` or `logs_only`, defaults to config)." },
                    "max_size": { "type": "integer", "minimum": 256, "maximum": 4096, "description": "Maximum encoded video long edge in pixels." },
                    "max_fps": { "type": "integer", "minimum": 1, "maximum": 120, "description": "Target maximum frame rate." },
                    "video_bit_rate": { "type": "integer", "minimum": 100_000, "description": "Target H.264 video bitrate in bps." },
                    "audio_enabled": { "type": "boolean", "description": "Enable playback audio capture on API >= 33 (defaults to config)." },
                    "logcat_enabled": { "type": "boolean", "description": "Enable logcat stream (default true)." },
                    "input_enabled": { "type": "boolean", "description": "Enable touch/input stream (default true)." },
                    "heartbeat_ms": { "type": "integer", "minimum": 50, "description": "Companion beacon heartbeat interval in ms." },
                    "companion_jar": { "type": "string", "description": "Optional path to arb-companion.dex.jar." },
                    "scrcpy_jar": { "type": "string", "description": "Optional path to replay-scrcpy-audio.dex.jar." }
                },
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_capture_stop",
            "Stop the active live capture session while keeping buffered ring data available for saving.",
            &json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_config_get",
            "Read the persisted validated ArbConfig (seconds-based save_window_secs, ring budget, video/audio settings, crash auto-save).",
            &json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_config_update",
            "Patch and atomically persist ArbConfig settings (validated on the daemon).",
            &json!({
                "type": "object",
                "properties": {
                    "save_window_secs": { "type": "integer", "minimum": 5, "maximum": 600 },
                    "ring_budget_bytes": { "type": "integer", "minimum": 16_777_216 },
                    "min_free_disk_bytes": { "type": "integer", "minimum": 67_108_864 },
                    "video_max_size": { "type": "integer", "minimum": 256, "maximum": 4096 },
                    "video_max_fps": { "type": "integer", "minimum": 1, "maximum": 120 },
                    "audio_enabled": { "type": "boolean" },
                    "capture_mode": { "type": "string", "enum": ["video_and_logs", "logs_only"] },
                    "device_serial": { "type": ["string", "null"] },
                    "crash_autosave_enabled": { "type": "boolean" },
                    "crash_autosave_cooldown_secs": { "type": "integer", "minimum": 1, "maximum": 3600 },
                    "output_dir_override": { "type": ["string", "null"] },
                    "screenshot_dir_override": { "type": ["string", "null"] },
                    "ide_default_save_action": { "type": "string", "enum": ["save_and_open", "save_only"] }
                },
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_save",
            "Save the last `window_secs` of the live ring buffer into an immutable WORM bundle.",
            &json!({
                "type": "object",
                "properties": {
                    "window_secs": { "type": "integer", "minimum": 5, "maximum": 600, "description": "Lookback duration in seconds (defaults to config.save_window_secs)." },
                    "dest_dir": { "type": "string", "description": "Optional destination parent directory." },
                    "drain_timeout_ms": { "type": "integer", "minimum": 0, "description": "Boundary drain timeout in ms (default 500)." },
                    "note": { "type": "string", "description": "Optional caller note echoed in the tool result." }
                },
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_screenshot",
            "Capture the current Android device screen as a validated PNG and optionally return an inline MCP image/png block.",
            &json!({
                "type": "object",
                "properties": {
                    "serial": { "type": "string", "description": "Optional ADB device serial." },
                    "output_path": { "type": "string", "description": "Optional explicit output PNG file path." },
                    "output_dir": { "type": "string", "description": "Optional output directory where an auto-named screenshot PNG will be written." },
                    "overwrite": { "type": "boolean", "description": "Allow atomic overwrite if output_path exists (default false)." },
                    "include_image": { "type": "boolean", "description": "Include inline MCP image/png content block (default true)." }
                },
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_save_logs",
            "Save a log-only WORM bundle (`timeline.sqlite` + `logs.jsonl` + `unanchored.jsonl`) without interrupting video capture.",
            &json!({
                "type": "object",
                "properties": {
                    "window_secs": { "type": "integer", "minimum": 5, "maximum": 600 },
                    "dest_dir": { "type": "string" },
                    "drain_timeout_ms": { "type": "integer", "minimum": 0 }
                },
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_list_bundles",
            "List saved, trimmed, crash-autosaved, legacy read-only, or trashed bundles from the arbd catalog.",
            &json!({
                "type": "object",
                "properties": {
                    "state": { "type": "string", "enum": ["active", "trashed"], "description": "Optional lifecycle state filter." }
                },
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_trash_bundle",
            "Non-destructively move an active catalog bundle to Trash (files remain intact on disk and restorable).",
            &json!({
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string", "description": "Catalog bundle_id to move to Trash." }
                },
                "required": ["bundle_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_restore_bundle",
            "Restore a previously trashed bundle back to active state.",
            &json!({
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string", "description": "Catalog bundle_id to restore from Trash." }
                },
                "required": ["bundle_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_timeline_overview",
            "Return a compact one-shot structured overview of a bundle: frame count, duration, dimensions, audio status, Activity screen spans, taps with frame indices, crashes, discontinuities, per-app extracted `app_logs` summaries, and top E/F error log highlights.",
            &json!({
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string", "description": "Catalog bundle_id or filesystem path." },
                    "max_error_logs": { "type": "integer", "minimum": 0, "maximum": 200, "description": "Maximum E/F error highlights (default 50)." }
                },
                "required": ["bundle_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_query_logs",
            "Query bounded logcat rows from a bundle (`main` or `unanchored` bucket) with optional frame range, package, tag, level, and keyword filters.",
            &json!({
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string", "description": "Catalog bundle_id or filesystem path." },
                    "bucket": { "type": "string", "enum": ["main", "unanchored", "all"], "description": "Timeline bucket filter (default all)." },
                    "start_frame": { "type": "integer", "minimum": 0 },
                    "end_frame": { "type": "integer", "minimum": 0 },
                    "start_ns": { "type": "integer" },
                    "end_ns": { "type": "integer" },
                    "package": { "type": "string", "description": "Exact Android package name filter (matches foreground/crashed package attribution)." },
                    "tag": { "type": "string", "description": "Case-insensitive tag substring filter." },
                    "level": { "type": "string", "enum": ["V", "D", "I", "W", "E", "F", "A"] },
                    "keyword": { "type": "string", "description": "Case-insensitive message/tag substring filter." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100_000 }
                },
                "required": ["bundle_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_inspect_frames",
            "Visually inspect frames of a bundle and return an MCP native image/png + JSON metadata. Supports `contact_sheet` (up to 16 labeled tiles with #frame_index + timestamps), `single` full-res frame, and `tap_crop` (28%x12% crop with center crosshair).",
            &json!({
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string", "description": "Catalog bundle_id or filesystem path." },
                    "mode": { "type": "string", "enum": ["contact_sheet", "single", "tap_crop"] },
                    "start_frame": { "type": "integer", "minimum": 0 },
                    "end_frame": { "type": "integer", "minimum": 0 },
                    "frame_indices": {
                        "type": "array",
                        "items": { "type": "integer", "minimum": 0 },
                        "minItems": 1,
                        "maxItems": 16
                    },
                    "max_tiles": { "type": "integer", "minimum": 1, "maximum": 16 },
                    "frame_index": { "type": "integer", "minimum": 0 },
                    "tap_index": { "type": "integer", "minimum": 0, "description": "Zero-based tap index from arb_timeline_overview." },
                    "tap_x": { "type": "integer", "minimum": 0 },
                    "tap_y": { "type": "integer", "minimum": 0 },
                    "include_tap_marker": { "type": "boolean" },
                    "display_width": { "type": "integer", "minimum": 1, "description": "Device display width in pixels; scales recorded tap coordinates onto the video. Required for tap_index crops/markers when taps lie outside the video frame." },
                    "display_height": { "type": "integer", "minimum": 1, "description": "Device display height in pixels (see display_width)." },
                    "max_tile_width": { "type": "integer", "minimum": 16, "maximum": 1024 },
                    "max_tile_height": { "type": "integer", "minimum": 16, "maximum": 1024 },
                    "columns": { "type": "integer", "minimum": 1, "maximum": 16 }
                },
                "required": ["bundle_id", "mode"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_export_trim",
            "Precision-trim a bundle by strict integer [in_frame_index..=out_frame_index] (or export full bundle for share_mp4) into either a standalone faststart zero-elst `share_mp4` (H.264 + AAC-LC) or an immutable WORM `bundle`. Optional `trash_original` moves source to non-destructive Trash strictly after commit (refused on legacy read-only bundles).",
            &json!({
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string", "description": "Catalog bundle_id or filesystem path." },
                    "format": { "type": "string", "enum": ["share_mp4", "bundle"], "description": "Output format (default share_mp4)." },
                    "in_frame_index": { "type": "integer", "minimum": 0 },
                    "out_frame_index": { "type": "integer", "minimum": 0 },
                    "output_path": { "type": "string", "description": "Optional explicit .mp4 output path (for share_mp4)." },
                    "dest_dir": { "type": "string", "description": "Optional parent directory for the trimmed WORM bundle." },
                    "trash_original": { "type": "boolean", "description": "Move source bundle to non-destructive Trash after verified commit (default false)." }
                },
                "required": ["bundle_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "arb_export_frame_png",
            "Export one exact zero-based integer `frame_index` from a bundle to a PNG file with provenance tEXt chunks.",
            &json!({
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string", "description": "Catalog bundle_id or filesystem path." },
                    "frame_index": { "type": "integer", "minimum": 0 },
                    "output_path": { "type": "string", "description": "Destination PNG file path (never overwrites source)." }
                },
                "required": ["bundle_id", "frame_index", "output_path"],
                "additionalProperties": false
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use std::io::Cursor;

    use super::{
        KNOWN_TOOLS, LineRead, McpServer, base64_encode, is_retry_safe_method, read_bounded_line,
        tool_definitions,
    };
    use arb_os::paths::ArbPaths;
    use serde_json::{Value, json};

    #[test]
    fn tool_registry_matches_known_tools_with_valid_schemas() {
        let defs = tool_definitions();
        assert_eq!(
            defs.len(),
            KNOWN_TOOLS.len(),
            "tool_definitions count must match KNOWN_TOOLS"
        );
        let mut names = BTreeSet::new();
        for def in &defs {
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
        for &known in KNOWN_TOOLS {
            assert!(
                names.contains(known),
                "KNOWN_TOOLS entry `{known}` missing from tool_definitions"
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
        let resp = srv.handle_line(&batch.to_string()).expect("batch response");
        let arr = resp.as_array().expect("array response");
        assert_eq!(arr.len(), 2, "notification must not produce a response");
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[1]["id"], 2);
        let empty = srv.handle_line("[]").expect("empty batch error");
        assert_eq!(empty["error"]["code"], -32600);
    }
}
