//! Error types for the `arbd` single-writer local daemon.

use arb_adb::adb::AdbError;
use arb_adb::mock::CaptureSourceError;
use arb_adb::wire::WireError;
use arb_core::selection::SelectionError;
use arb_media::MediaError;
use arb_os::OsError;
use arb_storage::StorageError;
use serde_json::{Value, json};
use thiserror::Error;

/// JSON-RPC 2.0 standard error code: parse error.
pub const RPC_ERR_PARSE: i32 = -32_700;
/// JSON-RPC 2.0 standard error code: invalid request.
pub const RPC_ERR_INVALID_REQUEST: i32 = -32_600;
/// JSON-RPC 2.0 standard error code: method not found.
pub const RPC_ERR_METHOD_NOT_FOUND: i32 = -32_601;
/// JSON-RPC 2.0 standard error code: invalid params.
pub const RPC_ERR_INVALID_PARAMS: i32 = -32_602;
/// JSON-RPC 2.0 standard error code: internal error.
pub const RPC_ERR_INTERNAL: i32 = -32_603;
/// Application error code: resource not found.
pub const RPC_ERR_NOT_FOUND: i32 = -32_001;
/// Application error code: invalid state transition or conflict.
pub const RPC_ERR_CONFLICT: i32 = -32_002;
/// Application error code: storage pressure or I/O failure.
pub const RPC_ERR_STORAGE: i32 = -32_003;
/// Application error code: media processing failure.
pub const RPC_ERR_MEDIA: i32 = -32_004;
/// Application error code: ADB or device capture failure.
pub const RPC_ERR_ADB: i32 = -32_005;
/// Application error code: the serialized response exceeds the IPC frame limit.
pub const RPC_ERR_RESPONSE_TOO_LARGE: i32 = -32_010;

