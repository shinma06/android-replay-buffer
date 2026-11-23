//! Immutable WORM bundle v2 manifest, record schemas, and validator.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::BUNDLE_SCHEMA_VERSION;
use crate::time::{TimeAlignment, TimestampSemantics};

/// High-level bundle payload category recorded in `manifest.json`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleKind {
    /// Standard full replay bundle (video, optional audio, logcat, input).
    #[default]
    Full,
    /// Log-only WORM bundle containing logcat, unanchored sidecar, and discontinuities
    /// without video or audio tracks.
    LogsOnly,
}

/// Extracted per-app logcat sidecar summary recorded in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppLogSummary {
    /// Android application package name (e.g., `com.example.app`).
    pub package_name: String,
    /// Bundle-relative JSONL file path (e.g., `app-logs/com.example.app.jsonl`).
    pub relative_path: String,
    /// Number of log records written into `relative_path`.
    pub line_count: u64,
    /// Number of crash markers attributed to this package in the bundle.
    pub crash_count: u64,
    /// Lowercase 64-character hexadecimal SHA-256 digest of `relative_path`.
    pub sha256: String,
}

/// Top-level `manifest.json` schema for ARB v2 immutable saved bundles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestV2 {
    /// Bundle manifest schema version; must equal [`BUNDLE_SCHEMA_VERSION`] (`2`).
    pub schema_version: u32,
    /// High-level bundle payload category (`full` or `logs_only`; defaults to `full`).
    #[serde(default)]
    pub bundle_kind: BundleKind,
    /// Globally unique bundle identifier (e.g., ULID or UUID).
    pub bundle_id: String,
    /// Creation timestamp in ISO-8601 UTC format.
    pub created_at_utc: String,
    /// Origin of this bundle (original capture or non-destructive trim).
    pub provenance: Provenance,
    /// Target Android device metadata.
    pub device: DeviceInfo,
    /// User- or automation-requested save/trim window.
    pub requested_window: RequestedWindow,
    /// Device boot epochs covered by this bundle.
    pub epochs: Vec<EpochSpan>,
    /// Per-stream availability, honesty flags, and semantics.
    pub streams: StreamsSummary,
    /// Summary of quarantined `unanchored` records and truncation counts.
    pub unanchored_summary: UnanchoredSummary,
    /// Per-app extracted logcat summaries for foreground / crashed non-system apps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app_logs: Vec<AppLogSummary>,
    /// SHA-256 digests of all immutable payload files inside the bundle directory.
    pub files: Vec<FileDigest>,
}

/// Origin of a v2 bundle.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// Created directly from the live ring store.
    Original,
    /// Non-destructively trimmed from an existing parent bundle.
    Trimmed {
        /// Identifier of the immutable parent bundle.
        parent_bundle_id: String,
        /// Inclusive starting frame index in the parent bundle.
        in_frame_index: u64,
        /// Inclusive ending frame index in the parent bundle.
        out_frame_index: u64,
    },
}

/// Android device metadata recorded in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// ADB serial identifier.
    pub serial: String,
    /// Device model name.
    pub model: String,
    /// Android API level (e.g., 35).
    pub api_level: u32,
    /// Whether the device is an emulator rather than physical hardware.
    pub is_emulator: bool,
}

/// Logical window requested when creating a bundle.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestedWindow {
    /// Real elapsed seconds looking back from the save trigger.
    RealElapsedSec {
        /// Requested duration in seconds.
        duration_sec: u64,
        /// Host PC wall-clock UTC timestamp of the trigger for display.
        trigger_pc_wall_utc: String,
    },
    /// Exact integer frame range requested during trim.
    TrimFrameRange {
        /// Inclusive starting frame index.
        in_frame_index: u64,
        /// Inclusive ending frame index.
        out_frame_index: u64,
    },
}

/// Device `boot_id` span present in the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EpochSpan {
    /// Device boot UUID.
    pub boot_id: String,
    /// Start of observed data in this boot epoch on `DeviceBoottime`.
    pub start_boottime_ns: i64,
    /// End of observed data in this boot epoch on `DeviceBoottime`.
    pub end_boottime_ns: i64,
}

