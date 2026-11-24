//! Pure read-only parsers for legacy ARB schema 3 (`v0.2.0`) and schema 4 (`develop`).
//!
//! Legacy saved bundles are never modified on disk. These parsers tolerate
//! unknown optional JSON fields, decode stringified 64-bit nanosecond/microsecond
//! values, interpret `null` and `Long.MAX_VALUE` (`9223372036854775807`) as
//! explicit unknown-time sentinels, and support schema 4 log-only saves cleanly.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::time::{TimeAlignment, TimestampSemantics, UncertaintyReason};

/// Legacy `Long.MAX_VALUE` (`9223372036854775807`) uncertainty sentinel used in schema 3 and 4.
pub const LEGACY_UNCERTAINTY_SENTINEL_NS: i64 = i64::MAX;

/// Legacy saved bundle schema generation.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacySchemaVersion {
    /// `v0.2.0` / `main` format (`session.json`, `video-001.mp4`, `frames.jsonl`).
    Schema3,
    /// `develop` format (`replay.json`, `video.mp4`, `video-frames.jsonl`, `touches.jsonl`, log-only).
    Schema4,
}

/// Logical save content category inferred from a legacy manifest.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacySaveKind {
    /// Standard save containing video and logcat streams.
    VideoAndLogs,
    /// Log-only save (`-logs`, `LOGS_ONLY`, or `video_excluded: true`) with no MP4.
    LogsOnly,
    /// Trimmed schema 4 save folder.
    Trim,
}

/// Decoded uncertainty value from a legacy record or manifest.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ns", rename_all = "snake_case")]
pub enum LegacyUncertainty {
    /// Concrete non-negative error bound in nanoseconds.
    Known(u64),
    /// Legacy `Long.MAX_VALUE` (`9223372036854775807`) unknown-time sentinel.
    UnknownSentinel,
    /// Field was `null` or omitted.
    #[default]
    Missing,
}

impl LegacyUncertainty {
    /// Returns the finite error bound in nanoseconds when [`Self::Known`].
    pub const fn known_ns(self) -> Option<u64> {
        match self {
            Self::Known(ns) => Some(ns),
            Self::UnknownSentinel | Self::Missing => None,
        }
    }
}

/// File inventory projected from a legacy schema 3 or schema 4 bundle.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LegacyBundleFiles {
    /// Manifest file name (`replay.json` or `session.json`).
    pub manifest: String,
    /// Optional summary readme file name.
    pub readme: Option<String>,
    /// Optional MP4 video file name (`video.mp4` or `video-001.mp4`).
    pub video: Option<String>,
    /// Optional frame index JSONL file name (`video-frames.jsonl` or `frames.jsonl`).
    pub video_frames: Option<String>,
    /// Optional raw Annex-B video source file name.
    pub video_source: Option<String>,
    /// Optional device logcat JSONL file name.
    pub logcat_device: Option<String>,
    /// Optional app-filtered logcat JSONL file name.
    pub logcat_app: Option<String>,
    /// Optional touch events JSONL file name (schema 4 only).
    pub touches: Option<String>,
}

/// Row counts projected from a legacy manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LegacyCounts {
    /// Device logcat row count, if recorded.
    pub device_log_rows: Option<u64>,
    /// App logcat row count, if recorded.
    pub app_log_rows: Option<u64>,
    /// Video frame row count, if recorded.
    pub video_frames: Option<u64>,
    /// Total touch row count, if recorded.
    pub touches: Option<u64>,
    /// Touch rows overlapping the movie timeline, if recorded.
    pub touches_in_movie: Option<u64>,
}

/// Timebase summary projected from schema 3 or schema 4 metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyTimebase {
    /// Timebase domain label (e.g., `device_boottime`), if recorded.
    pub domain: Option<String>,
    /// Whether the window clock alignment was known.
    pub window_known: bool,
    /// Optional window start timestamp in nanoseconds (schema 3).
    pub window_start_ns: Option<i64>,
    /// Optional window end timestamp in nanoseconds (schema 3).
    pub window_end_ns: Option<i64>,
    /// Optional window duration in nanoseconds.
    pub window_duration_ns: Option<i64>,
    /// Window end uncertainty (interpreting `Long.MAX_VALUE` as `UnknownSentinel`).
    pub window_end_uncertainty: LegacyUncertainty,
}

impl LegacyTimebase {
    /// Returns the absolute `DeviceBoottime` origin (`window_start_elapsed_ns`)
    /// when this timebase is known and on the `device_boottime` domain.
    ///
    /// Returns `None` when the origin is missing, `!window_known`, or the
    /// manifest uses a non-boottime domain such as Schema 3 `sequence_ns`, so
    /// callers never place window-relative or sequence-relative offsets into
    /// `DeviceBoottime`.
    #[must_use]
    pub fn device_boottime_origin_ns(&self) -> Option<i64> {
        if !self.window_known {
            return None;
        }
        if self.domain.as_deref() != Some("device_boottime") {
            return None;
        }
        self.window_start_ns
    }
}

/// Video playback segment metadata projected from legacy `parts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyVideoPart {
    /// Video filename (e.g., `video.mp4` or `video-001.mp4`).
    pub file: String,
    /// Zero-based segment index.
    pub segment_index: u64,
    /// Capture generation number, if present.
    pub generation: Option<u64>,
    /// Segment start on movie timeline in microseconds.
    pub movie_start_us: i64,
    /// Segment end on movie timeline in microseconds.
    pub movie_end_us: i64,
    /// Segment duration in microseconds.
    pub duration_us: i64,
    /// Aligned window start in nanoseconds (`t_start_ns` or `window_start_ns`).
    pub t_start_ns: Option<i64>,
    /// Aligned window end in nanoseconds (`t_end_ns` or `window_end_ns`).
    pub t_end_ns: Option<i64>,
    /// Whether clock alignment was known for this segment.
    pub clock_alignment_known: bool,
    /// Number of decoder preroll samples before the edit start.
    pub preroll_samples: u64,
}

