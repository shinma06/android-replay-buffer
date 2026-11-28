//! Tool discovery and validation share the same versioned contract.
use std::sync::LazyLock;

use serde_json::{Value, json};

use super::{args, version::ProtocolVersion};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct Table {
    tools: Vec<Value>,
    overlays: Vec<Overlay>,
    #[serde(rename = "$defs")]
    defs: Value,
    #[serde(rename = "argumentGroups")]
    pub(super) argument_groups: Vec<args::Group>,
}

#[derive(Deserialize)]
struct Overlay {
    when: Capability,
    tools: serde_json::Map<String, Value>,
    #[serde(default)]
    definitions: serde_json::Map<String, Value>,
}
#[derive(Deserialize)]
enum Capability {
    #[serde(rename = "legacyFullLogRows")]
    FullLogRows,
    #[serde(rename = "legacyScreenshotInline")]
    ScreenshotInline,
    #[serde(rename = "legacyScreenshotOverwrite")]
    ScreenshotOverwrite,
    #[serde(rename = "legacyTrashOriginal")]
    TrashOriginal,
    #[serde(rename = "legacyAssertAbbreviation")]
    AssertAbbreviation,
}
impl Capability {
    fn applies(&self, version: ProtocolVersion) -> bool {
        match self {
            Self::FullLogRows => !version.default_compact_logs(),
            Self::ScreenshotInline => version.legacy_screenshot_inline(),
            Self::ScreenshotOverwrite => version.allows_legacy_screenshot_overwrite(),
            Self::TrashOriginal => version.allows_legacy_trash_original(),
            Self::AssertAbbreviation => version.accepts_assert_abbreviation(),
        }
    }
}

/// Object patches override only their named fields. Arrays and scalars replace their targets.
fn overlay(node: &mut Value, patch: &Value) {
    if let Some(fields) = patch.as_object() {
        for (key, value) in fields {
            overlay(&mut node[key], value);
        }
    } else {
        node.clone_from(patch);
    }
}

pub(super) static TABLE: LazyLock<Table> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../../docs/specs/mcp-tools-table.json"
    ))
    .expect("valid MCP tool table")
});

/// Accept only labels advertised by the public error schema, including daemon subtypes.
pub(super) fn known_error_label(label: &str) -> Option<&'static str> {
    TABLE.defs["ErrorFields"]["properties"]["error_code"]["enum"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find(|&known| known == label)
}

pub(super) fn for_version(version: ProtocolVersion) -> &'static [Value] {
    use ProtocolVersion::{V2024_11_05, V2025_03_26, V2025_06_18, V2025_11_25};
    static NOVEMBER: LazyLock<Vec<Value>> = LazyLock::new(|| build_catalog(V2025_11_25));
    static JUNE: LazyLock<Vec<Value>> = LazyLock::new(|| build_catalog(V2025_06_18));
    static FIRST: LazyLock<Vec<Value>> = LazyLock::new(|| build_catalog(V2024_11_05));
    static MARCH: LazyLock<Vec<Value>> = LazyLock::new(|| build_catalog(V2025_03_26));
    match version {
        V2024_11_05 => &FIRST,
        V2025_03_26 => &MARCH,
        V2025_06_18 => &JUNE,
        V2025_11_25 => &NOVEMBER,
    }
}

pub(super) fn long_running(name: &str) -> bool {
    TABLE
        .tools
        .iter()
        .any(|t| t["name"] == name && t.get("execution").is_some())
}

fn build_catalog(version: ProtocolVersion) -> Vec<Value> {
    let mut defs = TABLE.defs.clone();
    let overlays: Vec<_> = TABLE
        .overlays
        .iter()
        .filter(|o| o.when.applies(version))
        .collect();
    for patch in &overlays {
        for (name, fields) in &patch.definitions {
            overlay(&mut defs[name], fields);
        }
    }
    let mut tools = TABLE.tools.clone();
    for tool in &mut tools {
        let name = tool["name"].as_str().expect("tool name").to_owned();
        for fields in overlays.iter().filter_map(|o| o.tools.get(&name)) {
            overlay(tool, fields);
        }
        args::describe(&name, &mut tool["inputSchema"]);
        let obj = tool.as_object_mut().expect("tool object");
        if !version.supports_tasks() {
            obj.remove("execution");
        }
        let success = obj.remove("outputType").expect("success type");
        let errors = obj
            .remove("errorTypes")
            .unwrap_or_else(|| json!(["ToolError"]));
        let variants: Vec<_> = std::iter::once(&success)
            .chain(errors.as_array().expect("error types").iter())
            .map(|t| json!({"$ref":format!("#/$defs/{}",t.as_str().expect("definition name"))}))
            .collect();
        let mut output = local_output_schema(json!({"type":"object","oneOf":variants}), &defs);
        drop_implied_object_type(&mut output);
        tool["outputSchema"] = output;
    }
    tools
}

/// The root already requires an object, so its inlined variants (and their `allOf` parts) do not
/// need to repeat `"type": "object"`. Validation results are unchanged.
fn drop_implied_object_type(root: &mut Value) {
    fn strip(node: &mut Value) {
        if let Some(obj) = node.as_object_mut()
            && obj.get("type").is_some_and(|t| t == "object")
        {
            obj.remove("type");
        }
    }
    for variant in root
        .get_mut("oneOf")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        strip(variant);
        for part in variant
            .get_mut("allOf")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            strip(part);
        }
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
            // A shared definition also costs its key, value and punctuation in $defs.
            let size = definition.to_string().len();
            let uses = counts[name];
            let savings = size.saturating_sub(node.to_string().len()) * uses;
            if uses > 1 && savings > size + name.len() + 4 {
                if !kept.contains_key(name) {
                    pack(&mut definition, defs, counts, kept);
                    kept.insert(name.to_owned(), definition);
                }
            } else {
                pack(&mut definition, defs, counts, kept);
                // A description beside a reference describes this particular field.
                if let Some(description) = node.get("description") {
                    definition["description"] = description.clone();
                }
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