/// Presence state of a single capture stream inside a bundle.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamStatus {
    /// Stream was captured and included.
    Present,
    /// Stream was captured with gaps, opt-outs, or partial coverage.
    Partial,
    /// Stream was unavailable or produced no data.
    Absent,
    /// Stream was intentionally excluded (e.g., logs-only save).
    Excluded,
}

/// Summary metadata for all four capture tracks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamsSummary {
    /// Video stream metadata, if applicable.
    pub video: Option<VideoStreamMeta>,
    /// Audio stream metadata, if applicable.
    pub audio: Option<AudioStreamMeta>,
    /// Logcat stream metadata, if applicable.
    pub logcat: Option<LogcatStreamMeta>,
    /// Input / tap stream metadata, if applicable.
    pub input: Option<InputStreamMeta>,
}

/// Video track summary metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoStreamMeta {
    /// Presence state of the video stream.
    pub status: StreamStatus,
    /// Number of video frames stored in `video.mp4` / `timeline.sqlite`.
    pub frame_count: u64,
    /// `true` if boundary drain timed out before confirming a post-trigger heartbeat.
    pub tail_may_be_incomplete: bool,
    /// Timestamp semantics for frame PTS values.
    pub semantics: TimestampSemantics,
    /// Physical device display width in pixels (e.g., from `wm size`), used to scale raw
    /// touch coordinates onto downscaled video. `None` on legacy or pre-T-6A bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_width: Option<u32>,
    /// Physical device display height in pixels (e.g., from `wm size`), used to scale raw
    /// touch coordinates onto downscaled video. `None` on legacy or pre-T-6A bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_height: Option<u32>,
}

/// Calibration status of audio-to-video synchronization offset.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvOffsetStatus {
    /// Offset has not been calibrated; pipeline lag is not compensated.
    Uncalibrated,
    /// Offset was calibrated empirically for this device/session.
    Calibrated,
    /// Offset was manually adjusted by the user.
    UserOverridden,
}

/// Audio track summary metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioStreamMeta {
    /// Presence state of the audio stream.
    pub status: StreamStatus,
    /// Honesty status of the A/V offset value.
    pub av_offset_status: AvOffsetStatus,
    /// Applied or observed A/V offset in milliseconds.
    pub av_offset_ms: i64,
    /// Exact measured lead-in skew between the two file timelines in nanoseconds:
    /// `first_selected_audio_pts_device_mono_ns - first_selected_video_pts_device_mono_ns`
    /// (`audio.m4a` pts0 sits this many ns later than `video.mp4` pts0 on the shared device
    /// clock; may be negative). `av_offset_ms` is the millisecond truncation of this value.
    /// `0` when either stream was absent or the bundle predates this field.
    #[serde(default)]
    pub audio_lead_in_offset_ns: i64,
    /// `true` if boundary drain timed out before confirming a post-trigger heartbeat.
    #[serde(default)]
    pub tail_may_be_incomplete: bool,
}

/// Honesty indicator for logcat ring-buffer completeness.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogCompleteness {
    /// Confirmed no ring-buffer overflow occurred in the window.
    VerifiedNoOverflow,
    /// One or more logcat buffer overflows were detected.
    OverflowDetected,
    /// Cannot prove absence of kernel/logd overflow under standard ADB permissions.
    Unverified,
}

/// Logcat stream summary metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogcatStreamMeta {
    /// Presence state of the logcat stream.
    pub status: StreamStatus,
    /// Honest completeness status.
    pub completeness: LogCompleteness,
    /// Count of explicitly observed overflow events.
    pub overflow_events: u64,
    /// `true` if boundary drain timed out before confirming a post-trigger heartbeat.
    #[serde(default)]
    pub tail_may_be_incomplete: bool,
}

/// Input / tap stream summary metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InputStreamMeta {
    /// Presence state of the input stream.
    pub status: StreamStatus,
    /// `true` if boundary drain timed out before confirming a post-trigger heartbeat.
    #[serde(default)]
    pub tail_may_be_incomplete: bool,
}