/// Errors returned by [`crate::DaemonServer`] and internal daemon subsystems.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum DaemonError {
    /// OS abstraction error (IPC, SHM, clocks, filesystem, single-instance lock).
    #[error("os error: {0}")]
    Os(#[from] OsError),
    /// Three-zone storage engine error (ring store, bundle I/O, catalog).
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    /// Media container, codec, trim, or playback error.
    #[error("media error: {0}")]
    Media(#[from] MediaError),
    /// ADB host client or device deployment error.
    #[error("adb error: {0}")]
    Adb(#[from] AdbError),
    /// Device capture source error.
    #[error("capture source error: {0}")]
    CaptureSource(#[from] CaptureSourceError),
    /// Device wire protocol codec error.
    #[error("wire error: {0}")]
    Wire(#[from] WireError),
    /// Core 3-valued selection error.
    #[error("selection error: {0}")]
    Selection(#[from] SelectionError),
    /// JSON serialization or deserialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Standard I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Caller supplied invalid parameters.
    #[error("{0}")]
    InvalidParams(String),
    /// The requested bundle exists but contains no video track.
    #[error("bundle {0} has no video track")]
    NoVideo(String),
    /// Atomic publication refused to replace an existing file.
    #[error("output file already exists: {0}")]
    OutputExists(std::path::PathBuf),
    /// Screenshot target is absent or offline.
    #[error("対象の ADB 端末が接続されていません。")]
    DeviceNotFound(Option<String>),
    /// Requested entity was not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// Requested operation conflicts with current daemon state.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Requested RPC method does not exist.
    #[error("method not found: {0}")]
    MethodNotFound(String),
    /// Internal runtime error.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Public RPC failure with a separate diagnostic cause.
#[derive(Debug)]
pub struct ClassifiedError {
    /// Stable application label.
    pub code: &'static str,
    /// JSON-RPC numeric code used by existing clients.
    pub rpc_code: i32,
    /// Message suitable for display to the caller.
    pub user_message: String,
    /// Structured context for this failure.
    pub details: Value,
    /// Internal diagnostic, separate from the public message.
    pub cause: Option<String>,
}

impl DaemonError {
    /// Classifies a failure once for all RPC consumers.
    #[must_use]
    pub fn classify(&self) -> ClassifiedError {
        use arb_core::state::{BundleLifecycleState, StateTransitionError};
        let mut details = json!({});
        let mut diagnostic = true;
        let (rpc_code, label, message) = match self {
            Self::DeviceNotFound(serial) => {
                details = json!({"serial":serial});
                diagnostic = false;
                (
                    RPC_ERR_NOT_FOUND,
                    Some("DEVICE_NOT_FOUND"),
                    "対象の ADB 端末が接続されていません。",
                )
            }
            Self::NoVideo(id) => {
                details = json!({"bundle_id": id});
                diagnostic = false;
                (
                    RPC_ERR_INVALID_PARAMS,
                    Some("NO_VIDEO"),
                    "このバンドルには映像トラックがありません。",
                )
            }
            Self::OutputExists(path) => {
                details = json!({"path":path, "reason":"already_exists"});
                diagnostic = false;
                (RPC_ERR_CONFLICT, None, "保存先に同名のファイルがあります。")
            }
            Self::MethodNotFound(_) => (
                RPC_ERR_METHOD_NOT_FOUND,
                None,
                "このデーモンは指定された操作に対応していません。",
            ),
            Self::Storage(StorageError::NotFound { .. }) | Self::NotFound(_) => {
                (RPC_ERR_NOT_FOUND, None, "指定された対象が見つかりません。")
            }
            Self::InvalidParams(_)
            | Self::Selection(_)
            | Self::Media(
                MediaError::InvalidFrameRange { .. } | MediaError::FrameOutOfRange { .. },
            )
            | Self::Storage(
                StorageError::InvalidArgument { .. } | StorageError::ConfigValidation(_),
            ) => (
                RPC_ERR_INVALID_PARAMS,
                None,
                "指定された引数を確認してください。",
            ),
            Self::Storage(StorageError::StateTransition(
                StateTransitionError::InvalidBundleTransition { from, to },
            )) if from == to && *to == BundleLifecycleState::Trashed => (
                RPC_ERR_CONFLICT,
                Some("ALREADY_TRASHED"),
                "このバンドルは既にごみ箱にあります。",
            ),
            Self::Storage(StorageError::StateTransition(
                StateTransitionError::InvalidBundleTransition { from, to },
            )) if from == to && *to == BundleLifecycleState::Active => (
                RPC_ERR_CONFLICT,
                Some("ALREADY_ACTIVE"),
                "このバンドルは既に復元されています。",
            ),
            Self::Storage(StorageError::StateTransition(_)) | Self::Conflict(_) => (
                RPC_ERR_CONFLICT,
                None,
                "現在の状態ではこの操作を実行できません。",
            ),
            Self::Storage(_) => (
                RPC_ERR_STORAGE,
                None,
                "保存データを読み書きできませんでした。",
            ),
            Self::Media(MediaError::Io(err)) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                (RPC_ERR_CONFLICT, None, "保存先に同名のファイルがあります。")
            }
            Self::Media(_) => (RPC_ERR_MEDIA, None, "メディアを処理できませんでした。"),
            Self::Adb(_) | Self::CaptureSource(_) | Self::Wire(_) => {
                (RPC_ERR_ADB, None, "端末との通信に失敗しました。")
            }
            Self::Os(_) | Self::Json(_) | Self::Io(_) | Self::Internal(_) => (
                RPC_ERR_INTERNAL,
                None,
                "デーモンが操作を完了できませんでした。",
            ),
        };
        let user_message = match self {
            // Parameter validation explains the rejected field/type/range to the caller.
            Self::InvalidParams(reason)
                if !reason.is_empty()
                    && !["error:", "not found:", "conflict:"]
                        .iter()
                        .any(|prefix| reason.contains(prefix)) =>
            {
                format!("{message} {reason}")
            }
            _ => message.to_owned(),
        };
        ClassifiedError {
            code: label.unwrap_or_else(|| arb_os::rpc::error_label(rpc_code)),
            rpc_code,
            user_message,
            details,
            cause: diagnostic
                .then(|| self.public_cause())
                .filter(|s| !s.is_empty()),
        }
    }

    fn public_cause(&self) -> String {
        use std::error::Error;
        // Unwrap typed diagnostic layers before dealing with already-formatted causes.
        let mut source: &dyn Error = self;
        while let Some(next) = source.source() {
            source = next;
        }
        let text = source.to_string();
        let mut cause = text.trim();
        const PREFIXES: &[&str] = &[
            "os error:",
            "storage error:",
            "media error:",
            "adb error:",
            "wire error:",
            "json error:",
            "io error:",
            "i/o error:",
            "selection error:",
            "capture source error:",
            "not found:",
            "conflict:",
            "internal error:",
            "state transition error:",
            "method not found:",
        ];
        while let Some(rest) = PREFIXES.iter().find_map(|p| cause.strip_prefix(p)) {
            cause = rest.trim_start();
        }
        cause.to_owned()
    }

    pub(crate) fn into_response(self, id: Value) -> crate::protocol::RpcResponse {
        let ClassifiedError {
            code,
            rpc_code,
            user_message,
            mut details,
            cause,
        } = self.classify();
        if let Some(cause) = cause {
            details["cause"] = Value::String(cause);
        }
        let (kind, hint, retryable) = arb_os::rpc::error_guidance(code);
        let mut response = crate::protocol::RpcResponse::err(id, rpc_code, user_message);
        if let Some(error) = &mut response.error {
            error.data = Some(
                json!({"error_code":code,"kind":kind,"hint":hint,"retryable":retryable,"details":details}),
            );
        }
        response
    }

    /// Maps this error to a stable JSON-RPC 2.0 numeric error code.
    #[must_use]
    pub fn rpc_code(&self) -> i32 {
        self.classify().rpc_code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #[test]
        fn daemon_errors_have_public_messages_without_internal_prefixes(suffix in ".{0,32}") {
            check_public_messages(&format!("storage error: {suffix}"));
        }
    }

    fn check_public_messages(reason: &str) {
        let io = || std::io::Error::other(reason.to_owned());
        let errors = [
            DaemonError::Os(OsError::ClockOverflow { clock: "clock" }),
            StorageError::CatalogPoisoned.into(),
            StorageError::StateTransition(
                arb_core::state::StateTransitionError::EmptyDestinationPath,
            )
            .into(),
            MediaError::Io(io()).into(),
            AdbError::AdbNotFound.into(),
            CaptureSourceError::Io(io()).into(),
            WireError::Io(io()).into(),
            SelectionError::FrameNotFound { frame_index: 0 }.into(),
            serde_json::from_str::<bool>("?").unwrap_err().into(),
            io().into(),
            DaemonError::InvalidParams(reason.into()),
            DaemonError::NotFound("bundle fixture".into()),
            DaemonError::Conflict(reason.into()),
            DaemonError::MethodNotFound(reason.into()),
            DaemonError::Internal(reason.into()),
            DaemonError::NoVideo("logs-only".into()),
            DaemonError::DeviceNotFound(Some("serial".into())),
            DaemonError::OutputExists("fixture.png".into()),
            StorageError::NotFound {
                entity: "bundle",
                id: "fixture".into(),
            }
            .into(),
        ];
        for err in errors {
            let message = err.classify().user_message;
            let response = err.into_response(1.into()).error.unwrap();
            assert_eq!(response.message, message);
            let details = response.data.unwrap()["details"].to_string();
            assert_ne!(message.trim(), "");
            for prefix in [
                "os error:",
                "storage error:",
                "media error:",
                "adb error:",
                "wire error:",
                "json error:",
                "io error:",
                "i/o error:",
                "selection error:",
                "capture source error:",
                "not found:",
                "conflict:",
                "internal error:",
                "state transition error:",
                "method not found:",
            ] {
                assert!(!message.contains(prefix), "{message}");
                assert!(!details.contains(prefix), "{details}");
            }
        }
    }
}
