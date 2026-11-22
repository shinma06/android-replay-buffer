//! Error types for the ARB v2 three-zone storage layer.

use std::io;
use std::path::{Path, PathBuf};

use arb_core::bundle::ValidationError;
use arb_core::legacy::LegacyParseError;
use arb_core::state::StateTransitionError;
use arb_core::time::DiscontinuityEvent;
use arb_os::OsError;
use thiserror::Error;

/// Errors returned by Zone 1 (`ring`), Zone 2 (`bundle_io`), and Zone 3 (`catalog`).
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum StorageError {
    /// Underlying OS abstraction failure from `arb-os`.
    #[error("os error: {0}")]
    Os(#[from] OsError),

    /// Filesystem I/O failure associated with a specific operation and path.
    #[error("i/o error during {operation} at `{path}`: {source}")]
    Io {
        /// Short operation label.
        operation: &'static str,
        /// Filesystem path involved in the failure.
        path: PathBuf,
        /// Underlying OS I/O error.
        #[source]
        source: io::Error,
    },

    /// `SQLite` database error during catalog or timeline access.
    #[error("sqlite error during {operation}: {source}")]
    Sqlite {
        /// Short operation label.
        operation: &'static str,
        /// Underlying `rusqlite` error.
        #[source]
        source: rusqlite::Error,
    },

    /// JSON serialization or deserialization failure.
    #[error("json error during {operation}: {source}")]
    Json {
        /// Short operation label.
        operation: &'static str,
        /// Underlying `serde_json` error.
        #[source]
        source: serde_json::Error,
    },

    /// WORM v2 manifest failed structural validation.
    #[error("manifest validation failed: {0}")]
    ManifestValidation(#[from] ValidationError),

    /// Illegal lifecycle state transition in a save job or bundle.
    #[error("state transition error: {0}")]
    StateTransition(#[from] StateTransitionError),

    /// Legacy schema 3 or schema 4 parsing failure.
    #[error("legacy bundle parse failed: {0}")]
    LegacyParse(#[from] LegacyParseError),

    /// Host free disk space fell below the configured safety floor; new writes stopped.
    #[error(
        "storage pressure stop: available {host_free_bytes} B < floor {min_free_disk_bytes} B (pinned {pinned_bytes} B preserved)"
    )]
    StoragePressureStop {
        /// Observed free disk bytes on the host filesystem.
        host_free_bytes: u64,
        /// Configured minimum free disk floor.
        min_free_disk_bytes: u64,
        /// Total bytes held by pinned chunks (preserved untouched).
        pinned_bytes: u64,
        /// Discontinuity event recorded for the capture timeline.
        event: Box<DiscontinuityEvent>,
    },

    /// Live ring quota is full with pinned or open chunks; new writes stopped safely.
    #[error(
        "ring full with pinned or open chunks: remaining {remaining_bytes} B + incoming {incoming_bytes} B > cap {max_ring_bytes} B"
    )]
    RingFullPinned {
        /// Remaining bytes held by pinned and open chunks.
        remaining_bytes: u64,
        /// Size of the rejected incoming write.
        incoming_bytes: u64,
        /// Configured ring byte budget.
        max_ring_bytes: u64,
        /// Total bytes held by pinned chunks (preserved untouched).
        pinned_bytes: u64,
    },

    /// Bundle file size or SHA-256 digest did not match the manifest.
    #[error("bundle integrity mismatch for `{path}`: {reason}")]
    IntegrityMismatch {
        /// Relative file path inside the bundle.
        path: String,
        /// Explanation of the mismatch.
        reason: String,
    },

    /// Target directory is not a valid v2 or legacy bundle.
    #[error("invalid bundle at `{path}`: {reason}")]
    InvalidBundle {
        /// Bundle directory path.
        path: PathBuf,
        /// Explanation of why the bundle could not be opened.
        reason: String,
    },

    /// Requested record or chunk does not exist.
    #[error("{entity} `{id}` not found")]
    NotFound {
        /// Logical entity kind (for example, `bundle`, `lease`, `save_job`, `chunk`).
        entity: &'static str,
        /// Missing identifier.
        id: String,
    },

    /// Caller supplied an invalid identifier, path, or range parameter.
    #[error("invalid argument `{name}`: {reason}")]
    InvalidArgument {
        /// Parameter or field name.
        name: &'static str,
        /// Explanation of why the argument is invalid.
        reason: String,
    },

    /// Persisted catalog or timeline row contained an unrecognized or out-of-range value.
    #[error("corrupt stored data: {reason}")]
    CorruptData {
        /// Description of the data integrity violation.
        reason: String,
    },
}

impl StorageError {
    /// Constructs a [`StorageError::Io`] with operation context and path.
    pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    /// Constructs a [`StorageError::Sqlite`] with operation context.
    pub(crate) const fn sqlite(operation: &'static str, source: rusqlite::Error) -> Self {
        Self::Sqlite { operation, source }
    }

    /// Constructs a [`StorageError::Json`] with operation context.
    pub(crate) const fn json(operation: &'static str, source: serde_json::Error) -> Self {
        Self::Json { operation, source }
    }

    /// Returns the published WORM bundle path if an atomic commit succeeded on
    /// disk and only the post-commit parent-directory `fsync` failed.
    ///
    /// Callers must treat the bundle at this path as already published and must
    /// never delete it or retry committing into the same destination path.
    #[must_use]
    pub fn committed_bundle_path_on_parent_sync_error(&self) -> Option<&Path> {
        match self {
            Self::Os(OsError::CommittedParentSyncFailed { path, .. }) => Some(path.as_path()),
            _ => None,
        }
    }
}
