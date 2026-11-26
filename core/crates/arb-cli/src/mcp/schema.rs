use serde_json::Value;

fn matches_type(kind: &str, v: &Value) -> bool {
    match kind {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "null" => v.is_null(),
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        _ => false,
    }
}

/// JSON Schema treats 1.0 and 1e0 as integers. Normalize those before serde's
/// integer-only RPC decoder, without rounding a fractional argument.
pub(super) fn normalize_integers(schema: &Value, v: &mut Value) {
    if schema["type"] == "integer"
        && v.is_f64()
        && let Some(n) = v.as_f64()
        && n.fract() == 0.0
    {
        let decimal = format!("{n:.0}");
        if let Ok(n) = decimal.parse::<u64>() {
            *v = n.into();
        } else if let Ok(n) = decimal.parse::<i64>() {
            *v = n.into();
        }
    } else if let Some(obj) = v.as_object_mut() {
        for (k, value) in obj {
            if let Some(field) = schema["properties"].get(k) {
                normalize_integers(field, value);
            }
        }
    } else if let Some(items) = v.as_array_mut() {
        for item in items {
            normalize_integers(&schema["items"], item);
        }
    }
}

pub(super) fn validate(schema: &Value, v: &Value, path: &str) -> Result<(), String> {
    let kind = &schema["type"];
    let correct = if let Some(k) = kind.as_str() {
        matches_type(k, v)
    } else {
        kind.as_array().is_none_or(|ks| {
            ks.iter()
                .filter_map(Value::as_str)
                .any(|k| matches_type(k, v))
        })
    };
    if !correct {
        return Err(format!("{path} の型は {kind} です"));
    }
    if schema.get("const").is_some_and(|c| c != v)
        || schema["enum"].as_array().is_some_and(|a| !a.contains(v))
    {
        return Err(format!("{path} はサポートされていない値です"));
    }
    if let (Some(s), Some(p)) = (v.as_str(), schema["pattern"].as_str())
        && !regex_lite::Regex::new(p)
            .expect("valid schema pattern")
            .is_match(s)
    {
        return Err(format!("{path} の書式が不正です ({p})"));
    }
    if let Some(n) = v.as_number().and_then(serde_json::Number::as_i128)
        && (schema["minimum"]
            .as_number()
            .and_then(serde_json::Number::as_i128)
            .is_some_and(|m| n < m)
            || schema["maximum"]
                .as_number()
                .and_then(serde_json::Number::as_i128)
                .is_some_and(|m| n > m))
    {
        return Err(format!("{path} は許容範囲外です"));
    }
    validate_object(schema, v, path)?;
    if let Some(a) = v.as_array() {
        let len = u64::try_from(a.len()).unwrap_or(u64::MAX);
        if schema["minItems"].as_u64().is_some_and(|n| len < n)
            || schema["maxItems"].as_u64().is_some_and(|n| len > n)
        {
            return Err(format!("{path} の件数が範囲外です"));
        }
        if let Some(items) = schema.get("items") {
            for x in a {
                validate(items, x, path)?;
            }
        }
    }
    for part in schema["allOf"].as_array().into_iter().flatten() {
        validate(part, v, path)?;
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(options) = schema[key].as_array() {
            let matches = options
                .iter()
                .filter(|s| validate(s, v, path).is_ok())
                .count();
            if matches == 0 || (key == "oneOf" && matches != 1) {
                return Err(format!("{path} の組み合わせが不正です ({key})"));
            }
        }
    }
    if schema
        .get("not")
        .is_some_and(|s| validate(s, v, path).is_ok())
    {
        return Err(format!("{path} の引数は併用できません"));
    }
    if schema
        .get("if")
        .is_some_and(|s| validate(s, v, path).is_ok())
    {
        validate(&schema["then"], v, path)?;
    }
    Ok(())
}

fn validate_object(schema: &Value, v: &Value, path: &str) -> Result<(), String> {
    if let Some(obj) = v.as_object() {
        for k in schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !obj.contains_key(k) {
                return Err(format!("{path}.{k} は必須です"));
            }
        }
        for (k, x) in obj {
            if let Some(sub) = schema["properties"].get(k) {
                validate(sub, x, &format!("{path}.{k}"))?;
            } else if schema["additionalProperties"] == false {
                return Err(format!("{path}.{k} は未定義です"));
            } else if schema["additionalProperties"].is_object() {
                validate(&schema["additionalProperties"], x, &format!("{path}.{k}"))?;
            }
            if let Some(keys) = schema["dependentRequired"][k].as_array() {
                for dep in keys.iter().filter_map(Value::as_str) {
                    if !obj.contains_key(dep) {
                        return Err(format!("{path}.{k} には {dep} が必要です"));
                    }
                }
            }
        }
    }
    Ok(())
}
