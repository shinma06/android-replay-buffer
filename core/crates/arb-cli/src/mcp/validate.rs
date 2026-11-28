use serde_json::{Value, json};

/// A validation failure carries the public field names, machine-readable evidence and a remedy.
#[derive(Debug)]
pub(super) struct Violation {
    pub(super) message: String,
    pub(super) details: Value,
    pub(super) hint: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Violation {
    pub(super) fn new(message: String, details: Value, hint: String) -> Self {
        Self {
            message,
            details,
            hint,
        }
    }
    /// Names that exclude each other were supplied together.
    pub(super) fn conflicts(names: &[&str]) -> Self {
        let fields = names.join(" と ");
        Self::new(
            format!("{fields} は併用できません。"),
            json!({"conflicts":names}),
            format!("{fields} のうち{}だけ指定してください。", one_of(names)),
        )
    }
    fn missing(field: &str) -> Self {
        Self::new(
            format!("{field} が必須です。"),
            json!({"field":field,"missing":field}),
            format!("{field} を指定してください。"),
        )
    }
    fn range(field: &str, got: &Value, min: &Value, max: &Value) -> Self {
        Self::bounds(field, got, min, max, "値", "")
    }
    fn count(field: &str, got: &Value, min: &Value, max: &Value) -> Self {
        Self::bounds(field, got, min, max, "件数", " 件")
    }
    fn bounds(field: &str, got: &Value, min: &Value, max: &Value, noun: &str, unit: &str) -> Self {
        let range = match (min.is_null(), max.is_null()) {
            (false, false) => format!("{min}..={max}{unit}"),
            (true, _) => format!("{max}{unit} 以下"),
            _ => format!("{min}{unit} 以上"),
        };
        let mut details = json!({"field":field,"got":got});
        if !min.is_null() {
            details["min"] = min.clone();
        }
        if !max.is_null() {
            details["max"] = max.clone();
        }
        // A trailing unit word is followed directly by the particle ("1..=16 件の範囲").
        let joiner = if range.ends_with('件') { "" } else { " " };
        Self::new(
            format!("{field} の{noun} {got} は {range}の範囲外です。"),
            details,
            format!("{field} は {range}{joiner}の範囲で指定してください。"),
        )
    }
}

/// How many of `names` may be supplied, as a phrase.
pub(super) fn one_of(names: &[&str]) -> &'static str {
    if names.len() > 2 { "1 つ" } else { "片方" }
}

fn value_type(v: &Value) -> &'static str {
    match v {
        Value::Object(_) => "object",
        Value::Array(_) => "array",
        Value::String(_) => "string",
        Value::Bool(_) => "boolean",
        Value::Null => "null",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
    }
}

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

pub(super) fn validate(schema: &Value, v: &Value, path: &str) -> Result<(), Violation> {
    validate_node(schema, v, path, schema)
}

fn validate_node(schema: &Value, v: &Value, path: &str, root: &Value) -> Result<(), Violation> {
    let field = path.strip_prefix("arguments.").unwrap_or(path);
    if let Some(reference) = schema["$ref"].as_str() {
        let definition = reference
            .strip_prefix('#')
            .and_then(|pointer| root.pointer(pointer))
            .expect("catalog references must resolve locally");
        validate_node(definition, v, path, root)?;
    }
    validate_value(schema, v, field)?;
    validate_object(schema, v, path, root)?;
    if let Some(a) = v.as_array() {
        let len = u64::try_from(a.len()).unwrap_or(u64::MAX);
        if schema["minItems"].as_u64().is_some_and(|n| len < n)
            || schema["maxItems"].as_u64().is_some_and(|n| len > n)
        {
            return Err(Violation::count(
                field,
                &json!(len),
                &schema["minItems"],
                &schema["maxItems"],
            ));
        }
        if let Some(items) = schema.get("items") {
            for (i, x) in a.iter().enumerate() {
                validate_node(items, x, &format!("{path}[{i}]"), root)?;
            }
        }
    }
    validate_combinations(schema, v, path, root)
}

