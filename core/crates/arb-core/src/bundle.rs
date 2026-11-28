//! Immutable WORM bundle v2 manifest, record schemas, and validator.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::BUNDLE_SCHEMA_VERSION;
use crate::gesture::{GestureKind, TouchPoint};
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
    /// Bundle-relative human-readable logcat file path (e.g., `app-logs/com.example.app.log`).
    pub relative_path: String,
    /// Number of log records written into `relative_path`.
    pub line_count: u64,
    /// Number of crash markers attributed to this package in the bundle.
    pub crash_count: u64,
    /// Lowercase 64-character hexadecimal SHA-256 digest of `relative_path`.
    pub sha256: String,
}

/// Top-level `manifest.json` schema for ARB immutable saved bundles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestV2 {
    /// User note captured with the save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
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
    /// Host continuous and wall clocks read together at the save trigger (#591). Absent from
    /// older manifests and from saves outside the live host clock domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_clock_anchor: Option<HostClockAnchor>,
    /// Device wall clock minus host wall clock computed at save time from the saved clock
    /// anchors and [`Self::host_clock_anchor`]. Absent from older manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_clock_offset: Option<crate::time::DeviceClockOffset>,
}

/// Host clocks sampled together at a save trigger. Stored in `manifest.json`, never in the
/// `clock_anchors` table, so older readers keep opening the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostClockAnchor {
    /// Host `PcContinuousMono` nanoseconds at the trigger.
    pub trigger_pc_cont_ns: i64,
    /// Host wall clock (Unix epoch nanoseconds) read with `trigger_pc_cont_ns`.
    pub trigger_wall_epoch_ns: i64,
}

impl HostClockAnchor {
    /// The anchor as a host clock sample.
    pub const fn sample(&self) -> crate::time::HostClockSample {
        crate::time::HostClockSample {
            continuous_mono_ns: self.trigger_pc_cont_ns,
            wall_epoch_ns: self.trigger_wall_epoch_ns,
        }
    }
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
        /// Requested post-crash interval in milliseconds.
        #[serde(default)]
        post_roll_ms: u64,
        /// Post-crash time covered by video or logcat evidence.
        #[serde(default)]
        actual_post_roll_ms: u64,
        /// The requested post-crash boundary could not be confirmed.
        #[serde(default)]
        post_roll_truncated: bool,
    },
    /// Exact integer frame range requested during trim.
    TrimFrameRange {
        /// Optional half-open bounds on the parent media timeline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_time_ns: Option<u64>,
        /// Exclusive end on the parent media timeline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        out_time_ns: Option<u64>,
        /// Inclusive starting frame index.
        in_frame_index: u64,
        /// Inclusive ending frame index.
        out_frame_index: u64,
    },
}

impl RequestedWindow {
    /// Milliseconds of post-crash evidence recorded in the published manifest.
    #[must_use]
    pub const fn actual_post_roll_ms(&self) -> u64 {
        match self {
            Self::RealElapsedSec {
                actual_post_roll_ms,
                ..
            } => *actual_post_roll_ms,
            Self::TrimFrameRange { .. } => 0,
        }
    }
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
    /// Frame carried in from before the save window because coverage proved it was still on
    /// screen at the window start (#564). `None` when the first frame was captured in the window
    /// and on bundles written before carry-in existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_in: Option<CarriedInFrame>,
}

/// Evidence that justified displaying a frame from before the save window.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CarryInEvidence {
    /// One continuous video coverage interval spans the frame's capture and the window start.
    Coverage,
}

/// A frame captured before the window start and displayed from the window start (#564).
///
/// The frame's own [`FrameRecord::alignment`] keeps its capture time; only the display start
/// recorded here moves to the window start. Readers map media time through
/// [`frame_display_start_ns`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CarriedInFrame {
    /// Frame index of the carried-in frame (always the first frame, `0`).
    pub frame_index: u64,
    /// Capture time of the frame on `DeviceBoottime` (unchanged).
    pub captured_boottime_ns: i64,
    /// Display start on `DeviceBoottime`: the window start `W0` (media time 0).
    pub display_start_boottime_ns: i64,
    /// Why the frame was carried in.
    pub evidence: CarryInEvidence,
}

