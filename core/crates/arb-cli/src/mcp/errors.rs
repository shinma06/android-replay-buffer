use super::{JSONRPC_INVALID_PARAMS, validate::Violation, version::ProtocolVersion};
use crate::rpc::RpcClientError;
use serde_json::{Value, json};
/// PNG export never replaces a file, so the remedy is always a new destination.
const EXPORT_PNG_CONFLICT: &str = "既存のファイルは上書きされません。別の output_path、または自動命名の output_dir を指定してください。";

#[derive(Debug)]
pub(crate) struct Failure {
    pub(super) rpc_code: i32,
    pub(super) label: &'static str,
    pub(super) message: String,
    pub(super) detail: Option<Value>,
    pub(super) hint: Option<String>,
}
impl Failure {
    pub(super) fn new(code: i32, label: &'static str, message: impl Into<String>) -> Self {
        Self {
            rpc_code: code,
            label,
            message: message.into(),
            detail: None,
            hint: None,
        }
    }
    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self::new(JSONRPC_INVALID_PARAMS, "INVALID_PARAMS", message)
    }
    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(-32000, "DAEMON_UNAVAILABLE", message)
    }
    pub(crate) fn from_rpc(e: RpcClientError) -> Self {
        match e {
            RpcClientError::Rpc {
                code,
                message,
                error_code,
                details,
            } => Self {
                detail: details.map(|details| json!({"details": details})),
                ..Self::new(
                    code,
                    error_code
                        .as_deref()
                        .and_then(super::catalog::known_error_label)
                        .unwrap_or_else(|| arb_os::rpc::error_label(code)),
                    message,
                )
            },
            _ => Self::unavailable(e.to_string()),
        }
    }
    pub(super) fn from_violation(tool: &str, mut violation: Violation) -> Self {
        if tool == "arb_query_logs" && violation.details["field"] == "limit" {
            super::project::logs::limit_hint(&mut violation.hint);
        }
        let hint = match (tool, violation.details["unknown"].as_str()) {
            ("arb_screenshot", Some("overwrite")) => "別の output_path を指定してください。".into(),
            ("arb_export_frame_png", Some("overwrite")) => EXPORT_PNG_CONFLICT.into(),
            ("arb_export_trim", Some("trash_original")) => {
                "元バンドルをごみ箱に移すには arb_trash_bundle を実行してください。".into()
            }
            _ => violation.hint,
        };
        Self {
            detail: Some(json!({"details":violation.details})),
            hint: Some(hint),
            ..Self::invalid(violation.message)
        }
    }

    pub(super) fn add_save(&mut self, save: Value) {
        self.detail.get_or_insert_with(|| json!({}))["save"] = save;
    }

    pub(super) fn for_tool(mut self, tool: &str, version: ProtocolVersion) -> Self {
        if self.label == "CONFLICT" {
            let hint = match tool {
                "arb_screenshot"
                    if self
                        .detail
                        .as_ref()
                        .is_some_and(|d| d["details"]["reason"] == "already_exists") =>
                {
                    if version.allows_legacy_screenshot_overwrite() {
                        Some(
                            "別の output_path を指定するか、上書きする場合は overwrite: true を指定してください。",
                        )
                    } else {
                        Some("別の output_path を指定してください。")
                    }
                }
                "arb_export_frame_png" => Some(EXPORT_PNG_CONFLICT),
                _ => None,
            };
            self.hint = hint.map(str::to_owned);
        }
        self
    }

    pub(crate) fn result(&self, version: ProtocolVersion) -> Value {
        let mut body = self.detail.clone().unwrap_or_else(|| json!({}));
        let (kind, hint, retryable) =
            arb_os::rpc::error_guidance_with_details(self.label, body.get("details"));
        body["kind"] = json!(kind);
        body["hint"] = json!(self.hint.as_deref().unwrap_or(&hint));
        body["retryable"] = json!(retryable);
        body["error_code"] = json!(self.label);
        body["rpc_code"] = json!(self.rpc_code);
        body["message"] = json!(self.message);
        super::output::error(body, version)
    }
}
impl From<String> for Failure {
    fn from(s: String) -> Self {
        Self::invalid(s)
    }
}
impl From<&str> for Failure {
    fn from(s: &str) -> Self {
        Self::invalid(s)
    }
}
