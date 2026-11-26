//! Bundle resources and investigation prompts exposed by the stdio session.
use std::fmt::Write as _;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Failure, McpServer, take_png};

const RESOURCES: &[(&str, &str)] = &[("status", "録画状態"), ("bundles", "保存バンドル一覧")];
const TEMPLATES: &[(&str, &str)] = &[
    ("manifest", "保存時のマニフェスト"),
    ("screens", "画面遷移"),
    ("crashes", "クラッシュ"),
    ("logs", "ログの先頭100件"),
    ("frame/{frame_index}", "指定フレームの PNG"),
];

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

pub(super) fn bundle_link(id: &Value, kind: &str) -> Value {
    let id = id.as_str().unwrap_or_default();
    let description = TEMPLATES
        .iter()
        .find(|(t, _)| t.split('/').next() == kind.split('/').next())
        .map_or(kind, |(_, label)| *label);
    json!({"type":"resource_link","uri":format!("arb://bundles/{}/{kind}",encode_segment(id)),"name":format!("{id}/{kind}"),"mimeType":mime(kind),"description":description})
}

fn prompts() -> Value {
    json!([
        {"name":"investigate_crash","description":"保存済みバンドルのクラッシュを調査","arguments":[{"name":"bundle_id","description":"調査するバンドル ID","required":false}]},
        {"name":"triage_recent_bug","description":"直近の不具合を保存して調査","arguments":[{"name":"duration_secs","description":"保存秒数","required":false},{"name":"keyword","description":"ログの検索語","required":false}]},
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
    let guide = match name {
        "investigate_crash" => {
            "ID 未指定時は arb_list_bundles で対象を選び、crashes を読みます。前後ログを arb_query_logs の level=ERROR と whole_word=true で絞り、screens で画面遷移を追い、必要なフレームだけ resources/read で確認します。"
        }
        "triage_recent_bug" => {
            "arb_save_and_inspect で保存します。window_shortfall と warnings で証跡の完全性を確認し、arb_query_logs の whole_word=true でキーワードを検索して、該当フレームだけを取得します。"
        }
        _ => {
            "必ず arb_gc_bundles(dry_run: true) を先に実行し、対象と保護理由、回収可能バイト数を提示してください。ユーザーの明示確認を待ち、同じ ignore_grace で実行してください。"
        }
    };
    Ok(
        json!({"description":def["description"],"messages":[{"role":"user","content":{"type":"text","text":format!("{guide}\n指定値 {}",p.get("arguments").cloned().unwrap_or_else(||json!({})))}}]}),
    )
}

impl McpServer {
    pub(super) fn resource_request(&mut self, method: &str, p: &Value) -> Result<Value, Failure> {
        match method {
            "resources/list" => Ok(
                json!({"resources":RESOURCES.iter().map(|(key,label)|json!({"uri":format!("arb://{key}"),"name":key,"description":label,"mimeType":"application/json"})).collect::<Vec<_>>()}),
            ),
            "resources/templates/list" => Ok(
                json!({"resourceTemplates":TEMPLATES.iter().map(|(key,label)|json!({"uriTemplate":format!("arb://bundles/{{bundle_id}}/{key}"),"name":key,"description":label,"mimeType":mime(key)})).collect::<Vec<_>>()}),
            ),
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
            Some("ref/resource") => {
                let path = r["uri"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("arb://bundles/{bundle_id}/"))
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

    fn read_resource(&mut self, uri: &str) -> Result<Value, Failure> {
        let response = match uri {
            "arb://status" => self.call_daemon("system.status", &json!({}))?,
            "arb://bundles" => self.call_daemon("bundle.list", &json!({}))?,
            _ => {
                let (id, kind) = uri
                    .strip_prefix("arb://bundles/")
                    .and_then(|s| s.split_once('/'))
                    .ok_or("不正なリソース URI です")?;
                let id = decode_segment(id)?;
                let id = self.external_bundles.get(&id).cloned().unwrap_or(id);
                let mut target = json!({"bundle_id_or_path":id});
                let method = match kind {
                    "manifest" => "bundle.manifest",
                    "screens" => "timeline.screens",
                    "crashes" => "timeline.crashes",
                    "logs" => {
                        target["limit"] = json!(100);
                        "bundle.query_logs"
                    }
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
                                if error.rpc_code == -32004
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
fn resource_error(e: Failure, uri: &str) -> Failure {
    if e.rpc_code == -32001 {
        Failure::new(
            -32002,
            "RESOURCE_NOT_FOUND",
            format!("リソースがありません {uri}"),
        )
    } else {
        e
    }
}
