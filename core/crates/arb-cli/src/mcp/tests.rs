use std::collections::BTreeSet;

use std::io::Cursor;

use super::{
    LineRead, McpServer, base64_encode, catalog, is_retry_safe_method, read_bounded_line,
    version::ProtocolVersion,
};
use arb_os::paths::ArbPaths;
use serde_json::{Value, json};

#[test]
fn tool_registry_matches_known_tools_with_valid_schemas() {
    let defs = catalog::for_version(ProtocolVersion::default());
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

/// One `tools/list` frame stays below 90,000 bytes on every negotiated version (target 86,000).
#[test]
fn tools_list_frame_fits_the_budget_on_every_version() {
    use ProtocolVersion::{V2024_11_05, V2025_03_26, V2025_06_18, V2025_11_25};
    for version in [V2024_11_05, V2025_03_26, V2025_06_18, V2025_11_25] {
        let frame = super::rpc_ok(&json!(2), &json!({"tools":catalog::for_version(version)}));
        let bytes = frame.to_string().len() + 1;
        assert!(bytes <= 86_000, "{version:?}: {bytes} bytes");
    }
}

#[test]
fn only_equal_bundle_aliases_normalize_to_one_target() {
    for tool in catalog::for_version(ProtocolVersion::default()) {
        let schema = &tool["inputSchema"];
        if schema["properties"].get("bundle_id_or_path").is_none() {
            continue;
        }
        let mut args = json!({"bundle_id":"b","bundle_id_or_path":"b"});
        let name = tool["name"].as_str().unwrap();
        match name {
            "arb_inspect_frames" => args["mode"] = json!("single"),
            "arb_export_frame_png" => {
                args["frame_index"] = json!(0);
                args["output_dir"] = json!("out");
            }
            _ => (),
        }
        let normalized = super::args::normalize(name, schema, args.clone()).unwrap();
        assert_eq!(normalized["bundle_id"], "b");
        assert!(!normalized.contains_key("bundle_id_or_path"));
        args["bundle_id_or_path"] = json!("other");
        assert_eq!(
            super::args::normalize(name, schema, args)
                .unwrap_err()
                .details,
            json!({"conflicts":["bundle_id","bundle_id_or_path"]})
        );
    }
}

/// Only the version predicates choose defaults: inline screenshots end with the March release,
/// and compact log rows begin with structured content.
#[test]
fn defaults_follow_the_version_predicates() {
    use ProtocolVersion::{V2024_11_05, V2025_03_26, V2025_06_18, V2025_11_25};
    for (version, inline, compact) in [
        (V2024_11_05, true, false),
        (V2025_03_26, true, false),
        (V2025_06_18, false, true),
        (V2025_11_25, false, true),
    ] {
        let tools = catalog::for_version(version);
        let tool = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(version.legacy_screenshot_inline(), inline);
        assert_eq!(version.default_compact_logs(), compact);
        assert_eq!(
            tool("arb_screenshot")["inputSchema"]["properties"]["include_image"]["default"],
            inline
        );
        assert_eq!(
            tool("arb_query_logs")["inputSchema"]["properties"]["fields"]["default"],
            if compact { "compact" } else { "full" }
        );
        // The same ErrorGroups contract and filters on every version (#563).
        let save = tool("arb_save_and_inspect");
        let errors = &save["outputSchema"]["oneOf"][0]["properties"]["error_logs"];
        assert!(errors.to_string().contains("continuation"), "{version:?}");
        for key in ["package", "tag", "exclude_tags", "time_zone"] {
            assert_eq!(
                save["inputSchema"]["properties"][key],
                tool("arb_timeline_overview")["inputSchema"]["properties"][key],
                "{key}"
            );
        }
        assert_eq!(
            save["inputSchema"]["properties"]["max_errors"]["maximum"],
            200
        );
    }
}

/// #563: the parity table pairs each `arb_save_and_inspect` filter with the overview argument of
/// the same meaning, type and validation, and the projection forwards exactly those filters.
#[test]
fn investigation_parity_table_matches_both_schemas_and_the_projection() {
    use ProtocolVersion::{V2024_11_05, V2025_03_26, V2025_06_18, V2025_11_25};
    let table: Value = serde_json::from_str(include_str!(
        "../../../../../docs/specs/mcp-tools-table.json"
    ))
    .unwrap();
    let pairs = table["investigationParity"]["arguments"]
        .as_object()
        .unwrap();
    for version in [V2024_11_05, V2025_03_26, V2025_06_18, V2025_11_25] {
        let tools = catalog::for_version(version);
        let input = |name: &str| {
            tools.iter().find(|t| t["name"] == name).unwrap()["inputSchema"]["properties"].clone()
        };
        let (save, overview) = (
            input("arb_save_and_inspect"),
            input("arb_timeline_overview"),
        );
        assert!(
            save["dest_dir"]["description"]
                .as_str()
                .unwrap()
                .contains("管理域外は GC 対象外")
        );
        assert!(
            overview["max_error_logs"]["description"]
                .as_str()
                .unwrap()
                .contains("最新")
        );
        let investigation = tools
            .iter()
            .find(|tool| tool["name"] == "arb_save_and_inspect")
            .unwrap();
        let groups =
            &investigation["outputSchema"]["oneOf"][0]["properties"]["error_logs"]["properties"];
        assert!(
            groups["continuation"]["description"]
                .as_str()
                .unwrap()
                .contains("オフセットカーソル")
        );
        assert!(
            groups["raw_query"]["description"]
                .as_str()
                .unwrap()
                .contains("E・F")
        );
        for (save_arg, overview_arg) in pairs {
            let (a, b) = (&save[save_arg], &overview[overview_arg.as_str().unwrap()]);
            if save_arg == "max_errors" {
                // Same meaning and cap; the save tool asks for at least one group.
                assert_eq!(a["description"], b["description"]);
                assert_eq!(a["maximum"], b["maximum"]);
                assert!(a["minimum"].as_u64() >= b["minimum"].as_u64());
            } else {
                assert_eq!(a, b, "{version:?} {save_arg}");
            }
        }
    }
    let table_filters: BTreeSet<&str> = pairs
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "max_errors")
        .collect();
    let forwarded: BTreeSet<&str> = super::project::overview::FILTERS.into_iter().collect();
    assert_eq!(table_filters, forwarded);
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
                "params": { "protocolVersion": ProtocolVersion::V2024_11_05.as_str() }
            })
            .to_string(),
        )
        .expect("initialize must emit a response");
    assert_eq!(init["id"], 1, "echoed id");
    assert_eq!(
        init["result"]["protocolVersion"],
        ProtocolVersion::V2024_11_05.as_str(),
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
    super::tools::capture::summarize_audio_reason(&mut response);
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
    for version in [
        ProtocolVersion::V2025_11_25.as_str(),
        ProtocolVersion::V2025_06_18.as_str(),
        ProtocolVersion::V2025_03_26.as_str(),
    ] {
        let init = srv
            .handle_message(&json!({
                "jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":version}
            }))
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], version);
        if version != ProtocolVersion::V2025_03_26.as_str() {
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
    let schema = catalog::for_version(ProtocolVersion::default())
        .iter()
        .find(|t| t["name"] == "arb_capture_start")
        .unwrap();
    super::validate::validate(&schema["outputSchema"], &capture, "capture.start").unwrap();
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
            write_frame_line(r2.get_mut(), &resp.to_string(), DEFAULT_MAX_IPC_LINE_BYTES).unwrap();
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