/// Summary of quarantined `unanchored` records in the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnanchoredSummary {
    /// Number of uncertain records included in the `unanchored` sidecar.
    pub included_count: u64,
    /// Number of eligible uncertain records omitted due to quarantine caps.
    pub truncated_count: u64,
}

/// Integrity digest for one immutable file within a v2 bundle directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileDigest {
    /// Relative file path inside the bundle directory (e.g., `video.mp4`).
    pub path: String,
    /// Exact byte length of the file.
    pub bytes: u64,
    /// Lowercase 64-character hexadecimal SHA-256 digest.
    pub sha256: String,
}

/// Timeline bucket assignment stored in `timeline.sqlite`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordBucket {
    /// Confirmed inside the window on the main synchronized timeline.
    Main,
    /// Quarantined in the `unanchored` sidecar.
    Unanchored,
}

/// Frame index row in `timeline.sqlite` (`frames` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRecord {
    /// Zero-based sequential frame index in `video.mp4`.
    pub frame_index: u64,
    /// Device boot epoch of this frame.
    pub boot_id: String,
    /// Raw device `CLOCK_MONOTONIC` presentation timestamp in nanoseconds.
    pub pts_device_mono_ns: i64,
    /// Immutable host `PcContinuousMono` ingestion timestamp in nanoseconds.
    pub pc_ingest_cont_ns: i64,
    /// Derived alignment onto `DeviceBoottime`.
    pub alignment: TimeAlignment,
    /// Whether this frame is an IDR / sync keyframe.
    pub is_keyframe: bool,
    /// Monotonically increasing GOP identifier.
    pub gop_id: u64,
    /// Byte offset of the encoded sample inside `video.mp4`.
    pub mp4_sample_offset: u64,
    /// Encoded sample length in bytes.
    pub mp4_sample_size: u32,
}

/// Logcat row in `timeline.sqlite` (`logs` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    /// Monotonic sequence number within the bundle.
    pub seq: u64,
    /// Device boot epoch, if known.
    pub boot_id: Option<String>,
    /// Raw logcat header wall timestamp (`CLOCK_REALTIME`) in nanoseconds.
    pub header_epoch_ns: Option<i64>,
    /// Raw device `CLOCK_BOOTTIME` when read from logd (causal upper bound).
    pub device_read_boottime_ns: Option<i64>,
    /// Immutable host `PcContinuousMono` ingestion timestamp in nanoseconds.
    pub pc_ingest_cont_ns: i64,
    /// Derived alignment onto `DeviceBoottime`.
    pub alignment: TimeAlignment,
    /// Main timeline vs `unanchored` sidecar bucket.
    pub bucket: RecordBucket,
    /// Log priority level (e.g., `E`, `W`, `I`, `D`, `V`).
    pub level: Option<String>,
    /// Logcat tag.
    pub tag: Option<String>,
    /// Process ID.
    pub pid: Option<i32>,
    /// Thread ID.
    pub tid: Option<i32>,
    /// Process UID (`logger_entry_v4.uid`), when reported by the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Resolved Android application package name (e.g., `com.example.app`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    /// Log message payload.
    pub message: String,
}

/// Touch / input row in `timeline.sqlite` (`taps` table).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TapRecord {
    /// Monotonic sequence number within the bundle.
    pub seq: u64,
    /// Device boot epoch, if known.
    pub boot_id: Option<String>,
    /// Raw device `CLOCK_MONOTONIC` event timestamp in nanoseconds.
    pub event_mono_ns: Option<i64>,
    /// Immutable host `PcContinuousMono` ingestion timestamp in nanoseconds.
    pub pc_ingest_cont_ns: i64,
    /// Derived alignment onto `DeviceBoottime`.
    pub alignment: TimeAlignment,
    /// Main timeline vs `unanchored` sidecar bucket.
    pub bucket: RecordBucket,
    /// Input action label (e.g., `DOWN`, `MOVE`, `UP`).
    pub action: String,
    /// X coordinate in display pixels, if available.
    pub x: Option<f64>,
    /// Y coordinate in display pixels, if available.
    pub y: Option<f64>,
    /// Multi-touch pointer identifier, if available.
    pub pointer_id: Option<i32>,
}

