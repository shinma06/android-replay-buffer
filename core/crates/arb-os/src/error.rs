//! Error types returned by `arb-os` platform abstractions.

use std::io;
use std::path::PathBuf;

/// Errors returned by cross-platform OS abstractions in `arb-os`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OsError {
    /// An OS clock call failed.
    #[error("failed to read host clock `{clock}`: {source}")]
    ClockRead {
        /// Human-readable clock identifier (for example, `CLOCK_UPTIME_RAW`).
        clock: &'static str,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// A clock timestamp could not be represented as signed 64-bit nanoseconds.
    #[error("host clock `{clock}` timestamp overflowed i64 nanoseconds")]
    ClockOverflow {
        /// Human-readable clock identifier.
        clock: &'static str,
    },

    /// A filesystem operation failed at a specific path.
    #[error("{operation} failed at `{path}`: {source}")]
    Fs {
        /// Short operation description.
        operation: &'static str,
        /// Filesystem path involved in the failure.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// An atomic commit or bind target already exists and will not be overwritten.
    #[error("target path already exists and cannot be overwritten: `{path}`")]
    AlreadyExists {
        /// Existing target path.
        path: PathBuf,
    },

    /// A caller supplied an invalid argument or descriptor.
    #[error("invalid argument `{name}`: {reason}")]
    InvalidArgument {
        /// Parameter or field name.
        name: &'static str,
        /// Explanation of why the argument is invalid.
        reason: String,
    },

    /// Another process already holds the single-instance lock.
    #[error("single-instance lock is already held at `{path}`")]
    LockContended {
        /// Path to the contended lock file.
        path: PathBuf,
    },

    /// A local IPC transport operation failed.
    #[error("ipc {operation} failed on `{endpoint}`: {source}")]
    Ipc {
        /// Short operation name (for example, `bind`, `accept`, or `connect`).
        operation: &'static str,
        /// Socket path or named pipe identifier.
        endpoint: String,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// An IPC NDJSON frame exceeded the configured maximum line length.
    #[error("ipc frame exceeded maximum length of {max_bytes} bytes")]
    FrameTooLarge {
        /// Maximum allowed bytes per line.
        max_bytes: usize,
    },

    /// An IPC NDJSON frame contained invalid UTF-8 bytes.
    #[error("ipc frame is not valid utf-8: {source}")]
    InvalidUtf8 {
        /// Underlying UTF-8 decoding error.
        #[source]
        source: std::string::FromUtf8Error,
    },

    /// A shared-memory frame ring failed header or slot validation.
    #[error("invalid shared-memory frame ring: {reason}")]
    InvalidShmRing {
        /// Validation failure explanation.
        reason: String,
    },

    /// A required standard OS base directory could not be resolved.
    #[error("could not resolve standard OS directory: {reason}")]
    PathResolution {
        /// Explanation of the missing environment variable or directory.
        reason: String,
    },

    /// A generic OS I/O error without an associated path or endpoint.
    #[error("os i/o error during {operation}: {source}")]
    Io {
        /// Short operation description.
        operation: &'static str,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// An operation is not supported on the current host platform.
    #[error("unsupported OS operation `{operation}`: {reason}")]
    Unsupported {
        /// Short operation identifier.
        operation: &'static str,
        /// Explanation of why the operation is unsupported.
        reason: String,
    },

    /// An atomic commit succeeded on disk, publishing `path`, but syncing the
    /// parent directory failed afterwards.
    ///
    /// Callers must treat the WORM bundle at `path` as already published and
    /// must not retry the commit under the same path.
    #[error("committed `{path}` successfully, but parent directory fsync failed: {source}")]
    CommittedParentSyncFailed {
        /// Final published destination path that already exists on disk.
        path: PathBuf,
        /// Underlying parent directory fsync error.
        #[source]
        source: io::Error,
    },
}
