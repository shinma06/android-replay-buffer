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
    assert_eq!(defs.len(), 21);
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

/// One `tools/list` frame stays at or below 30,000 bytes on every negotiated version (#586-A).
/// Output schemas are served separately by `arb://schemas/{tool}`.
#[test]
fn tools_list_frame_fits_the_budget_on_every_version() {
    for version in ProtocolVersion::ALL {
        let listed = catalog::listed(version);
        let frame = super::rpc_ok(&json!(2), &json!({ "tools": listed }));
        let bytes = frame.to_string().len() + 1;
        eprintln!("tools/list {}: {bytes} bytes", version.as_str());
        assert!(bytes <= 30_000, "{version:?}: {bytes} bytes");
        assert_eq!(listed.len(), 21);
        for (tool, full) in listed.iter().zip(catalog::for_version(version)) {
            assert!(tool.get("outputSchema").is_none(), "{}", tool["name"]);
            let mut expected = full.clone();
            expected.as_object_mut().unwrap().remove("outputSchema");
            assert_eq!(tool, &expected, "only outputSchema is left out");
        }
    }
}

/// Every reference in `node` resolves inside `root`.
fn assert_local_refs(node: &Value, root: &Value) {
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        let pointer = reference.strip_prefix('#').expect("local reference");
        assert!(root.pointer(pointer).is_some(), "{reference}");
    }
    match node {
        Value::Object(fields) => fields.values().for_each(|v| assert_local_refs(v, root)),
        Value::Array(items) => items.iter().for_each(|v| assert_local_refs(v, root)),
        _ => (),
    }
}

fn unit_paths(tag: &str) -> ArbPaths {
    ArbPaths {
        data_dir: std::env::temp_dir().join(format!("arb-mcp-unit-{tag}-d")),
        cache_dir: std::env::temp_dir().join(format!("arb-mcp-unit-{tag}-c")),
        runtime_dir: std::env::temp_dir().join(format!("arb-mcp-unit-{tag}-r")),
    }
}

/// `arb://schemas/{tool}` returns the same complete schema execution is checked against.
#[test]
fn schema_resources_serve_every_complete_output_schema() {
    let mut srv = McpServer::new(unit_paths("schemas"));
    for version in ProtocolVersion::ALL {
        srv.version = version;
        let templates = srv
            .resource_request("resources/templates/list", &json!({}))
            .unwrap();
        assert!(
            templates["resourceTemplates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["uriTemplate"] == "arb://schemas/{tool}"
                    && t["mimeType"] == "application/schema+json")
        );
        for tool in catalog::for_version(version) {
            let name = tool["name"].as_str().unwrap();
            let uri = format!("arb://schemas/{name}");
            let read = srv
                .resource_request("resources/read", &json!({ "uri": uri }))
                .unwrap();
            let content = &read["contents"][0];
            assert_eq!(content["uri"], uri);
            assert_eq!(content["mimeType"], "application/schema+json");
            let mut schema: Value =
                serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
            assert_eq!(
                schema["$schema"],
                "https://json-schema.org/draft/2020-12/schema"
            );
            assert_local_refs(&schema, &schema);
            schema.as_object_mut().unwrap().remove("$schema");
            assert_eq!(schema, tool["outputSchema"], "{version:?} {name}");
        }
        for uri in [
            "arb://schemas/",
            "arb://schemas/arb_unknown",
            "arb://schemas/../arb_status",
            "arb://schemas/%2e%2e",
            "arb://schemas/arb_status/extra",
            "arb://schemas/arb_export_share_mp4",
        ] {
            let error = srv
                .resource_request("resources/read", &json!({ "uri": uri }))
                .unwrap_err();
            assert_eq!(error.rpc_code, -32_602, "{uri}");
            assert_eq!(error.label, "INVALID_PARAMS", "{uri}");
        }
        let complete = |srv: &mut McpServer, arg: &str, value: &str| {
            srv.resource_request(
                "completion/complete",
                &json!({"ref":{"type":"ref/resource","uri":"arb://schemas/{tool}"},"argument":{"name":arg,"value":value}}),
            )
        };
        let names = complete(&mut srv, "tool", "arb_save").unwrap();
        assert_eq!(
            names["completion"],
            json!({"values":["arb_save","arb_save_logs","arb_save_and_inspect"],"total":3,"hasMore":false})
        );
        assert_eq!(
            complete(&mut srv, "tool", "").unwrap()["completion"]["total"],
            21
        );
        assert!(complete(&mut srv, "bundle_id", "").is_err());
    }
}