/// Validation failure returned by [`ManifestV2::validate`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// `schema_version` was not `2`.
    #[error("unsupported manifest schema_version {found}; expected {BUNDLE_SCHEMA_VERSION}")]
    UnsupportedSchemaVersion {
        /// Observed schema version.
        found: u32,
    },
    /// Required string field was empty or whitespace-only.
    #[error("required field '{field}' must not be empty")]
    EmptyField {
        /// Dotted field path.
        field: &'static str,
    },
    /// Trimmed bundle referenced itself as its own parent.
    #[error("provenance.parent_bundle_id must differ from bundle_id")]
    SelfReferentialProvenance,
    /// Start frame index exceeded end frame index.
    #[error(
        "invalid frame range: in_frame_index ({in_frame_index}) > out_frame_index ({out_frame_index})"
    )]
    InvalidFrameRange {
        /// Start frame index.
        in_frame_index: u64,
        /// End frame index.
        out_frame_index: u64,
    },
    /// Requested duration must be strictly positive.
    #[error("requested_window.duration_sec must be greater than zero")]
    ZeroRequestedDuration,
    /// Epoch span had `start_boottime_ns > end_boottime_ns` or empty `boot_id`.
    #[error("invalid epoch span at index {index}: {reason}")]
    InvalidEpochSpan {
        /// Zero-based index in `manifest.epochs`.
        index: usize,
        /// Description of the violated invariant.
        reason: &'static str,
    },
    /// Stream metadata contained contradictory status and counters.
    #[error("inconsistent stream metadata: {reason}")]
    InconsistentStreamMeta {
        /// Description of the violated invariant.
        reason: &'static str,
    },
    /// `files` list was empty.
    #[error("manifest.files must contain at least one immutable payload file")]
    EmptyFileList,
    /// File path was empty, absolute, contained traversal, or named `manifest.json`.
    #[error("invalid bundle file path '{path}': {reason}")]
    InvalidFilePath {
        /// Offending file path.
        path: String,
        /// Description of the violation.
        reason: &'static str,
    },
    /// Duplicate path appeared in `manifest.files`.
    #[error("duplicate file entry '{path}' in manifest.files")]
    DuplicateFilePath {
        /// Duplicate relative path.
        path: String,
    },
    /// SHA-256 digest was not exactly 64 lowercase hexadecimal characters.
    #[error("invalid lowercase 64-hex SHA-256 digest for '{path}': '{digest}'")]
    InvalidSha256 {
        /// Relative file path.
        path: String,
        /// Invalid digest string.
        digest: String,
    },
}

