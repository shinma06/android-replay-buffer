//! All-tool contract test against an isolated arbd with synthetic capture and ADB discovery.
use super::{
    BASE_BT_NS, FRAME_STEP_NS, build_minimal_valid_png, seed_bundle, seed_bundle_with_events,
};
use arb_adb::mock::{
    CaptureEvent, CaptureSourceError, DeviceEventSource, FaultScenario, MockDeviceSource,
};
use arb_core::bundle::{LogRecord, RecordBucket};
use arb_core::config::CaptureMode;
use arb_core::time::{TimeAlignment, TimestampSemantics};
use arb_os::paths::ArbPaths;
use arb_os::rpc::RpcClient;
use arbd::{DaemonConfig, DaemonServer};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use tempfile::TempDir;

use super::version::ProtocolVersion;

mod arguments;

#[path = "../../src/mcp/validate.rs"]
mod validate;

pub(super) fn assert_valid_jobs_response(schema: &Value, body: &Value) {
    validate::validate(schema, body, "arb_jobs").unwrap();
}

/// The recorded window contains one Error and one Fatal row, generated from the same anchored sample.
struct ErrorLogSource {
    inner: MockDeviceSource,
    pending: std::collections::VecDeque<CaptureEvent>,
}

impl DeviceEventSource for ErrorLogSource {
    fn serial(&self) -> &str {
        self.inner.serial()
    }

    fn next_event(&mut self) -> Result<Option<CaptureEvent>, CaptureSourceError> {
        if let Some(row) = self.pending.pop_front() {
            return Ok(Some(row));
        }
        let event = self.inner.next_event()?;
        if let Some(info @ CaptureEvent::LogRecord { .. }) = &event {
            for priority in [6, 7] {
                let mut row = info.clone();
                if let CaptureEvent::LogRecord { payload, .. } = &mut row {
                    payload.priority = priority;
                    payload.payload[0] = priority;
                }
                self.pending.push_back(row);
            }
        }
        Ok(event)
    }
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: u64,
    pending: BTreeMap<u64, Value>,
    events: Vec<Value>,
    schemas: BTreeMap<String, Value>,
    covered: BTreeSet<String>,
}

