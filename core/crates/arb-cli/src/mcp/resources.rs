//! Bundle resources and investigation prompts exposed by the stdio session.
use std::fmt::Write as _;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Failure, McpServer, catalog, take_png};

const RESOURCES: &[(&str, &str)] = &[("status", "録画状態"), ("bundles", "保存バンドル一覧")];
const TEMPLATES: &[(&str, &str)] = &[
    ("manifest", "保存時のマニフェスト"),
    ("screens", "画面遷移"),
    ("crashes", "クラッシュ"),
    (
        LOGS_TEMPLATE,
        "ログ（arb_query_logs と同じ compact 行・既定 100 件・48 KB まで）",
    ),
    ("frame/{frame_index}", "指定フレームの PNG"),
];

/// RFC 6570 form-style query expansion: with no variables it is the plain `logs` URI (#586-D).
const LOGS_TEMPLATE: &str = "logs{?min_level,package,cursor,limit}";
/// Rows per logs resource page when `limit` is omitted (the `arb_query_logs` default).
const LOGS_DEFAULT_LIMIT: u64 = 100;

/// Validated `arb://bundles/{id}/logs` query variables as `bundle.query_logs` parameters.
///
/// Every variable may appear once: `min_level` (`V`/`D`/`I`/`W`/`E`/`F`), `package` (non-blank),
/// `cursor` (a previous `next_cursor`) and `limit` (`1..=500`). Anything else is `-32602`.
fn log_query(query: Option<&str>) -> Result<serde_json::Map<String, Value>, Failure> {
    use super::project::logs::MAX_PAGE_ROWS;
    let mut params = serde_json::Map::new();
    let invalid = |field: &str, got: &str, cause: &str| {
        let mut failure = Failure::invalid(format!("logs リソースの {field} が不正です: {cause}"));
        failure.detail = Some(json!({"details": {"field": field, "got": got}}));
        failure.hint = Some(format!(
            "arb://bundles/{{bundle_id}}/{LOGS_TEMPLATE} の形で、min_level は V/D/I/W/E/F、limit は 1〜{MAX_PAGE_ROWS}、cursor は前回の next_cursor を指定してください。"
        ));
        failure
    };
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let Some(key) = query_component(raw_key) else {
            return Err(invalid("query", pair, "パーセントエンコーディング"));
        };
        let Some(value) = query_component(raw_value) else {
            return Err(invalid(&key, raw_value, "パーセントエンコーディング"));
        };
        let parsed = match key.as_str() {
            "min_level" if matches!(value.as_str(), "V" | "D" | "I" | "W" | "E" | "F") => {
                json!(value)
            }
            "package" if !value.trim().is_empty() => json!(value.trim()),
            "cursor" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                json!(value)
            }
            "limit" => match value.parse::<u64>() {
                Ok(n) if (1..=MAX_PAGE_ROWS).contains(&n) => json!(n),
                _ => {
                    return Err(invalid(
                        "limit",
                        &value,
                        &format!("1〜{MAX_PAGE_ROWS} の整数"),
                    ));
                }
            },
            "min_level" | "package" | "cursor" => {
                return Err(invalid(&key, &value, "値の形式"));
            }
            _ => return Err(invalid(&key, &value, "未知の変数")),
        };
        if params.insert(key.clone(), parsed).is_some() {
            return Err(invalid(&key, &value, "同じ変数を 2 回指定"));
        }
    }
    Ok(params)
}

/// One form-encoded query component, decoded strictly like a path segment: `+` is a space,
/// every `%` is followed by two hex digits and the bytes are UTF-8. `None` otherwise.
fn query_component(raw: &str) -> Option<String> {
    let mut bytes = raw.bytes();
    let mut out = Vec::with_capacity(raw.len());
    while let Some(b) = bytes.next() {
        out.push(match b {
            b'+' => b' ',
            b'%' => {
                let mut hex = || bytes.next().and_then(|x| char::from(x).to_digit(16));
                let (hi, lo) = (hex()?, hex()?);
                u8::try_from(hi * 16 + lo).ok()?
            }
            other => other,
        });
    }
    String::from_utf8(out).ok()
}