/// Unified read-only metadata projection for both schema 3 and schema 4 bundles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyBundleMetadata {
    /// Legacy schema generation (`Schema3` or `Schema4`).
    pub schema: LegacySchemaVersion,
    /// Whether the legacy save finished publishing (`complete: true`).
    pub complete: bool,
    /// Save identifier (`save_id`).
    pub save_id: String,
    /// Folder name recorded in the manifest, if present.
    pub folder_name: Option<String>,
    /// Capture mode string (e.g., `VIDEO_AND_LOGS` or `LOGS_ONLY`).
    pub capture_mode: Option<String>,
    /// Trigger kind (e.g., `manual`, `crash`, `trim`).
    pub trigger: Option<String>,
    /// High-level save kind (video+logs, log-only, or trim).
    pub save_kind: LegacySaveKind,
    /// `true` when video was explicitly excluded (log-only save).
    pub video_excluded: bool,
    /// Creation timestamp string, if present.
    pub created_at: Option<String>,
    /// Configured replay buffer seconds at save time, if present.
    pub replay_seconds: Option<u64>,
    /// Projected file paths inside the legacy bundle directory.
    pub files: LegacyBundleFiles,
    /// Projected record counts.
    pub counts: LegacyCounts,
    /// Projected timebase metadata.
    pub timebase: LegacyTimebase,
    /// Total movie duration in microseconds, if a video track is present.
    pub video_movie_duration_us: Option<i64>,
    /// Video playback segments (`parts`).
    pub parts: Vec<LegacyVideoPart>,
    /// Per-file SHA-256 digests recorded in `files_sha256`.
    pub files_sha256: BTreeMap<String, String>,
}

/// Presentation role of a decoded frame inside a legacy MP4 edit list.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyFramePresentation {
    /// Frame is presented on the movie timeline.
    Presented,
    /// Frame is decoder preroll preceding the edit list start.
    Preroll,
    /// Frame is neither presented nor marked preroll.
    NotPresented,
}

/// Whether clock alignment was proved for a legacy frame row.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyClockAlignment {
    /// Frame timestamp has a known clock alignment.
    Known,
    /// Frame clock alignment was unknown.
    Unknown,
}

/// Unified read-only projection of a row from `video-frames.jsonl` (schema 4) or `frames.jsonl` (schema 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyFrameRow {
    /// Video part file name.
    pub part: String,
    /// Zero-based segment index.
    pub segment_index: u64,
    /// Zero-based sample index inside the MP4 track.
    pub sample_index: u64,
    /// Capture generation number, if present.
    pub generation: Option<u64>,
    /// Whether the source packet was a keyframe.
    pub source_key: bool,
    /// Whether the output packet is a keyframe.
    pub output_key: bool,
    /// Encoded output width in pixels, if present.
    pub output_width: Option<u32>,
    /// Encoded output height in pixels, if present.
    pub output_height: Option<u32>,
    /// Source presentation timestamp in microseconds.
    pub source_pts_us: Option<i64>,
    /// Media timeline timestamp in microseconds.
    pub media_pts_us: Option<i64>,
    /// Movie timeline timestamp in microseconds (`None` for preroll samples).
    pub movie_pts_us: Option<i64>,
    /// Movie timeline end timestamp in microseconds.
    pub movie_end_us: Option<i64>,
    /// Device elapsed / boottime observation in nanoseconds, if present.
    pub elapsed_ns: Option<i64>,
    /// Window-relative aligned timestamp in nanoseconds (`t_ns` in schema 4, `window_ns` in schema 3).
    pub t_ns: Option<i64>,
    /// Alignment uncertainty (interpreting `Long.MAX_VALUE` as `UnknownSentinel`).
    pub uncertainty: LegacyUncertainty,
    /// Whether clock alignment was known for this frame.
    pub clock_alignment: LegacyClockAlignment,
    /// Presentation role (`Presented`, `Preroll`, or `NotPresented`).
    pub presentation: LegacyFramePresentation,
    /// Boot segment or clock epoch label, if present.
    pub boot_segment: Option<String>,
}

impl LegacyFrameRow {
    /// Projects this legacy frame row onto the v2 [`TimeAlignment`] model by
    /// rebasing window-relative `t_ns` onto `window_start_elapsed_ns`.
    ///
    /// Never places window-relative `t_ns` directly into `DeviceBoottime`:
    /// if `window_start_elapsed_ns` is `None` (missing/unknown or Schema 3
    /// `sequence_ns`) or `origin.checked_add(t_ns)` overflows, this degrades
    /// honestly to [`TimeAlignment::Unanchored`].
    pub fn to_time_alignment_with_origin(
        &self,
        boot_id: &str,
        window_start_elapsed_ns: Option<i64>,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        if self.clock_alignment == LegacyClockAlignment::Known
            && let (Some(origin), Some(t_ns), Some(half_width_ns)) = (
                window_start_elapsed_ns,
                self.t_ns,
                self.uncertainty.known_ns(),
            )
            && let Some(boottime_ns) = origin.checked_add(t_ns)
        {
            return TimeAlignment::Anchored {
                boot_id: boot_id.to_owned(),
                boottime_ns,
                half_width_ns,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            };
        }
        TimeAlignment::Unanchored {
            pc_ingest_cont_ns,
            reason: UncertaintyReason::LegacySentinelUnknown,
        }
    }

    /// Projects this legacy frame row onto [`TimeAlignment`] without a known
    /// `window_start_elapsed_ns` origin.
    ///
    /// Because legacy `t_ns` / `window_ns` is window-relative rather than an
    /// absolute `DeviceBoottime`, calling this without an origin always emits
    /// [`TimeAlignment::Unanchored`]. Prefer
    /// [`Self::to_time_alignment_with_origin`] with
    /// [`LegacyTimebase::device_boottime_origin_ns`].
    pub fn to_time_alignment(&self, boot_id: &str, pc_ingest_cont_ns: i64) -> TimeAlignment {
        self.to_time_alignment_with_origin(boot_id, None, pc_ingest_cont_ns)
    }
}

/// Unified read-only projection of a row from `logcat-device.jsonl` or `logcat-app.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyLogRow {
    /// Record identifier, if present.
    pub record_id: Option<String>,
    /// Boot segment or clock epoch label, if present.
    pub boot_segment: Option<String>,
    /// Capture generation number, if present.
    pub generation: Option<u64>,
    /// Raw wall-clock epoch timestamp in nanoseconds, if present.
    pub epoch_ns: Option<i64>,
    /// Device elapsed / boottime timestamp in nanoseconds, if present.
    pub elapsed_ns: Option<i64>,
    /// Window-relative aligned timestamp in nanoseconds (`t_ns` or `window_ns`).
    pub t_ns: Option<i64>,
    /// Alignment uncertainty (interpreting `Long.MAX_VALUE` as `UnknownSentinel`).
    pub uncertainty: LegacyUncertainty,
    /// Android UID, if present.
    pub uid: Option<i32>,
    /// Process ID, if present.
    pub pid: Option<i32>,
    /// Thread ID, if present.
    pub tid: Option<i32>,
    /// Logcat priority integer, if present.
    pub priority: Option<i32>,
    /// Logcat tag, if present.
    pub tag: Option<String>,
    /// Log message text.
    pub message: String,
    /// Whether the log belonged to the target app (`None` when undetermined).
    pub app_membership: Option<bool>,
}

