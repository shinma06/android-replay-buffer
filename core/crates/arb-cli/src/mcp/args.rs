//! The same argument groups govern discovery and execution.
use super::{
    catalog,
    validate::{self, Violation},
};
use crate::{absolutize_bundle_target, absolutize_path};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::Path;

#[derive(Deserialize)]
pub(super) struct Group {
    tools: Vec<String>,
    names: Vec<String>,
    #[serde(default)]
    aliases: bool,
    #[serde(default, rename = "allowEqual")]
    allow_equal: bool,
    #[serde(default)]
    required: bool,
}

fn groups(name: &str) -> impl Iterator<Item = &'static Group> {
    catalog::TABLE
        .argument_groups
        .iter()
        .filter(move |g| g.tools.iter().any(|t| t == name))
}

/// Publishes each group's canonical name only. Deprecated aliases stay accepted by [`normalize`],
/// which folds them into the canonical name before the schema sees the arguments.
pub(super) fn describe(name: &str, schema: &mut Value) {
    let mut exclusions = Vec::new();
    for group in groups(name) {
        if group.required {
            let required = schema["required"]
                .as_array_mut()
                .map_or_else(Vec::new, std::mem::take);
            let canonical = json!(group.names[0]);
            schema["required"] = Value::Array(
                required
                    .into_iter()
                    .filter(|k| *k != canonical)
                    .chain(std::iter::once(canonical.clone()))
                    .collect(),
            );
        }
        // Aliases are not listed, and JSON Schema cannot compare two sibling values.
        if group.aliases {
            continue;
        }
        for (i, a) in group.names.iter().enumerate() {
            for b in &group.names[i + 1..] {
                exclusions.push(json!({"not":{"required":[a,b]}}));
            }
        }
    }
    if !exclusions.is_empty() {
        schema["allOf"] = Value::Array(exclusions);
    }
}

/// Checks every argument group, folds aliases into their canonical name, then validates.
///
/// Within a group, every supplied name is compared: equal values are one argument when the group
/// `allowEqual`s it; any other combination is reported with all supplied names.
pub(super) fn normalize(
    name: &str,
    schema: &Value,
    mut args: Value,
) -> Result<Map<String, Value>, Violation> {
    let props = &schema["properties"];
    // Report every submitted name even if another required argument is absent.
    for group in groups(name) {
        let canonical = &group.names[0];
        let present: Vec<&str> = group
            .names
            .iter()
            .map(String::as_str)
            .filter(|k| args.get(*k).is_some())
            .collect();
        for alias in present.iter().filter(|_| group.aliases) {
            validate::normalize_integers(&props[canonical], &mut args[*alias]);
        }
        if present.len() > 1 {
            let first = &args[present[0]];
            if !group.allow_equal || present.iter().any(|k| args[*k] != *first) {
                return Err(if group.allow_equal {
                    differing(&present)
                } else {
                    Violation::conflicts(&present)
                });
            }
        }
        if !group.aliases {
            continue;
        }
        for alias in present.iter().filter(|k| **k != canonical) {
            validate::validate(
                &props[canonical],
                &args[*alias],
                &format!("arguments.{alias}"),
            )?;
        }
        if let Some(obj) = args.as_object_mut() {
            for alias in &group.names[1..] {
                if let Some(value) = obj.remove(alias) {
                    obj.insert(canonical.clone(), value);
                }
            }
        }
    }
    validate::normalize_integers(schema, &mut args);
    validate::validate(schema, &args, "arguments")?;
    let Value::Object(args) = args else {
        unreachable!("tool inputs are object schemas")
    };
    Ok(args)
}

/// Aliases of one argument were supplied with different values.
fn differing(names: &[&str]) -> Violation {
    let fields = names.join(" と ");
    Violation::new(
        format!("{fields} は同じ意味の引数ですが、値が異なります。"),
        json!({ "conflicts": names }),
        format!(
            "{fields} は同じ値にするか、{}だけ指定してください。",
            validate::one_of(names)
        ),
    )
}

/// Both save RPCs accept `duration_sec` and `note`, including `save.logs` via its serde alias.
pub(super) fn save_params(args: &Map<String, Value>) -> Result<Value, String> {
    let mut params = Map::new();
    for (public, rpc) in [
        ("duration_secs", "duration_sec"),
        ("note", "note"),
        ("drain_timeout_ms", "drain_timeout_ms"),
    ] {
        if let Some(value) = args.get(public) {
            params.insert(rpc.into(), value.clone());
        }
    }
    if let Some(path) = optional_abs_path(args, "dest_dir")? {
        params.insert("dest_dir".into(), json!(path));
    }
    Ok(Value::Object(params))
}
pub(super) fn require_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing required non-empty string `{key}`"))
}

pub(super) fn require_bundle_target(args: &Map<String, Value>) -> Result<String, String> {
    let raw = require_str(args, "bundle_id")?;
    absolutize_bundle_target(raw).map_err(|e| e.message)
}

pub(super) fn optional_abs_path(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, String> {
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

pub(super) fn absolutize_optional_path_field(
    params: &mut Map<String, Value>,
    key: &str,
) -> Result<(), String> {
    if let Some(raw) = params.get(key).and_then(Value::as_str) {
        let abs = absolutize_path(Path::new(raw)).map_err(|e| e.message)?;
        params.insert(key.to_owned(), json!(abs));
    }
    Ok(())
}

/// Resolve the two historical tool names before looking up their canonical catalog entry.
pub(super) fn tool_name<'a>(name: &'a str, args: &mut Value) -> Result<&'a str, Violation> {
    let format = match name {
        "arb_export_share_mp4" => "share_mp4",
        "arb_trim_bundle" => "bundle",
        _ => return Ok(name),
    };
    if let Some(obj) = args.as_object_mut() {
        if let Some(got) = obj.get("format").filter(|v| *v != format) {
            return Err(Violation::new(
                format!("{name} の format は {format} です。入力値は {got} です。"),
                json!({"field":"format","allowed":[format],"got":got}),
                format!("format に {format} を指定してください。"),
            ));
        }
        obj.insert("format".into(), json!(format));
    }
    Ok("arb_export_trim")
}
