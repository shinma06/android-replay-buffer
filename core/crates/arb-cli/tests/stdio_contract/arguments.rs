use super::{Client, Value, json};

fn rejected(
    c: &mut Client,
    tool: &str,
    args: &Value,
    details: &Value,
    message: &[&str],
    hint: &[&str],
) {
    let result = c.result("tools/call", json!({"name":tool,"arguments":args}));
    let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(result["isError"], true, "{tool}: {body}");
    assert_eq!(body["rpc_code"], -32602, "{tool}: {body}");
    assert_eq!(body["error_code"], "INVALID_PARAMS");
    assert_eq!(&body["details"], details, "{tool}: {args}");
    super::validate::validate(&c.schemas[tool]["outputSchema"], &body, tool).unwrap();
    for (field, words) in [("message", message), ("hint", hint)] {
        for word in words {
            assert!(
                body[field].as_str().unwrap().contains(word),
                "{tool}: {body}"
            );
        }
    }
}

pub(super) fn verify(c: &mut Client) {
    rejected(
        c,
        "arb_screenshot",
        &json!({"include_image":"yes"}),
        &json!({"field":"include_image","actual":"string","expected":"boolean"}),
        &["include_image", "boolean", "string"],
        &["include_image", "boolean"],
    );
    rejected(
        c,
        "arb_restore_bundle",
        &json!({}),
        &json!({"field":"bundle_id","missing":"bundle_id"}),
        &["bundle_id"],
        &["bundle_id"],
    );
    rejected(
        c,
        "arb_status",
        &json!({"flag":1}),
        &json!({"field":"flag","unknown":"flag"}),
        &["flag"],
        &["flag"],
    );
    rejected(
        c,
        "arb_forget_bundle",
        &json!({"bundle_id":"test","all_missing":true}),
        &json!({"conflicts":["bundle_id","all_missing"]}),
        &["bundle_id", "all_missing"],
        &["だけ"],
    );
    for (name, key, got, min, max) in [
        ("arb_save", "duration_secs", 3, 5, 600),
        ("arb_config_update", "save_window_secs", 9999, 5, 600),
        ("arb_save_and_inspect", "drain_timeout_ms", 999_999, 0, 5000),
    ] {
        let range = format!("{min}..={max}");
        rejected(
            c,
            name,
            &json!({key:got}),
            &json!({"field":key,"min":min,"max":max,"got":got}),
            &[key, &range, &got.to_string()],
            &[&range],
        );
    }
    let allowed = ["active", "trashed", "purging"];
    rejected(
        c,
        "arb_list_bundles",
        &json!({"state":"nope"}),
        &json!({"field":"state","allowed":allowed,"got":"nope"}),
        &["state", "nope", "active", "trashed", "purging"],
        &allowed,
    );
    let legacy_assert =
        c.schemas["arb_query_logs"]["inputSchema"]["properties"]["level"]["pattern"]
            .as_str()
            .unwrap()
            .starts_with("^([Aa]|");
    let levels: Vec<&str> = legacy_assert
        .then_some("A")
        .into_iter()
        .chain([
            "V", "D", "I", "W", "E", "F", "VERBOSE", "DEBUG", "INFO", "WARN", "WARNING", "ERROR",
            "FATAL", "ASSERT",
        ])
        .collect();
    rejected(
        c,
        "arb_query_logs",
        &json!({"bundle_id":"fixture","level":"Z"}),
        &json!({"field":"level","allowed":levels,"got":"Z"}),
        &["level", "Z", "VERBOSE", "ERROR", "大文字"],
        &["ERROR", "ASSERT"],
    );
    rejected(
        c,
        "arb_inspect_frames",
        &json!({"bundle_id":"fixture","mode":"single","frame_indices":[]}),
        &json!({"field":"frame_indices","min":1,"max":16,"got":0}),
        &["frame_indices", "件数 0", "1..=16 件の範囲外です"],
        &["frame_indices は 1..=16 件の範囲で指定してください。"],
    );
    // Exclusive pairs conflict even with equal values.
    for (tool, first, second, args) in [
        (
            "arb_query_logs",
            "level",
            "min_level",
            json!({"bundle_id":"fixture","level":"E","min_level":"E"}),
        ),
        (
            "arb_query_logs",
            "cursor",
            "after_seq",
            json!({"bundle_id":"fixture","cursor":"1","after_seq":1}),
        ),
    ] {
        rejected(
            c,
            tool,
            &args,
            &json!({"conflicts":[first,second]}),
            &[first, second],
            &[first, second, "だけ"],
        );
    }
    aliases(c);
    // #586-C: an MCP page holds at most 500 rows; the remedy is the cursor.
    for got in [0, 501, 100_000] {
        rejected(
            c,
            "arb_query_logs",
            &json!({"bundle_id":"fixture","limit":got}),
            &json!({"field":"limit","min":1,"max":500,"got":got}),
            &["limit", "1..=500"],
            &["1..=500", "next_cursor", "cursor"],
        );
    }
    for tool in [
        "arb_timeline_overview",
        "arb_query_logs",
        "arb_inspect_frames",
        "arb_export_trim",
        "arb_export_frame_png",
    ] {
        rejected(
            c,
            tool,
            &json!({"bundle_id":"fixture","bundle_id_or_path":"different"}),
            &json!({"conflicts":["bundle_id","bundle_id_or_path"]}),
            &["bundle_id", "bundle_id_or_path"],
            &["だけ"],
        );
    }
    rejected(
        c,
        "arb_forget_bundle",
        &json!({}),
        &json!({"one_of_required":["bundle_id","all_missing"]}),
        &["bundle_id", "all_missing"],
        &["bundle_id", "all_missing"],
    );
    // #572: exactly one PNG destination, and no way to replace an existing file.
    rejected(
        c,
        "arb_export_frame_png",
        &json!({"bundle_id":"fixture","frame_index":0,"output_path":"a.png","output_dir":"d"}),
        &json!({"conflicts":["output_path","output_dir"]}),
        &["output_path", "output_dir"],
        &["output_path", "output_dir"],
    );
    rejected(
        c,
        "arb_export_frame_png",
        &json!({"bundle_id":"fixture","frame_index":0}),
        &json!({"one_of_required":["output_path","output_dir"]}),
        &["output_path", "output_dir"],
        &["output_path", "output_dir"],
    );
    rejected(
        c,
        "arb_export_frame_png",
        &json!({"bundle_id":"fixture","frame_index":0,"output_path":"a.png","overwrite":true}),
        &json!({"field":"overwrite","unknown":"overwrite"}),
        &["overwrite"],
        &["上書きされません", "output_dir"],
    );
    rejected(
        c,
        "arb_timeline_overview",
        &json!({}),
        &json!({"field":"bundle_id","missing":"bundle_id"}),
        &["bundle_id"],
        &["bundle_id"],
    );
}