/// Arguments other than the group's, enough for `tool`'s schema to pass.
fn base_arguments(tool: &str) -> Value {
    match tool {
        "arb_inspect_frames" => json!({"mode":"single","frame_index":0}),
        "arb_export_trim" => json!({"full_range":true}),
        "arb_export_frame_png" => json!({"frame_index":0,"output_dir":"out"}),
        _ => json!({}),
    }
}

/// The alias groups of the shared table as (tools, names), canonical name first.
fn alias_groups() -> Vec<(Vec<String>, Vec<String>)> {
    let table: Value = serde_json::from_str(include_str!(
        "../../../../../docs/specs/mcp-tools-table.json"
    ))
    .unwrap();
    let strings = |group: &Value, key: &str| -> Vec<String> {
        group[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };
    let groups: Vec<_> = table["argumentGroups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["aliases"] == true)
        .inspect(|g| {
            assert_eq!(
                g["allowEqual"], true,
                "every alias group allows equal values"
            );
        })
        .map(|g| (strings(g, "tools"), strings(g, "names")))
        .collect();
    assert_eq!(groups.len(), 3);
    for g in table["argumentGroups"].as_array().unwrap() {
        if g["aliases"] != true {
            assert_ne!(g["allowEqual"], true, "exclusive groups always conflict");
        }
    }
    groups
}

/// A valid value for the group's canonical argument; `k` picks one of three distinct values.
fn group_value(canonical: &str, k: u8) -> Value {
    match canonical {
        "duration_secs" => json!(5 + u64::from(k) * 7),
        _ => json!(format!("value-{k}")),
    }
}

fn input_schema(version: ProtocolVersion, tool: &str) -> &'static Value {
    &catalog::for_version(version)
        .iter()
        .find(|t| t["name"] == tool)
        .unwrap()["inputSchema"]
}

/// Aliases are accepted at run time but not listed; the exclusive groups stay listed.
#[test]
fn deprecated_aliases_are_hidden_but_accepted() {
    for version in ProtocolVersion::ALL {
        for (tools, names) in alias_groups() {
            for tool in &tools {
                let schema = input_schema(version, tool);
                assert!(schema["properties"].get(&names[0]).is_some(), "{tool}");
                for alias in &names[1..] {
                    assert!(
                        schema["properties"].get(alias).is_none(),
                        "{version:?} {tool}.{alias}"
                    );
                    let mut args = base_arguments(tool);
                    args[alias] = group_value(&names[0], 1);
                    let normalized = super::args::normalize(tool, schema, args).unwrap();
                    assert_eq!(normalized[&names[0]], group_value(&names[0], 1));
                    assert!(!normalized.contains_key(alias));
                    // A wrongly typed alias is reported under the name the caller used.
                    let mut args = base_arguments(tool);
                    args[alias] = json!({"wrong":"type"});
                    let error = super::args::normalize(tool, schema, args).unwrap_err();
                    assert_eq!(error.details["field"], alias.as_str(), "{tool}");
                }
            }
        }
        let logs = input_schema(version, "arb_query_logs");
        for exclusive in ["cursor", "after_seq", "level", "min_level"] {
            assert!(logs["properties"].get(exclusive).is_some());
        }
        // The bundle target is required under its canonical name only.
        assert!(
            logs["required"]
                .as_array()
                .unwrap()
                .contains(&json!("bundle_id"))
        );
    }
    // 10, 10.0 and 1e1 are the same integer once normalized.
    let save = input_schema(ProtocolVersion::default(), "arb_save");
    let args = json!({"duration_secs":10,"window_secs":10.0,"duration_sec":1e1});
    assert_eq!(
        super::args::normalize("arb_save", save, args).unwrap()["duration_secs"],
        10
    );
    // Three names with one different value: every supplied name is reported.
    let args = json!({"duration_secs":10,"window_secs":10,"duration_sec":20});
    assert_eq!(
        super::args::normalize("arb_save", save, args)
            .unwrap_err()
            .details,
        json!({"conflicts":["duration_secs","window_secs","duration_sec"]})
    );
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(96))]
    /// #587: for every alias group, any 2-3 supplied names are accepted exactly when all values
    /// are equal (and normalize to that value); otherwise every supplied name is reported.
    #[test]
    fn alias_groups_accept_only_equal_values(
        picks in proptest::collection::vec(0u8..3, 3),
        mask in 3u8..8,
        version in 0usize..4,
    ) {
        let version = ProtocolVersion::ALL[version];
        for (tools, names) in alias_groups() {
            let chosen: Vec<(usize, &String)> = names
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .collect();
            if chosen.len() < 2 {
                continue;
            }
            for tool in &tools {
                let mut args = base_arguments(tool);
                for (i, name) in &chosen {
                    args[name.as_str()] = group_value(&names[0], picks[*i]);
                }
                let first = picks[chosen[0].0];
                let equal = chosen.iter().all(|(i, _)| picks[*i] == first);
                let result = super::args::normalize(tool, input_schema(version, tool), args);
                if equal {
                    let normalized = result.unwrap();
                    proptest::prop_assert_eq!(&normalized[&names[0]], &group_value(&names[0], first));
                    for alias in &names[1..] {
                        proptest::prop_assert!(!normalized.contains_key(alias));
                    }
                } else {
                    let error = result.unwrap_err();
                    let supplied: Vec<_> = chosen.iter().map(|(_, n)| n.as_str()).collect();
                    proptest::prop_assert_eq!(&error.details, &json!({"conflicts":supplied}));
                }
            }
        }
    }
}

