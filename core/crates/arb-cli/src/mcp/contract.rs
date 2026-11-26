//! Tool discovery and validation share the same versioned contract.
use std::sync::LazyLock;

use serde_json::{Value, json};

pub(super) static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let table: Value = serde_json::from_str(include_str!(
        "../../../../../docs/specs/mcp-tools-table.json"
    ))
    .expect("MCP contract must be valid JSON");
    let mut tools = table["tools"].as_array().expect("tool definitions").clone();
    for tool in &mut tools {
        let key = tool
            .as_object_mut()
            .expect("tool object")
            .remove("outputType")
            .expect("output type");
        let success = expand(
            &table["$defs"][key.as_str().expect("type name")],
            &table["$defs"],
        );
        let mut variants = vec![
            success,
            expand(&table["$defs"]["ToolError"], &table["$defs"]),
        ];
        if matches!(
            tool["name"].as_str(),
            Some("arb_save" | "arb_save_logs" | "arb_save_and_inspect")
        ) {
            variants.push(expand(&table["$defs"]["SaveError"], &table["$defs"]));
        }
        tool["outputSchema"] = json!({"type":"object","oneOf":variants});
    }
    tools
});

pub(super) fn for_version(version: &str) -> &'static [Value] {
    static JUNE: LazyLock<Vec<Value>> = LazyLock::new(|| {
        let mut tools = TOOLS.clone();
        for (name, flag, description) in [
            (
                "arb_screenshot",
                "overwrite",
                "同名のスクリーンショットを上書きします。",
            ),
            (
                "arb_export_trim",
                "trash_original",
                "書き出し確定後に元バンドルをごみ箱に移します。",
            ),
        ] {
            let tool = tools
                .iter_mut()
                .find(|t| t["name"] == name)
                .expect("legacy tool");
            let field = &mut tool["inputSchema"]["properties"][flag];
            field.as_object_mut().expect("flag schema").remove("const");
            field["description"] = json!(description);
            tool["annotations"]["destructiveHint"] = json!(true);
        }
        tools
    });
    static LEGACY: LazyLock<Vec<Value>> = LazyLock::new(|| {
        let mut tools = JUNE.clone();
        let query = tools
            .iter_mut()
            .find(|t| t["name"] == "arb_query_logs")
            .expect("log search tool");
        let level = &mut query["inputSchema"]["properties"]["level"];
        let pattern = level["pattern"]
            .as_str()
            .expect("level pattern")
            .replacen("^(", "^([Aa]|", 1);
        level["pattern"] = json!(pattern);
        level["description"] = json!("Android のログレベル。旧クライアントの A も受け付けます。");
        tools
    });
    match version {
        "2024-11-05" | "2025-03-26" => &LEGACY,
        "2025-06-18" => &JUNE,
        _ => &TOOLS,
    }
}

fn expand(node: &Value, defs: &Value) -> Value {
    if let Some(name) = node["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("#/$defs/"))
    {
        return expand(defs.get(name).expect("defined local schema"), defs);
    }
    match node {
        Value::Array(items) => items.iter().map(|v| expand(v, defs)).collect(),
        Value::Object(obj) => Value::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), expand(v, defs)))
                .collect(),
        ),
        scalar => scalar.clone(),
    }
}