fn required_names(schema: &Value) -> impl Iterator<Item = &str> {
    schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn validate_combinations(
    schema: &Value,
    v: &Value,
    path: &str,
    root: &Value,
) -> Result<(), Violation> {
    for part in schema["allOf"].as_array().into_iter().flatten() {
        validate_node(part, v, path, root)?;
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(options) = schema[key].as_array() {
            let matched: Vec<_> = options
                .iter()
                .filter(|s| validate_node(s, v, path, root).is_ok())
                .collect();
            if key == "oneOf" && matched.len() > 1 {
                let conflicts: Vec<_> = matched.iter().flat_map(|s| required_names(s)).collect();
                return Err(Violation::conflicts(&conflicts));
            }
            if matched.is_empty() {
                let labels: Vec<_> = options
                    .iter()
                    .map(|s| {
                        required_names(s)
                            .map(|k| match s["properties"][k].get("const") {
                                Some(value) => format!("{k}={value}"),
                                None => k.to_owned(),
                            })
                            .collect::<Vec<_>>()
                            .join(" と ")
                    })
                    .collect();
                let names: Vec<_> = options.iter().flat_map(required_names).collect();
                let detail = if key == "oneOf" {
                    "one_of_required"
                } else {
                    "any_of_required"
                };
                let choice = labels.join(" または ");
                let hint = format!(
                    "{choice} の{}を指定してください。",
                    if key == "oneOf" {
                        "どちらか一方"
                    } else {
                        "いずれか"
                    }
                );
                return Err(Violation::new(
                    format!("{path} には {choice} が必要です。"),
                    json!({detail:names}),
                    hint,
                ));
            }
        }
    }
    if schema
        .get("not")
        .is_some_and(|s| validate_node(s, v, path, root).is_ok())
    {
        let fields: Vec<_> = schema["not"]["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        return Err(Violation::conflicts(&fields));
    }
    if schema
        .get("if")
        .is_some_and(|s| validate_node(s, v, path, root).is_ok())
    {
        validate_node(&schema["then"], v, path, root)?;
    }
    Ok(())
}

fn validate_value(schema: &Value, v: &Value, field: &str) -> Result<(), Violation> {
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
        let actual = value_type(v);
        return Err(Violation::new(
            format!("{field} の型は {kind} が必要です。入力の型は {actual} です。"),
            json!({"field":field,"expected":kind,"actual":actual}),
            format!("{field} を {kind} 型で指定してください。"),
        ));
    }
    if schema.get("const").is_some_and(|c| c != v)
        || schema["enum"].as_array().is_some_and(|a| !a.contains(v))
    {
        let allowed = schema
            .get("enum")
            .cloned()
            .unwrap_or_else(|| json!([schema["const"]]));
        return Err(Violation::new(
            format!("{field} の値 {v} は候補 {allowed} に含まれません。"),
            json!({"field":field,"allowed":allowed,"got":v}),
            format!("{field} は {allowed} から選んでください。"),
        ));
    }
    if let (Some(s), Some(p)) = (v.as_str(), schema["pattern"].as_str())
        && !regex_lite::Regex::new(p)
            .expect("valid schema pattern")
            .is_match(s)
    {
        if let Some(words) = case_insensitive_words(p) {
            let allowed = json!(words);
            return Err(Violation::new(
                format!(
                    "{field} の値 {v} は候補 {allowed} に含まれません（大文字・小文字は区別しません）。"
                ),
                json!({"field":field,"allowed":allowed,"got":v}),
                format!(
                    "{field} は {allowed} から選んでください（大文字・小文字は区別しません）。"
                ),
            ));
        }
        return Err(Violation::new(
            format!("{field} の値 {v} は書式 {p} に一致しません。"),
            json!({"field":field,"pattern":p,"got":v}),
            format!("{field} は {p} に一致する文字列を指定してください。"),
        ));
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
        return Err(Violation::range(
            field,
            v,
            &schema["minimum"],
            &schema["maximum"],
        ));
    }
    Ok(())
}

/// Words of a case-insensitive enumeration written as `^([Aa]|[Bb][Cc])$`, in upper case.
fn case_insensitive_words(pattern: &str) -> Option<Vec<String>> {
    let body = pattern.strip_prefix("^(")?.strip_suffix(")$")?;
    body.split('|')
        .map(|word| {
            let chars: Vec<char> = word.chars().collect();
            let letters: Option<String> = chars
                .chunks(4)
                .map(|c| match c {
                    ['[', upper, lower, ']']
                        if upper.is_ascii_uppercase() && upper.to_ascii_lowercase() == *lower =>
                    {
                        Some(*upper)
                    }
                    _ => None,
                })
                .collect();
            letters.filter(|w| !w.is_empty())
        })
        .collect()
}

// Our closed composed record schemas declare fields in properties and allOf/$ref branches.
fn declares_field(schema: &Value, key: &str, root: &Value) -> bool {
    schema["properties"].get(key).is_some()
        || schema["$ref"]
            .as_str()
            .and_then(|r| root.pointer(r.strip_prefix('#')?))
            .is_some_and(|sub| declares_field(sub, key, root))
        || schema["allOf"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|sub| declares_field(sub, key, root)))
}

fn validate_object(schema: &Value, v: &Value, path: &str, root: &Value) -> Result<(), Violation> {
    if let Some(obj) = v.as_object() {
        for k in schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !obj.contains_key(k) {
                return Err(Violation::missing(k));
            }
        }
        for (k, x) in obj {
            if let Some(sub) = schema["properties"].get(k) {
                validate_node(sub, x, &format!("{path}.{k}"), root)?;
            } else if schema["additionalProperties"] == false
                || (schema["unevaluatedProperties"] == false && !declares_field(schema, k, root))
            {
                return Err(Violation::new(
                    format!("{path} に未定義の引数 {k} があります。"),
                    json!({"field":k,"unknown":k}),
                    format!("{k} を除いて指定してください。"),
                ));
            } else if schema["additionalProperties"].is_object() {
                validate_node(
                    &schema["additionalProperties"],
                    x,
                    &format!("{path}.{k}"),
                    root,
                )?;
            }
        }
        for k in obj.keys() {
            if let Some(keys) = schema["dependentRequired"][k].as_array() {
                for dep in keys.iter().filter_map(Value::as_str) {
                    if !obj.contains_key(dep) {
                        return Err(Violation::new(
                            format!("{k} には {dep} が必要です。"),
                            json!({"field":k,"missing":dep}),
                            format!("{k} と {dep} を一緒に指定してください。"),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}