/// The same resource URI with `cursor` set to `next`, the other variables kept in order.
fn next_logs_uri(base: &str, params: &serde_json::Map<String, Value>, next: &str) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for key in ["min_level", "package", "limit"] {
        match params.get(key) {
            Some(Value::String(v)) => {
                query.append_pair(key, v);
            }
            Some(v @ Value::Number(_)) => {
                query.append_pair(key, &v.to_string());
            }
            _ => {}
        }
    }
    query.append_pair("cursor", next);
    format!("{base}?{}", query.finish())
}

/// The logs resource document: exactly the `arb_query_logs` page projection (same rows, count,
/// cursor and 48 KB split), plus `next_uri`, which is not counted in the byte budget so the
/// resource and the tool never page differently.
pub(super) fn logs_resource_body(
    page: Value,
    base: &str,
    variables: &serde_json::Map<String, Value>,
) -> Value {
    let mut body = super::project::logs::project(
        page,
        super::project::logs::Fields::Compact,
        super::project::time::Zone::UTC,
    );
    let next = body["next_cursor"]
        .as_str()
        .filter(|_| body["has_more"] == true)
        .map(|next| next_logs_uri(base, variables, next));
    body["next_uri"] = json!(next);
    body
}

/// Each listed tool's complete `outputSchema` for the negotiated version, read on demand.
const SCHEMA_TEMPLATE: &str = "arb://schemas/{tool}";
const SCHEMA_PREFIX: &str = "arb://schemas/";
const SCHEMA_MIME: &str = "application/schema+json";

fn mime(kind: &str) -> &'static str {
    if kind.starts_with("frame/") {
        "image/png"
    } else {
        "application/json"
    }
}

fn encode_segment(id: &str) -> String {
    let mut encoded = String::new();
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to String");
        }
    }
    encoded
}

fn decode_segment(segment: &str) -> Result<String, Failure> {
    let mut bytes = segment.bytes();
    let mut out = Vec::new();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let mut hex = || {
                bytes
                    .next()
                    .and_then(|x| char::from(x).to_digit(16))
                    .and_then(|x| u8::try_from(x).ok())
                    .ok_or_else(|| Failure::invalid("URI のエスケープが不正です"))
            };
            out.push((hex()? << 4) | hex()?);
        } else if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b);
        } else {
            return Err(Failure::invalid("URI の文字が不正です"));
        }
    }
    let id =
        String::from_utf8(out).map_err(|_| Failure::invalid("URI は UTF-8 で指定してください"))?;
    if id.is_empty()
        || matches!(id.as_str(), "." | "..")
        || id.contains(['/', '\\', ':'])
        || id.chars().any(char::is_control)
    {
        return Err(Failure::invalid("bundle_id が不正です"));
    }
    Ok(id)
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResourceLink {
    uri: String,
    name: String,
    mime_type: &'static str,
    #[serde(skip)]
    description: String,
}
impl ResourceLink {
    pub(super) fn screenshot(path: &std::path::Path, uri: url::Url) -> Self {
        Self::file(path, uri, "image/png", "保存したスクリーンショット")
    }

    pub(super) fn file(
        path: &std::path::Path,
        uri: url::Url,
        mime_type: &'static str,
        description: &str,
    ) -> Self {
        Self {
            uri: uri.into(),
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            mime_type,
            description: description.into(),
        }
    }

    pub(super) fn block(&self) -> Value {
        let mut block = json!(self);
        block["type"] = json!("resource_link");
        block["description"] = json!(self.description);
        block
    }
}
pub(super) fn bundle_link(id: &Value, kind: &str) -> ResourceLink {
    let id = id.as_str().unwrap_or_default();
    let description = TEMPLATES
        .iter()
        .find(|(t, _)| t.split('/').next() == kind.split('/').next())
        .map_or(kind, |(_, label)| *label);
    ResourceLink {
        uri: format!("arb://bundles/{}/{kind}", encode_segment(id)),
        name: format!("{id}/{kind}"),
        mime_type: mime(kind),
        description: description.into(),
    }
}

fn prompts() -> Value {
    json!([
        {"name":"investigate_crash","description":"保存済みバンドルのクラッシュを調査","arguments":[{"name":"package","description":"対象パッケージ","required":false},{"name":"bundle_id","description":"調査するバンドル ID","required":false}]},
        {"name":"triage_recent_bug","description":"直近の不具合を保存して調査","arguments":[{"name":"package","description":"対象パッケージ","required":false},{"name":"duration_secs","description":"保存秒数","required":false},{"name":"keyword","description":"ログの検索語","required":false}]},
        {"name":"safe_cleanup_review","description":"ごみ箱の整理候補を確認","arguments":[{"name":"ignore_grace","description":"猶予期間内も候補に含めるか","required":false}]}
    ])
}