impl LegacyLogRow {
    /// Projects this legacy log row onto the v2 [`TimeAlignment`] model by
    /// rebasing window-relative `t_ns` onto `window_start_elapsed_ns`.
    ///
    /// Never places window-relative `t_ns` directly into `DeviceBoottime`:
    /// if `window_start_elapsed_ns` is `None` or `origin.checked_add(t_ns)`
    /// overflows, this degrades honestly to [`TimeAlignment::Unanchored`].
    pub fn to_time_alignment_with_origin(
        &self,
        boot_id: &str,
        window_start_elapsed_ns: Option<i64>,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        if let (Some(origin), Some(t_ns), Some(half_width_ns)) = (
            window_start_elapsed_ns,
            self.t_ns,
            self.uncertainty.known_ns(),
        ) && let Some(boottime_ns) = origin.checked_add(t_ns)
        {
            return TimeAlignment::Anchored {
                boot_id: boot_id.to_owned(),
                boottime_ns,
                half_width_ns,
                semantics: TimestampSemantics::LogcatHeaderWall,
            };
        }
        TimeAlignment::Unanchored {
            pc_ingest_cont_ns,
            reason: UncertaintyReason::LegacySentinelUnknown,
        }
    }

    /// Projects this legacy log row onto [`TimeAlignment`] without a known
    /// `window_start_elapsed_ns` origin (always degrades to
    /// [`TimeAlignment::Unanchored`]). Prefer
    /// [`Self::to_time_alignment_with_origin`] with
    /// [`LegacyTimebase::device_boottime_origin_ns`].
    pub fn to_time_alignment(&self, boot_id: &str, pc_ingest_cont_ns: i64) -> TimeAlignment {
        self.to_time_alignment_with_origin(boot_id, None, pc_ingest_cont_ns)
    }
}

/// Unified read-only projection of a row from schema 4 `touches.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyTouchRow {
    /// Boot segment label, if present.
    pub boot_segment: Option<String>,
    /// Capture generation number, if present.
    pub generation: Option<u64>,
    /// Raw device `CLOCK_MONOTONIC` timestamp in nanoseconds, if present.
    pub device_mono_ns: Option<i64>,
    /// Device elapsed / boottime timestamp in nanoseconds, if present.
    pub elapsed_ns: Option<i64>,
    /// Window-relative aligned timestamp in nanoseconds, if mapped.
    pub t_ns: Option<i64>,
    /// Alignment uncertainty (interpreting `Long.MAX_VALUE` as `UnknownSentinel`).
    pub uncertainty: LegacyUncertainty,
    /// Movie position in microseconds (`None` when outside movie or unmapped).
    pub movie_us: Option<i64>,
    /// Touch action string (e.g., `DOWN`, `MOVE`, `UP`).
    pub action: String,
    /// Pointer identifier, if present.
    pub pointer: Option<i32>,
    /// Natural-orientation display X coordinate in pixels.
    pub x: Option<f64>,
    /// Natural-orientation display Y coordinate in pixels.
    pub y: Option<f64>,
    /// Rotated screen X coordinate in pixels.
    pub screen_x: Option<f64>,
    /// Rotated screen Y coordinate in pixels.
    pub screen_y: Option<f64>,
    /// Input source (`touchscreen` or `injected`), if present.
    pub source: Option<String>,
}

impl LegacyTouchRow {
    /// Projects this legacy touch row onto the v2 [`TimeAlignment`] model by
    /// rebasing window-relative `t_ns` onto `window_start_elapsed_ns`.
    ///
    /// Never places window-relative `t_ns` directly into `DeviceBoottime`:
    /// if `window_start_elapsed_ns` is `None` or `origin.checked_add(t_ns)`
    /// overflows, this degrades honestly to [`TimeAlignment::Unanchored`].
    pub fn to_time_alignment_with_origin(
        &self,
        boot_id: &str,
        window_start_elapsed_ns: Option<i64>,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        if let (Some(origin), Some(t_ns), Some(half_width_ns)) = (
            window_start_elapsed_ns,
            self.t_ns,
            self.uncertainty.known_ns(),
        ) && let Some(boottime_ns) = origin.checked_add(t_ns)
        {
            return TimeAlignment::Anchored {
                boot_id: boot_id.to_owned(),
                boottime_ns,
                half_width_ns,
                semantics: TimestampSemantics::InputEventTime,
            };
        }
        TimeAlignment::Unanchored {
            pc_ingest_cont_ns,
            reason: UncertaintyReason::LegacySentinelUnknown,
        }
    }

    /// Projects this legacy touch row onto [`TimeAlignment`] without a known
    /// `window_start_elapsed_ns` origin (always degrades to
    /// [`TimeAlignment::Unanchored`]). Prefer
    /// [`Self::to_time_alignment_with_origin`] with
    /// [`LegacyTimebase::device_boottime_origin_ns`].
    pub fn to_time_alignment(&self, boot_id: &str, pc_ingest_cont_ns: i64) -> TimeAlignment {
        self.to_time_alignment_with_origin(boot_id, None, pc_ingest_cont_ns)
    }
}

/// Errors returned while parsing legacy schema 3 or schema 4 JSON / JSONL inputs.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum LegacyParseError {
    /// Failed to parse top-level JSON document.
    #[error("failed to parse legacy manifest JSON: {source}")]
    Json {
        /// Underlying serde JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// Failed to parse a specific 1-based line in a JSONL stream.
    #[error("failed to parse legacy JSONL line {line}: {source}")]
    JsonLine {
        /// 1-based line number in the JSONL input.
        line: usize,
        /// Underlying serde JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// Manifest declared a schema version other than the one requested by the parser.
    #[error("unexpected legacy schema version {actual}; expected {expected}")]
    UnexpectedSchema {
        /// Schema version expected by the called parser.
        expected: u32,
        /// Schema version found in the document.
        actual: u32,
    },
    /// Required manifest field was missing or empty.
    #[error("missing required legacy field '{field}'")]
    MissingField {
        /// Name of the missing field.
        field: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Flexible serde helpers for stringified 64-bit timestamps & sentinels
// ---------------------------------------------------------------------------

fn de_opt_i64_flexible<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptI64Visitor;

    impl<'de> Visitor<'de> for OptI64Visitor {
        type Value = Option<i64>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("null, an integer, or a decimal string representing i64")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D2: Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Self::Value, D2::Error> {
            deserializer.deserialize_any(self)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            i64::try_from(v)
                .map(Some)
                .map_err(|_| E::custom(format!("integer {v} exceeds i64::MAX")))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<i64>()
                .map(Some)
                .map_err(|err| E::custom(format!("invalid i64 string '{v}': {err}")))
        }
    }

    deserializer.deserialize_option(OptI64Visitor)
}

fn de_i64_flexible<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    de_opt_i64_flexible(deserializer)?
        .ok_or_else(|| de::Error::custom("expected integer or decimal string, got null"))
}

fn de_uncertainty_flexible<'de, D>(deserializer: D) -> Result<LegacyUncertainty, D::Error>
where
    D: Deserializer<'de>,
{
    match de_opt_i64_flexible(deserializer)? {
        None => Ok(LegacyUncertainty::Missing),
        Some(v) if v == LEGACY_UNCERTAINTY_SENTINEL_NS => Ok(LegacyUncertainty::UnknownSentinel),
        Some(v) => u64::try_from(v)
            .map(LegacyUncertainty::Known)
            .map_err(|_| de::Error::custom(format!("negative uncertainty value {v}"))),
    }
}

fn de_opt_string_or_int<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptStringOrIntVisitor;

    impl<'de> Visitor<'de> for OptStringOrIntVisitor {
        type Value = Option<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("null, string, or integer")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D2: Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Self::Value, D2::Error> {
            deserializer.deserialize_any(self)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.to_owned()))
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
    }

    deserializer.deserialize_option(OptStringOrIntVisitor)
}