/// Returns `true` iff `value` is a valid lowercase 64-character hexadecimal SHA-256 digest.
pub fn is_valid_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl ManifestV2 {
    /// Validates schema version, required fields, SHA-256 digests, and invariants.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] describing the first violated invariant.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.validate_header()?;
        self.validate_epochs_and_streams()?;
        self.validate_files()
    }

    fn validate_header(&self) -> Result<(), ValidationError> {
        if self.schema_version != BUNDLE_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedSchemaVersion {
                found: self.schema_version,
            });
        }
        if self.bundle_id.trim().is_empty() {
            return Err(ValidationError::EmptyField { field: "bundle_id" });
        }
        if self.created_at_utc.trim().is_empty() {
            return Err(ValidationError::EmptyField {
                field: "created_at_utc",
            });
        }
        if self.device.serial.trim().is_empty() {
            return Err(ValidationError::EmptyField {
                field: "device.serial",
            });
        }

        if let Provenance::Trimmed {
            parent_bundle_id,
            in_frame_index,
            out_frame_index,
        } = &self.provenance
        {
            if parent_bundle_id.trim().is_empty() {
                return Err(ValidationError::EmptyField {
                    field: "provenance.parent_bundle_id",
                });
            }
            if parent_bundle_id == &self.bundle_id {
                return Err(ValidationError::SelfReferentialProvenance);
            }
            if in_frame_index > out_frame_index {
                return Err(ValidationError::InvalidFrameRange {
                    in_frame_index: *in_frame_index,
                    out_frame_index: *out_frame_index,
                });
            }
        }

        match &self.requested_window {
            RequestedWindow::RealElapsedSec {
                duration_sec,
                trigger_pc_wall_utc,
            } => {
                if *duration_sec == 0 {
                    return Err(ValidationError::ZeroRequestedDuration);
                }
                if trigger_pc_wall_utc.trim().is_empty() {
                    return Err(ValidationError::EmptyField {
                        field: "requested_window.trigger_pc_wall_utc",
                    });
                }
            }
            RequestedWindow::TrimFrameRange {
                in_frame_index,
                out_frame_index,
            } => {
                if in_frame_index > out_frame_index {
                    return Err(ValidationError::InvalidFrameRange {
                        in_frame_index: *in_frame_index,
                        out_frame_index: *out_frame_index,
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_epochs_and_streams(&self) -> Result<(), ValidationError> {
        for (index, epoch) in self.epochs.iter().enumerate() {
            if epoch.boot_id.trim().is_empty() {
                return Err(ValidationError::InvalidEpochSpan {
                    index,
                    reason: "boot_id must not be empty",
                });
            }
            if epoch.start_boottime_ns > epoch.end_boottime_ns {
                return Err(ValidationError::InvalidEpochSpan {
                    index,
                    reason: "start_boottime_ns must be <= end_boottime_ns",
                });
            }
        }

        if self.bundle_kind == BundleKind::LogsOnly {
            if let Some(video) = &self.streams.video
                && !matches!(video.status, StreamStatus::Absent | StreamStatus::Excluded)
            {
                return Err(ValidationError::InconsistentStreamMeta {
                    reason: "logs_only bundle must have absent or excluded video stream",
                });
            }
            if let Some(audio) = &self.streams.audio
                && !matches!(audio.status, StreamStatus::Absent | StreamStatus::Excluded)
            {
                return Err(ValidationError::InconsistentStreamMeta {
                    reason: "logs_only bundle must have absent or excluded audio stream",
                });
            }
        }

        if let Some(video) = &self.streams.video
            && matches!(video.status, StreamStatus::Absent | StreamStatus::Excluded)
            && video.frame_count > 0
        {
            return Err(ValidationError::InconsistentStreamMeta {
                reason: "absent or excluded video stream cannot declare non-zero frame_count",
            });
        }

        if let Some(video) = &self.streams.video
            && (video.display_width == Some(0) || video.display_height == Some(0))
        {
            return Err(ValidationError::InconsistentStreamMeta {
                reason: "video display_width and display_height must be > 0 when present",
            });
        }

        if let Some(logcat) = &self.streams.logcat
            && logcat.completeness == LogCompleteness::VerifiedNoOverflow
            && logcat.overflow_events > 0
        {
            return Err(ValidationError::InconsistentStreamMeta {
                reason: "logcat verified_no_overflow contradicts non-zero overflow_events",
            });
        }
        Ok(())
    }

    fn validate_files(&self) -> Result<(), ValidationError> {
        if self.files.is_empty() {
            return Err(ValidationError::EmptyFileList);
        }

        let mut seen_paths = BTreeSet::new();
        for file in &self.files {
            if file.path.trim().is_empty() {
                return Err(ValidationError::InvalidFilePath {
                    path: file.path.clone(),
                    reason: "path must not be empty",
                });
            }
            if file.path.starts_with('/')
                || file.path.starts_with('\\')
                || file.path.contains('\\')
                || file
                    .path
                    .split('/')
                    .any(|seg| seg.is_empty() || seg == "." || seg == "..")
            {
                return Err(ValidationError::InvalidFilePath {
                    path: file.path.clone(),
                    reason: "path must be a normalized relative bundle path without traversal",
                });
            }
            if file.path == "manifest.json" {
                return Err(ValidationError::InvalidFilePath {
                    path: file.path.clone(),
                    reason: "manifest.json cannot hash itself",
                });
            }
            if !seen_paths.insert(file.path.as_str()) {
                return Err(ValidationError::DuplicateFilePath {
                    path: file.path.clone(),
                });
            }
            if !is_valid_lowercase_sha256(&file.sha256) {
                return Err(ValidationError::InvalidSha256 {
                    path: file.path.clone(),
                    digest: file.sha256.clone(),
                });
            }
        }
        for app_log in &self.app_logs {
            if app_log.package_name.trim().is_empty() {
                return Err(ValidationError::EmptyField {
                    field: "app_logs[].package_name",
                });
            }
            if !app_log.relative_path.starts_with("app-logs/")
                || app_log.relative_path.contains('\\')
                || app_log
                    .relative_path
                    .split('/')
                    .any(|seg| seg.is_empty() || seg == "." || seg == "..")
            {
                return Err(ValidationError::InvalidFilePath {
                    path: app_log.relative_path.clone(),
                    reason: "app_logs relative_path must live under app-logs/ without traversal",
                });
            }
            if !is_valid_lowercase_sha256(&app_log.sha256) {
                return Err(ValidationError::InvalidSha256 {
                    path: app_log.relative_path.clone(),
                    digest: app_log.sha256.clone(),
                });
            }
            let matching_file = self.files.iter().find(|f| f.path == app_log.relative_path);
            match matching_file {
                Some(entry) if entry.sha256 == app_log.sha256 => {}
                Some(_) => {
                    return Err(ValidationError::InvalidSha256 {
                        path: app_log.relative_path.clone(),
                        digest: app_log.sha256.clone(),
                    });
                }
                None => {
                    return Err(ValidationError::InvalidFilePath {
                        path: app_log.relative_path.clone(),
                        reason: "app_logs entry must also be listed in manifest.files",
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AudioStreamMeta, AvOffsetStatus, BundleKind, DeviceInfo, EpochSpan, FileDigest,
        InputStreamMeta, LogCompleteness, LogcatStreamMeta, ManifestV2, Provenance,
        RequestedWindow, StreamStatus, StreamsSummary, UnanchoredSummary, ValidationError,
        VideoStreamMeta,
    };
    use crate::time::TimestampSemantics;

    fn valid_manifest() -> ManifestV2 {
        ManifestV2 {
            schema_version: 2,
            bundle_kind: BundleKind::Full,
            bundle_id: "01JARB00000000000000000001".into(),
            created_at_utc: "2026-11-22T12:00:00Z".into(),
            provenance: Provenance::Original,
            device: DeviceInfo {
                serial: "emulator-5554".into(),
                model: "Pixel 9".into(),
                api_level: 35,
                is_emulator: true,
            },
            requested_window: RequestedWindow::RealElapsedSec {
                duration_sec: 30,
                trigger_pc_wall_utc: "2026-11-22T12:00:00Z".into(),
            },
            epochs: vec![EpochSpan {
                boot_id: "boot-1".into(),
                start_boottime_ns: 1_000,
                end_boottime_ns: 31_000,
            }],
            streams: StreamsSummary {
                video: Some(VideoStreamMeta {
                    status: StreamStatus::Present,
                    frame_count: 900,
                    tail_may_be_incomplete: false,
                    semantics: TimestampSemantics::VideoEncoderInputEstimate,
                    display_width: Some(1080),
                    display_height: Some(2400),
                }),
                audio: Some(AudioStreamMeta {
                    status: StreamStatus::Partial,
                    av_offset_status: AvOffsetStatus::Uncalibrated,
                    av_offset_ms: 0,
                    audio_lead_in_offset_ns: 0,
                    tail_may_be_incomplete: false,
                }),
                logcat: Some(LogcatStreamMeta {
                    status: StreamStatus::Present,
                    completeness: LogCompleteness::Unverified,
                    overflow_events: 0,
                    tail_may_be_incomplete: false,
                }),
                input: Some(InputStreamMeta {
                    status: StreamStatus::Present,
                    tail_may_be_incomplete: false,
                }),
            },
            unanchored_summary: UnanchoredSummary {
                included_count: 12,
                truncated_count: 0,
            },
            app_logs: Vec::new(),
            files: vec![
                FileDigest {
                    path: "video.mp4".into(),
                    bytes: 1_234_567,
                    sha256: "a".repeat(64),
                },
                FileDigest {
                    path: "timeline.sqlite".into(),
                    bytes: 234_567,
                    sha256: "0123456789abcdef".repeat(4),
                },
            ],
        }
    }

    #[test]
    fn valid_manifest_roundtrips_json_and_validates() {
        let manifest = valid_manifest();
        assert_eq!(
            manifest.validate(),
            Ok(()),
            "baseline manifest must validate"
        );

        let json = serde_json::to_string(&manifest).expect("manifest serialization must succeed");
        let parsed: ManifestV2 =
            serde_json::from_str(&json).expect("manifest deserialization must succeed");
        assert_eq!(
            parsed, manifest,
            "manifest JSON round-trip must be lossless"
        );
    }

    #[test]
    fn rejects_invalid_schema_uppercase_sha_and_contradictions() {
        let mut m = valid_manifest();
        m.schema_version = 4;
        assert!(
            matches!(
                m.validate(),
                Err(ValidationError::UnsupportedSchemaVersion { found: 4 })
            ),
            "non-v2 schema_version must be rejected"
        );

        let mut m = valid_manifest();
        m.files[0].sha256 = "A".repeat(64);
        assert!(
            matches!(m.validate(), Err(ValidationError::InvalidSha256 { .. })),
            "uppercase SHA-256 hex must be rejected"
        );

        let mut m = valid_manifest();
        m.files[0].path = "../escape.mp4".into();
        assert!(
            matches!(m.validate(), Err(ValidationError::InvalidFilePath { .. })),
            "path traversal must be rejected"
        );

        let mut m = valid_manifest();
        if let Some(logcat) = m.streams.logcat.as_mut() {
            logcat.completeness = LogCompleteness::VerifiedNoOverflow;
            logcat.overflow_events = 3;
        }
        assert!(
            matches!(
                m.validate(),
                Err(ValidationError::InconsistentStreamMeta { .. })
            ),
            "verified_no_overflow with overflow_events > 0 must be rejected"
        );

        let mut m = valid_manifest();
        if let Some(video) = m.streams.video.as_mut() {
            video.display_width = Some(0);
        }
        assert!(
            matches!(
                m.validate(),
                Err(ValidationError::InconsistentStreamMeta { .. })
            ),
            "zero display_width must be rejected"
        );
    }

    #[test]
    fn video_meta_without_display_dimensions_deserializes_for_legacy_compatibility() {
        let mut m = valid_manifest();
        if let Some(video) = m.streams.video.as_mut() {
            video.display_width = None;
            video.display_height = None;
        }
        let json = serde_json::to_string(&m).expect("serialize");
        assert!(
            !json.contains("display_width"),
            "None display_width should be omitted from JSON"
        );
        let parsed: ManifestV2 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, m);
    }

    #[test]
    fn app_logs_manifest_validation_enforces_path_and_digest_consistency() {
        use super::AppLogSummary;

        let mut m = valid_manifest();
        let sha = "b".repeat(64);
        m.files.push(FileDigest {
            path: "app-logs/com.example.app.jsonl".into(),
            bytes: 128,
            sha256: sha.clone(),
        });
        m.app_logs.push(AppLogSummary {
            package_name: "com.example.app".into(),
            relative_path: "app-logs/com.example.app.jsonl".into(),
            line_count: 4,
            crash_count: 1,
            sha256: sha,
        });
        assert_eq!(m.validate(), Ok(()), "consistent app_logs must validate");

        let mut bad_missing = valid_manifest();
        bad_missing.app_logs.push(AppLogSummary {
            package_name: "com.example.app".into(),
            relative_path: "app-logs/com.example.app.jsonl".into(),
            line_count: 1,
            crash_count: 0,
            sha256: "c".repeat(64),
        });
        assert!(
            matches!(
                bad_missing.validate(),
                Err(ValidationError::InvalidFilePath { .. })
            ),
            "app_logs missing from manifest.files must fail validation"
        );

        let mut bad_path = valid_manifest();
        bad_path.app_logs.push(AppLogSummary {
            package_name: "com.example.app".into(),
            relative_path: "../escape.jsonl".into(),
            line_count: 1,
            crash_count: 0,
            sha256: "d".repeat(64),
        });
        assert!(
            matches!(
                bad_path.validate(),
                Err(ValidationError::InvalidFilePath { .. })
            ),
            "app_logs outside app-logs/ must fail validation"
        );
    }
}