impl Client {
    fn open(paths: &ArbPaths) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arb-cli"))
            .arg("--data-dir")
            .arg(&paths.data_dir)
            .arg("--cache-dir")
            .arg(&paths.cache_dir)
            .arg("--runtime-dir")
            .arg(&paths.runtime_dir)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            seq: 0,
            pending: BTreeMap::new(),
            events: Vec::new(),
            schemas: BTreeMap::new(),
            covered: BTreeSet::new(),
        }
    }
    fn send(&mut self, method: &str, params: Value) -> u64 {
        self.seq += 1;
        let mut request = json!({"jsonrpc":"2.0","id":self.seq,"method":method});
        request["params"] = params;
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        self.seq
    }
    fn recv(&mut self, id: u64) -> Value {
        loop {
            if let Some(response) = self.pending.remove(&id) {
                return response;
            }
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "unexpected EOF for {id}"
            );
            let v: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(v["jsonrpc"], "2.0");
            if let Some(key) = v["id"].as_u64() {
                self.pending.insert(key, v);
            } else {
                self.events.push(v);
            }
        }
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.recv(id)
    }
    fn result(&mut self, method: &str, params: Value) -> Value {
        let mut resp = self.request(method, params);
        assert!(resp.get("error").is_none(), "{method}: {resp}");
        resp["result"].take()
    }
    fn init(&mut self, version: &str) -> Value {
        self.result("initialize",json!({"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"contract","version":"1"}}))
    }
    /// Lists the negotiated catalog and attaches each tool's complete `outputSchema` from
    /// `arb://schemas/{tool}`, which `tools/list` leaves out (#586-A).
    fn load_schemas(&mut self) -> Vec<Value> {
        let tools = self.result("tools/list", json!({}))["tools"].take();
        let mut defs = Vec::new();
        for def in tools.as_array().unwrap() {
            assert!(def.get("outputSchema").is_none(), "{def}");
            let name = def["name"].as_str().unwrap();
            let uri = format!("arb://schemas/{name}");
            let read = self.result("resources/read", json!({ "uri": uri }));
            let content = &read["contents"][0];
            assert_eq!(content["uri"], uri);
            assert_eq!(content["mimeType"], "application/schema+json");
            let schema: Value = serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
            assert_eq!(
                schema["$schema"],
                "https://json-schema.org/draft/2020-12/schema"
            );
            assert_eq!(schema["type"], "object");
            let mut full = def.clone();
            full["outputSchema"] = schema;
            self.schemas.insert(name.to_owned(), full.clone());
            defs.push(full);
        }
        defs
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let mut params = json!({"name":name});
        params["arguments"] = args;
        let r = self.result("tools/call", params);
        self.check_tool(name, r)
    }
    fn check_tool(&mut self, name: &str, mut r: Value) -> Value {
        assert_eq!(r["isError"], false, "{name}: {r}");
        let body: Value = serde_json::from_str(
            r["content"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["type"] == "text")
                .unwrap()["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        if let Some(structured) = r.get_mut("structuredContent") {
            assert_eq!(body, structured.take());
        }
        validate::validate(&self.schemas[name]["outputSchema"], &body, name)
            .unwrap_or_else(|e| panic!("{e} ({}, {}): {body}", e.details, e.hint));
        self.covered.insert(name.to_owned());
        body
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn pdd_contract_tables_direct_rpc_preserves_legacy_requests_with_warnings() {
    let root = TempDir::new().unwrap();
    let paths = ArbPaths {
        data_dir: root.path().join("d"),
        runtime_dir: root.path().join("r"),
        cache_dir: root.path().join("c"),
    };
    let _server = DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap();
    let mut injection = fixture_log(
        6,
        "I",
        "adbd",
        "adbd service requested 'shell:input tap 1 2'",
    );
    injection.uid = Some(2000);
    let bundle = seed_bundle_with_events(
        root.path(),
        root.path(),
        "silent-success",
        vec![
            injection,
            fixture_log(
                7,
                "E",
                "AndroidRuntime",
                "FATAL EXCEPTION: main\nProcess: com.example.demo, PID: 42\njava.lang.IllegalStateException: test",
            ),
        ],
        vec![],
    );
    let mut rpc = RpcClient::connect_endpoint(&paths.ipc_endpoint()).unwrap();
    let table: Value = serde_json::from_str(include_str!(
        "../../../../../docs/specs/ui-feedback-contract-table.json"
    ))
    .unwrap();
    for row in table["silent_success_rows"].as_array().unwrap() {
        let mut args = row["arguments"].clone();
        args["bundle_id"] = json!(bundle);
        match row["tool"].as_str().unwrap() {
            "arb_inspect_frames" => {
                for metadata in [true, false] {
                    args["metadata_only"] = json!(metadata);
                    let body = rpc.call("media.inspect_frames", args.clone()).unwrap();
                    assert_eq!(body["warnings"][0]["code"], row["warning"], "{row}");
                    if args["mode"] == "contact_sheet" {
                        assert_eq!(body["frame_indices_rendered"], json!([0, 4]));
                        assert_eq!(body["dropped_frame_indices"], json!([1, 2, 3]));
                    } else {
                        assert_eq!(body["frame_indices_rendered"], json!([0]));
                    }
                }
            }
            "arb_query_logs" => {
                args.as_object_mut().unwrap().remove("bundle_id");
                args["bundle_id_or_path"] = json!(bundle);
                let body = rpc.call("bundle.query_logs", args).unwrap();
                assert_eq!(body["warnings"][0]["code"], row["warning"]);
            }
            "arb_export_trim" if row["warning"] == "no_taps_to_burn" => {
                let method = if args["format"] == "bundle" {
                    "export.trim"
                } else {
                    "export.share_mp4"
                };
                args.as_object_mut().unwrap().remove("format");
                if method == "export.trim" {
                    args.as_object_mut().unwrap().remove("bundle_id");
                    args["bundle_id_or_path"] = json!(bundle);
                }
                let body = rpc.call(method, args).unwrap();
                let detail = &body["warnings"][0];
                assert_eq!(detail["code"], row["warning"]);
                assert_eq!(body["burned_tap_count"], 0);
                let capture = &detail["details"]["input_capture"];
                assert_eq!(capture["physical"]["gestures"], 0);
                assert_eq!(capture["injected"]["gestures"], 1);
            }
            _ => {}
        }
    }
    // The crash at frame 7 outranks the transition at frame 2 when only one interior tile fits.
    for metadata in [true, false] {
        let args = json!({"bundle_id":bundle,"mode":"contact_sheet","end_frame":10,"max_tiles":3,"metadata_only":metadata});
        let body = rpc.call("media.inspect_frames", args).unwrap();
        assert_eq!(body["frame_indices_rendered"], json!([0, 7, 10]), "{body}");
        let args = json!({"bundle_id":bundle,"mode":"contact_sheet","end_frame":10,"max_tiles":4,"metadata_only":metadata});
        let body = rpc.call("media.inspect_frames", args).unwrap();
        assert_eq!(
            body["frame_indices_rendered"],
            json!([0, 2, 7, 10]),
            "{body}"
        );
    }
}

#[test]
fn daemon_distinguishes_missing_resources_and_an_empty_video_track() {
    let root = TempDir::new().unwrap();
    let paths = ArbPaths {
        data_dir: root.path().join("d"),
        runtime_dir: root.path().join("r"),
        cache_dir: root.path().join("c"),
    };
    let _server = DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap();
    let mut client = RpcClient::connect_endpoint(&paths.ipc_endpoint()).unwrap();
    let bundle = seed_bundle(root.path(), root.path(), "zero-sample-track");
    // Emulate an existing bundle with an empty MP4 sample table using only this test's files.
    let path = bundle.join("video.mp4");
    let mut bytes = fs::read(&path).unwrap();
    let stsz = bytes.windows(4).position(|tag| tag == b"stsz").unwrap();
    bytes[stsz + 12..stsz + 16].copy_from_slice(&0_u32.to_be_bytes());
    fs::write(path, bytes).unwrap();
    for (method, args, label, rpc_code) in [
        (
            "media.inspect_frames",
            json!({"bundle_id":bundle,"mode":"single","metadata_only":true}),
            "NO_VIDEO",
            -32602,
        ),
        (
            "media.inspect_frames",
            json!({"bundle_id":bundle,"mode":"single","metadata_only":false}),
            "NO_VIDEO",
            -32602,
        ),
        (
            "save.retry",
            json!({"job_id":"absent-job"}),
            "JOB_NOT_FOUND",
            -32001,
        ),
        (
            "save.discard",
            json!({"job_id":"absent-job"}),
            "JOB_NOT_FOUND",
            -32001,
        ),
        (
            "bundle.lease_renew",
            json!({"lease_id":"absent-lease"}),
            "LEASE_NOT_FOUND",
            -32001,
        ),
        (
            "media.seek_frame",
            json!({"viewport_id":"absent-viewport","frame_index":0}),
            "VIEWPORT_NOT_FOUND",
            -32001,
        ),
    ] {
        let error = client.call(method, args).unwrap_err();
        let arb_os::rpc::RpcClientError::Rpc {
            code, error_code, ..
        } = error
        else {
            panic!("{method}: {error}")
        };
        assert_eq!(error_code.as_deref(), Some(label), "{method}");
        assert_eq!(code, rpc_code, "{method}");
    }
}

#[test]
fn tap_crop_reports_the_overview_range_on_every_version() {
    let root = TempDir::new().unwrap();
    let paths = ArbPaths {
        data_dir: root.path().join("d"),
        runtime_dir: root.path().join("r"),
        cache_dir: root.path().join("c"),
    };
    let _server = DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap();
    let empty = seed_bundle_with_events(root.path(), root.path(), "empty-taps", vec![], vec![]);
    let one = seed_bundle(root.path(), root.path(), "one-tap");
    let mut c = Client::open(&paths);
    for version in [
        ProtocolVersion::V2024_11_05,
        ProtocolVersion::V2025_03_26,
        ProtocolVersion::V2025_06_18,
        ProtocolVersion::V2025_11_25,
    ] {
        c.init(version.as_str());
        for (target, count) in [
            (json!(empty), Some(0_usize)),
            (json!(one), Some(1)),
            (json!("absent-bundle"), None),
        ] {
            let got = count.unwrap_or(0);
            let response = c.result(
                "tools/call",
                json!({"name":"arb_inspect_frames","arguments":{
                    "bundle_id":target, "mode":"tap_crop", "tap_index":got
                }}),
            );
            assert_eq!(response["isError"], true);
            let error: Value =
                serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
            if version.supports_structured_content() {
                assert_eq!(response["structuredContent"], error);
            }
            if let Some(count) = count {
                assert_eq!(
                    error["error_code"], "INVALID_PARAMS",
                    "{version:?}: {error}"
                );
                assert_eq!(error["rpc_code"], -32602);
                assert_eq!(
                    error["details"],
                    json!({"field":"tap_index","got":got,
                    "overview_tap_count":count, "allowed_max": count.checked_sub(1),
                    "cause":format!("tap_index {got} out of range (bundle has {count} overview taps)")})
                );
                let hint = error["hint"].as_str().unwrap();
                if count == 0 {
                    assert_eq!(
                        hint,
                        "このバンドルの overview taps は 0 件です。tap_index を外すか tap_x / tap_y（映像 px）を指定してください。"
                    );
                } else {
                    assert!(hint.contains("0..=0"), "{hint}");
                }
            } else {
                assert_eq!(error["error_code"], "BUNDLE_NOT_FOUND");
                assert_eq!(error["rpc_code"], -32001);
            }
        }
    }
}

fn assert_rejected(c: &mut Client, name: &str, args: &Value) {
    let r = c.result("tools/call", json!({"name":name,"arguments":args}));
    assert_eq!(r["isError"], true, "{name} {args}");
    if let Some(def) = c.schemas.get(name) {
        validate::validate(&def["outputSchema"], &r["structuredContent"], name)
            .expect("structured error matches output schema");
    }
    assert_eq!(
        r["structuredContent"]["error_code"], "INVALID_PARAMS",
        "{name} {args}: {r}"
    );
    assert_eq!(r["structuredContent"]["rpc_code"], -32602);
    assert_eq!(r["structuredContent"]["retryable"], false);
    assert_eq!(r["structuredContent"]["kind"], "invalid_params");
    assert!(
        r["structuredContent"]["hint"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    for (key, val) in args.as_object().into_iter().flatten() {
        if val.get("wrong").is_some() || key == "unexpected" {
            assert!(
                r["structuredContent"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(key),
                "{name}: {r}"
            );
        }
    }
}

fn registry(c: &mut Client) {
    for version in [
        ProtocolVersion::V2024_11_05.as_str(),
        ProtocolVersion::V2025_03_26.as_str(),
        ProtocolVersion::V2025_06_18.as_str(),
        ProtocolVersion::V2025_11_25.as_str(),
        "2099-11-25",
    ] {
        let init = c.init(version);
        assert_eq!(
            init["protocolVersion"],
            if version.starts_with("2099") {
                ProtocolVersion::V2025_11_25.as_str()
            } else {
                version
            }
        );
        assert_eq!(init["serverInfo"]["name"], "android-replay-buffer");
        assert_eq!(
            init["capabilities"].get("tasks").is_some(),
            version >= ProtocolVersion::V2025_11_25.as_str()
        );
        let tools = c.result("tools/list", json!({}));
        assert_eq!(tools["tools"].as_array().unwrap().len(), 21);
        for (name, flag) in [
            ("arb_export_trim", "trash_original"),
            ("arb_screenshot", "overwrite"),
        ] {
            let tool = tools["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap();
            let legacy = version < ProtocolVersion::V2025_11_25.as_str();
            assert_eq!(tool["annotations"]["destructiveHint"], legacy);
            assert_eq!(
                tool["inputSchema"]["properties"].get(flag).is_some(),
                legacy
            );
            if !legacy {
                let mut args = json!({});
                if name == "arb_export_trim" {
                    args["bundle_id"] = json!("fixture");
                }
                args[flag] = json!(true);
                assert_rejected(c, name, &args);
            }
        }
        assert_eq!(
            c.result("tools/call", json!({"name":"arb_status"}))["isError"],
            false
        );
    }
    for def in &c.load_schemas() {
        let name = def["name"].as_str().unwrap();
        let input = &def["inputSchema"];
        assert_eq!(input["type"], "object");
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(def["annotations"][hint].is_boolean());
        }
        assert!(def["annotations"]["title"].is_string());
        let mut base = json!({});
        for k in input["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            base[k] = match k {
                "bundle_id" => json!("fixture"),
                "output_path" => json!("out.png"),
                "frame_index" => json!(0),
                "mode" => json!("single"),
                "action" => json!("list"),
                _ => panic!("unexpected required {name}.{k}"),
            };
        }
        if input["properties"].get("bundle_id").is_some() {
            base["bundle_id"] = json!("fixture");
        }
        if name == "arb_export_frame_png" {
            // Exactly one destination is required (`oneOf`).
            base["output_path"] = json!("out.png");
        }
        if name == "arb_inspect_frames" {
            base["frame_index"] = json!(0);
        }
        if name == "arb_export_trim" {
            base["full_range"] = json!(true);
        }
        validate::normalize_integers(input, &mut base);
        validate::validate(input, &base, name).unwrap_or_else(|e| panic!("{name}: {e}"));
        if base.get("bundle_id").is_some() {
            let mut absent = base.clone();
            absent["bundle_id"] = json!("no-such-bundle-contract");
            let result = c.result("tools/call", json!({"name":name,"arguments":absent}));
            assert_eq!(result["isError"], true, "{name}");
            let error = &result["structuredContent"];
            assert_eq!(error["error_code"], "BUNDLE_NOT_FOUND", "{name}: {error}");
            assert_eq!(error["rpc_code"], -32001, "{name}");
            validate::validate(&def["outputSchema"], error, name).unwrap();
        }
        for (k, field) in input["properties"].as_object().unwrap() {
            let mut args = base.clone();
            args[k] = if k == "bundle_id" {
                json!(123)
            } else {
                json!({"wrong":"type"})
            };
            assert_rejected(c, name, &args);
            if field.get("enum").is_some() || k == "level" {
                args[k] = json!("BOGUS");
                assert_rejected(c, name, &args);
            }
        }
        for key in input["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let mut args = base.clone();
            args.as_object_mut().unwrap().remove(key);
            assert_rejected(c, name, &args);
        }
        base["unexpected"] = json!(true);
        assert_rejected(c, name, &base);
    }
}

fn resources_and_prompts(c: &mut Client, id: &str) {
    assert_eq!(
        c.result("resources/list", json!({}))["resources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let templates = c.result("resources/templates/list", json!({}))["resourceTemplates"].take();
    assert_eq!(templates.as_array().unwrap().len(), 6);
    assert!(templates.as_array().unwrap().iter().any(|t| t
        == &json!({"uriTemplate":"arb://schemas/{tool}","name":"schema","description":"ツールの outputSchema（交渉した版）","mimeType":"application/schema+json"})));
    for (uri, code) in [
        ("arb://schemas/arb_unknown", -32602),
        ("arb://schemas/../arb_status", -32602),
        ("arb://schemas/%2e%2e%2farb_status", -32602),
        ("arb://schemas/", -32602),
    ] {
        let response = c.request("resources/read", json!({"uri":uri}));
        assert_eq!(response["error"]["code"], code, "{uri}");
    }
    let r = c.result(
        "completion/complete",
        json!({"ref":{"type":"ref/resource","uri":"arb://schemas/{tool}"},"argument":{"name":"tool","value":"arb_export"}}),
    );
    assert_eq!(
        r["completion"],
        json!({"values":["arb_export_trim","arb_export_frame_png"],"total":2,"hasMore":false})
    );
    for uri in [
        "arb://status".into(),
        "arb://bundles".into(),
        format!("arb://bundles/{id}/manifest"),
        format!("arb://bundles/{id}/screens"),
        format!("arb://bundles/{id}/crashes"),
        format!("arb://bundles/{id}/logs"),
        format!("arb://bundles/{id}/frame/2"),
    ] {
        let r = c.result("resources/read", json!({"uri":uri}));
        let data = &r["contents"][0];
        assert_eq!(data["uri"], uri);
        if uri.ends_with("/2") {
            assert!(data["blob"].as_str().unwrap().starts_with("iVBORw0KGgo"));
        } else {
            let document: Value = serde_json::from_str(data["text"].as_str().unwrap()).unwrap();
            assert!(document.is_object());
        }
    }
    for (uri, code) in [
        (format!("arb://bundles/{id}/frame/999"), -32002),
        ("arb://bundles/no-such-one/logs".into(), -32002),
        ("arb://bundles/%2e%2e/logs".into(), -32602),
        ("file:///etc/hosts".into(), -32602),
    ] {
        let response = c.request("resources/read", json!({"uri":uri}));
        assert_eq!(response["error"]["code"], code, "{uri}");
        if code == -32002 {
            assert_eq!(response["error"]["data"]["uri"], uri);
        }
    }
    let prompts = c.result("prompts/list", json!({}));
    assert_eq!(prompts["prompts"].as_array().unwrap().len(), 3);
    for p in prompts["prompts"].as_array().unwrap() {
        assert!(
            c.result("prompts/get", json!({"name":p["name"]}))["messages"][0]["content"]["text"]
                .is_string()
        );
    }
    for (name, args, expected) in [
        (
            "investigate_crash",
            json!({"bundle_id":"fixture"}),
            "arb://bundles/fixture/crashes",
        ),
        (
            "triage_recent_bug",
            json!({"duration_secs":"15","keyword":"DOGFOOD_ERR_001"}),
            "arb_save_and_inspect({\"duration_secs\":15})",
        ),
        (
            "safe_cleanup_review",
            json!({"ignore_grace":"true"}),
            "\"ignore_grace\":true",
        ),
    ] {
        let prompt = c.result("prompts/get", json!({"name":name,"arguments":args}));
        let text = prompt["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains(expected), "{text}");
        if name == "triage_recent_bug" {
            assert!(text.contains("\"keyword\":\"DOGFOOD_ERR_001\""));
        }
        assert!(!text.contains("指定値"));
    }
    for reference in [
        json!({"type":"ref/resource","uri":"arb://bundles/{bundle_id}/manifest"}),
        json!({"type":"ref/prompt","name":"investigate_crash"}),
    ] {
        let r = c.result(
            "completion/complete",
            json!({"ref":reference,"argument":{"name":"bundle_id","value":id}}),
        );
        assert_eq!(
            r["completion"],
            json!({"values":[id],"total":1,"hasMore":false})
        );
    }
}

// The subprocess owns ADB discovery and a synthetic ring. Parallel tests and physical devices
// never see the fixture environment.
#[test]
fn every_tool_and_session_feature_matches_the_wire_contract() {
    const TEST: &str = "stdio_contract::every_tool_and_session_feature_matches_the_wire_contract";
    let root = TempDir::new().unwrap();
    if std::env::var_os("ARB_MCP_CONTRACT_CHILD").is_none() {
        let fake = root
            .path()
            .join(format!("fake-adb{}", std::env::consts::EXE_SUFFIX));
        assert!(
            Command::new("rustc")
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../arbd/tests/fixtures/fake_adb.rs")
                )
                .arg("--edition=2024")
                .arg("-o")
                .arg(&fake)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env("ARB_MCP_CONTRACT_CHILD", "1")
                .env("ARB_ADB_PATH", &fake)
                .env("FAKE_DEVICE_STATE", root.path())
                .status()
                .unwrap()
                .success()
        );
        return;
    }
    let paths = ArbPaths {
        data_dir: root.path().join("d"),
        runtime_dir: root.path().join("r"),
        cache_dir: root.path().join("c"),
    };
    // Seed the on-disk ring, then exercise the same recovery path as a restarted daemon.
    let seed = DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap();
    seed.ingest_device_source_with_mode(
        &mut ErrorLogSource {
            inner: FaultScenario::NormalFhd30.build(),
            pending: std::collections::VecDeque::new(),
        },
        CaptureMode::LogsOnly,
    )
    .unwrap();
    seed.shutdown();
    let arbd = Path::new(env!("CARGO_BIN_EXE_arb-cli"))
        .with_file_name(format!("arbd{}", std::env::consts::EXE_SUFFIX));
    // Workspace gates build the real binary. A standalone arb-cli crate test can run
    // the production server from its development dependency in the same isolated process.
    let mut process = None;
    let _standalone = if arbd.is_file() {
        let mut child = Command::new(arbd)
            .arg("--data-dir")
            .arg(&paths.data_dir)
            .arg("--runtime-dir")
            .arg(&paths.runtime_dir)
            .arg("--cache-dir")
            .arg(&paths.cache_dir)
            .arg("--exit-on-stdin-eof")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert!(ready.starts_with("arbd ready"), "{ready}");
        process = Some(child);
        None
    } else {
        Some(DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap())
    };
    let id = "bundle-contract";
    let path = seed_bundle(root.path(), &paths.data_dir.join("bundles"), id);
    let mut rpc = RpcClient::connect_endpoint(&paths.ipc_endpoint()).unwrap();
    let open = rpc
        .call(
            "bundle.open",
            json!({"bundle_id_or_path":path,"holder_kind":"cli","holder_pid":std::process::id()}),
        )
        .unwrap();
    rpc.call("bundle.close", json!({"lease_id":open["lease_id"]}))
        .unwrap();
    let mut c = Client::open(&paths);
    registry(&mut c);
    let target = json!({"bundle_id":id});
    let listed = c.tool("arb_jobs", json!({"action":"list"}));
    assert!(listed["jobs"].is_array());
    let status = c.tool("arb_status", json!({}));
    assert_eq!(
        status["status"].get("configured_device_serial"),
        Some(&Value::Null)
    );
    assert_eq!(status["status"]["serial_mismatch"], false);
    for name in ["arb_devices", "arb_config_get", "arb_list_bundles"] {
        c.tool(name, json!({}));
    }
    c.tool("arb_config_update", json!({"save_window_secs":10}));
    let cap = c.tool(
        "arb_capture_start",
        json!({"wait_connected":true,"wait_timeout_ms":1000,"capture_mode":"logs_only"}),
    );
    assert!(cap["connected"].is_boolean());
    assert_eq!(cap["serial_source"], "auto");
    assert!(cap["config_changed"].is_object());
    let stopped = c.tool("arb_capture_stop", json!({}));
    assert_eq!(
        stopped["config_changed"]["capture_enabled"],
        json!([true, false])
    );
    for name in ["arb_save", "arb_save_logs"] {
        let r = c.result(
            "tools/call",
            json!({"name":name,"arguments":{"window_secs":5,"note":"調査記録"}}),
        );
        assert_eq!(
            r["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|b| b["type"] == "resource_link")
                .count(),
            3
        );
        let saved = c.check_tool(name, r);
        if name == "arb_save_logs" {
            let response = c.result("tools/call", json!({"name":"arb_inspect_frames","arguments":{"bundle_id":saved["bundle_id"],"mode":"single","frame_index":0}}));
            let error = &response["structuredContent"];
            assert_eq!(response["isError"], true);
            assert_eq!(error["error_code"], "NO_VIDEO");
            assert_eq!(error["details"]["bundle_id"], saved["bundle_id"]);
            validate::validate(
                &c.schemas["arb_inspect_frames"]["outputSchema"],
                error,
                "arb_inspect_frames",
            )
            .unwrap();
        }
        assert_eq!(saved["requested_duration_secs"], 5);
        assert_eq!(saved["note"], "調査記録");
        assert!(saved["warning_codes"].is_array());
        // #570: one message per code, projected from the canonical details in the same order.
        let codes = saved["warning_codes"].as_array().unwrap();
        let details = saved["warning_details"].as_array().unwrap();
        assert_eq!(saved["warnings"].as_array().unwrap().len(), codes.len());
        assert_eq!(details.len(), codes.len());
        for ((d, code), message) in details
            .iter()
            .zip(codes)
            .zip(saved["warnings"].as_array().unwrap())
        {
            assert_eq!(&d["code"], code);
            assert_eq!(&d["message"], message);
        }
        let entries = c.tool("arb_list_bundles", json!({}));
        let entry = entries["bundles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["bundle_id"] == saved["bundle_id"])
            .unwrap();
        assert_eq!(entry["memo"], "調査記録");
    }
    let adb_dir = std::path::PathBuf::from(std::env::var_os("FAKE_DEVICE_STATE").unwrap());
    fs::write(
        adb_dir.join("screencap.png"),
        build_minimal_valid_png(10, 20),
    )
    .unwrap();
    fs::write(adb_dir.join("devices"), "contract-device\tdevice\n").unwrap();
    // #586-B: every version saves without an inline PNG unless asked, and returns `path`,
    // `file_uri` and `resource_links` for `resources/read`.
    for (version, inline, expected) in [
        (ProtocolVersion::V2024_11_05.as_str(), None, false),
        (ProtocolVersion::V2025_03_26.as_str(), None, false),
        (ProtocolVersion::V2025_06_18.as_str(), None, false),
        (ProtocolVersion::V2025_11_25.as_str(), None, false),
        (ProtocolVersion::V2024_11_05.as_str(), Some(true), true),
        (ProtocolVersion::V2025_03_26.as_str(), Some(true), true),
        (ProtocolVersion::V2025_11_25.as_str(), Some(true), true),
    ] {
        c.init(version);
        c.load_schemas();
        let out = root.path().join(format!("shot-{version}-{expected}.png"));
        let mut args = json!({"serial":"contract-device","output_path":out});
        if let Some(flag) = inline {
            args["include_image"] = json!(flag);
        }
        let result = c.result(
            "tools/call",
            json!({"name":"arb_screenshot","arguments":args}),
        );
        let image = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "image");
        assert_eq!(image, expected);
        let link = json!({"uri":url::Url::from_file_path(&out).unwrap().as_str(),"name":out.file_name().unwrap().to_str().unwrap(),"mimeType":"image/png"});
        let link_blocks: Vec<_> = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["type"] == "resource_link")
            .collect();
        assert_eq!(
            link_blocks.len(),
            usize::from(version >= ProtocolVersion::V2025_06_18.as_str())
        );
        if let Some(block) = link_blocks.first() {
            assert_eq!(block["uri"], link["uri"]);
        }
        let shot = c.check_tool("arb_screenshot", result);
        assert_eq!(shot["resource_links"], json!([link]));
        assert_eq!(shot["path"], json!(out));
        // Every version returns the same `file_uri` as the link.
        assert_eq!(
            shot.get("file_uri"),
            Some(&link["uri"]),
            "{version}: {shot}"
        );
        assert!(out.is_file());
        let png = c.result("resources/read", json!({"uri":link["uri"]}));
        assert_eq!(png["contents"][0]["mimeType"], "image/png");
        assert_eq!(
            png["contents"][0]["blob"],
            arb_core::base64_encode(&fs::read(&out).unwrap())
        );
        fs::write(&out, b"replacement").unwrap();
        let stale = c.request("resources/read", json!({"uri":link["uri"]}));
        assert!(stale["error"].is_object(), "{stale}");
        let collision = c.result(
            "tools/call",
            json!({"name":"arb_screenshot","arguments":args}),
        );
        let body: Value =
            serde_json::from_str(collision["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error_code"], "CONFLICT");
        assert_eq!(body["details"]["reason"], "already_exists");
        let hint = body["hint"].as_str().unwrap();
        assert!(hint.contains("output_path"), "{hint}");
        assert_eq!(
            hint.contains("overwrite"),
            version != ProtocolVersion::V2025_11_25.as_str()
        );
        // The CONFLICT hint offers overwrite exactly where the schema accepts it.
        let accepts = c.result("tools/list", json!({}))["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "arb_screenshot")
            .unwrap()["inputSchema"]["properties"]
            .get("overwrite")
            .is_some();
        assert_eq!(hint.contains("overwrite"), accepts, "{version}");
    }
    c.init(ProtocolVersion::V2025_11_25.as_str());
    let shot = c.tool(
        "arb_screenshot",
        json!({"serial":"contract-device","output_dir":root.path().join("shots"),"time_zone":"host"}),
    );
    assert_local_time(&shot["captured_at_local"]);
    fs::remove_file(adb_dir.join("devices")).unwrap();
    let offline = c.result(
        "tools/call",
        json!({"name":"arb_screenshot","arguments":{"serial":"absent-device"}}),
    );
    let error = &offline["structuredContent"];
    assert_eq!(error["error_code"], "DEVICE_NOT_FOUND");
    assert_eq!(error["retryable"], true);
    assert!(error["message"].as_str().unwrap().contains("端末"));
    assert!(error["hint"].as_str().unwrap().contains("接続"));
    // #583: an absent explicit serial fails the start and leaves config.json untouched.
    let before = c.tool("arb_config_get", json!({}))["config"].clone();
    let start = c.result(
        "tools/call",
        json!({"name":"arb_capture_start","arguments":{"serial":"1.2.3.4:5555"}}),
    );
    assert_eq!(start["isError"], true, "{start}");
    let error = &start["structuredContent"];
    assert_eq!(error["error_code"], "DEVICE_NOT_FOUND", "{start}");
    assert_eq!(error["details"]["serial"], "1.2.3.4:5555");
    assert_eq!(error["details"]["config_changed"], json!({}));
    assert!(error["hint"].as_str().unwrap().contains("arb_devices"));
    assert_eq!(c.tool("arb_config_get", json!({}))["config"], before);
    let overview = c.tool("arb_timeline_overview", target.clone());
    assert!(overview["logcat_completeness"].is_object());
    assert_eq!(overview["noise_candidates"], json!([]));
    noise_and_local_time(&mut c, root.path(), &paths);
    crash_detail_and_package_filter(&mut c, root.path(), &paths);
    c.tool(
        "arb_query_logs",
        json!({"bundle_id":id,"level":"eRrOr","whole_word":true,"keyword":"failed","limit":1}),
    );
    let page = c.tool("arb_query_logs", json!({"bundle_id":id,"limit":1}));
    assert_eq!(page["has_more"], true);
    let page = c.tool(
        "arb_query_logs",
        json!({"bundle_id":id,"limit":1,"cursor":page["next_cursor"]}),
    );
    assert_eq!(page["has_more"], false);
    for version in [
        ProtocolVersion::V2024_11_05.as_str(),
        ProtocolVersion::V2025_03_26.as_str(),
        ProtocolVersion::V2025_06_18.as_str(),
        ProtocolVersion::V2025_11_25.as_str(),
    ] {
        c.init(version);
        c.load_schemas();
        arguments::verify(&mut c);
        log_fields(&mut c, id, version);
        inspect_shape(&mut c, id);
        let dest = root.path().join(format!("external-{version}"));
        fs::create_dir(&dest).unwrap();
        for (tool, args) in [
            (
                "arb_inspect_frames",
                json!({"bundle_id_or_path":id,"mode":"single","frame_index":0}),
            ),
            (
                "arb_save_and_inspect",
                json!({"dest_dir":dest,"drain_timeout_ms":0,"logs_only":version<ProtocolVersion::V2025_06_18.as_str(),"memo":"調査","exclude_tags":["VendorHal"],"max_errors":5}),
            ),
        ] {
            let result = c.result("tools/call", json!({"name":tool,"arguments":args}));
            let blocks = result["content"].as_array().unwrap();
            let body: Value = serde_json::from_str(blocks[0]["text"].as_str().unwrap()).unwrap();
            let links = body["resource_links"]
                .as_array()
                .expect("body resource_links");
            assert_ne!(links.len(), 0);
            assert_eq!(blocks.iter().filter(|b| b["type"] == "text").count(), 1);
            let modern = version >= ProtocolVersion::V2025_06_18.as_str();
            assert_eq!(result.get("structuredContent").is_some(), modern);
            assert_eq!(blocks.len(), if modern { links.len() + 1 } else { 1 });
            for (i, link) in links.iter().enumerate() {
                assert!(link["uri"].as_str().unwrap().starts_with("arb://"));
                for k in ["uri", "name", "mimeType"] {
                    assert!(link[k].is_string());
                    if modern {
                        assert_eq!(blocks[i + 1]["type"], "resource_link");
                        assert_eq!(blocks[i + 1][k], link[k]);
                    }
                }
            }
            let checked = c.check_tool(tool, result);
            if tool == "arb_save_and_inspect" {
                let path = Path::new(checked["save"]["bundle_path"].as_str().unwrap());
                assert!(path.is_dir() && path.starts_with(&dest));
                assert_eq!(checked["save"]["note"], "調査");
                same_errors_as_overview(
                    &mut c,
                    &mut rpc,
                    &checked,
                    &json!({"exclude_tags":["VendorHal"]}),
                    5,
                );
            } else {
                assert_eq!(checked["frames"][0]["frame_index"], 0);
            }
        }
    }
    c.tool(
        "arb_export_trim",
        json!({"bundle_id":id,"full_range":true,"output_path":root.path().join("clip.mp4")}),
    );
    c.tool(
        "arb_export_trim",
        json!({"bundle_id":id,"format":"bundle","in_frame_index":0,"out_frame_index":4}),
    );
    c.tool(
        "arb_export_frame_png",
        json!({"bundle_id":id,"frame_index":2,"output_path":root.path().join("frame.png")}),
    );
    let collision=c.result("tools/call",json!({"name":"arb_export_frame_png","arguments":{"bundle_id":id,"frame_index":2,"output_path":root.path().join("frame.png")}}));
    assert_eq!(collision["structuredContent"]["rpc_code"], -32002);
    let hint = collision["structuredContent"]["hint"].as_str().unwrap();
    for word in ["上書きされません", "output_path", "output_dir"] {
        assert!(hint.contains(word), "{hint}");
    }
    // Automatic names never collide with each other or with existing files.
    let pngs = root.path().join("pngs");
    let written: Vec<_> = (0..2)
        .map(|_| {
            let out = c.tool(
                "arb_export_frame_png",
                json!({"bundle_id":id,"frame_index":2,"output_dir":pngs}),
            );
            let path = std::path::PathBuf::from(out["output_path"].as_str().unwrap());
            assert!(path.starts_with(&pngs) && path.is_file(), "{out}");
            assert_eq!(out["report"]["frame_index"], 2);
            path
        })
        .collect();
    assert_ne!(written[0], written[1]);
    assert!(written.iter().all(|p| p.is_file()));
    let reversed=c.result("tools/call",json!({"name":"arb_inspect_frames","arguments":{"bundle_id":id,"mode":"contact_sheet","start_frame":12,"end_frame":4}}));
    assert_eq!(reversed["structuredContent"]["rpc_code"], -32602);
    for format in ["share_mp4", "bundle"] {
        let cut=c.tool("arb_export_trim",json!({"format":format,"bundle_id":id,"in_time_ns":12_345_678,"out_time_ns":23_456_789}));
        assert_eq!(cut["in_time_ns"], 12_345_678);
        assert_eq!(cut["out_time_ns"], 23_456_789);
    }
    for args in [
        json!({"bundle_id":id,"in_time_ns":1}),
        json!({"bundle_id":id,"in_time_ns":5,"out_time_ns":4}),
        json!({"bundle_id":id,"in_time_ns":5,"out_time_ns":99_000_000_000_u64}),
    ] {
        let r = c.request(
            "tools/call",
            json!({"name":"arb_export_trim","arguments":args}),
        );
        assert!(
            r["error"]["code"] == -32602 || r["result"]["structuredContent"]["rpc_code"] == -32602,
            "{r}"
        );
    }
    resources_and_prompts(&mut c, id);
    let trashed = c.tool("arb_trash_bundle", target.clone());
    assert_eq!(trashed["frame_count"], 18);
    assert!(trashed.get("created_utc").is_none());
    let duplicate = c.result(
        "tools/call",
        json!({"name":"arb_trash_bundle","arguments":target}),
    );
    assert_eq!(
        duplicate["structuredContent"]["error_code"],
        "ALREADY_TRASHED"
    );
    assert_eq!(duplicate["structuredContent"]["retryable"], false);
    c.tool(
        "arb_gc_bundles",
        json!({"dry_run":true,"ignore_grace":true}),
    );
    let restored = c.tool("arb_restore_bundle", target.clone());
    assert_eq!(restored["frame_count"], trashed["frame_count"]);
    let duplicate = c.result(
        "tools/call",
        json!({"name":"arb_restore_bundle","arguments":target}),
    );
    assert_eq!(
        duplicate["structuredContent"]["error_code"],
        "ALREADY_ACTIVE"
    );
    c.tool("arb_forget_bundle", target);
    tasks(&mut c);
    assert_eq!(c.covered, c.schemas.keys().cloned().collect());
    drop(c);
    drop(rpc);
    if let Some(mut p) = process {
        drop(p.stdin.take());
        assert!(p.wait().unwrap().success());
    }
}

fn tasks(c: &mut Client) {
    c.result("logging/setLevel", json!({"level":"info"}));
    for method in ["tasks/get", "tasks/result", "tasks/cancel"] {
        for (params, message) in [
            (json!({}), "必須"),
            (json!({"taskId":123}), "文字列"),
            (json!({"taskId":"absent"}), "期限切れ"),
        ] {
            let reply = c.request(method, params);
            assert_eq!(reply["error"]["code"], -32602);
            assert!(
                reply["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(message)
            );
        }
    }
    let unsupported = c.request("tools/call", json!({"name":"arb_status","task":{}}));
    assert_eq!(unsupported["error"]["code"], -32602);
    c.events.clear();
    let call = json!({"name":"arb_save_and_inspect","arguments":{"logs_only":true,"memo":"調査"},"task":{"ttl":60000},"_meta":{"progressToken":"save"}});
    let task = c.result("tools/call", call)["task"].clone();
    assert_eq!(task["status"], "working");
    let params = json!({"taskId":task["taskId"]});
    let done = c.result("tasks/result", params.clone());
    assert_eq!(
        done["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
        task["taskId"]
    );
    assert_eq!(c.check_tool("arb_save_and_inspect", done)["memo"], "調査");
    assert_eq!(c.result("tasks/get", params.clone())["status"], "completed");
    assert_eq!(c.request("tasks/cancel", params)["error"]["code"], -32602);
    assert!(
        c.result("tasks/list", json!({}))["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["taskId"] == task["taskId"])
    );
    let mut progress = 0;
    for n in &c.events {
        if n["method"] == "notifications/progress" {
            assert_eq!(n["params"]["progressToken"], "save");
            let step = n["params"]["progress"].as_u64().unwrap();
            assert!(step > progress);
            progress = step;
        }
    }
    assert!(progress >= 6);
    assert!(
        !c.events
            .iter()
            .any(|n| n["method"] == "notifications/message")
    );
    let invalid = c.result(
        "tools/call",
        json!({"name":"arb_trim_bundle","arguments":{"bundle_id":123},"task":{"ttl":5000}}),
    );
    let target = json!({"taskId":invalid["task"]["taskId"]});
    let failed = c.result("tasks/result", target.clone());
    assert_eq!(failed["structuredContent"]["error_code"], "INVALID_PARAMS");
    assert_eq!(c.result("tasks/get", target)["status"], "failed");
    let missing = c.result(
        "tools/call",
        json!({"name":"arb_export_share_mp4","arguments":{"bundle_id":"absent-bundle","full_range":true},"task":{}}),
    );
    let missing = c.result("tasks/result", json!({"taskId":missing["task"]["taskId"]}));
    assert_eq!(missing["structuredContent"]["rpc_code"], -32001);
    let tiny = c.result(
        "tools/call",
        json!({"name":"arb_trim_bundle","arguments":{"bundle_id":false},"task":{"ttl":1}}),
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        c.request("tasks/get", json!({"taskId":tiny["task"]["taskId"]}))["error"]["code"],
        -32602
    );
    let dir = std::path::PathBuf::from(std::env::var_os("FAKE_DEVICE_STATE").unwrap());
    fs::write(dir.join("block"), []).unwrap();
    let task = c.result(
        "tools/call",
        json!({"name":"arb_capture_start","arguments":{"wait_connected":true},"task":{}}),
    )["task"]
        .clone();
    let params = json!({"taskId":task["taskId"]});
    let waiter = c.send("tasks/result", params.clone());
    let result = c.result("tasks/cancel", params.clone());
    fs::remove_file(dir.join("block")).unwrap();
    assert_eq!(result["status"], "cancelled");
    assert_eq!(
        c.recv(waiter)["result"]["structuredContent"]["error_code"],
        "CANCELLED"
    );
    assert_eq!(c.result("tasks/get", params)["status"], "cancelled");
}

/// `time_zone: "host"` adds RFC 3339 local time with an explicit numeric offset.
fn assert_local_time(value: &Value) {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("local time: {value}"));
    let b = text.as_bytes();
    assert_eq!(b.len(), 29, "{text}");
    assert!(
        matches!(b[23], b'+' | b'-') && b[26] == b':' && b[10] == b'T' && b[19] == b'.',
        "{text}"
    );
}

fn fixture_log(seq: u64, level: &str, tag: &str, message: &str) -> LogRecord {
    let frame = i64::try_from(seq % 17).unwrap();
    let bt = BASE_BT_NS + frame * FRAME_STEP_NS;
    LogRecord {
        seq,
        boot_id: Some("boot-mcp".into()),
        header_epoch_ns: Some(1_795_829_025_123_000_000 + i64::try_from(seq).unwrap() * 1_000_000),
        device_read_boottime_ns: Some(bt),
        pc_ingest_cont_ns: bt,
        alignment: TimeAlignment::Anchored {
            boot_id: "boot-mcp".into(),
            boottime_ns: bt,
            half_width_ns: 1_000_000,
            semantics: TimestampSemantics::LogcatDeviceReadBoottime,
        },
        bucket: RecordBucket::Main,
        level: Some(level.into()),
        tag: Some(tag.into()),
        pid: Some(42),
        tid: Some(42),
        uid: None,
        package_name: None,
        message: message.into(),
    }
}

/// #574: repeated non-fatal tags are suggested, never removed; fatal tags are never suggested.
fn noise_and_local_time(c: &mut Client, work: &Path, paths: &ArbPaths) {
    let mut extra = Vec::new();
    for i in 0..12 {
        extra.push(fixture_log(
            10 + i,
            "E",
            "VendorHal",
            if i % 2 == 0 {
                "poll failed: -110"
            } else {
                "poll failed: -11"
            },
        ));
    }
    for i in 0..8 {
        extra.push(fixture_log(30 + i, "E", "AudioHal", "underrun"));
    }
    extra.push(fixture_log(40, "F", "audiohal", "fatal underrun"));
    extra.push(fixture_log(41, "E", "MyApp", "request failed"));
    extra.push(fixture_log(42, "E", "MyApp", "retry failed"));
    let bundle = seed_bundle_with_events(
        work,
        &paths.data_dir.join("external-noise"),
        "bundle-noise",
        extra,
        vec![super::fixture_tap()],
    );
    c.init(ProtocolVersion::V2025_11_25.as_str());
    let target = json!({"bundle_id":bundle,"time_zone":"host"});
    let all = c.tool("arb_timeline_overview", target.clone());
    assert_eq!(
        all["noise_candidates"],
        json!([{"tag":"VendorHal","total_count":12,"unique_messages":2,"sample_message":"poll failed: -11"}])
    );
    assert_eq!(all["total_error_logs"], 24);
    assert_eq!(all["unique_error_logs"], 7);
    let highlights = all["error_log_highlights"].as_array().unwrap();
    assert!(
        highlights.iter().any(|h| h["tag"] == "VendorHal"),
        "suggesting a tag hides nothing"
    );
    for h in highlights.iter().filter(|h| h["tag"] == "VendorHal") {
        assert_local_time(&h["timestamp_local"]);
    }
    // Excluding is the caller's explicit choice and removes exactly that tag's rows.
    let mut args = target;
    args["exclude_tags"] = json!(["vendorhal"]);
    let excluded = c.tool("arb_timeline_overview", args);
    assert_eq!(excluded["total_error_logs"], 12);
    assert_eq!(excluded["noise_candidates"], json!([]));
    assert_eq!(excluded["crashes"], all["crashes"]);
    let rows = c.tool(
        "arb_query_logs",
        json!({"bundle_id":bundle,"tag":"VendorHal","time_zone":"host"}),
    );
    assert_eq!(
        rows["returned_count"], 12,
        "query_logs is never filtered by suggestions"
    );
    assert_eq!(rows["fields"], "compact");
    assert_eq!(rows["boot_id"], "boot-mcp");
    for log in rows["logs"].as_array().unwrap() {
        assert_local_time(&log["timestamp_local"]);
        assert!(log.get("boot_id").is_none() && log.get("alignment").is_none());
    }
    let utc = c.tool(
        "arb_query_logs",
        json!({"bundle_id":bundle,"tag":"VendorHal"}),
    );
    assert!(
        utc["logs"][0].get("timestamp_local").is_none(),
        "UTC is the default"
    );
    // W3B: `arb_query_logs` applies `exclude_tags` with the overview's rule on every version,
    // so the raw E+F rows are exactly the rows behind the excluded overview's groups.
    for version in [
        ProtocolVersion::V2024_11_05,
        ProtocolVersion::V2025_03_26,
        ProtocolVersion::V2025_06_18,
        ProtocolVersion::V2025_11_25,
    ] {
        c.init(version.as_str());
        let raw = c.tool(
            "arb_query_logs",
            json!({"bundle_id":bundle,"min_level":"E","bucket":"main","exclude_tags":["vendorhal"]}),
        );
        let logs = raw["logs"].as_array().unwrap();
        assert_eq!(
            raw["total_matched"], excluded["total_error_logs"],
            "{version:?}"
        );
        assert_eq!(logs.len(), 12, "{version:?}");
        assert!(logs.iter().all(|r| r["tag"] != "VendorHal"), "{version:?}");
        let tags: BTreeSet<_> = logs.iter().map(|r| r["tag"].as_str().unwrap()).collect();
        assert!(
            ["AudioHal", "audiohal", "MyApp"]
                .iter()
                .all(|t| tags.contains(t))
        );
        let unfiltered = c.tool(
            "arb_query_logs",
            json!({"bundle_id":bundle,"min_level":"E","bucket":"main"}),
        );
        assert_eq!(
            unfiltered["total_matched"], all["total_error_logs"],
            "{version:?}"
        );
    }
    c.init(ProtocolVersion::V2025_11_25.as_str());
    let listed = c.tool("arb_list_bundles", json!({"time_zone":"host"}));
    for entry in listed["bundles"].as_array().unwrap() {
        assert_local_time(&entry["created_local"]);
    }
    let listed = c.tool("arb_list_bundles", json!({}));
    assert!(listed["bundles"][0].get("created_local").is_none());
}

/// #586-B: `fields` defaults to compact on every version; `full` stays available.
/// #589 / #584: crash stacks and process events are read from the saved logs, and a `package`
/// filter annotates every row instead of hiding it, warning when the package is never seen.
fn crash_detail_and_package_filter(c: &mut Client, work: &Path, paths: &ArbPaths) {
    let row = |seq: u64, tag: &str, uid: u32, pid: i32, message: &str| {
        let bt = BASE_BT_NS + 9 * FRAME_STEP_NS + i64::try_from(seq).unwrap() * 1_000_000;
        LogRecord {
            seq,
            boot_id: Some("boot-mcp".into()),
            header_epoch_ns: None,
            device_read_boottime_ns: Some(bt),
            pc_ingest_cont_ns: bt,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-mcp".into(),
                boottime_ns: bt,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::LogcatDeviceReadBoottime,
            },
            bucket: RecordBucket::Main,
            level: Some("E".into()),
            tag: Some(tag.into()),
            pid: Some(pid),
            tid: Some(pid),
            uid: Some(uid),
            package_name: None,
            message: message.into(),
        }
    };
    let app_frame = "at com.example.demo.LoginActivity.onCreate(LoginActivity.kt:7)";
    let extra = vec![
        row(10, "AndroidRuntime", 10_200, 1234, "FATAL EXCEPTION: main"),
        row(
            11,
            "AndroidRuntime",
            10_200,
            1234,
            "Process: com.example.demo, PID: 1234",
        ),
        row(
            12,
            "AndroidRuntime",
            10_200,
            1234,
            "java.lang.IllegalStateException: boom",
        ),
        row(
            13,
            "AndroidRuntime",
            10_200,
            1234,
            "\tat android.os.Looper.loop(Looper.java:1)",
        ),
        row(
            14,
            "AndroidRuntime",
            10_200,
            1234,
            &format!("\t{app_frame}"),
        ),
        row(
            15,
            "AndroidRuntime",
            10_200,
            1234,
            "Caused by: java.lang.NullPointerException: root",
        ),
        row(
            20,
            "ActivityManager",
            1_000,
            600,
            "Force stopping com.example.other appid=10300 user=0: from pid 600",
        ),
        row(
            21,
            "ActivityManager",
            1_000,
            600,
            "Killing 4321:com.example.other/u0a300 (adj 0): stop com.example.other due to from pid 600",
        ),
        row(
            22,
            "ActivityManager",
            10_200,
            1234,
            "Process com.example.victim (pid 99) has died: fg TOP",
        ),
    ];
    let bundle = seed_bundle_with_events(
        work,
        &paths.data_dir.join("external-crash"),
        "bundle-crash",
        extra,
        vec![super::fixture_tap()],
    );
    c.init(ProtocolVersion::V2025_11_25.as_str());
    let all = c.tool("arb_timeline_overview", json!({"bundle_id":bundle}));
    let crash = &all["crashes"][0];
    assert_eq!(crash["package_name"], "com.example.demo");
    assert_eq!(crash["first_app_frame"], app_frame);
    assert_eq!(
        crash["stack_top"],
        json!(["at android.os.Looper.loop(Looper.java:1)", app_frame])
    );
    assert_eq!(
        crash["caused_by"],
        "Caused by: java.lang.NullPointerException: root"
    );
    assert!(
        crash.get("matches_package").is_none(),
        "no annotation without a filter"
    );
    let events = all["process_events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "forged app-UID death is ignored: {events:?}"
    );
    assert_eq!(events[0]["kind"], "force_stop");
    assert_eq!(events[0]["package_name"], "com.example.other");
    assert_eq!(events[0]["pid"], 4321);
    assert_eq!(events[0]["is_crash"], false);
    assert!(events[0]["offset_ms"].is_i64(), "placed like log rows");
    assert_eq!(all["package_filter"], Value::Null);
    assert_eq!(all["warnings"], json!([]));
    let linked: Vec<_> = all["error_log_highlights"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["tag"] == "AndroidRuntime")
        .map(|h| &h["crash_index"])
        .collect();
    assert!(
        !linked.is_empty() && linked.iter().all(|i| **i == json!(0)),
        "{linked:?}"
    );

    let lengths = |ov: &Value| {
        ["crashes", "process_events", "screens", "taps", "app_logs"]
            .map(|k| ov[k].as_array().unwrap().len())
    };
    let own = c.tool(
        "arb_timeline_overview",
        json!({"bundle_id":bundle,"package":"com.example.demo"}),
    );
    assert_eq!(lengths(&own), lengths(&all), "a filter never drops rows");
    assert_eq!(own["warnings"], json!([]));
    assert_eq!(own["crashes"][0]["matches_package"], true);
    assert_eq!(own["process_events"][0]["matches_package"], false);
    assert_eq!(own["screens"][0]["matches_package"], true);
    assert_eq!(own["taps"][0]["foreground_package"], "com.example.demo");
    let filter = &own["package_filter"];
    assert_eq!(filter["package"], "com.example.demo");
    assert_eq!(
        filter["applied_to"],
        json!(["error_log_highlights", "noise_candidates"])
    );
    assert_eq!(filter["matched_crashes"], 1);
    assert_eq!(filter["matched_process_events"], 0);
    assert_eq!(filter["matched_screens"], 1);

    let missing = c.tool(
        "arb_timeline_overview",
        json!({"bundle_id":bundle,"package":"no.such.pkg"}),
    );
    assert_eq!(
        lengths(&missing),
        lengths(&all),
        "a filter never drops rows"
    );
    assert_eq!(missing["error_log_highlights"], json!([]));
    assert_eq!(missing["warnings"][0]["code"], "package_not_seen");
    assert_eq!(missing["warnings"][0]["details"]["package"], "no.such.pkg");
    let sample = &missing["package_filter"]["known_packages_sample"];
    assert_eq!(
        missing["warnings"][0]["details"]["known_packages_sample"],
        *sample
    );
    for known in ["com.example.demo", "com.example.other"] {
        assert!(
            sample.as_array().unwrap().contains(&json!(known)),
            "{sample}"
        );
    }
    assert!(
        missing["crashes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["matches_package"] == false)
    );

    for blank in ["", "   "] {
        let ov = c.tool(
            "arb_timeline_overview",
            json!({"bundle_id":bundle,"package":blank}),
        );
        assert_eq!(ov["package_filter"], Value::Null, "{blank:?} is no filter");
        assert_eq!(ov["warnings"], json!([]), "{blank:?} is no filter");
        assert_eq!(
            ov["error_log_highlights"], all["error_log_highlights"],
            "{blank:?} is no filter"
        );
    }
    let padded = c.tool(
        "arb_timeline_overview",
        json!({"bundle_id":bundle,"package":" com.example.demo "}),
    );
    assert_eq!(padded["package_filter"], own["package_filter"], "trimmed");
    assert_eq!(padded["warnings"], json!([]), "trimmed");
    assert_eq!(padded["crashes"], own["crashes"], "trimmed");
    assert_eq!(
        padded["error_log_highlights"], own["error_log_highlights"],
        "trimmed"
    );
}

fn log_fields(c: &mut Client, id: &str, version: &str) {
    let page = c.tool("arb_query_logs", json!({"bundle_id":id}));
    assert_eq!(page["fields"], "compact", "{version}");
    assert!(page["logs"][0].get("alignment").is_none(), "{version}");
    assert_eq!(page["truncated_by_byte_budget"], false);
    let full = c.tool("arb_query_logs", json!({"bundle_id":id,"fields":"full"}));
    let light = c.tool("arb_query_logs", json!({"bundle_id":id,"fields":"compact"}));
    assert_eq!(light["boot_id"], "boot-mcp");
    assert_eq!(full["total_matched"], light["total_matched"]);
    for (f, l) in full["logs"]
        .as_array()
        .unwrap()
        .iter()
        .zip(light["logs"].as_array().unwrap())
    {
        for key in [
            "seq",
            "timestamp_utc",
            "offset_ms",
            "frame_index",
            "level",
            "tag",
            "message",
        ] {
            assert_eq!(f[key], l[key], "{key}");
        }
        assert_eq!(f["boot_id"], "boot-mcp");
    }
}

/// #571: `include_images` adds a PNG block; the body keeps one shape.
fn inspect_shape(c: &mut Client, id: &str) {
    let mut args = json!({"bundle_id":id,"mode":"contact_sheet","start_frame":0,"end_frame":5,"include_images":false});
    validate::validate(
        &c.schemas["arb_inspect_frames"]["inputSchema"],
        &args,
        "arguments",
    )
    .unwrap();
    // The unlisted alias with the same value is still one target.
    args["bundle_id_or_path"] = json!(id);
    let meta = c.tool("arb_inspect_frames", args.clone());
    let mut inline_args = args;
    inline_args["include_images"] = json!(true);
    let r = c.result(
        "tools/call",
        json!({"name":"arb_inspect_frames","arguments":inline_args}),
    );
    let images = r["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "image")
        .count();
    assert_eq!(images, 1);
    let with = c.check_tool("arb_inspect_frames", r);
    for key in [
        "resource_links",
        "mode",
        "bundle_id",
        "frame_indices_rendered",
        "width",
        "height",
        "screens",
        "frames",
    ] {
        assert_eq!(with[key], meta[key], "{key}");
    }
    let keys = |body: &Value| {
        body.as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(keys(&meta), keys(&with));
    assert_eq!(meta.get("png_path"), Some(&Value::Null));
    assert_eq!(meta.get("png_bytes_len"), Some(&Value::Null));
    assert!(with["png_path"].is_string() && with["png_bytes_len"].as_u64() > Some(0));
    let screens = meta["screens"].as_array().unwrap();
    let mut previous = None;
    for frame in meta["frames"].as_array().unwrap() {
        if let Some(i) = frame["screen_index"].as_u64() {
            assert!(usize::try_from(i).unwrap() < screens.len());
        }
        let ns = frame["offset_ns"].as_u64().unwrap();
        assert_eq!(frame["offset_ms"].as_u64(), Some(ns / 1_000_000));
        assert!(previous < Some(ns));
        previous = Some(ns);
    }
    assert_eq!(meta["frames"][0]["offset_ns"], 0);
}

/// #563: the saved bundle's error view equals `arb_timeline_overview` with the same filters.
fn same_errors_as_overview(
    c: &mut Client,
    rpc: &mut RpcClient,
    checked: &Value,
    filters: &Value,
    max: u64,
) {
    // The shared table names which arguments correspond and which fields must be equal.
    let table: Value = serde_json::from_str(include_str!(
        "../../../../../docs/specs/mcp-tools-table.json"
    ))
    .unwrap();
    let parity = &table["investigationParity"];
    let saved = &checked["save"]["bundle_id"];
    let mut args = json!({"bundle_id":saved});
    for (save_arg, overview_arg) in parity["arguments"].as_object().unwrap() {
        let value = filters
            .get(save_arg)
            .cloned()
            .or_else(|| (save_arg == "max_errors").then(|| json!(max)));
        if let Some(value) = value {
            args[overview_arg.as_str().unwrap()] = value;
        }
    }
    let ov = c.tool("arb_timeline_overview", args);
    for (save_path, overview_path) in parity["fields"].as_object().unwrap() {
        let mine = checked
            .pointer(save_path)
            .unwrap_or_else(|| panic!("{save_path}"));
        let theirs = ov.pointer(overview_path.as_str().unwrap());
        assert_eq!(Some(mine), theirs, "{save_path}");
    }
    let groups = &checked["error_logs"];
    assert_eq!(
        groups["returned_count"],
        json!(ov["error_log_highlights"].as_array().unwrap().len())
    );
    assert_eq!(
        groups["continuation"].is_null(),
        groups["has_more"] == false
    );
    assert_eq!(groups["raw_query"]["tool"], "arb_query_logs");
    assert_eq!(&groups["raw_query"]["arguments"]["bundle_id"], saved);
    let call = &groups["raw_query"];
    let name = call["tool"].as_str().unwrap();
    validate::validate(
        &c.schemas[name]["inputSchema"],
        &call["arguments"],
        "raw_query",
    )
    .unwrap();
    // W3B: the raw rows behind the groups use the same exclusion as the groups.
    assert_eq!(
        call["arguments"].get("exclude_tags"),
        filters.get("exclude_tags")
    );
    let page = c.tool(name, call["arguments"].clone());
    let levels: Vec<_> = page["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["level"].as_str().unwrap())
        .collect();
    assert_eq!(levels, ["E", "F"]);
    assert_eq!(page["total_matched"], groups["total_matched"]);
    assert_eq!(page["has_more"], false);
    // Excluding the tag of those rows (any case) leaves none of them.
    let mut hidden = call["arguments"].clone();
    hidden["exclude_tags"] = json!([page["logs"][0]["tag"].as_str().unwrap().to_uppercase()]);
    let none = c.tool(name, hidden);
    assert_eq!(
        (&none["total_matched"], &none["returned_count"]),
        (&json!(0), &json!(0))
    );
    let exact = c.tool(name, json!({"bundle_id":saved,"level":"E"}));
    assert_eq!(exact["total_matched"], 1);
    let summary = rpc
        .call("timeline.summary", json!({"bundle_id_or_path":saved}))
        .unwrap();
    assert_eq!(
        checked["summary"], summary,
        "summary is the daemon response verbatim"
    );
}