/// Every version shares one set of light defaults (#586-B), and the advertised `default` is the
/// value execution uses.
#[test]
fn light_defaults_are_the_same_on_every_version() {
    for version in ProtocolVersion::ALL {
        let tools = catalog::for_version(version);
        let tool = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(
            tool("arb_screenshot")["inputSchema"]["properties"]["include_image"]["default"],
            false
        );
        let logs = &tool("arb_query_logs")["inputSchema"]["properties"];
        assert_eq!(logs["fields"]["default"], "compact");
        assert_eq!(
            super::project::logs::Fields::from_args(&serde_json::Map::new()),
            super::project::logs::Fields::Compact
        );
        assert_eq!(logs["limit"]["default"], 100);
        assert_eq!(logs["limit"]["minimum"], 1);
        assert_eq!(
            logs["limit"]["maximum"],
            super::project::logs::MAX_PAGE_ROWS
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

/// #586-C: MCP pages stop at 500 rows with a paging remedy; the daemon RPC keeps 100,000.
#[test]
fn query_logs_limit_is_capped_for_mcp_only() {
    let schema = input_schema(ProtocolVersion::default(), "arb_query_logs");
    for ok in [1, 100, 500] {
        super::args::normalize(
            "arb_query_logs",
            schema,
            json!({"bundle_id":"b","limit":ok}),
        )
        .unwrap();
    }
    for got in [0, 501, 100_000] {
        let violation = super::args::normalize(
            "arb_query_logs",
            schema,
            json!({"bundle_id":"b","limit":got}),
        )
        .unwrap_err();
        assert_eq!(
            violation.details,
            json!({"field":"limit","min":1,"max":500,"got":got})
        );
        let failure = super::Failure::from_violation("arb_query_logs", violation);
        let body = failure.result(ProtocolVersion::default());
        let hint = body["structuredContent"]["hint"].as_str().unwrap();
        assert!(
            hint.contains("1..=500") && hint.contains("next_cursor"),
            "{hint}"
        );
    }
    assert_eq!(arbd::protocol::MAX_QUERY_LIMIT, 100_000);
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
            {"code": "log_shortfall", "message": "ログが要求した保存時間に達していません", "details": {"log_coverage_ms": 500}},
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