fn prompt_get(p: &Value) -> Result<Value, Failure> {
    let name = p["name"].as_str().ok_or("prompt name が必要です")?;
    let defs = prompts();
    let def = defs
        .as_array()
        .and_then(|defs| defs.iter().find(|d| d["name"] == name))
        .ok_or("未知の prompt です")?;
    if let Some(arguments) = p.get("arguments") {
        for (key, v) in arguments
            .as_object()
            .ok_or("arguments はオブジェクトです")?
        {
            if !v.is_string()
                || !def["arguments"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|d| d["name"] == key.as_str()))
            {
                return Err(Failure::invalid(format!("prompt 引数 {key} が不正です")));
            }
        }
    }
    let args = &p["arguments"];
    let guide = match name {
        "investigate_crash" => {
            let id = args["bundle_id"].as_str().unwrap_or("<選んだバンドルID>");
            let prefix = if args["bundle_id"].is_null() {
                "arb_list_bundles({}) で対象を選択します。\n"
            } else {
                ""
            };
            let mut overview = json!({"bundle_id":id,"max_error_logs":20});
            if let Some(package) = args["package"].as_str() {
                overview["package"] = json!(package);
            }
            let frame = json!({"bundle_id":id,"mode":"single","frame_index":45});
            let logs = json!({"bundle_id":id,"start_frame":15,"end_frame":45,"min_level":"W","fields":"compact"});
            format!(
                "{prefix}1. arb_timeline_overview({overview}) を呼びます。\n2. crashes[0] の stack_top・first_app_frame・caused_by と process_events を確認します。\n3. frame_index_hint を使って画像を開きます。例（hint=45） arb_inspect_frames({frame})。\n4. arb_query_logs({logs}) で直前を確認します。開始は max(0, hint−30)、終了は hint。hint がない場合は screens のフレームを選んでください。"
            )
        }
        "triage_recent_bug" => {
            let mut save = json!({"duration_secs":60});
            if let Some(package) = args["package"].as_str() {
                save["package"] = json!(package);
            }
            if let Some(secs) = args["duration_secs"].as_str() {
                let secs = secs
                    .parse::<u64>()
                    .ok()
                    .filter(|v| (5..=600).contains(v))
                    .ok_or("duration_secs は 5～600 の整数で指定してください")?;
                save["duration_secs"] = json!(secs);
            }
            let mut logs = json!({"bundle_id":"<保存ID>","start_frame":15,"end_frame":45,"min_level":"W","fields":"compact"});
            if let Some(word) = args["keyword"].as_str() {
                logs["keyword"] = json!(word);
            }
            format!(
                "1. arb_status({{}}) で録画状態と input_capture を確認します。\n2. arb_save_and_inspect({save}) で保存し、save の warning_codes を読みます。\n3. crashes・process_events・screens・taps・injected_inputs から注目フレームを選び、例えば 45 なら arb_inspect_frames({{\"bundle_id\":\"<保存ID>\",\"mode\":\"single\",\"frame_index\":45}}) と arb_query_logs({logs}) で画面とログを照合します。"
            )
        }
        _ => {
            let ignore = args["ignore_grace"]
                .as_str()
                .unwrap_or("false")
                .parse::<bool>()
                .map_err(|_| {
                    Failure::invalid("ignore_grace は true または false で指定してください")
                })?;
            let preview = if ignore {
                json!({"ignore_grace":true})
            } else {
                json!({})
            };
            let mut execute = preview.clone();
            execute["dry_run"] = json!(false);
            format!(
                "1. arb_gc_bundles({preview}) でプレビューします（既定 dry_run: true）。\n2. 候補・保護理由・解放見込みを確認します。\n3. 対象を確認できたら arb_gc_bundles({execute}) を実行します。失敗ジョブが残るときは arb_jobs({{\"action\":\"list\"}}) で確認します。"
            )
        }
    };
    Ok(
        json!({"description":def["description"],"messages":[{"role":"user","content":{"type":"text","text":guide}}]}),
    )
}

