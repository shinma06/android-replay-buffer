//! Save failure policy shared by preflight, durable jobs and client guidance.

use serde::{Deserialize, Serialize};

/// Boundary at which the operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[non_exhaustive]
pub enum SaveFailureStage {
    /// Probing an output directory before pinning.
    Preflight,
    /// Reading or protecting internal save inputs.
    Pin,
    /// Writing the selected bundle to its destination.
    Materialize,
    /// Registering an already published bundle.
    CatalogRegister,
}

/// Platform-neutral cause used by the save policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[non_exhaustive]
pub enum SaveFailureIoKind {
    /// A path component could not be found.
    NotFound,
    /// Write access was denied.
    PermissionDenied,
    /// A path component is a file.
    NotADirectory,
    /// The filesystem is mounted read-only.
    ReadOnlyFilesystem,
    /// The requested name is invalid.
    InvalidFilename,
    /// The target is reserved for daemon state or immutable content.
    PrivateDirConflict,
    /// The filesystem has no free space.
    StorageFull,
    /// The account or volume quota was exceeded.
    QuotaExceeded,
    /// The operation was interrupted.
    Interrupted,
    /// The operation cannot complete immediately.
    WouldBlock,
    /// A resource is temporarily held by another writer.
    Busy,
    /// A required input is absent.
    ChunkMissing,
    /// An input failed decoding or verification.
    ChunkCorrupt,
    /// No more specific cause is available.
    Other,
}

/// Recovery category persisted with a failed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SaveFailureClass {
    /// The destination must change before writing can succeed.
    PermanentDestination,
    /// Retry can succeed after the underlying condition clears.
    Transient,
    /// Required source data cannot be reconstructed.
    Integrity,
}

/// User action suggested for a failed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RecommendedJobAction {
    /// Retry at the current destination.
    Retry,
    /// Select a different destination.
    Retarget,
    /// Release the unrecoverable job and its pins.
    Discard,
}

impl SaveFailureClass {
    /// Returns the operation that addresses this failure category.
    #[must_use]
    pub const fn recommended_action(self) -> RecommendedJobAction {
        match self {
            Self::PermanentDestination => RecommendedJobAction::Retarget,
            Self::Transient => RecommendedJobAction::Retry,
            Self::Integrity => RecommendedJobAction::Discard,
        }
    }
}

/// Only errors at the user-selected destination can be corrected by retargeting.
#[must_use]
pub const fn classify(stage: SaveFailureStage, kind: SaveFailureIoKind) -> SaveFailureClass {
    use SaveFailureIoKind as Io;
    match kind {
        Io::ChunkMissing | Io::ChunkCorrupt => SaveFailureClass::Integrity,
        Io::NotFound
        | Io::PermissionDenied
        | Io::NotADirectory
        | Io::ReadOnlyFilesystem
        | Io::InvalidFilename
        | Io::PrivateDirConflict
            if matches!(
                stage,
                SaveFailureStage::Preflight | SaveFailureStage::Materialize
            ) =>
        {
            SaveFailureClass::PermanentDestination
        }
        _ => SaveFailureClass::Transient,
    }
}

#[cfg(kani)]
#[kani::proof]
fn k16_save_failure_recovery() {
    let stage = kani::any();
    let kind = kani::any();
    let class = classify(stage, kind);
    let action = class.recommended_action();
    assert!(
        class != SaveFailureClass::PermanentDestination || action == RecommendedJobAction::Retarget
    );
    assert!(class != SaveFailureClass::Integrity || action == RecommendedJobAction::Discard);
    assert!(class != SaveFailureClass::Transient || action == RecommendedJobAction::Retry);
    kani::cover!(action == RecommendedJobAction::Retry);
    kani::cover!(action == RecommendedJobAction::Retarget);
    kani::cover!(action == RecommendedJobAction::Discard);
}