const fn default_missing_uncertainty() -> LegacyUncertainty {
    LegacyUncertainty::Missing
}

// ---------------------------------------------------------------------------
// Internal raw deserialization DTOs (unknown fields are intentionally ignored)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawSchema4Manifest {
    schema: u32,
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    save_id: Option<String>,
    #[serde(default)]
    folder_name: Option<String>,
    #[serde(default)]
    capture_mode: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    replay_seconds: Option<u64>,
    #[serde(default)]
    files: Option<RawSchema4Files>,
    #[serde(default)]
    counts: Option<RawSchema4Counts>,
    #[serde(default)]
    save_request: Option<RawSaveRequest>,
    #[serde(default)]
    coverage: Option<BTreeMap<String, RawCoverageEntry>>,
    #[serde(default)]
    timebase: Option<RawSchema4Timebase>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    video_movie_duration_us: Option<i64>,
    #[serde(default)]
    parts: Vec<RawLegacyPart>,
    #[serde(default)]
    files_sha256: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
struct RawSchema4Files {
    #[serde(default)]
    readme: Option<String>,
    #[serde(default)]
    record: Option<String>,
    #[serde(default)]
    video: Option<String>,
    #[serde(default)]
    video_frames: Option<String>,
    #[serde(default)]
    video_source: Option<String>,
    #[serde(default)]
    logcat_device: Option<String>,
    #[serde(default)]
    logcat_app: Option<String>,
    #[serde(default)]
    touches: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSchema4Counts {
    #[serde(default)]
    device_log_rows: Option<u64>,
    #[serde(default)]
    app_log_rows: Option<u64>,
    #[serde(default)]
    video_frames: Option<u64>,
    #[serde(default)]
    touches: Option<u64>,
    #[serde(default)]
    touches_in_movie: Option<u64>,
}

#[derive(Deserialize, Default)]
struct RawSaveRequest {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    video_excluded: Option<bool>,
}

#[derive(Deserialize, Default)]
struct RawCoverageEntry {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSchema4Timebase {
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    known: bool,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    window_start_elapsed_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    window_duration_ns: Option<i64>,
    #[serde(
        default = "default_missing_uncertainty",
        deserialize_with = "de_uncertainty_flexible"
    )]
    window_end_uncertainty_ns: LegacyUncertainty,
}

#[derive(Deserialize)]
struct RawLegacyPart {
    file: String,
    segment_index: u64,
    #[serde(default)]
    generation: Option<u64>,
    #[serde(deserialize_with = "de_i64_flexible")]
    movie_start_us: i64,
    #[serde(deserialize_with = "de_i64_flexible")]
    movie_end_us: i64,
    #[serde(deserialize_with = "de_i64_flexible")]
    duration_us: i64,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    t_start_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    t_end_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    window_start_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    window_end_ns: Option<i64>,
    #[serde(default)]
    clock_alignment_known: bool,
    #[serde(default)]
    preroll_samples: u64,
}

#[derive(Deserialize)]
struct RawSchema3Session {
    schema: u32,
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    save_id: Option<String>,
    #[serde(default)]
    capture_mode: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    replay_seconds: Option<u64>,
    #[serde(default)]
    window_clock_known: bool,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    window_start_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    window_end_ns: Option<i64>,
    #[serde(
        default = "default_missing_uncertainty",
        deserialize_with = "de_uncertainty_flexible"
    )]
    window_end_uncertainty_ns: LegacyUncertainty,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    video_movie_duration_us: Option<i64>,
    #[serde(default)]
    parts: Vec<RawLegacyPart>,
    #[serde(default)]
    files_sha256: BTreeMap<String, String>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors immutable legacy schema 4 JSONL wire format"
)]
#[derive(Deserialize)]
struct RawSchema4FrameRow {
    part: String,
    segment_index: u64,
    sample_index: u64,
    #[serde(default)]
    generation: Option<u64>,
    #[serde(default)]
    source_key: bool,
    #[serde(default)]
    output_key: bool,
    #[serde(default)]
    output_width: Option<u32>,
    #[serde(default)]
    output_height: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    source_pts_us: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    media_pts_us: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    movie_pts_us: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    movie_end_us: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    elapsed_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    t_ns: Option<i64>,
    #[serde(
        default = "default_missing_uncertainty",
        deserialize_with = "de_uncertainty_flexible"
    )]
    t_uncertainty_ns: LegacyUncertainty,
    #[serde(default)]
    clock_alignment_known: bool,
    #[serde(default)]
    presented: bool,
    #[serde(default)]
    preroll: bool,
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    boot_segment: Option<String>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors immutable legacy schema 3 JSONL wire format"
)]
#[derive(Deserialize)]
struct RawSchema3FrameRow {
    part: String,
    segment_index: u64,
    sample_index: u64,
    #[serde(default)]
    generation: Option<u64>,
    #[serde(default)]
    source_key: bool,
    #[serde(default)]
    output_key: bool,
    #[serde(default)]
    output_width: Option<u32>,
    #[serde(default)]
    output_height: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    source_pts_us: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    media_pts_us: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    movie_pts_us: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    movie_end_us: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    elapsed_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    window_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    mapped_window_ns: Option<i64>,
    #[serde(
        default = "default_missing_uncertainty",
        deserialize_with = "de_uncertainty_flexible"
    )]
    uncertainty_ns: LegacyUncertainty,
    #[serde(default)]
    clock_alignment_known: bool,
    #[serde(default)]
    presented: bool,
    #[serde(default)]
    preroll: bool,
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    clock_epoch: Option<String>,
}