impl McpServer {
    pub(super) fn resource_request(&mut self, method: &str, p: &Value) -> Result<Value, Failure> {
        match method {
            "resources/list" => Ok(
                json!({"resources":RESOURCES.iter().map(|(key,label)|json!({"uri":format!("arb://{key}"),"name":key,"description":label,"mimeType":"application/json"})).collect::<Vec<_>>()}),
            ),
            "resources/templates/list" => {
                let mut templates: Vec<_> = TEMPLATES.iter().map(|(key,label)|json!({"uriTemplate":format!("arb://bundles/{{bundle_id}}/{key}"),"name":key,"description":label,"mimeType":mime(key)})).collect();
                templates.push(json!({"uriTemplate":SCHEMA_TEMPLATE,"name":"schema","description":"ツールの outputSchema（交渉した版）","mimeType":SCHEMA_MIME}));
                Ok(json!({ "resourceTemplates": templates }))
            }
            "resources/read" => self.read_resource(p["uri"].as_str().ok_or("uri が必要です")?),
            "prompts/list" => Ok(json!({"prompts":prompts()})),
            "prompts/get" => prompt_get(p),
            "completion/complete" => self.complete(p),
            _ => Err(Failure::invalid("未知の操作")),
        }
    }

    fn complete(&mut self, p: &Value) -> Result<Value, Failure> {
        let arg = p["argument"]["name"]
            .as_str()
            .ok_or("argument.name が必要です")?;
        let prefix = p["argument"]["value"]
            .as_str()
            .ok_or("argument.value が必要です")?;
        let r = &p["ref"];
        let supported = match r["type"].as_str() {
            Some("ref/prompt") => {
                let defs = prompts();
                let def = defs
                    .as_array()
                    .and_then(|a| a.iter().find(|d| d["name"] == r["name"]))
                    .ok_or("未知の prompt です")?;
                def["arguments"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|d| d["name"] == arg))
            }
            Some("ref/resource") if r["uri"] == SCHEMA_TEMPLATE => {
                if arg != "tool" {
                    return Err(Failure::invalid("未知の補完引数です"));
                }
                let names: Vec<_> = self
                    .tool_names()
                    .filter(|name| name.starts_with(prefix))
                    .map(str::to_owned)
                    .collect();
                let total = names.len();
                return Ok(json!({"completion":{"values":names,"total":total,"hasMore":false}}));
            }
            Some("ref/resource") => {
                let path = r["uri"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("arb://bundles/{bundle_id}/"))
                    .map(|s| if s == "logs" { LOGS_TEMPLATE } else { s })
                    .filter(|s| TEMPLATES.iter().any(|(key, _)| key == s))
                    .ok_or("未知の URI テンプレートです")?;
                arg == "bundle_id" || (arg == "frame_index" && path.starts_with("frame/"))
            }
            _ => return Err(Failure::invalid("未知の補完対象です")),
        };
        if !supported {
            return Err(Failure::invalid("未知の補完引数です"));
        }
        let mut values = Vec::new();
        let mut total = 0;
        if arg == "bundle_id" {
            let result = self.call_daemon("bundle.list", &json!({}))?;
            for id in result["bundles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["bundle_id"].as_str())
                .filter(|id| id.starts_with(prefix))
            {
                if values.len() < 100 {
                    values.push(id.to_owned());
                }
                total += 1;
            }
        }
        Ok(json!({"completion":{"values":values,"total":total,"hasMore":total>values.len()}}))
    }

    // An explicit filesystem target can be inspected before it enters the catalog.
    // Keep only caller-supplied paths and never interpret a URI segment as a path.
    pub(super) fn frame_resource_id(&mut self, target: &str, id: &Value) -> Result<Value, Failure> {
        if !std::path::Path::new(target).is_absolute() {
            return Ok(id.clone());
        }
        let key = format!("external-{:x}", Sha256::digest(target.as_bytes()));
        if self.external_bundles.len() >= 256 && !self.external_bundles.contains_key(&key) {
            return Err(Failure::new(
                -32002,
                "CONFLICT",
                "外部参照の上限です。バンドルをカタログに登録してください。",
            ));
        }
        self.external_bundles.insert(key.clone(), target.to_owned());
        Ok(json!(key))
    }

