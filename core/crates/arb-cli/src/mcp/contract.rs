//! Tool discovery and validation share the same versioned contract.
use std::sync::LazyLock;

use serde_json::{Value, json};

pub(super) static TOOLS: LazyLock<Vec<Value>> =
    LazyLock::new(|| build_catalog(super::MCP_PROTOCOL_LATEST));

fn build_catalog(version: &str) -> Vec<Value> {
    let legacy = version != super::MCP_PROTOCOL_LATEST;
    let mut table: Value = serde_json::from_str(include_str!(
        "../../../../../docs/specs/mcp-tools-table.json"
    ))
    .expect("MCP contract must be valid JSON");
    if legacy {
        table["$defs"]["Investigation"]["properties"]["error_logs"] =
            json!({"$ref":"#/$defs/TimelineLogsResponse"});
    }
    let mut tools = table["tools"].as_array().expect("tool definitions").clone();
    for tool in &mut tools {
        let key = tool
            .as_object_mut()
            .expect("tool object")
            .remove("outputType")
            .expect("output type");
        if legacy && tool["name"] == "arb_save_and_inspect" {
            tool["inputSchema"]["properties"]["max_errors"]["maximum"] = json!(100_000);
        }
        let defs = &table["$defs"];
        let error_type = if tool["name"] == "arb_save_and_inspect" {
            "InvestigationError"
        } else {
            "ToolError"
        };
        let mut variants = vec![
            json!({"$ref":format!("#/$defs/{}",key.as_str().expect("type name"))}),
            json!({"$ref":format!("#/$defs/{error_type}")}),
        ];
        if matches!(
            tool["name"].as_str(),
            Some("arb_save" | "arb_save_logs" | "arb_save_and_inspect")
        ) {
            variants.push(json!({"$ref":"#/$defs/SaveError"}));
        }
        tool["outputSchema"] = local_output_schema(json!({"type":"object","oneOf":variants}), defs);
    }
    tools
}

pub(super) fn for_version(version: &str) -> &'static [Value] {
    static JUNE: LazyLock<Vec<Value>> = LazyLock::new(|| {
        let mut tools = build_catalog("2025-06-18");
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
            *field = json!({"type":"boolean","default":false});
            if name == "arb_screenshot" {
                tool["inputSchema"]["properties"]["include_image"]["default"] = json!(true);
            }
            tool["inputSchema"]["properties"][flag]["description"] = json!(description);
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

/// Inline single-use definitions and keep repeated structures once within each tool's schema.
/// The result resolves offline, independent of which tools the client has already discovered.
fn local_output_schema(mut root: Value, definitions: &Value) -> Value {
    use std::collections::BTreeMap;
    fn count(node: &Value, defs: &Value, counts: &mut BTreeMap<String, usize>) {
        if let Some(name) = node["$ref"]
            .as_str()
            .and_then(|v| v.strip_prefix("#/$defs/"))
        {
            let n = counts.entry(name.to_owned()).or_default();
            *n += 1;
            if *n == 1 {
                count(&defs[name], defs, counts);
            }
        } else if let Some(obj) = node.as_object() {
            for value in obj.values() {
                count(value, defs, counts);
            }
        } else if let Some(items) = node.as_array() {
            for value in items {
                count(value, defs, counts);
            }
        }
    }
    fn pack(
        node: &mut Value,
        defs: &Value,
        counts: &BTreeMap<String, usize>,
        kept: &mut serde_json::Map<String, Value>,
    ) {
        if let Some(name) = node["$ref"]
            .as_str()
            .and_then(|v| v.strip_prefix("#/$defs/"))
        {
            let mut definition = defs[name].clone();
            if counts[name] > 1 && definition.to_string().len() > node.to_string().len() {
                if !kept.contains_key(name) {
                    pack(&mut definition, defs, counts, kept);
                    kept.insert(name.to_owned(), definition);
                }
            } else {
                pack(&mut definition, defs, counts, kept);
                *node = definition;
            }
            return;
        }
        if let Some(obj) = node.as_object_mut() {
            for value in obj.values_mut() {
                pack(value, defs, counts, kept);
            }
        } else if let Some(items) = node.as_array_mut() {
            for value in items {
                pack(value, defs, counts, kept);
            }
        }
    }
    let mut counts = BTreeMap::new();
    let mut kept = serde_json::Map::new();
    count(&root, definitions, &mut counts);
    pack(&mut root, definitions, &counts, &mut kept);
    if !kept.is_empty() {
        root["$defs"] = Value::Object(kept);
    }
    root
}