#[derive(Deserialize)]
struct RawLegacyLogRow {
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    record_id: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    boot_segment: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    clock_epoch: Option<String>,
    #[serde(default)]
    generation: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    epoch_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    elapsed_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    t_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    window_ns: Option<i64>,
    #[serde(
        default = "default_missing_uncertainty",
        deserialize_with = "de_uncertainty_flexible"
    )]
    t_uncertainty_ns: LegacyUncertainty,
    #[serde(
        default = "default_missing_uncertainty",
        deserialize_with = "de_uncertainty_flexible"
    )]
    uncertainty_ns: LegacyUncertainty,
    #[serde(default)]
    uid: Option<i32>,
    #[serde(default)]
    pid: Option<i32>,
    #[serde(default)]
    tid: Option<i32>,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    app_membership: Option<bool>,
}

#[derive(Deserialize)]
struct RawSchema4TouchRow {
    #[serde(default, deserialize_with = "de_opt_string_or_int")]
    boot_segment: Option<String>,
    #[serde(default)]
    generation: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    device_mono_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    elapsed_ns: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    t_ns: Option<i64>,
    #[serde(
        default = "default_missing_uncertainty",
        deserialize_with = "de_uncertainty_flexible"
    )]
    t_uncertainty_ns: LegacyUncertainty,
    #[serde(default, deserialize_with = "de_opt_i64_flexible")]
    movie_us: Option<i64>,
    action: String,
    #[serde(default)]
    pointer: Option<i32>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    screen_x: Option<f64>,
    #[serde(default)]
    screen_y: Option<f64>,
    #[serde(default)]
    source: Option<String>,
}

fn project_part(raw: RawLegacyPart) -> LegacyVideoPart {
    LegacyVideoPart {
        file: raw.file,
        segment_index: raw.segment_index,
        generation: raw.generation,
        movie_start_us: raw.movie_start_us,
        movie_end_us: raw.movie_end_us,
        duration_us: raw.duration_us,
        t_start_ns: raw.t_start_ns.or(raw.window_start_ns),
        t_end_ns: raw.t_end_ns.or(raw.window_end_ns),
        clock_alignment_known: raw.clock_alignment_known,
        preroll_samples: raw.preroll_samples,
    }
}

/// Parses a schema 4 `replay.json` manifest (including log-only saves) into [`LegacyBundleMetadata`].
///
/// Unknown optional JSON fields are tolerated.
///
/// # Errors
///
/// Returns [`LegacyParseError`] if JSON deserialization fails, `schema != 4`,
/// or `save_id` is missing/empty.
pub fn parse_schema4_manifest(json: &str) -> Result<LegacyBundleMetadata, LegacyParseError> {
    let raw: RawSchema4Manifest =
        serde_json::from_str(json).map_err(|source| LegacyParseError::Json { source })?;

    if raw.schema != 4 {
        return Err(LegacyParseError::UnexpectedSchema {
            expected: 4,
            actual: raw.schema,
        });
    }

    let save_id = raw
        .save_id
        .filter(|s| !s.trim().is_empty())
        .ok_or(LegacyParseError::MissingField { field: "save_id" })?;

    let raw_files = raw.files.unwrap_or_default();
    let files = LegacyBundleFiles {
        manifest: raw_files.record.unwrap_or_else(|| "replay.json".to_owned()),
        readme: raw_files.readme,
        video: raw_files.video,
        video_frames: raw_files.video_frames,
        video_source: raw_files.video_source,
        logcat_device: raw_files.logcat_device,
        logcat_app: raw_files.logcat_app,
        touches: raw_files.touches,
    };

    let raw_counts = raw.counts.unwrap_or_default();
    let counts = LegacyCounts {
        device_log_rows: raw_counts.device_log_rows,
        app_log_rows: raw_counts.app_log_rows,
        video_frames: raw_counts.video_frames,
        touches: raw_counts.touches,
        touches_in_movie: raw_counts.touches_in_movie,
    };

    let video_coverage_excluded = raw
        .coverage
        .as_ref()
        .and_then(|cov| cov.get("video"))
        .and_then(|entry| entry.state.as_deref())
        == Some("EXCLUDED");

    let request_logs_only = raw.save_request.as_ref().is_some_and(|req| {
        req.video_excluded == Some(true) || req.content.as_deref() == Some("LOGS_ONLY")
    });

    let capture_mode_logs_only = raw.capture_mode.as_deref() == Some("LOGS_ONLY");
    let video_excluded = video_coverage_excluded || request_logs_only || capture_mode_logs_only;

    let is_trim = raw.trigger.as_deref() == Some("trim")
        || raw
            .folder_name
            .as_deref()
            .is_some_and(|name| name.contains("-trim-"));

    let save_kind = if is_trim {
        LegacySaveKind::Trim
    } else if video_excluded || (files.video.is_none() && raw.parts.is_empty()) {
        LegacySaveKind::LogsOnly
    } else {
        LegacySaveKind::VideoAndLogs
    };

    let raw_tb = raw.timebase.unwrap_or_default();
    let timebase = LegacyTimebase {
        domain: raw_tb.domain,
        window_known: raw_tb.known,
        window_start_ns: raw_tb.window_start_elapsed_ns,
        window_end_ns: None,
        window_duration_ns: raw_tb.window_duration_ns,
        window_end_uncertainty: raw_tb.window_end_uncertainty_ns,
    };

    Ok(LegacyBundleMetadata {
        schema: LegacySchemaVersion::Schema4,
        complete: raw.complete,
        save_id,
        folder_name: raw.folder_name,
        capture_mode: raw.capture_mode,
        trigger: raw.trigger,
        save_kind,
        video_excluded,
        created_at: raw.created_at,
        replay_seconds: raw.replay_seconds,
        files,
        counts,
        timebase,
        video_movie_duration_us: raw.video_movie_duration_us,
        parts: raw.parts.into_iter().map(project_part).collect(),
        files_sha256: raw.files_sha256,
    })
}

