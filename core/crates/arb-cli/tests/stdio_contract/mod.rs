//! All-tool contract test against an isolated arbd with synthetic capture and ADB discovery.
use super::{build_minimal_valid_png, seed_bundle};
use arb_adb::mock::FaultScenario;
use arb_core::config::CaptureMode;
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

#[path = "../../src/mcp/schema.rs"]
mod schema;

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
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let mut params = json!({"name":name});
        params["arguments"] = args;
        let r = self.result("tools/call", params);
        self.check_tool(name, r)
    }
    fn check_tool(&mut self, name: &str, mut r: Value) -> Value {
        assert_eq!(r["isError"], false, "{name}: {r}");
        let body = r["structuredContent"].take();
        schema::validate(&self.schemas[name]["outputSchema"], &body, name)
            .unwrap_or_else(|e| panic!("{e}: {body}"));
        let text = r["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["type"] == "text")
            .unwrap()["text"]
            .as_str()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            body,
            "{name}: legacy text"
        );
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

fn assert_rejected(c: &mut Client, name: &str, args: &Value) {
    let r = c.result("tools/call", json!({"name":name,"arguments":args}));
    assert_eq!(r["isError"], true, "{name} {args}");
    if let Some(def) = c.schemas.get(name) {
        schema::validate(&def["outputSchema"], &r["structuredContent"], name)
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
        "2024-11-05",
        "2025-03-26",
        "2025-06-18",
        "2025-11-25",
        "2099-11-25",
    ] {
        let init = c.init(version);
        assert_eq!(
            init["protocolVersion"],
            if version.starts_with("2099") {
                "2025-11-25"
            } else {
                version
            }
        );
        assert_eq!(init["serverInfo"]["name"], "android-replay-buffer");
        assert_eq!(
            init["capabilities"].get("tasks").is_some(),
            version >= "2025-11-25"
        );
        let tools = c.result("tools/list", json!({}));
        assert_eq!(tools["tools"].as_array().unwrap().len(), 20);
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
            let legacy = version < "2025-11-25";
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
    let tools = c.result("tools/list", json!({}));
    for def in tools["tools"].as_array().unwrap() {
        let name = def["name"].as_str().unwrap();
        let input = &def["inputSchema"];
        assert_eq!(input["type"], "object");
        assert_eq!(def["outputSchema"]["type"], "object");
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(def["annotations"][hint].is_boolean());
        }
        assert!(def["annotations"]["title"].is_string());
        c.schemas.insert(name.to_owned(), def.clone());
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
                _ => panic!("unexpected required {name}.{k}"),
            };
        }
        if input["properties"].get("bundle_id").is_some() {
            base["bundle_id"] = json!("fixture");
        }
        schema::normalize_integers(input, &mut base);
        schema::validate(input, &base, name).unwrap_or_else(|e| panic!("{name}: {e}"));
        if base.get("bundle_id").is_some() {
            let mut absent = base.clone();
            absent["bundle_id"] = json!("no-such-bundle-contract");
            let result = c.result("tools/call", json!({"name":name,"arguments":absent}));
            assert_eq!(result["isError"], true, "{name}");
            let error = &result["structuredContent"];
            assert_eq!(error["error_code"], "BUNDLE_NOT_FOUND", "{name}: {error}");
            assert_eq!(error["rpc_code"], -32001, "{name}");
            schema::validate(&def["outputSchema"], error, name).unwrap();
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
    assert_eq!(
        c.result("resources/templates/list", json!({}))["resourceTemplates"]
            .as_array()
            .unwrap()
            .len(),
        5
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
        &mut FaultScenario::NormalFhd30.build(),
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
    for name in [
        "arb_status",
        "arb_devices",
        "arb_config_get",
        "arb_list_bundles",
    ] {
        c.tool(name, json!({}));
    }
    c.tool("arb_config_update", json!({"save_window_secs":10}));
    let cap = c.tool(
        "arb_capture_start",
        json!({"wait_connected":true,"wait_timeout_ms":1000,"capture_mode":"logs_only"}),
    );
    assert!(cap["connected"].is_boolean());
    c.tool("arb_capture_stop", json!({}));
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
        assert_eq!(saved["requested_duration_secs"], 5);
        assert_eq!(saved["note"], "調査記録");
        assert!(saved["warning_codes"].is_array());
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
    for (version, inline, expected) in [
        ("2024-11-05", None, true),
        ("2025-06-18", None, true),
        ("2025-11-25", None, false),
        ("2025-11-25", Some(true), true),
    ] {
        c.init(version);
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
        c.check_tool("arb_screenshot", result);
        assert!(out.is_file());
    }
    fs::remove_file(adb_dir.join("devices")).unwrap();
    let overview = c.tool("arb_timeline_overview", target.clone());
    assert!(overview["logcat_completeness"].is_object());
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
    let r = c.result(
        "tools/call",
        json!({"name":"arb_inspect_frames","arguments":{"bundle_id":id,"mode":"single"}}),
    );
    assert_eq!(r["content"][1]["type"], "resource_link");
    assert_eq!(r["content"].as_array().unwrap().len(), 2);
    assert_eq!(
        c.check_tool("arb_inspect_frames", r)["frames"][0]["frame_index"],
        0
    );
    c.tool(
        "arb_export_trim",
        json!({"bundle_id":id,"output_path":root.path().join("clip.mp4")}),
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
        json!({"name":"arb_export_share_mp4","arguments":{"bundle_id":"absent-bundle"},"task":{}}),
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
