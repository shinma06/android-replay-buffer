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
    #[serde(default)]
    required: bool,
}

fn groups(name: &str) -> impl Iterator<Item = &'static Group> {
    catalog::TABLE
        .argument_groups
        .iter()
        .filter(move |g| g.tools.iter().any(|t| t == name))
}

pub(super) fn describe(name: &str, schema: &mut Value) {
    let mut exclusions = Vec::new();
    for group in groups(name) {
        let canonical = &group.names[0];
        if group.aliases {
            for alias in &group.names[1..] {
                let mut field = schema["properties"][canonical].clone();
                field["deprecated"] = json!(true);
                field["description"] = json!(format!("{canonical} の別名。"));
                schema["properties"][alias] = field;
            }
        }
        if group.required {
            schema["anyOf"] = Value::Array(
                group
                    .names
                    .iter()
                    .map(|k| json!({"required":[k]}))
                    .collect(),
            );
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

pub(super) fn normalize(
    name: &str,
    schema: &Value,
    mut args: Value,
) -> Result<Map<String, Value>, Violation> {
    // Report both submitted names even if another required argument is absent.
    for group in groups(name) {
        let mut present = group.names.iter().filter(|k| args.get(*k).is_some());
        if let (Some(a), Some(b)) = (present.next(), present.next()) {
            return Err(Violation::conflicts(&[a, b]));
        }
    }
    validate::normalize_integers(schema, &mut args);
    validate::validate(schema, &args, "arguments")?;
    let Value::Object(mut args) = args else {
        unreachable!("tool inputs are object schemas")
    };
    for group in groups(name).filter(|g| g.aliases) {
        for alias in &group.names[1..] {
            if let Some(value) = args.remove(alias) {
                args.insert(group.names[0].clone(), value);
            }
        }
    }
    Ok(args)
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
