//! Error types for the `arbd` single-writer local daemon.

use arb_adb::adb::AdbError;
use arb_adb::mock::CaptureSourceError;
use arb_adb::wire::WireError;
use arb_core::selection::SelectionError;
use arb_media::MediaError;
use arb_os::OsError;
use arb_storage::StorageError;
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
    #[error("invalid argument: {0}")]
    InvalidParams(String),
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

impl DaemonError {
    /// Maps this error to a stable JSON-RPC 2.0 numeric error code.
    #[must_use]
    pub const fn rpc_code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) | Self::Selection(_) => RPC_ERR_INVALID_PARAMS,
            Self::MethodNotFound(_) => RPC_ERR_METHOD_NOT_FOUND,
            Self::NotFound(_) => RPC_ERR_NOT_FOUND,
            Self::Conflict(_) => RPC_ERR_CONFLICT,
            Self::Storage(_) => RPC_ERR_STORAGE,
            Self::Media(_) => RPC_ERR_MEDIA,
            Self::Adb(_) | Self::CaptureSource(_) | Self::Wire(_) => RPC_ERR_ADB,
            Self::Os(_) | Self::Json(_) | Self::Io(_) | Self::Internal(_) => RPC_ERR_INTERNAL,
        }
    }
}
