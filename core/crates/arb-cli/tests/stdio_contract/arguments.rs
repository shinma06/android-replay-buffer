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
    for (tool, first, second, args) in [
        (
            "arb_save",
            "duration_secs",
            "window_secs",
            json!({"window_secs":5,"duration_secs":5}),
        ),
        (
            "arb_save_logs",
            "note",
            "memo",
            json!({"note":"a","memo":"a"}),
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
            &json!({"bundle_id":"fixture","bundle_id_or_path":"fixture"}),
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
    rejected(
        c,
        "arb_timeline_overview",
        &json!({}),
        &json!({"any_of_required":["bundle_id","bundle_id_or_path"]}),
        &["bundle_id"],
        &["bundle_id"],
    );
}