    pub(super) fn remember_screenshot(&mut self, uri: &url::Url, digest: &str) {
        self.screenshots.retain(|(old, _)| old != uri);
        if self.screenshots.len() >= 256 {
            self.screenshots.pop_front();
        }
        self.screenshots.push_back((uri.clone(), digest.to_owned()));
    }

    fn tool_names(&self) -> impl Iterator<Item = &'static str> {
        catalog::listed(self.version)
            .iter()
            .filter_map(|tool| tool["name"].as_str())
    }

    /// `arb://schemas/<tool>` names exactly one listed tool; anything else is invalid.
    fn read_schema(&self, uri: &str, name: &str) -> Result<Value, Failure> {
        let schema = self
            .tool_names()
            .find(|known| *known == name)
            .and_then(|known| catalog::tool_output_schema(self.version, known))
            .ok_or_else(|| {
                Failure::invalid(format!(
                    "{SCHEMA_PREFIX}<tool> には tools/list のツール名を指定してください: {uri}"
                ))
            })?;
        Ok(json!({"contents":[{"uri":uri,"mimeType":SCHEMA_MIME,"text":schema.to_string()}]}))
    }

    /// `arb://bundles/{id}/logs{?min_level,package,cursor,limit}`: the `arb_query_logs` page for
    /// the same variables (main timeline, compact rows, 100 rows and 48 KB by default). A page
    /// with more rows names the next one in `next_uri`.
    fn read_logs(&mut self, uri: &str, id: &str, query: Option<&str>) -> Result<Value, Failure> {
        let variables = log_query(query)?;
        let mut params = variables.clone();
        params.insert("bundle_id_or_path".into(), json!(id));
        params.entry("limit").or_insert(json!(LOGS_DEFAULT_LIMIT));
        params.insert("bucket".into(), json!("main"));
        let page = self
            .call_daemon("bundle.query_logs", &Value::Object(params))
            .map_err(|e| resource_error(e, uri))?;
        let body = logs_resource_body(page, uri.split('?').next().unwrap_or(uri), &variables);
        Ok(json!({"contents":[{"uri":uri,"mimeType":"application/json","text":body.to_string()}]}))
    }

    fn read_resource(&mut self, uri: &str) -> Result<Value, Failure> {
        if let Some(name) = uri.strip_prefix(SCHEMA_PREFIX) {
            return self.read_schema(uri, name);
        }
        let response = match uri {
            uri if uri.starts_with("file:") => {
                // Only URLs returned by save.screenshot in this session can reach the filesystem.
                let (issued, digest) = self
                    .screenshots
                    .iter()
                    .find(|(issued, _)| issued.as_str() == uri)
                    .ok_or_else(|| {
                        Failure::invalid("このセッションが発行した PNG の URI を指定してください。")
                    })?;
                let blob = issued
                    .to_file_path()
                    .ok()
                    .and_then(|path| screenshot_blob(&path, digest))
                    .ok_or_else(|| {
                        Failure::new(
                            -32002,
                            "RESOURCE_NOT_FOUND",
                            "保存時のスクリーンショットが見つかりません。",
                        )
                    })?;
                return Ok(json!({"contents":[{"uri":uri,"mimeType":"image/png","blob":blob}]}));
            }
            "arb://status" => self.call_daemon("system.status", &json!({}))?,
            "arb://bundles" => self.call_daemon("bundle.list", &json!({}))?,
            _ => {
                let (id, kind) = uri
                    .strip_prefix("arb://bundles/")
                    .and_then(|s| s.split_once('/'))
                    .ok_or("不正なリソース URI です")?;
                let (kind, query) = match kind.split_once('?') {
                    Some((kind, query)) => (kind, Some(query)),
                    None => (kind, None),
                };
                if query.is_some() && kind != "logs" {
                    return Err(Failure::invalid(format!(
                        "クエリを指定できるのは logs リソースだけです: {uri}"
                    )));
                }
                let id = decode_segment(id)?;
                let id = self.external_bundles.get(&id).cloned().unwrap_or(id);
                let target = json!({"bundle_id_or_path":id});
                let method = match kind {
                    "manifest" => "bundle.manifest",
                    "screens" => "timeline.screens",
                    "crashes" => "timeline.crashes",
                    "logs" => return self.read_logs(uri, &id, query),
                    _ => {
                        let digits = kind
                            .strip_prefix("frame/")
                            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                            .ok_or("不正なフレーム URI です")?;
                        let i = digits
                            .parse::<u64>()
                            .map_err(|_| Failure::invalid("フレーム番号が範囲外です"))?;
                        let mut frame = match self.call_daemon(
                            "media.inspect_frames",
                            &json!({"bundle_id":id,"mode":"single","frame_index":i}),
                        ) {
                            Ok(frame) => frame,
                            Err(error) => {
                                // Legacy media RPCs use a single code for out-of-range and codec errors.
                                if matches!(error.rpc_code, -32004 | -32602)
                                    && self.call_daemon("timeline.summary", &target)?["frame_count"]
                                        .as_u64()
                                        .is_some_and(|n| i >= n)
                                {
                                    return Err(Failure::new(
                                        -32002,
                                        "RESOURCE_NOT_FOUND",
                                        format!("フレームがありません {uri}"),
                                    ));
                                }
                                return Err(resource_error(error, uri));
                            }
                        };
                        let blob = take_png(&mut frame)?;
                        return Ok(
                            json!({"contents":[{"uri":uri,"mimeType":"image/png","blob":blob}]}),
                        );
                    }
                };
                self.call_daemon(method, &target)
                    .map_err(|e| resource_error(e, uri))?
            }
        };
        Ok(
            json!({"contents":[{"uri":uri,"mimeType":"application/json","text":response.to_string()}]}),
        )
    }
}
/// Only publish the bytes whose hash the daemon computed when it saved this screenshot.
pub(super) fn screenshot_blob(path: &std::path::Path, digest: &str) -> Option<String> {
    use arb_core::analytics::MAX_SCREENSHOT_BYTES;
    use std::{fs, io::Read};
    if !fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_SCREENSHOT_BYTES + 1).ok()?)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_SCREENSHOT_BYTES && format!("{:x}", Sha256::digest(&bytes)) == digest)
        .then(|| arb_core::base64_encode(&bytes))
}