/// #587: in every alias group, equal values are one argument and any difference names every
/// supplied key. Acceptance is observed through a later, unrelated range error, so no save runs.
fn aliases(c: &mut Client) {
    let past_aliases = json!({"field":"drain_timeout_ms","min":0,"max":5000,"got":999_999});
    for tool in ["arb_save", "arb_save_logs", "arb_save_and_inspect"] {
        for args in [
            json!({"duration_secs":10,"window_secs":10}),
            json!({"duration_secs":10,"duration_sec":10}),
            json!({"window_secs":10,"duration_sec":10}),
            json!({"duration_secs":10,"window_secs":10,"duration_sec":10}),
            json!({"note":"a","memo":"a"}),
            json!({"duration_secs":10,"window_secs":10.0,"note":"a","memo":"a"}),
        ] {
            let mut args = args;
            args["drain_timeout_ms"] = json!(999_999);
            rejected(
                c,
                tool,
                &args,
                &past_aliases,
                &["drain_timeout_ms"],
                &["0..=5000"],
            );
        }
        for (args, keys) in [
            (
                json!({"duration_secs":10,"window_secs":11}),
                vec!["duration_secs", "window_secs"],
            ),
            (
                json!({"window_secs":10,"duration_sec":11}),
                vec!["window_secs", "duration_sec"],
            ),
            (
                json!({"duration_secs":10,"window_secs":10,"duration_sec":20}),
                vec!["duration_secs", "window_secs", "duration_sec"],
            ),
            (json!({"note":"a","memo":"b"}), vec!["note", "memo"]),
        ] {
            let mut hint = keys.clone();
            hint.push("同じ値");
            rejected(c, tool, &args, &json!({ "conflicts": keys }), &keys, &hint);
        }
        // The unlisted alias keeps its own name in errors.
        rejected(
            c,
            tool,
            &json!({"window_secs":3}),
            &json!({"field":"window_secs","min":5,"max":600,"got":3}),
            &["window_secs", "5..=600"],
            &["window_secs"],
        );
    }
    for tool in [
        "arb_timeline_overview",
        "arb_query_logs",
        "arb_inspect_frames",
        "arb_export_trim",
        "arb_export_frame_png",
    ] {
        let schema = &c.schemas[tool]["inputSchema"]["properties"];
        assert!(schema.get("bundle_id_or_path").is_none(), "{tool}");
        let mut args =
            json!({"bundle_id":"no-such-bundle-alias","bundle_id_or_path":"no-such-bundle-alias"});
        match tool {
            "arb_inspect_frames" => args["mode"] = json!("single"),
            "arb_export_frame_png" => {
                args["frame_index"] = json!(0);
                args["output_dir"] = json!("out");
            }
            _ => (),
        }
        let result = c.result("tools/call", json!({"name":tool,"arguments":args}));
        let body: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            body["error_code"], "BUNDLE_NOT_FOUND",
            "{tool}: equal aliases reach the daemon as one target: {body}"
        );
    }
}