/// Parses a schema 3 `session.json` (`v0.2.0`) manifest into [`LegacyBundleMetadata`].
///
/// Unknown optional JSON fields are tolerated.
///
/// # Errors
///
/// Returns [`LegacyParseError`] if JSON deserialization fails, `schema != 3`,
/// or `save_id` is missing/empty.
pub fn parse_schema3_session(json: &str) -> Result<LegacyBundleMetadata, LegacyParseError> {
    let raw: RawSchema3Session =
        serde_json::from_str(json).map_err(|source| LegacyParseError::Json { source })?;

    if raw.schema != 3 {
        return Err(LegacyParseError::UnexpectedSchema {
            expected: 3,
            actual: raw.schema,
        });
    }

    let save_id = raw
        .save_id
        .filter(|s| !s.trim().is_empty())
        .ok_or(LegacyParseError::MissingField { field: "save_id" })?;

    let parts: Vec<LegacyVideoPart> = raw.parts.into_iter().map(project_part).collect();
    let video_file = parts.first().map(|p| p.file.clone()).or_else(|| {
        raw.files_sha256
            .contains_key("video-001.mp4")
            .then(|| "video-001.mp4".to_owned())
    });
    let frames_file = if video_file.is_some() || raw.files_sha256.contains_key("frames.jsonl") {
        Some("frames.jsonl".to_owned())
    } else {
        None
    };
    let video_source = raw
        .files_sha256
        .contains_key("video-source.bin")
        .then(|| "video-source.bin".to_owned());

    let files = LegacyBundleFiles {
        manifest: "session.json".to_owned(),
        readme: Some("README.txt".to_owned()),
        video: video_file.clone(),
        video_frames: frames_file,
        video_source,
        logcat_device: Some("logcat-device.jsonl".to_owned()),
        logcat_app: Some("logcat-app.jsonl".to_owned()),
        touches: None,
    };

    let video_excluded = raw.capture_mode.as_deref() == Some("LOGS_ONLY");
    let save_kind = if video_excluded || (video_file.is_none() && parts.is_empty()) {
        LegacySaveKind::LogsOnly
    } else {
        LegacySaveKind::VideoAndLogs
    };

    let window_duration_ns = match (raw.window_start_ns, raw.window_end_ns) {
        (Some(start), Some(end)) => i64::try_from(i128::from(end) - i128::from(start)).ok(),
        _ => None,
    };

    let timebase = LegacyTimebase {
        domain: Some("sequence_ns".to_owned()),
        window_known: raw.window_clock_known,
        window_start_ns: raw.window_start_ns,
        window_end_ns: raw.window_end_ns,
        window_duration_ns,
        window_end_uncertainty: raw.window_end_uncertainty_ns,
    };

    Ok(LegacyBundleMetadata {
        schema: LegacySchemaVersion::Schema3,
        complete: raw.complete,
        save_id,
        folder_name: None,
        capture_mode: raw.capture_mode,
        trigger: None,
        save_kind,
        video_excluded,
        created_at: raw.created_at,
        replay_seconds: raw.replay_seconds,
        files,
        counts: LegacyCounts::default(),
        timebase,
        video_movie_duration_us: raw.video_movie_duration_us,
        parts,
        files_sha256: raw.files_sha256,
    })
}

/// Parses schema 4 `video-frames.jsonl` lines into [`LegacyFrameRow`] records.
///
/// # Errors
///
/// Returns [`LegacyParseError::JsonLine`] with the 1-based line number on invalid JSON.
pub fn parse_schema4_frames_jsonl(jsonl: &str) -> Result<Vec<LegacyFrameRow>, LegacyParseError> {
    let mut rows = Vec::new();
    for (idx, line) in jsonl.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw: RawSchema4FrameRow =
            serde_json::from_str(trimmed).map_err(|source| LegacyParseError::JsonLine {
                line: idx + 1,
                source,
            })?;
        rows.push(LegacyFrameRow {
            part: raw.part,
            segment_index: raw.segment_index,
            sample_index: raw.sample_index,
            generation: raw.generation,
            source_key: raw.source_key,
            output_key: raw.output_key,
            output_width: raw.output_width,
            output_height: raw.output_height,
            source_pts_us: raw.source_pts_us,
            media_pts_us: raw.media_pts_us,
            movie_pts_us: raw.movie_pts_us,
            movie_end_us: raw.movie_end_us,
            elapsed_ns: raw.elapsed_ns,
            t_ns: raw.t_ns,
            uncertainty: raw.t_uncertainty_ns,
            clock_alignment: if raw.clock_alignment_known {
                LegacyClockAlignment::Known
            } else {
                LegacyClockAlignment::Unknown
            },
            presentation: if raw.preroll {
                LegacyFramePresentation::Preroll
            } else if raw.presented {
                LegacyFramePresentation::Presented
            } else {
                LegacyFramePresentation::NotPresented
            },
            boot_segment: raw.boot_segment,
        });
    }
    Ok(rows)
}

/// Parses schema 3 `frames.jsonl` lines into [`LegacyFrameRow`] records.
///
/// # Errors
///
/// Returns [`LegacyParseError::JsonLine`] with the 1-based line number on invalid JSON.
pub fn parse_schema3_frames_jsonl(jsonl: &str) -> Result<Vec<LegacyFrameRow>, LegacyParseError> {
    let mut rows = Vec::new();
    for (idx, line) in jsonl.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw: RawSchema3FrameRow =
            serde_json::from_str(trimmed).map_err(|source| LegacyParseError::JsonLine {
                line: idx + 1,
                source,
            })?;
        rows.push(LegacyFrameRow {
            part: raw.part,
            segment_index: raw.segment_index,
            sample_index: raw.sample_index,
            generation: raw.generation,
            source_key: raw.source_key,
            output_key: raw.output_key,
            output_width: raw.output_width,
            output_height: raw.output_height,
            source_pts_us: raw.source_pts_us,
            media_pts_us: raw.media_pts_us,
            movie_pts_us: raw.movie_pts_us,
            movie_end_us: raw.movie_end_us,
            elapsed_ns: raw.elapsed_ns,
            t_ns: raw.window_ns.or(raw.mapped_window_ns),
            uncertainty: raw.uncertainty_ns,
            clock_alignment: if raw.clock_alignment_known {
                LegacyClockAlignment::Known
            } else {
                LegacyClockAlignment::Unknown
            },
            presentation: if raw.preroll {
                LegacyFramePresentation::Preroll
            } else if raw.presented {
                LegacyFramePresentation::Presented
            } else {
                LegacyFramePresentation::NotPresented
            },
            boot_segment: raw.clock_epoch,
        });
    }
    Ok(rows)
}

/// Parses schema 4 (or schema 3) `logcat-device.jsonl` / `logcat-app.jsonl` lines into [`LegacyLogRow`] records.
///
/// # Errors
///
/// Returns [`LegacyParseError::JsonLine`] with the 1-based line number on invalid JSON.
pub fn parse_schema4_logs_jsonl(jsonl: &str) -> Result<Vec<LegacyLogRow>, LegacyParseError> {
    let mut rows = Vec::new();
    for (idx, line) in jsonl.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw: RawLegacyLogRow =
            serde_json::from_str(trimmed).map_err(|source| LegacyParseError::JsonLine {
                line: idx + 1,
                source,
            })?;
        let uncertainty = match raw.t_uncertainty_ns {
            LegacyUncertainty::Missing => raw.uncertainty_ns,
            other => other,
        };
        rows.push(LegacyLogRow {
            record_id: raw.record_id,
            boot_segment: raw.boot_segment.or(raw.clock_epoch),
            generation: raw.generation,
            epoch_ns: raw.epoch_ns,
            elapsed_ns: raw.elapsed_ns,
            t_ns: raw.t_ns.or(raw.window_ns),
            uncertainty,
            uid: raw.uid,
            pid: raw.pid,
            tid: raw.tid,
            priority: raw.priority,
            tag: raw.tag,
            message: raw.message,
            app_membership: raw.app_membership,
        });
    }
    Ok(rows)
}