fn resource_error(e: Failure, uri: &str) -> Failure {
    if e.label == "BUNDLE_NOT_FOUND" {
        Failure::new(
            -32002,
            "RESOURCE_NOT_FOUND",
            format!("リソースがありません {uri}"),
        )
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::{LOGS_TEMPLATE, log_query, next_logs_uri};
    use serde_json::json;

    #[test]
    fn logs_query_variables_become_query_logs_parameters() {
        assert_eq!(log_query(None).unwrap(), serde_json::Map::new());
        assert_eq!(log_query(Some("")).unwrap(), serde_json::Map::new());
        let params = log_query(Some(
            "min_level=E&package=+com.example.app+&cursor=41&limit=500",
        ))
        .unwrap();
        assert_eq!(
            serde_json::Value::Object(params.clone()),
            json!({"min_level":"E","package":"com.example.app","cursor":"41","limit":500})
        );
        assert_eq!(
            next_logs_uri("arb://bundles/b1/logs", &params, "99"),
            "arb://bundles/b1/logs?min_level=E&package=com.example.app&limit=500&cursor=99"
        );
        // Percent-encoded package names decode like any form value.
        assert_eq!(
            log_query(Some("package=com.example%2Eapp")).unwrap()["package"],
            "com.example.app"
        );
    }

    #[test]
    fn invalid_logs_query_variables_are_rejected_with_the_field() {
        for (query, field) in [
            ("limit=0", "limit"),
            ("limit=501", "limit"),
            ("limit=ten", "limit"),
            ("limit=1&limit=2", "limit"),
            ("min_level=e", "min_level"),
            ("min_level=ERROR", "min_level"),
            ("package=%20", "package"),
            ("cursor=abc", "cursor"),
            ("cursor=", "cursor"),
            ("level=E", "level"),
            ("bucket=all", "bucket"),
            ("package=%FF", "package"),
            ("package=com.example%ZZ", "package"),
            ("package=com.example%2", "package"),
            ("min_level=%E", "min_level"),
            ("cursor=4%31%", "cursor"),
            ("%FFlimit=1", "query"),
        ] {
            let failure = log_query(Some(query)).unwrap_err();
            assert_eq!(failure.rpc_code, -32602, "{query}");
            assert_eq!(
                failure.detail.unwrap()["details"]["field"],
                field,
                "{query}"
            );
            assert!(failure.hint.unwrap().contains(LOGS_TEMPLATE), "{query}");
        }
    }
}