/// Device boottime at which `frame` starts being displayed (media time origin for frame 0).
///
/// A carried-in frame starts at its recorded display start (`W0`); every other frame starts at
/// its capture time (`Anchored` center, `Bounded` lower bound). `None` for unanchored frames.
#[must_use]
pub fn frame_display_start_ns(
    frame: &FrameRecord,
    carried_in: Option<&CarriedInFrame>,
) -> Option<i64> {
    if let Some(c) = carried_in
        && c.frame_index == frame.frame_index
    {
        return Some(c.display_start_boottime_ns);
    }
    match &frame.alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns, ..
        } => Some(*min_boottime_ns),
        TimeAlignment::Unanchored { .. } => None,
    }
}

/// Read-side view of `frames` whose carried-in frame is placed at its display start.
///
/// The stored rows stay WORM; this copy shifts only the carried-in frame's alignment and
/// device-mono stamp by `display_start - captured` so every reader (log coordinates, screens,
/// viewport times, real-time trim) shares the single media origin `W0`.
#[must_use]
pub fn display_aligned_frames(
    mut frames: Vec<FrameRecord>,
    carried_in: Option<&CarriedInFrame>,
) -> Vec<FrameRecord> {
    let Some(c) = carried_in else {
        return frames;
    };
    let shift = c
        .display_start_boottime_ns
        .saturating_sub(c.captured_boottime_ns);
    if let Some(frame) = frames.iter_mut().find(|f| f.frame_index == c.frame_index) {
        frame.pts_device_mono_ns = frame.pts_device_mono_ns.saturating_add(shift);
        match &mut frame.alignment {
            TimeAlignment::Anchored { boottime_ns, .. } => {
                *boottime_ns = boottime_ns.saturating_add(shift);
            }
            TimeAlignment::Bounded {
                min_boottime_ns,
                max_boottime_ns,
                ..
            } => {
                *min_boottime_ns = min_boottime_ns.saturating_add(shift);
                *max_boottime_ns = max_boottime_ns.saturating_add(shift);
            }
            TimeAlignment::Unanchored { .. } => {}
        }
    }
    frames
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
    /// Lost log records when every observed interval has an exact count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_count: Option<u64>,
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
    /// Raw getevent rows observed inside the saved window, when counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_events: Option<u64>,
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

/// Maps device boottime records onto the video's monotonic clock across suspend gaps.
#[derive(Debug)]
pub struct FrameMediaClock {
    anchors: Vec<(i64, i64)>,
    origin_mono_ns: i64,
}

impl FrameMediaClock {
    /// Builds a clock for one boot epoch. Unanchored frames cannot locate an event.
    #[must_use]
    pub fn new(frames: &[FrameRecord], boot_id: &str, origin_boot_ns: i64) -> Self {
        let mut anchors: Vec<_> = frames
            .iter()
            .filter(|f| f.boot_id == boot_id)
            .filter_map(|f| {
                let boot = match &f.alignment {
                    TimeAlignment::Anchored { boottime_ns, .. } => *boottime_ns,
                    TimeAlignment::Bounded {
                        min_boottime_ns, ..
                    } => *min_boottime_ns,
                    _ => return None,
                };
                Some((boot, f.pts_device_mono_ns))
            })
            .collect();
        anchors.sort_unstable();
        let mut clock = Self {
            anchors,
            origin_mono_ns: 0,
        };
        clock.origin_mono_ns = clock.monotonic_time(origin_boot_ns);
        clock
    }

    fn monotonic_time(&self, boot: i64) -> i64 {
        let next = self.anchors.partition_point(|&(t, _)| t <= boot);
        let Some(&(bt, mono)) = self.anchors.get(next.saturating_sub(1)) else {
            return boot;
        };
        let projected = mono.saturating_add(boot.saturating_sub(bt));
        self.anchors
            .get(next)
            .map_or(projected, |&(_, limit)| projected.min(limit))
    }

    /// Media time relative to the cut, excluding elapsed time spent suspended.
    #[must_use]
    pub fn relative_time(&self, boot: i64) -> i64 {
        self.monotonic_time(boot)
            .saturating_sub(self.origin_mono_ns)
    }
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
    /// X coordinate in display pixels, if available. For a classified [`GestureKind::Swipe`],
    /// this is the stroke's `DOWN` (start) point.
    pub x: Option<f64>,
    /// Y coordinate in display pixels, if available (`DOWN`/start point for `Swipe`).
    pub y: Option<f64>,
    /// Multi-touch pointer identifier, if available.
    pub pointer_id: Option<i32>,
    /// `Tap` vs `Swipe` classification (T-6H item 2). `#[serde(default)]` so every bundle saved
    /// before this field existed (and every legacy schema 3/4 import) deserializes as `Tap`.
    #[serde(default)]
    pub gesture_kind: GestureKind,
    /// End X coordinate for a [`GestureKind::Swipe`] (`None` for `Tap`, whose single `x`/`y`
    /// already identifies the whole gesture).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_x: Option<f64>,
    /// End Y coordinate for a [`GestureKind::Swipe`] (`None` for `Tap`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_y: Option<f64>,
    /// Nanoseconds from this tap's aligned `DOWN` boottime to the gesture's `UP`/end sample
    /// (the stroke's duration). `Some` for both `Tap` (brief press duration) and `Swipe`, `None`
    /// when unknown (e.g. legacy data with only a `DOWN` sample).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_t_offset_ns: Option<i64>,
    /// Decimated trajectory for a [`GestureKind::Swipe`] (`<= 24` points, always empty for `Tap`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<TouchPoint>,
}

/// Validation failure returned by [`ManifestV2::validate`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// The optional trim times are incomplete or empty.
    #[error("expected both trim times and a positive duration")]
    InvalidTrimTimes,
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
                ..
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
                in_time_ns,
                out_time_ns,
                in_frame_index,
                out_frame_index,
            } => {
                if !matches!((in_time_ns, out_time_ns), (None, None))
                    && !matches!((in_time_ns,out_time_ns),(Some(a),Some(b)) if a<b)
                {
                    return Err(ValidationError::InvalidTrimTimes);
                }
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
        let mut seen_app_log_paths = std::collections::BTreeSet::new();
        for app_log in &self.app_logs {
            if !seen_app_log_paths.insert(app_log.relative_path.to_ascii_lowercase()) {
                return Err(ValidationError::InvalidFilePath {
                    path: app_log.relative_path.clone(),
                    reason: "app_logs relative_path must be unique (case-insensitively)",
                });
            }
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

    #[test]
    fn g5_manifest_before_crash_post_roll_defaults_to_zero() {
        let mut old = serde_json::to_value(valid_manifest()).unwrap();
        let window = old["requested_window"].as_object_mut().unwrap();
        for field in ["post_roll_ms", "actual_post_roll_ms", "post_roll_truncated"] {
            window.remove(field);
        }
        let manifest: ManifestV2 = serde_json::from_value(old).unwrap();
        assert!(matches!(
            manifest.requested_window,
            RequestedWindow::RealElapsedSec {
                post_roll_ms: 0,
                actual_post_roll_ms: 0,
                post_roll_truncated: false,
                ..
            }
        ));
    }

    fn valid_manifest() -> ManifestV2 {
        ManifestV2 {
            memo: None,
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
                post_roll_ms: 0,
                actual_post_roll_ms: 0,
                post_roll_truncated: false,
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
                    carried_in: None,
                }),
                audio: Some(AudioStreamMeta {
                    status: StreamStatus::Partial,
                    av_offset_status: AvOffsetStatus::Uncalibrated,
                    av_offset_ms: 0,
                    audio_lead_in_offset_ns: 0,
                    tail_may_be_incomplete: false,
                }),
                logcat: Some(LogcatStreamMeta {
                    dropped_count: None,
                    status: StreamStatus::Present,
                    completeness: LogCompleteness::Unverified,
                    overflow_events: 0,
                    tail_may_be_incomplete: false,
                }),
                input: Some(InputStreamMeta {
                    input_events: None,
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
            host_clock_anchor: None,
            device_clock_offset: None,
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

    /// #591: the host clock anchor and the offset live in `manifest.json` as optional fields.
    /// Manifests without them still parse, and a manifest without them serializes exactly as
    /// before, so older readers see no new key.
    #[test]
    fn host_clock_anchor_is_optional_and_roundtrips() {
        let plain = valid_manifest();
        let json = serde_json::to_value(&plain).expect("serialize");
        assert!(json.get("host_clock_anchor").is_none());
        assert!(json.get("device_clock_offset").is_none());
        let parsed: ManifestV2 = serde_json::from_value(json).expect("older manifest");
        assert_eq!(parsed.host_clock_anchor, None);

        let mut anchored = valid_manifest();
        anchored.host_clock_anchor = Some(super::HostClockAnchor {
            trigger_pc_cont_ns: 5_000_000_000,
            trigger_wall_epoch_ns: 1_795_829_025_123_000_000,
        });
        anchored.device_clock_offset = Some(crate::time::DeviceClockOffset::from_estimate(
            Some(crate::time::DeviceClockOffsetEstimate {
                offset_ms: 1_700,
                uncertainty_ms: 3,
                source: crate::time::ClockOffsetSource::SyncProbe,
                sampled_at_unix_ms: 1_795_829_025_000,
            }),
            crate::time::ClockOffsetUnavailable::NoSyncProbe,
        ));
        let json = serde_json::to_value(&anchored).expect("serialize");
        assert_eq!(json["device_clock_offset"]["offset_ms"], 1_700);
        assert_eq!(json["device_clock_offset"]["source"], "sync_probe");
        assert_eq!(
            json["device_clock_offset"]["sampled_at_utc"],
            "2026-11-28T01:23:45.000Z"
        );
        assert!(json["device_clock_offset"].get("reason").is_none());
        assert_eq!(
            serde_json::from_value::<ManifestV2>(json).expect("parse"),
            anchored
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
    fn carried_in_is_optional_and_round_trips() {
        let m = valid_manifest();
        let json = serde_json::to_string(&m).expect("serialize");
        assert!(!json.contains("carried_in"), "absent carry-in is omitted");
        let mut carried = m;
        if let Some(video) = carried.streams.video.as_mut() {
            video.carried_in = Some(super::CarriedInFrame {
                frame_index: 0,
                captured_boottime_ns: 1_000,
                display_start_boottime_ns: 30_000,
                evidence: super::CarryInEvidence::Coverage,
            });
        }
        let json = serde_json::to_string(&carried).expect("serialize");
        assert!(json.contains(r#""evidence":"coverage""#));
        let parsed: ManifestV2 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, carried);
        carried
            .validate()
            .expect("a carried-in manifest stays valid");
    }

    #[test]
    fn display_start_moves_only_the_carried_in_frame() {
        use super::{
            CarriedInFrame, CarryInEvidence, FrameRecord, display_aligned_frames,
            frame_display_start_ns,
        };
        use crate::time::{TimeAlignment, TimestampSemantics, UncertaintyReason};
        let frame = |index: u64, alignment: TimeAlignment| FrameRecord {
            frame_index: index,
            boot_id: "b".into(),
            pts_device_mono_ns: 100 * i64::try_from(index).unwrap_or(0),
            pc_ingest_cont_ns: 0,
            alignment,
            is_keyframe: index == 0,
            gop_id: 0,
            mp4_sample_offset: 0,
            mp4_sample_size: 0,
        };
        let frames = vec![
            frame(
                0,
                TimeAlignment::Bounded {
                    boot_id: "b".into(),
                    min_boottime_ns: 10,
                    max_boottime_ns: 12,
                    reason: UncertaintyReason::UnbracketedMonotonicExtrapolation,
                },
            ),
            frame(
                1,
                TimeAlignment::Anchored {
                    boot_id: "b".into(),
                    boottime_ns: 500,
                    half_width_ns: 1,
                    semantics: TimestampSemantics::VideoEncoderInputEstimate,
                },
            ),
            frame(
                2,
                TimeAlignment::Unanchored {
                    pc_ingest_cont_ns: 9,
                    reason: UncertaintyReason::PcIngestOnly,
                },
            ),
        ];
        let carried = CarriedInFrame {
            frame_index: 0,
            captured_boottime_ns: 10,
            display_start_boottime_ns: 300,
            evidence: CarryInEvidence::Coverage,
        };
        assert_eq!(frame_display_start_ns(&frames[0], None), Some(10));
        assert_eq!(
            frame_display_start_ns(&frames[0], Some(&carried)),
            Some(300)
        );
        assert_eq!(
            frame_display_start_ns(&frames[1], Some(&carried)),
            Some(500)
        );
        assert_eq!(frame_display_start_ns(&frames[2], None), None);
        let view = display_aligned_frames(frames.clone(), Some(&carried));
        assert_eq!(frame_display_start_ns(&view[0], None), Some(300));
        assert_eq!(
            view[0].pts_device_mono_ns, 290,
            "mono moves with the display start"
        );
        assert_eq!(&view[1..], &frames[1..], "other frames are untouched");
        assert_eq!(display_aligned_frames(frames.clone(), None), frames);
    }

    #[test]
    fn app_logs_manifest_validation_enforces_path_and_digest_consistency() {
        use super::AppLogSummary;

        let mut m = valid_manifest();
        let sha = "b".repeat(64);
        m.files.push(FileDigest {
            path: "app-logs/com.example.app.log".into(),
            bytes: 128,
            sha256: sha.clone(),
        });
        m.app_logs.push(AppLogSummary {
            package_name: "com.example.app".into(),
            relative_path: "app-logs/com.example.app.log".into(),
            line_count: 4,
            crash_count: 1,
            sha256: sha,
        });
        assert_eq!(m.validate(), Ok(()), "consistent app_logs must validate");

        let mut bad_missing = valid_manifest();
        bad_missing.app_logs.push(AppLogSummary {
            package_name: "com.example.app".into(),
            relative_path: "app-logs/com.example.app.log".into(),
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
            relative_path: "../escape.log".into(),
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

    #[test]
    fn app_logs_manifest_validation_rejects_case_insensitive_duplicate_paths() {
        use super::AppLogSummary;

        let mut m = valid_manifest();
        for (pkg, path, sha) in [
            (
                "com.Example.app",
                "app-logs/com.Example.app.log",
                "e".repeat(64),
            ),
            (
                "com.example.app",
                "app-logs/com.example.app.log",
                "f".repeat(64),
            ),
        ] {
            m.files.push(FileDigest {
                path: path.into(),
                bytes: 1,
                sha256: sha.clone(),
            });
            m.app_logs.push(AppLogSummary {
                package_name: pkg.into(),
                relative_path: path.into(),
                line_count: 1,
                crash_count: 0,
                sha256: sha,
            });
        }
        assert!(
            matches!(m.validate(), Err(ValidationError::InvalidFilePath { .. })),
            "app_logs paths colliding on case-insensitive filesystems must fail validation"
        );
    }
}