/// Parses schema 4 `touches.jsonl` lines into [`LegacyTouchRow`] records.
///
/// # Errors
///
/// Returns [`LegacyParseError::JsonLine`] with the 1-based line number on invalid JSON.
pub fn parse_schema4_touches_jsonl(jsonl: &str) -> Result<Vec<LegacyTouchRow>, LegacyParseError> {
    let mut rows = Vec::new();
    for (idx, line) in jsonl.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw: RawSchema4TouchRow =
            serde_json::from_str(trimmed).map_err(|source| LegacyParseError::JsonLine {
                line: idx + 1,
                source,
            })?;
        rows.push(LegacyTouchRow {
            boot_segment: raw.boot_segment,
            generation: raw.generation,
            device_mono_ns: raw.device_mono_ns,
            elapsed_ns: raw.elapsed_ns,
            t_ns: raw.t_ns,
            uncertainty: raw.t_uncertainty_ns,
            movie_us: raw.movie_us,
            action: raw.action,
            pointer: raw.pointer,
            x: raw.x,
            y: raw.y,
            screen_x: raw.screen_x,
            screen_y: raw.screen_y,
            source: raw.source,
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::{
        LegacyParseError, LegacySaveKind, LegacySchemaVersion, LegacyUncertainty,
        parse_schema3_frames_jsonl, parse_schema3_session, parse_schema4_logs_jsonl,
        parse_schema4_manifest, parse_schema4_touches_jsonl,
    };
    use crate::time::{TimeAlignment, UncertaintyReason};

    #[test]
    fn parses_synthetic_schema3_session_and_frames_with_unknown_fields_and_sentinel() {
        let session_json = r#"{
            "schema": 3,
            "complete": true,
            "save_id": "s3-001",
            "capture_mode": "VIDEO_AND_LOGS",
            "created_at": "2026-11-10T02:49:18Z",
            "replay_seconds": 60,
            "window_clock_known": true,
            "window_start_ns": "1000000000",
            "window_end_ns": "31000000000",
            "window_end_uncertainty_ns": "9223372036854775807",
            "video_movie_duration_us": "2000000",
            "future_unknown_field": { "nested": [1, 2, 3] },
            "parts": [
                {
                    "file": "video-001.mp4",
                    "segment_index": 0,
                    "generation": 1,
                    "movie_start_us": "0",
                    "movie_end_us": "2000000",
                    "duration_us": "2000000",
                    "window_start_ns": "0",
                    "window_end_ns": "2000000000",
                    "clock_alignment_known": true,
                    "preroll_samples": 0
                }
            ],
            "files_sha256": {
                "video-001.mp4": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "frames.jsonl": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            }
        }"#;

        let meta =
            parse_schema3_session(session_json).expect("schema 3 session.json must parse cleanly");
        assert_eq!(
            meta.schema,
            LegacySchemaVersion::Schema3,
            "schema version must be Schema3"
        );
        assert_eq!(
            meta.files.video.as_deref(),
            Some("video-001.mp4"),
            "schema 3 video filename must resolve to video-001.mp4"
        );
        assert_eq!(
            meta.files.video_frames.as_deref(),
            Some("frames.jsonl"),
            "schema 3 frames filename must resolve to frames.jsonl"
        );
        assert_eq!(
            meta.timebase.window_duration_ns,
            Some(30_000_000_000),
            "schema 3 window duration must be computed from end - start"
        );
        assert_eq!(
            meta.timebase.window_end_uncertainty,
            LegacyUncertainty::UnknownSentinel,
            "Long.MAX_VALUE must decode as UnknownSentinel"
        );

        let frames_jsonl = concat!(
            r#"{"part":"video-001.mp4","segment_index":0,"sample_index":0,"source_key":true,"output_key":true,"source_pts_us":"0","movie_pts_us":"0","movie_end_us":"100000","window_ns":"0","uncertainty_ns":"5000","clock_alignment_known":true,"presented":true,"preroll":false,"clock_epoch":1,"extra_field":"ignored"}"#,
            "\n",
            r#"{"part":"video-001.mp4","segment_index":0,"sample_index":1,"source_key":false,"output_key":false,"source_pts_us":"100000","movie_pts_us":"100000","movie_end_us":"200000","window_ns":null,"uncertainty_ns":"9223372036854775807","clock_alignment_known":false,"presented":true,"preroll":false,"clock_epoch":1}"#
        );

        let frames = parse_schema3_frames_jsonl(frames_jsonl)
            .expect("schema 3 frames.jsonl must parse cleanly");
        assert_eq!(frames.len(), 2, "must parse both schema 3 frame rows");
        assert_eq!(
            frames[0].uncertainty,
            LegacyUncertainty::Known(5_000),
            "known uncertainty must parse"
        );
        assert_eq!(
            frames[1].uncertainty,
            LegacyUncertainty::UnknownSentinel,
            "Long.MAX_VALUE frame uncertainty must parse as UnknownSentinel"
        );
        assert!(
            matches!(
                frames[1].to_time_alignment("boot-legacy", 1234),
                TimeAlignment::Unanchored {
                    reason: UncertaintyReason::LegacySentinelUnknown,
                    ..
                }
            ),
            "sentinel frame must project to Unanchored"
        );
    }

    #[test]
    fn parses_schema4_log_only_manifest_cleanly() {
        let log_only_json = r#"{
            "schema": 4,
            "capture_mode": "VIDEO_AND_LOGS",
            "complete": true,
            "save_id": "logs-only-01",
            "folder_name": "replay-20261122-120000-manual-logs-deadbeef",
            "trigger": "manual",
            "files": {
                "readme": "README.md",
                "record": "replay.json",
                "video": null,
                "video_frames": null,
                "video_source": null,
                "logcat_device": "logcat-device.jsonl",
                "logcat_app": "logcat-app.jsonl",
                "touches": null
            },
            "counts": {
                "device_log_rows": 42,
                "app_log_rows": 7,
                "video_frames": 0,
                "touches": null,
                "touches_in_movie": null
            },
            "save_request": {
                "content": "LOGS_ONLY",
                "origin": "CLI",
                "video_excluded": true
            },
            "coverage": {
                "video": { "state": "EXCLUDED", "requested": false, "excluded": true }
            },
            "timebase": {
                "domain": "device_boottime",
                "known": true,
                "window_duration_ns": "30000000000",
                "window_end_uncertainty_ns": "0"
            },
            "parts": [],
            "files_sha256": {
                "README.md": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "logcat-device.jsonl": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "logcat-app.jsonl": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
            },
            "unknown_future_extension": true
        }"#;

        let meta = parse_schema4_manifest(log_only_json)
            .expect("log-only schema 4 manifest must parse cleanly");
        assert_eq!(
            meta.save_kind,
            LegacySaveKind::LogsOnly,
            "log-only save must be classified as LogsOnly"
        );
        assert!(meta.video_excluded, "video_excluded must be true");
        assert!(
            meta.files.video.is_none(),
            "log-only save has no video file"
        );
        assert!(meta.parts.is_empty(), "log-only save has no video parts");
        assert_eq!(
            meta.counts.device_log_rows,
            Some(42),
            "device log count must match"
        );
    }

    #[test]
    fn rejects_wrong_schema_and_handles_sentinel_logs_and_touches() {
        let wrong = r#"{"schema": 3, "save_id": "x"}"#;
        assert!(
            matches!(
                parse_schema4_manifest(wrong),
                Err(LegacyParseError::UnexpectedSchema {
                    expected: 4,
                    actual: 3
                })
            ),
            "schema 4 parser must reject schema 3 input"
        );

        let logs = parse_schema4_logs_jsonl(
            r#"{"record_id":"9","t_ns":null,"t_uncertainty_ns":"9223372036854775807","message":"unmapped","unknown_key":1}"#,
        )
        .expect("unmapped log row must parse");
        assert_eq!(logs.len(), 1, "one log row expected");
        assert_eq!(
            logs[0].uncertainty,
            LegacyUncertainty::UnknownSentinel,
            "sentinel log uncertainty must decode cleanly"
        );
        assert!(
            matches!(
                logs[0].to_time_alignment("boot-1", 500),
                TimeAlignment::Unanchored {
                    reason: UncertaintyReason::LegacySentinelUnknown,
                    ..
                }
            ),
            "sentinel log row must project to Unanchored"
        );

        let touches = parse_schema4_touches_jsonl(
            r#"{"t_ns":null,"t_uncertainty_ns":null,"movie_us":null,"action":"UP","x":10,"y":20,"extra":"ok"}"#,
        )
        .expect("null-timestamp touch row must parse");
        assert_eq!(touches.len(), 1, "one touch row expected");
        assert_eq!(
            touches[0].uncertainty,
            LegacyUncertainty::Missing,
            "null touch uncertainty must decode as Missing"
        );
    }

    #[test]
    fn regression_schema4_rebases_t_ns_onto_origin_and_missing_origin_degrades_to_unanchored() {
        let manifest_with_origin = r#"{
            "schema": 4,
            "complete": true,
            "save_id": "s4-origin",
            "timebase": {
                "domain": "device_boottime",
                "known": true,
                "window_start_elapsed_ns": "500000000000",
                "window_duration_ns": "30000000000",
                "window_end_uncertainty_ns": "0"
            }
        }"#;
        let meta = parse_schema4_manifest(manifest_with_origin)
            .expect("schema 4 manifest with origin must parse");
        let origin = meta.timebase.device_boottime_origin_ns();
        assert_eq!(
            origin,
            Some(500_000_000_000),
            "device_boottime_origin_ns must return window_start_elapsed_ns"
        );

        let frames = super::parse_schema4_frames_jsonl(
            r#"{"part":"video.mp4","segment_index":0,"sample_index":0,"t_ns":"1250000000","t_uncertainty_ns":"1000","clock_alignment_known":true,"presented":true}"#,
        )
        .expect("schema 4 frame must parse");
        assert_eq!(
            frames[0].to_time_alignment_with_origin("boot-4", origin, 999),
            TimeAlignment::Anchored {
                boot_id: "boot-4".into(),
                boottime_ns: 501_250_000_000,
                half_width_ns: 1_000,
                semantics: crate::time::TimestampSemantics::VideoEncoderInputEstimate,
            },
            "frame t_ns must be rebased onto window_start_elapsed_ns"
        );
        // Without origin, never place window-relative t_ns into DeviceBoottime.
        assert_eq!(
            frames[0].to_time_alignment_with_origin("boot-4", None, 999),
            TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 999,
                reason: UncertaintyReason::LegacySentinelUnknown,
            },
            "missing origin must degrade frame to Unanchored"
        );
        assert_eq!(
            frames[0].to_time_alignment("boot-4", 999),
            TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 999,
                reason: UncertaintyReason::LegacySentinelUnknown,
            },
            "to_time_alignment without origin must degrade to Unanchored"
        );
    }

    #[test]
    fn regression_schema4_logs_and_touches_rebase_onto_origin() {
        let origin = Some(500_000_000_000_i64);

        let logs = parse_schema4_logs_jsonl(
            r#"{"t_ns":"2500000000","t_uncertainty_ns":"5000","message":"hi"}"#,
        )
        .expect("schema 4 log must parse");
        assert_eq!(
            logs[0].to_time_alignment_with_origin("boot-4", origin, 888),
            TimeAlignment::Anchored {
                boot_id: "boot-4".into(),
                boottime_ns: 502_500_000_000,
                half_width_ns: 5_000,
                semantics: crate::time::TimestampSemantics::LogcatHeaderWall,
            },
            "log t_ns must be rebased onto window_start_elapsed_ns"
        );
        assert_eq!(
            logs[0].to_time_alignment_with_origin("boot-4", None, 888),
            TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 888,
                reason: UncertaintyReason::LegacySentinelUnknown,
            },
            "missing origin must degrade log to Unanchored"
        );

        let touches = parse_schema4_touches_jsonl(
            r#"{"t_ns":"750000000","t_uncertainty_ns":"2000","action":"DOWN"}"#,
        )
        .expect("schema 4 touch must parse");
        assert_eq!(
            touches[0].to_time_alignment_with_origin("boot-4", origin, 777),
            TimeAlignment::Anchored {
                boot_id: "boot-4".into(),
                boottime_ns: 500_750_000_000,
                half_width_ns: 2_000,
                semantics: crate::time::TimestampSemantics::InputEventTime,
            },
            "touch t_ns must be rebased onto window_start_elapsed_ns"
        );
        assert_eq!(
            touches[0].to_time_alignment_with_origin("boot-4", Some(i64::MAX), 777),
            TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 777,
                reason: UncertaintyReason::LegacySentinelUnknown,
            },
            "checked_add overflow must degrade to Unanchored"
        );

        // Schema 3 uses `sequence_ns`, so `device_boottime_origin_ns()` must return None.
        let s3_meta = parse_schema3_session(
            r#"{"schema":3,"save_id":"s3-seq","window_clock_known":true,"window_start_ns":"1000","window_end_ns":"5000"}"#,
        )
        .expect("schema 3 session must parse");
        assert_eq!(
            s3_meta.timebase.device_boottime_origin_ns(),
            None,
            "schema 3 sequence_ns domain must not be treated as a DeviceBoottime origin"
        );
    }
}
