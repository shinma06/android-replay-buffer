//! Zone 2: Immutable WORM v2 bundle writer/reader and legacy schema 3/4 read-only adapter.
//!
//! - [`V2BundleWriter`] stages files inside `.staging-<job-id>/` via
//!   [`arb_os::fs::AtomicStagingDir`], builds a rollback-journal (`DELETE` mode)
//!   read-only-ready `timeline.sqlite`, computes SHA-256 digests, validates
//!   `manifest.json` via [`ManifestV2::validate`], and commits atomically.
//! - [`V2BundleReader`] opens committed v2 bundles strictly read-only
//!   (`SQLITE_OPEN_READ_ONLY`) with zero write locks for concurrent multi-reader access.
//! - [`LegacyBundleReader`] auto-detects and reads Schema 4 (`replay.json`,
//!   including log-only saves) and Schema 3 (`session.json`, `video-001.mp4`,
//!   `frames.jsonl`) strictly read-only without ever writing to or locking the folder.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use arb_core::BUNDLE_SCHEMA_VERSION;
use arb_core::analytics::{
    CrashMarker, ScreenSpan, extract_app_logs_for_packages, extract_crash_markers,
    extract_screen_spans,
};
use arb_core::bundle::{
    AppLogSummary, BundleKind, DeviceInfo, EpochSpan, FileDigest, FrameRecord, InputStreamMeta,
    LogCompleteness, LogRecord, LogcatStreamMeta, ManifestV2, Provenance, RecordBucket,
    RequestedWindow, StreamStatus, StreamsSummary, TapRecord, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::gesture::{GestureKind, TouchPoint};
use arb_core::legacy::{
    LegacyBundleMetadata, LegacySaveKind, LegacySchemaVersion, parse_schema3_frames_jsonl,
    parse_schema3_session, parse_schema4_frames_jsonl, parse_schema4_logs_jsonl,
    parse_schema4_manifest, parse_schema4_touches_jsonl,
};
use arb_core::time::{
    DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind, PcSyncProbe, StreamKind,
    TimeAlignment, TimestampSemantics, UncertaintyReason,
};
use arb_os::OsError;
use arb_os::fs::{AtomicStagingDir, fsync_file};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StorageError;
use crate::ring::{hex_lower, validate_simple_id};

/// File name of the v2 bundle manifest.
pub const MANIFEST_V2_FILE: &str = "manifest.json";
/// File name of the v2 read-only `SQLite` timeline index.
pub const TIMELINE_DB_FILE: &str = "timeline.sqlite";
/// File name of the v2 video payload.
pub const VIDEO_MP4_FILE: &str = "video.mp4";
/// File name of the optional v2 audio payload.
pub const AUDIO_M4A_FILE: &str = "audio.m4a";
/// File name of the human-readable main logcat text file written whenever main logs exist.
pub const LOGCAT_LOG_FILE: &str = "logcat.log";
/// File name of the human-readable unanchored logcat text file written when unanchored logs exist.
pub const UNANCHORED_LOG_FILE: &str = "unanchored.log";
/// File name of the compact human/script-readable timeline events summary sidecar.
pub const EVENTS_JSONL_FILE: &str = "events.jsonl";
/// File name of the ready-to-share MP4 (without tap burn-in) inside a trimmed bundle.
pub const SHARE_MP4_FILE: &str = "share.mp4";
/// File name of the ready-to-share MP4 (with tap/swipe burn-in) inside a trimmed bundle.
pub const SHARE_TAPS_MP4_FILE: &str = "share-taps.mp4";
/// File name of the main-timeline JSONL log sidecar written for `logs_only` bundles.
pub const LOGS_JSONL_FILE: &str = "logs.jsonl";
/// File name of the quarantined `unanchored` JSONL log sidecar written for `logs_only` bundles.
pub const UNANCHORED_JSONL_FILE: &str = "unanchored.jsonl";

const LEGACY_SCHEMA4_MANIFEST: &str = "replay.json";
const LEGACY_SCHEMA3_MANIFEST: &str = "session.json";
const DEFAULT_LEGACY_BOOT_ID: &str = "legacy-boot";

const TIMELINE_DDL: &str = "
CREATE TABLE frames (
  frame_index INTEGER PRIMARY KEY,
  boot_id TEXT NOT NULL,
  pts_device_mono_ns INTEGER NOT NULL,
  aligned_boottime_ns INTEGER,
  half_width_ns INTEGER,
  certainty TEXT NOT NULL,
  is_keyframe INTEGER NOT NULL,
  gop_id INTEGER NOT NULL,
  mp4_sample_offset INTEGER NOT NULL,
  mp4_sample_size INTEGER NOT NULL,
  pc_ingest_cont_ns INTEGER NOT NULL DEFAULT 0,
  min_boottime_ns INTEGER,
  max_boottime_ns INTEGER,
  semantics TEXT,
  uncertainty_reason TEXT
);

CREATE TABLE discontinuities (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  stream TEXT,
  boot_id TEXT,
  start_boottime_ns INTEGER,
  end_boottime_ns INTEGER,
  pc_cont_start_ns INTEGER NOT NULL,
  pc_cont_end_ns INTEGER,
  detail_json TEXT
);

CREATE TABLE logs (
  seq INTEGER PRIMARY KEY,
  boot_id TEXT,
  header_epoch_ns INTEGER,
  device_read_boottime_ns INTEGER,
  pc_ingest_cont_ns INTEGER NOT NULL,
  aligned_boottime_ns INTEGER,
  min_boottime_ns INTEGER,
  max_boottime_ns INTEGER,
  certainty TEXT NOT NULL,
  bucket TEXT NOT NULL,
  level TEXT,
  tag TEXT,
  pid INTEGER,
  tid INTEGER,
  uid INTEGER,
  package_name TEXT,
  message TEXT NOT NULL,
  half_width_ns INTEGER,
  semantics TEXT,
  uncertainty_reason TEXT
);

CREATE TABLE taps (
  seq INTEGER PRIMARY KEY,
  boot_id TEXT,
  event_mono_ns INTEGER,
  pc_ingest_cont_ns INTEGER NOT NULL,
  aligned_boottime_ns INTEGER,
  certainty TEXT NOT NULL,
  bucket TEXT NOT NULL,
  action TEXT NOT NULL,
  x REAL,
  y REAL,
  pointer_id INTEGER,
  half_width_ns INTEGER,
  min_boottime_ns INTEGER,
  max_boottime_ns INTEGER,
  semantics TEXT,
  uncertainty_reason TEXT,
  gesture_kind TEXT,
  end_x REAL,
  end_y REAL,
  end_t_offset_ns INTEGER,
  path_json TEXT
);

CREATE TABLE clock_anchors (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  boot_id TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE TABLE crashes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  boottime_ns INTEGER,
  frame_index_hint INTEGER,
  process_name TEXT NOT NULL,
  package_name TEXT NOT NULL,
  pid INTEGER,
  exception_class TEXT NOT NULL,
  message TEXT NOT NULL,
  source_log_seq INTEGER NOT NULL
);

CREATE INDEX idx_frames_boottime_ns ON frames(aligned_boottime_ns);
CREATE INDEX idx_frames_gop_id ON frames(gop_id);
CREATE INDEX idx_discontinuities_boottime_ns ON discontinuities(start_boottime_ns);
CREATE INDEX idx_logs_bucket_boottime_ns ON logs(bucket, aligned_boottime_ns);
CREATE INDEX idx_logs_pc_ingest_cont_ns ON logs(pc_ingest_cont_ns);
CREATE INDEX idx_taps_bucket_boottime_ns ON taps(bucket, aligned_boottime_ns);
CREATE INDEX idx_clock_anchors_boot_id ON clock_anchors(boot_id);
CREATE INDEX idx_crashes_boottime_ns ON crashes(boottime_ns);
";

/// Inclusive nanosecond filter range for log and tap queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeRange {
    /// Optional inclusive lower bound in nanoseconds.
    pub start_ns: Option<i64>,
    /// Optional inclusive upper bound in nanoseconds.
    pub end_ns: Option<i64>,
}

impl TimeRange {
    /// Returns an unbounded range matching all timestamps.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            start_ns: None,
            end_ns: None,
        }
    }

    /// Returns a closed range `[start_ns, end_ns]`.
    #[must_use]
    pub const fn between(start_ns: i64, end_ns: i64) -> Self {
        Self {
            start_ns: Some(start_ns),
            end_ns: Some(end_ns),
        }
    }

    fn matches(&self, timestamp_ns: Option<i64>) -> bool {
        if self.start_ns.is_none() && self.end_ns.is_none() {
            return true;
        }
        let Some(ts) = timestamp_ns else {
            return false;
        };
        if let Some(start) = self.start_ns
            && ts < start
        {
            return false;
        }
        if let Some(end) = self.end_ns
            && ts > end
        {
            return false;
        }
        true
    }
}

/// On-disk bundle format generation detected by [`open_bundle`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleFormat {
    /// Immutable WORM v2 bundle (`manifest.json` + `timeline.sqlite`).
    V2,
    /// Legacy `develop` schema 4 folder (`replay.json`).
    LegacySchema4,
    /// Legacy `main` / `v0.2.0` schema 3 folder (`session.json`).
    LegacySchema3,
}

/// Unified read-only manifest summary exposed by both [`V2BundleReader`] and [`LegacyBundleReader`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleManifestSummary {
    /// Bundle identifier (`bundle_id` for v2, `save_id` for legacy).
    pub bundle_id: String,
    /// Detected storage format.
    pub format: BundleFormat,
    /// High-level bundle payload kind (`Full` or `LogsOnly`).
    pub bundle_kind: BundleKind,
    /// Creation timestamp string in UTC, when recorded.
    pub created_at_utc: Option<String>,
    /// Provenance (original capture or trim).
    pub provenance: Provenance,
    /// Device metadata when recorded in the manifest.
    pub device: Option<DeviceInfo>,
    /// Per-stream presence summary.
    pub streams: StreamsSummary,
    /// Summary of quarantined `unanchored` records.
    pub unanchored_summary: UnanchoredSummary,
    /// Covered device boot epochs.
    pub epochs: Vec<EpochSpan>,
    /// Per-app extracted foreground logcat summaries (`app-logs/<package>.jsonl`).
    pub app_logs: Vec<AppLogSummary>,
    /// Full v2 manifest when `format == BundleFormat::V2`.
    pub manifest_v2: Option<ManifestV2>,
    /// Full legacy metadata when `format` is schema 3 or 4.
    pub legacy_metadata: Option<LegacyBundleMetadata>,
}

/// Counts and anchored time bounds for a saved log timeline.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogStatistics {
    /// Total rows, including quarantine.
    pub count: usize,
    /// Quarantined rows.
    pub unanchored: usize,
    /// Longest anchored span within a single boot, in milliseconds.
    pub duration_ms: u64,
}

/// Unified read-only bundle reader interface implemented for v2 and legacy bundles.
pub trait BundleReader: fmt::Debug + Send + Sync {
    /// Digest of the log input captured by an immutable in-memory legacy reader.
    fn legacy_log_digest(&self) -> Option<&str> {
        None
    }

    /// Directory backing a live SQLite timeline, when available for cache validation.
    fn timeline_bundle_dir(&self) -> Option<&Path> {
        None
    }

    /// Returns the unified manifest summary for this bundle.
    fn manifest_summary(&self) -> &BundleManifestSummary;

    /// Returns all video frame index records ordered by `frame_index`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if reading or decoding frame rows fails.
    fn frames(&self) -> Result<Vec<FrameRecord>, StorageError>;

    /// [`Self::frames`] as readers map time: a carried-in first frame (#564) is placed at its
    /// display start `W0`, the media time origin, through
    /// [`arb_core::bundle::frame_display_start_ns`]. The stored rows are not modified.
    ///
    /// # Errors
    ///
    /// As [`Self::frames`].
    fn display_frames(&self) -> Result<Vec<FrameRecord>, StorageError> {
        let carried = self
            .manifest_summary()
            .streams
            .video
            .as_ref()
            .and_then(|v| v.carried_in.as_ref());
        Ok(arb_core::bundle::display_aligned_frames(
            self.frames()?,
            carried,
        ))
    }

    /// Returns all recorded timeline discontinuities ordered chronologically.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if reading or decoding discontinuity rows fails.
    fn discontinuities(&self) -> Result<Vec<DiscontinuityEvent>, StorageError>;

    /// Counts rows and measures the longest confirmed log span within a boot.
    ///
    /// # Errors
    /// Returns an error when the timeline cannot be read.
    fn log_statistics(&self) -> Result<LogStatistics, StorageError> {
        let logs = self.query_logs(None, TimeRange::all(), None)?;
        let mut spans = std::collections::BTreeMap::new();
        let mut stats = LogStatistics {
            count: logs.len(),
            ..LogStatistics::default()
        };
        for log in &logs {
            if log.bucket == RecordBucket::Unanchored {
                stats.unanchored += 1;
            } else if let TimeAlignment::Anchored {
                boot_id,
                boottime_ns,
                ..
            } = &log.alignment
            {
                let span = spans.entry(boot_id).or_insert((*boottime_ns, *boottime_ns));
                span.0 = span.0.min(*boottime_ns);
                span.1 = span.1.max(*boottime_ns);
                stats.duration_ms = stats.duration_ms.max(span.1.abs_diff(span.0) / 1_000_000);
            }
        }
        Ok(stats)
    }

    /// Counts rows matching the same bucket and bounds as a paged log query.
    ///
    /// # Errors
    /// Returns an error when the timeline cannot be read.
    fn count_logs(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
    ) -> Result<usize, StorageError> {
        self.query_logs(bucket, range, None).map(|rows| rows.len())
    }

    /// Queries logcat rows filtered by optional `bucket`, [`TimeRange`], and optional row `limit`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if reading or decoding log rows fails.
    fn query_logs(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        limit: Option<usize>,
    ) -> Result<Vec<LogRecord>, StorageError> {
        self.query_logs_after(bucket, range, None, limit)
    }

    /// Queries rows after an exclusive sequence cursor with the same time and bucket filters.
    ///
    /// # Errors
    ///
    /// Returns an error if the filtered rows cannot be read or decoded.
    fn query_logs_after(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogRecord>, StorageError>;

    /// Derives framework input requests without modifying the saved bundle.
    ///
    /// # Errors
    /// Returns storage errors encountered reading the source logs.
    fn visit_injected_commands(
        &self,
        visit: &mut dyn FnMut(arb_core::injected_input::InjectedCommandExtraction),
    ) -> Result<(), StorageError> {
        use arb_core::injected_input::extract_injected_commands;
        let api = self.manifest_summary().device.as_ref().map(|d| d.api_level);
        let mut cursor = None;
        loop {
            let rows = self.query_logs_after(None, TimeRange::all(), cursor, Some(1024))?;
            visit(extract_injected_commands(&rows, api));
            if rows.len() < 1024 {
                break;
            }
            cursor = rows.last().map(|r| r.seq);
        }
        Ok(())
    }

    /// Queries input / tap rows filtered by optional `bucket`, [`TimeRange`], and optional row `limit`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if reading or decoding tap rows fails.
    fn query_taps(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        limit: Option<usize>,
    ) -> Result<Vec<TapRecord>, StorageError>;

    /// Returns the path to `video.mp4` (or `video-001.mp4`) if a video file exists.
    fn video_path(&self) -> Option<PathBuf>;

    /// Returns the path to `audio.m4a` if a standalone audio track exists.
    fn audio_path(&self) -> Option<PathBuf>;

    /// Returns `true` for legacy schema 3 / schema 4 read-only bundles, `false` for v2.
    fn is_legacy(&self) -> bool;

    /// Returns structured crash markers detected in this bundle's logcat.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if reading persisted crash markers or log rows fails.
    fn crashes(&self) -> Result<Vec<CrashMarker>, StorageError>;

    /// Returns PC-derived Activity / screen transition spans from this bundle's logcat.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if reading log or frame rows fails.
    fn screens(&self) -> Result<Vec<ScreenSpan>, StorageError>;
}

/// Media payload input accepted by [`V2BundleWriter`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPayload {
    /// Raw encoded bytes in memory.
    Bytes(Vec<u8>),
    /// Existing file path to stream-copy into the staging directory.
    File(PathBuf),
}

/// Ready-to-share single-file MP4 staged atomically inside a bundle directory
/// (typically [`SHARE_MP4_FILE`] or [`SHARE_TAPS_MP4_FILE`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareMp4Sidecar {
    /// Normalized bundle-relative filename (e.g., `"share.mp4"` or `"share-taps.mp4"`).
    pub file_name: String,
    /// Encoded MP4 bytes or temporary file path to stream-copy into staging.
    pub payload: MediaPayload,
}

/// Clock anchor row stored in `timeline.sqlite`'s `clock_anchors` table.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum ClockAnchorRecord {
    /// Device 3-clock triple (`CLOCK_BOOTTIME`, `CLOCK_MONOTONIC`, `CLOCK_REALTIME`).
    DeviceTriple(DeviceClockTriple),
    /// Host-to-device round-trip synchronization probe.
    PcSyncProbe(PcSyncProbe),
}

/// Complete input draft materialized atomically by [`V2BundleWriter`].
#[derive(Debug, Clone)]
pub struct V2BundleDraft {
    /// User note stored alongside immutable save metadata.
    pub memo: Option<String>,
    /// High-level bundle payload category (`Full` or `LogsOnly`).
    pub bundle_kind: BundleKind,
    /// Globally unique bundle identifier.
    pub bundle_id: String,
    /// Creation timestamp in ISO-8601 UTC format.
    pub created_at_utc: String,
    /// Origin (original save or trim).
    pub provenance: Provenance,
    /// Target device metadata.
    pub device: DeviceInfo,
    /// Requested save or trim window.
    pub requested_window: RequestedWindow,
    /// Covered device boot epochs.
    pub epochs: Vec<EpochSpan>,
    /// Per-stream presence and honesty metadata.
    pub streams: StreamsSummary,
    /// Summary of quarantined `unanchored` records.
    pub unanchored_summary: UnanchoredSummary,
    /// Optional `video.mp4` payload.
    pub video_mp4: Option<MediaPayload>,
    /// Optional `audio.m4a` payload.
    pub audio_m4a: Option<MediaPayload>,
    /// Optional ready-to-share MP4 (`share.mp4` or `share-taps.mp4`) placed inside the bundle.
    pub share_mp4: Option<ShareMp4Sidecar>,
    /// Video frame index rows.
    pub frames: Vec<FrameRecord>,
    /// Discontinuity events.
    pub discontinuities: Vec<DiscontinuityEvent>,
    /// Logcat rows (both `Main` and `Unanchored` buckets).
    pub logs: Vec<LogRecord>,
    /// Input / tap rows (both `Main` and `Unanchored` buckets).
    pub taps: Vec<TapRecord>,
    /// Clock anchor observations (`device_triple` and `pc_sync_probe`).
    pub clock_anchors: Vec<ClockAnchorRecord>,
}

/// Atomic WORM v2 bundle writer backed by [`AtomicStagingDir`].
#[derive(Debug, Clone, Copy, Default)]
pub struct V2BundleWriter;

impl V2BundleWriter {
    /// Creates a new [`V2BundleWriter`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Writes a complete WORM v2 bundle into `.staging-<job_id>/`, computes
    /// SHA-256 digests, validates `manifest.json`, and commits atomically to
    /// `final_dir`.
    ///
    /// On any error before commit succeeds, the staging directory is cleaned up
    /// automatically and `final_dir` is never left partially written. If the
    /// atomic rename succeeds on disk and only the post-commit parent directory
    /// `fsync` fails, `Err(StorageError::Os(OsError::CommittedParentSyncFailed { .. }))`
    /// is returned while preserving the published WORM bundle untouched.
    ///
    /// Idempotent retry (R2 S14): a stale `.staging-<job_id>` sibling left by a crash is
    /// removed first, and when `final_dir` already exists and holds a committed bundle with
    /// the same `bundle_id` that passes [`V2BundleReader::verify_integrity`] (a crash after
    /// the commit rename but before the catalog was updated), its manifest is returned
    /// instead of failing forever with `AlreadyExists`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on staging creation failure, I/O error, `SQLite`
    /// write error, manifest validation error, or atomic commit failure (including
    /// `OsError::CommittedParentSyncFailed` when the bundle is already published).
    /// `OsError::AlreadyExists` is returned only when `final_dir` holds something other than
    /// an intact bundle with `draft.bundle_id`.
    pub fn write(
        final_dir: impl AsRef<Path>,
        job_id: &str,
        draft: &V2BundleDraft,
    ) -> Result<ManifestV2, StorageError> {
        Self::write_with_source_apps(final_dir, job_id, draft, &[])
    }

    /// Writes a trimmed bundle, preserving the source's app attribution across the cut.
    ///
    /// # Errors
    ///
    /// Returns the same staging, integrity, and commit errors as [`Self::write`].
    #[expect(
        clippy::too_many_lines,
        reason = "atomic staging writes media, text/JSONL sidecars, per-app logs, SQLite, and manifest in one transaction"
    )]
    pub fn write_with_source_apps(
        final_dir: impl AsRef<Path>,
        job_id: &str,
        draft: &V2BundleDraft,
        source_apps: &[AppLogSummary],
    ) -> Result<ManifestV2, StorageError> {
        let final_dir = final_dir.as_ref();
        validate_simple_id("job_id", job_id)?;
        remove_stale_staging_sibling(final_dir, job_id)?;
        let staging = match AtomicStagingDir::create(final_dir, job_id) {
            Ok(staging) => staging,
            Err(OsError::AlreadyExists { path }) => {
                return existing_committed_manifest(final_dir, &draft.bundle_id)
                    .ok_or(StorageError::Os(OsError::AlreadyExists { path }));
            }
            Err(err) => return Err(err.into()),
        };
        staging.write_file_synced(STAGING_OWNER, job_id.as_bytes())?;
        arb_os::fs::fsync_dir(staging.staging_path())?;
        let mut files = Vec::new();

        let mut video_bytes = None;
        if let Some(video) = &draft.video_mp4 {
            let digest = stage_media_file(&staging, VIDEO_MP4_FILE, video)?;
            video_bytes = Some(digest.bytes);
            files.push(digest);
        }
        check_draft_video_commit(draft, video_bytes)?;

        if let Some(audio) = &draft.audio_m4a {
            let digest = stage_media_file(&staging, AUDIO_M4A_FILE, audio)?;
            files.push(digest);
        }

        if let Some(share) = &draft.share_mp4 {
            validate_share_sidecar_name(&share.file_name)?;
            let digest = stage_media_file(&staging, &share.file_name, &share.payload)?;
            files.push(digest);
        }

        let clock = trim_clock(draft);
        if draft.bundle_kind == BundleKind::LogsOnly || clock.is_some() {
            let logs_digest = stage_logs_jsonl(
                &staging,
                LOGS_JSONL_FILE,
                &draft.logs,
                RecordBucket::Main,
                clock.as_ref(),
            )?;
            files.push(logs_digest);
            let unanchored_digest = stage_logs_jsonl(
                &staging,
                UNANCHORED_JSONL_FILE,
                &draft.logs,
                RecordBucket::Unanchored,
                None,
            )?;
            files.push(unanchored_digest);
        }

        // Human-readable logcat text sidecars for direct open/grep/attachment without SQLite.
        let main_logs: Vec<&LogRecord> = draft
            .logs
            .iter()
            .filter(|l| l.bucket == RecordBucket::Main)
            .collect();
        files.push(stage_logs_text(
            &staging,
            LOGCAT_LOG_FILE,
            &main_logs,
            LogTextFormat::Standard,
        )?);
        let unanchored_logs: Vec<&LogRecord> = draft
            .logs
            .iter()
            .filter(|l| l.bucket == RecordBucket::Unanchored)
            .collect();
        if !unanchored_logs.is_empty() {
            let digest = stage_logs_text(
                &staging,
                UNANCHORED_LOG_FILE,
                &unanchored_logs,
                LogTextFormat::WithUnanchoredReason,
            )?;
            files.push(digest);
        }

        // Write per-app foreground / crashed extracted logcat text sidecars (`app-logs/<pkg>.log`).
        let extracted_apps = extract_app_logs_for_packages(
            &draft.logs,
            &draft.frames,
            source_apps
                .iter()
                .map(|app| app.package_name.clone())
                .collect(),
        );
        let mut app_logs = Vec::with_capacity(extracted_apps.len());
        for extracted in &extracted_apps {
            let refs: Vec<&LogRecord> = extracted.logs.iter().collect();
            let digest = stage_logs_text(
                &staging,
                &extracted.relative_path,
                &refs,
                LogTextFormat::Standard,
            )?;
            let line_count = u64::try_from(extracted.logs.len()).unwrap_or(u64::MAX);
            app_logs.push(AppLogSummary {
                package_name: extracted.package_name.clone(),
                relative_path: extracted.relative_path.clone(),
                line_count,
                crash_count: extracted.crash_count,
                sha256: digest.sha256.clone(),
            });
            files.push(digest);
        }
        if !app_logs.is_empty() {
            arb_os::fs::fsync_dir(&staging.staging_path().join("app-logs"))?;
        }

        // Write the chronological event summary, including an empty file for a quiet interval.
        files.push(stage_events_jsonl(&staging, draft, clock.as_ref())?);

        let timeline_path = staging.staging_path().join(TIMELINE_DB_FILE);
        write_timeline_sqlite(&timeline_path, draft)?;
        let timeline_digest = hash_and_sync_file(&timeline_path, TIMELINE_DB_FILE)?;
        files.push(timeline_digest);

        files.sort_by(|a, b| a.path.cmp(&b.path));

        let manifest = ManifestV2 {
            memo: draft.memo.clone(),
            schema_version: BUNDLE_SCHEMA_VERSION,
            bundle_kind: draft.bundle_kind,
            bundle_id: draft.bundle_id.clone(),
            created_at_utc: draft.created_at_utc.clone(),
            provenance: draft.provenance.clone(),
            device: draft.device.clone(),
            requested_window: draft.requested_window.clone(),
            epochs: draft.epochs.clone(),
            streams: draft.streams.clone(),
            unanchored_summary: draft.unanchored_summary,
            app_logs,
            files,
        };
        manifest.validate()?;

        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|source| StorageError::json("serialize_manifest_v2", source))?;
        staging.write_file_synced(MANIFEST_V2_FILE, &manifest_bytes)?;
        let marker = staging.staging_path().join(STAGING_OWNER);
        fs::remove_file(&marker)
            .map_err(|e| StorageError::io("remove_staging_owner", &marker, e))?;

        staging.commit()?;
        Ok(manifest)
    }
}

/// Refuses to commit a bundle whose video declaration disagrees with `video.mp4` or the
/// `frames` rows (K15): a declared track needs a non-empty file and exactly
/// `streams.video.frame_count` rows; an `Absent` track has neither.
fn check_draft_video_commit(
    draft: &V2BundleDraft,
    video_bytes: Option<u64>,
) -> Result<(), StorageError> {
    let video = draft.streams.video.as_ref();
    let rows = u64::try_from(draft.frames.len()).unwrap_or(u64::MAX);
    arb_core::video_commit::check_video_commit(
        video.map(|v| v.status),
        video.map_or(0, |v| v.frame_count),
        rows,
        video_bytes,
    )
    .map_err(|violation| StorageError::IntegrityMismatch {
        path: VIDEO_MP4_FILE.to_owned(),
        reason: format!(
            "refusing to commit bundle {}: video declaration is inconsistent ({violation:?}; \
             status {:?}, frame_count {}, frames rows {rows}, video.mp4 bytes {video_bytes:?})",
            draft.bundle_id,
            video.map(|v| v.status),
            video.map_or(0, |v| v.frame_count),
        ),
    })
}

const STAGING_OWNER: &str = ".arb-staging-owner";

/// Removes a partial staging directory only if it belongs to this job.
/// A completed or unidentifiable directory is preserved for manual inspection.
///
/// # Errors
/// Returns an error if the directory cannot be inspected or removed.
pub fn remove_stale_staging_sibling(final_dir: &Path, job_id: &str) -> Result<bool, StorageError> {
    validate_simple_id("job_id", job_id)?;
    let Some(parent) = final_dir.parent() else {
        return Ok(false);
    };
    let stage = parent.join(format!(".staging-{job_id}"));
    let meta = match fs::symlink_metadata(&stage) {
        Ok(meta) => meta,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(StorageError::io("stat_staging_dir", &stage, e)),
    };
    let marker_path = stage.join(STAGING_OWNER);
    let owned = meta.is_dir()
        && fs::symlink_metadata(&marker_path)
            .is_ok_and(|m| m.is_file() && Some(m.len()) == u64::try_from(job_id.len()).ok())
        && fs::read(&marker_path).is_ok_and(|v| v == job_id.as_bytes());
    let completed = [
        arb_os::fs::COMMIT_MARKER_FILE,
        MANIFEST_V2_FILE,
        "replay.json",
        "session.json",
    ]
    .iter()
    .any(|name| match fs::symlink_metadata(stage.join(name)) {
        Ok(_) => true,
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    });
    if !owned || completed {
        let _ = writeln!(
            io::stderr(),
            "所有情報が不明または完成済みのため保持しました ({})",
            stage.display()
        );
        return Ok(false);
    }
    fs::remove_dir_all(&stage).map_err(|e| StorageError::io("remove_staging_dir", &stage, e))?;
    Ok(true)
}

/// Returns the manifest of an already committed, intact bundle with `bundle_id` at
/// `final_dir`, if that is what is there.
pub fn existing_committed_manifest(final_dir: &Path, bundle_id: &str) -> Option<ManifestV2> {
    let reader = V2BundleReader::open(final_dir).ok()?;
    if reader.manifest().bundle_id != bundle_id {
        return None;
    }
    reader.verify_integrity().ok()?;
    Some(reader.manifest().clone())
}

/// Strictly read-only reader for committed WORM v2 bundles.
///
/// Opens `timeline.sqlite` exclusively with `SQLITE_OPEN_READ_ONLY` and
/// `PRAGMA query_only = ON`, enabling concurrent multi-reader access with zero
/// write locks and zero sidecar file creation.
#[derive(Debug, Clone)]
pub struct V2BundleReader {
    root_dir: PathBuf,
    manifest: ManifestV2,
    summary: BundleManifestSummary,
}

impl V2BundleReader {
    /// Opens a committed v2 bundle directory strictly read-only and validates `manifest.json`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidBundle`] if `manifest.json` or `timeline.sqlite`
    /// is missing, [`StorageError::ManifestValidation`] if the manifest violates schema
    /// invariants, or [`StorageError::Sqlite`] if `timeline.sqlite` cannot be opened read-only.
    pub fn open(bundle_dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let root_dir = fs::canonicalize(bundle_dir.as_ref())
            .map_err(|e| StorageError::io("canonicalize_bundle", bundle_dir.as_ref(), e))?;
        let manifest_path = confined_path(&root_dir, MANIFEST_V2_FILE)?;
        if !manifest_path.is_file() {
            return Err(StorageError::InvalidBundle {
                path: root_dir,
                reason: "missing manifest.json".to_owned(),
            });
        }
        let manifest_raw = fs::read_to_string(&manifest_path)
            .map_err(|source| StorageError::io("read_manifest_v2", &manifest_path, source))?;
        let manifest: ManifestV2 = serde_json::from_str(&manifest_raw)
            .map_err(|source| StorageError::json("parse_manifest_v2", source))?;
        manifest.validate()?;

        for name in manifest.files.iter().map(|f| f.path.as_str()).chain([
            VIDEO_MP4_FILE,
            AUDIO_M4A_FILE,
            "app-logs",
        ]) {
            confined_path(&root_dir, name)?;
        }
        let timeline_path = confined_path(&root_dir, TIMELINE_DB_FILE)?;
        if !timeline_path.is_file() {
            return Err(StorageError::InvalidBundle {
                path: root_dir,
                reason: "missing timeline.sqlite".to_owned(),
            });
        }

        // Probe read-only open immediately so corrupt/unreadable DBs fail fast.
        let _probe_conn = open_readonly_sqlite(&timeline_path)?;

        let summary = BundleManifestSummary {
            bundle_id: manifest.bundle_id.clone(),
            format: BundleFormat::V2,
            bundle_kind: manifest.bundle_kind,
            created_at_utc: Some(manifest.created_at_utc.clone()),
            provenance: manifest.provenance.clone(),
            device: Some(manifest.device.clone()),
            streams: manifest.streams.clone(),
            unanchored_summary: manifest.unanchored_summary,
            epochs: manifest.epochs.clone(),
            app_logs: manifest.app_logs.clone(),
            manifest_v2: Some(manifest.clone()),
            legacy_metadata: None,
        };

        Ok(Self {
            root_dir,
            manifest,
            summary,
        })
    }

    /// Returns the bundle root directory path.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Returns the validated [`ManifestV2`].
    #[must_use]
    pub const fn manifest(&self) -> &ManifestV2 {
        &self.manifest
    }

    /// Returns `true` when `rel_path` is covered by `manifest.files` (and thus by
    /// [`Self::verify_integrity`]).
    fn manifest_lists(&self, rel_path: &str) -> bool {
        self.manifest.files.iter().any(|f| f.path == rel_path)
    }

    /// Verifies that every file listed in `manifest.files` exists, matches its
    /// declared byte length, and matches its lowercase 64-hex SHA-256 digest, and that the
    /// manifest covers every file the reader serves (R2 S15): `timeline.sqlite` always,
    /// `video.mp4` when `streams.video.status == Present`, and `audio.m4a` when
    /// `streams.audio.status == Present`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::IntegrityMismatch`] if a required file is not listed, or any
    /// listed file is missing, resized, or corrupted, or [`StorageError::Io`] on unreadable
    /// files.
    pub fn verify_integrity(&self) -> Result<(), StorageError> {
        let streams = &self.manifest.streams;
        let mut required = vec![TIMELINE_DB_FILE];
        if streams
            .video
            .as_ref()
            .is_some_and(|v| v.status == StreamStatus::Present)
        {
            required.push(VIDEO_MP4_FILE);
        }
        if streams
            .audio
            .as_ref()
            .is_some_and(|a| a.status == StreamStatus::Present)
        {
            required.push(AUDIO_M4A_FILE);
        }
        if self.manifest.bundle_kind == BundleKind::LogsOnly {
            required.push(LOGS_JSONL_FILE);
            required.push(UNANCHORED_JSONL_FILE);
        }
        for rel in required {
            if !self.manifest_lists(rel) {
                return Err(StorageError::IntegrityMismatch {
                    path: rel.to_owned(),
                    reason: "required bundle file is not listed in manifest.files".to_owned(),
                });
            }
        }

        for entry in &self.manifest.files {
            let file_path = confined_path(&self.root_dir, &entry.path)?;
            if !file_path.is_file() {
                return Err(StorageError::IntegrityMismatch {
                    path: entry.path.clone(),
                    reason: "file listed in manifest is missing on disk".to_owned(),
                });
            }
            let (actual_bytes, actual_sha256) = stream_hash_file(&file_path)?;
            if actual_bytes != entry.bytes {
                return Err(StorageError::IntegrityMismatch {
                    path: entry.path.clone(),
                    reason: format!(
                        "byte length mismatch: expected {}, got {actual_bytes}",
                        entry.bytes
                    ),
                });
            }
            if actual_sha256 != entry.sha256 {
                return Err(StorageError::IntegrityMismatch {
                    path: entry.path.clone(),
                    reason: format!(
                        "sha256 mismatch: expected {}, got {actual_sha256}",
                        entry.sha256
                    ),
                });
            }
        }
        Ok(())
    }

    /// Reads all recorded [`ClockAnchorRecord`] entries from `timeline.sqlite`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`], [`StorageError::Json`], or [`StorageError::CorruptData`].
    pub fn clock_anchors(&self) -> Result<Vec<ClockAnchorRecord>, StorageError> {
        let conn = self.open_timeline_ro()?;
        let mut stmt = conn
            .prepare("SELECT kind, payload_json FROM clock_anchors ORDER BY id ASC")
            .map_err(|source| StorageError::sqlite("prepare_clock_anchors", source))?;

        let rows = stmt
            .query_map([], |row| {
                let kind: String = row.get(0)?;
                let payload_json: String = row.get(1)?;
                Ok((kind, payload_json))
            })
            .map_err(|source| StorageError::sqlite("query_clock_anchors", source))?;

        let mut anchors = Vec::new();
        for row in rows {
            let (kind, payload_json) =
                row.map_err(|source| StorageError::sqlite("read_clock_anchor_row", source))?;
            match kind.as_str() {
                "device_triple" => {
                    let triple: DeviceClockTriple = serde_json::from_str(&payload_json)
                        .map_err(|source| StorageError::json("parse_device_triple", source))?;
                    anchors.push(ClockAnchorRecord::DeviceTriple(triple));
                }
                "pc_sync_probe" => {
                    let probe: PcSyncProbe = serde_json::from_str(&payload_json)
                        .map_err(|source| StorageError::json("parse_pc_sync_probe", source))?;
                    anchors.push(ClockAnchorRecord::PcSyncProbe(probe));
                }
                other => {
                    return Err(StorageError::CorruptData {
                        reason: format!("unknown clock_anchor kind `{other}`"),
                    });
                }
            }
        }
        Ok(anchors)
    }

    fn open_timeline_ro(&self) -> Result<Connection, StorageError> {
        open_readonly_sqlite(&confined_path(&self.root_dir, TIMELINE_DB_FILE)?)
    }
}

impl BundleReader for V2BundleReader {
    fn count_logs(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
    ) -> Result<usize, StorageError> {
        let conn = self.open_timeline_ro()?;
        if bucket.is_none() && range == TimeRange::all() {
            return conn
                .query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))
                .map_err(|err| StorageError::sqlite("count_all_logs", err));
        }
        conn.query_row(
            "SELECT COUNT(*) FROM logs
             WHERE (?1 IS NULL OR bucket = ?1)
               AND (?2 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) >= ?2)
               AND (?3 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) <= ?3)",
            params![
                bucket.map(record_bucket_to_str),
                range.start_ns,
                range.end_ns
            ],
            |r| r.get(0),
        )
        .map_err(|err| StorageError::sqlite("count_logs", err))
    }

    fn log_statistics(&self) -> Result<LogStatistics, StorageError> {
        let conn = self.open_timeline_ro()?;
        let mut stmt = conn
            .prepare(
                "SELECT boot_id, COUNT(*), COUNT(CASE WHEN bucket='unanchored' THEN 1 END),
            MIN(CASE WHEN bucket='main' AND certainty='anchored' THEN aligned_boottime_ns END),
            MAX(CASE WHEN bucket='main' AND certainty='anchored' THEN aligned_boottime_ns END)
            FROM logs GROUP BY boot_id",
            )
            .map_err(|e| StorageError::sqlite("prepare_log_statistics", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, usize>(1)?,
                    r.get::<_, usize>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })
            .map_err(|e| StorageError::sqlite("log_statistics", e))?;
        let mut stats = LogStatistics::default();
        for row in rows {
            let (count, unanchored, lo, hi) =
                row.map_err(|e| StorageError::sqlite("log_statistics_row", e))?;
            stats.count += count;
            stats.unanchored += unanchored;
            if let Some((lo, hi)) = lo.zip(hi) {
                stats.duration_ms = stats.duration_ms.max(hi.abs_diff(lo) / 1_000_000);
            }
        }
        Ok(stats)
    }

    fn timeline_bundle_dir(&self) -> Option<&Path> {
        Some(&self.root_dir)
    }

    fn manifest_summary(&self) -> &BundleManifestSummary {
        &self.summary
    }

    fn frames(&self) -> Result<Vec<FrameRecord>, StorageError> {
        let conn = self.open_timeline_ro()?;
        let mut stmt = conn
            .prepare(
                "SELECT frame_index, boot_id, pts_device_mono_ns, aligned_boottime_ns,
                        half_width_ns, certainty, is_keyframe, gop_id,
                        mp4_sample_offset, mp4_sample_size, pc_ingest_cont_ns,
                        min_boottime_ns, max_boottime_ns, semantics, uncertainty_reason
                 FROM frames
                 ORDER BY frame_index ASC",
            )
            .map_err(|source| StorageError::sqlite("prepare_frames", source))?;

        let raw_rows = stmt
            .query_map([], |row| {
                Ok(RawFrameSqlRow {
                    frame_index: row.get(0)?,
                    boot_id: row.get(1)?,
                    pts_device_mono_ns: row.get(2)?,
                    aligned_boottime_ns: row.get(3)?,
                    half_width_ns: row.get(4)?,
                    certainty: row.get(5)?,
                    is_keyframe: row.get(6)?,
                    gop_id: row.get(7)?,
                    mp4_sample_offset: row.get(8)?,
                    mp4_sample_size: row.get(9)?,
                    pc_ingest_cont_ns: row.get(10)?,
                    min_boottime_ns: row.get(11)?,
                    max_boottime_ns: row.get(12)?,
                    semantics: row.get(13)?,
                    uncertainty_reason: row.get(14)?,
                })
            })
            .map_err(|source| StorageError::sqlite("query_frames", source))?;

        let mut frames = Vec::new();
        for row in raw_rows {
            let raw = row.map_err(|source| StorageError::sqlite("read_frame_row", source))?;
            frames.push(decode_frame_row(raw)?);
        }
        Ok(frames)
    }

    fn discontinuities(&self) -> Result<Vec<DiscontinuityEvent>, StorageError> {
        let conn = self.open_timeline_ro()?;
        let mut stmt = conn
            .prepare(
                "SELECT kind, stream, boot_id, start_boottime_ns, end_boottime_ns,
                        pc_cont_start_ns, pc_cont_end_ns, detail_json
                 FROM discontinuities
                 ORDER BY id ASC",
            )
            .map_err(|source| StorageError::sqlite("prepare_discontinuities", source))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(RawDiscontinuitySqlRow {
                    kind: row.get(0)?,
                    stream: row.get(1)?,
                    boot_id: row.get(2)?,
                    start_boottime_ns: row.get(3)?,
                    end_boottime_ns: row.get(4)?,
                    pc_cont_start_ns: row.get(5)?,
                    pc_cont_end_ns: row.get(6)?,
                    detail_json: row.get(7)?,
                })
            })
            .map_err(|source| StorageError::sqlite("query_discontinuities", source))?;

        let mut events = Vec::new();
        for row in rows {
            let raw =
                row.map_err(|source| StorageError::sqlite("read_discontinuity_row", source))?;
            events.push(decode_discontinuity_row(raw)?);
        }
        Ok(events)
    }

    fn query_logs_after(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogRecord>, StorageError> {
        let conn = self.open_timeline_ro()?;
        let bucket_str = bucket.map(record_bucket_to_str);
        let limit_i64 = match limit {
            Some(n) => i64::try_from(n).unwrap_or(i64::MAX),
            None => -1,
        };

        let has_pkg_cols = sqlite_column_exists(&conn, "logs", "uid")?
            && sqlite_column_exists(&conn, "logs", "package_name")?;
        let sql = if has_pkg_cols {
            "SELECT seq, boot_id, header_epoch_ns, device_read_boottime_ns,
                    pc_ingest_cont_ns, aligned_boottime_ns, min_boottime_ns,
                    max_boottime_ns, certainty, bucket, level, tag, pid, tid,
                    message, half_width_ns, semantics, uncertainty_reason,
                    uid, package_name
             FROM logs
             WHERE (?1 IS NULL OR bucket = ?1)
               AND (?2 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) >= ?2)
               AND (?3 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) <= ?3)
               AND seq > ?5
             ORDER BY seq ASC
             LIMIT ?4"
        } else {
            "SELECT seq, boot_id, header_epoch_ns, device_read_boottime_ns,
                    pc_ingest_cont_ns, aligned_boottime_ns, min_boottime_ns,
                    max_boottime_ns, certainty, bucket, level, tag, pid, tid,
                    message, half_width_ns, semantics, uncertainty_reason,
                    NULL, NULL
             FROM logs
             WHERE (?1 IS NULL OR bucket = ?1)
               AND (?2 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) >= ?2)
               AND (?3 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) <= ?3)
               AND seq > ?5
             ORDER BY seq ASC
             LIMIT ?4"
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|source| StorageError::sqlite("prepare_query_logs", source))?;

        let rows = stmt
            .query_map(
                params![
                    bucket_str,
                    range.start_ns,
                    range.end_ns,
                    limit_i64,
                    after_seq.map_or(-1, |seq| i64::try_from(seq).unwrap_or(i64::MAX))
                ],
                |row| {
                    Ok(RawLogSqlRow {
                        seq: row.get(0)?,
                        boot_id: row.get(1)?,
                        header_epoch_ns: row.get(2)?,
                        device_read_boottime_ns: row.get(3)?,
                        pc_ingest_cont_ns: row.get(4)?,
                        aligned_boottime_ns: row.get(5)?,
                        min_boottime_ns: row.get(6)?,
                        max_boottime_ns: row.get(7)?,
                        certainty: row.get(8)?,
                        bucket: row.get(9)?,
                        level: row.get(10)?,
                        tag: row.get(11)?,
                        pid: row.get(12)?,
                        tid: row.get(13)?,
                        message: row.get(14)?,
                        half_width_ns: row.get(15)?,
                        semantics: row.get(16)?,
                        uncertainty_reason: row.get(17)?,
                        uid: row.get(18)?,
                        package_name: row.get(19)?,
                    })
                },
            )
            .map_err(|source| StorageError::sqlite("query_logs", source))?;

        let mut records = Vec::new();
        for row in rows {
            let raw = row.map_err(|source| StorageError::sqlite("read_log_row", source))?;
            records.push(decode_log_row(raw)?);
        }
        Ok(records)
    }

    fn query_taps(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        limit: Option<usize>,
    ) -> Result<Vec<TapRecord>, StorageError> {
        let conn = self.open_timeline_ro()?;
        let bucket_str = bucket.map(record_bucket_to_str);
        let limit_i64 = match limit {
            Some(n) => i64::try_from(n).unwrap_or(i64::MAX),
            None => -1,
        };

        let tap_cols = sqlite_table_columns(&conn, "taps")?;
        let has_gesture_cols = [
            "gesture_kind",
            "end_x",
            "end_y",
            "end_t_offset_ns",
            "path_json",
        ]
        .iter()
        .all(|col| tap_cols.contains(*col));
        let sql = if has_gesture_cols {
            "SELECT seq, boot_id, event_mono_ns, pc_ingest_cont_ns,
                    aligned_boottime_ns, certainty, bucket, action, x, y,
                    pointer_id, half_width_ns, min_boottime_ns, max_boottime_ns,
                    semantics, uncertainty_reason,
                    gesture_kind, end_x, end_y, end_t_offset_ns, path_json
             FROM taps
             WHERE (?1 IS NULL OR bucket = ?1)
               AND (?2 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) >= ?2)
               AND (?3 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) <= ?3)
             ORDER BY seq ASC
             LIMIT ?4"
        } else {
            "SELECT seq, boot_id, event_mono_ns, pc_ingest_cont_ns,
                    aligned_boottime_ns, certainty, bucket, action, x, y,
                    pointer_id, half_width_ns, min_boottime_ns, max_boottime_ns,
                    semantics, uncertainty_reason,
                    NULL, NULL, NULL, NULL, NULL
             FROM taps
             WHERE (?1 IS NULL OR bucket = ?1)
               AND (?2 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) >= ?2)
               AND (?3 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) <= ?3)
             ORDER BY seq ASC
             LIMIT ?4"
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|source| StorageError::sqlite("prepare_query_taps", source))?;

        let rows = stmt
            .query_map(
                params![bucket_str, range.start_ns, range.end_ns, limit_i64],
                |row| {
                    Ok(RawTapSqlRow {
                        seq: row.get(0)?,
                        boot_id: row.get(1)?,
                        event_mono_ns: row.get(2)?,
                        pc_ingest_cont_ns: row.get(3)?,
                        aligned_boottime_ns: row.get(4)?,
                        certainty: row.get(5)?,
                        bucket: row.get(6)?,
                        action: row.get(7)?,
                        x: row.get(8)?,
                        y: row.get(9)?,
                        pointer_id: row.get(10)?,
                        half_width_ns: row.get(11)?,
                        min_boottime_ns: row.get(12)?,
                        max_boottime_ns: row.get(13)?,
                        semantics: row.get(14)?,
                        uncertainty_reason: row.get(15)?,
                        gesture_kind: row.get(16)?,
                        end_x: row.get(17)?,
                        end_y: row.get(18)?,
                        end_t_offset_ns: row.get(19)?,
                        path_json: row.get(20)?,
                    })
                },
            )
            .map_err(|source| StorageError::sqlite("query_taps", source))?;

        let mut records = Vec::new();
        for row in rows {
            let raw = row.map_err(|source| StorageError::sqlite("read_tap_row", source))?;
            let mut tap = decode_tap_row(raw)?;
            if let Some(video) = &self.summary.streams.video
                && let (Some(w), Some(h)) = (video.display_width, video.display_height)
                && w > 0
                && h > 0
            {
                // Earlier v2 recordings could store out-of-range evdev values as pixels.
                // Bound their read-only display projection for Studio, crop and burn-in together.
                let bounds = (f64::from(w - 1), f64::from(h - 1));
                for x in [&mut tap.x, &mut tap.end_x].into_iter().flatten() {
                    *x = x.clamp(0.0, bounds.0);
                }
                for y in [&mut tap.y, &mut tap.end_y].into_iter().flatten() {
                    *y = y.clamp(0.0, bounds.1);
                }
                for point in &mut tap.path {
                    point.x = point.x.clamp(0.0, bounds.0);
                    point.y = point.y.clamp(0.0, bounds.1);
                }
            }
            records.push(tap);
        }
        Ok(records)
    }

    fn video_path(&self) -> Option<PathBuf> {
        // Never serve a media file the manifest (and thus integrity checks) does not cover.
        if !self.manifest_lists(VIDEO_MP4_FILE) {
            return None;
        }
        let path = confined_path(&self.root_dir, VIDEO_MP4_FILE).ok()?;
        path.is_file().then_some(path)
    }

    fn audio_path(&self) -> Option<PathBuf> {
        if !self.manifest_lists(AUDIO_M4A_FILE) {
            return None;
        }
        let path = confined_path(&self.root_dir, AUDIO_M4A_FILE).ok()?;
        path.is_file().then_some(path)
    }

    fn is_legacy(&self) -> bool {
        false
    }

    fn crashes(&self) -> Result<Vec<CrashMarker>, StorageError> {
        let conn = self.open_timeline_ro()?;
        if sqlite_table_exists(&conn, "crashes")? {
            return read_persisted_crashes(&conn);
        }
        let logs = self.query_logs(None, TimeRange::all(), None)?;
        let frames = self.frames()?;
        Ok(extract_crash_markers(&logs, &frames))
    }

    fn screens(&self) -> Result<Vec<ScreenSpan>, StorageError> {
        let logs = self.query_logs(None, TimeRange::all(), None)?;
        let frames = self.frames()?;
        Ok(extract_screen_spans(&logs, &frames))
    }
}

/// Strictly read-only adapter for legacy Schema 4 (`replay.json`, including log-only)
/// and Schema 3 (`session.json`, `video-001.mp4`, `frames.jsonl`) folders.
///
/// Never writes, locks, or modifies anything inside the legacy directory.
#[derive(Debug, Clone)]
pub struct LegacyBundleReader {
    root_dir: PathBuf,
    metadata: LegacyBundleMetadata,
    summary: BundleManifestSummary,
    frames: Vec<FrameRecord>,
    discontinuities: Vec<DiscontinuityEvent>,
    logs: Vec<LogRecord>,
    log_digest: String,
    app_logs: Vec<LogRecord>,
    taps: Vec<TapRecord>,
}

impl LegacyBundleReader {
    /// Auto-detects and opens a legacy Schema 4 (`replay.json`) or Schema 3
    /// (`session.json`) bundle directory strictly read-only.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidBundle`] if neither `replay.json` nor
    /// `session.json` is present, or [`StorageError::LegacyParse`] / [`StorageError::Io`]
    /// if the legacy files cannot be read or parsed.
    pub fn open(bundle_dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let root_dir = bundle_dir.as_ref().to_path_buf();
        let schema4_path = root_dir.join(LEGACY_SCHEMA4_MANIFEST);
        let schema3_path = root_dir.join(LEGACY_SCHEMA3_MANIFEST);

        let metadata = if schema4_path.is_file() {
            let text = fs::read_to_string(&schema4_path)
                .map_err(|source| StorageError::io("read_legacy_schema4", &schema4_path, source))?;
            parse_schema4_manifest(&text)?
        } else if schema3_path.is_file() {
            let text = fs::read_to_string(&schema3_path)
                .map_err(|source| StorageError::io("read_legacy_schema3", &schema3_path, source))?;
            parse_schema3_session(&text)?
        } else {
            return Err(StorageError::InvalidBundle {
                path: root_dir,
                reason: "neither replay.json (schema 4) nor session.json (schema 3) found"
                    .to_owned(),
            });
        };

        validate_legacy_manifest_paths(&root_dir, &metadata)?;

        let origin_ns = metadata.timebase.device_boottime_origin_ns();
        let frames = load_legacy_frames(&root_dir, &metadata, origin_ns)?;
        let (device_logs, device_digest) = load_legacy_logs(
            &root_dir,
            metadata.files.logcat_device.as_deref(),
            origin_ns,
        )?;
        let (app_logs, app_digest) =
            load_legacy_logs(&root_dir, metadata.files.logcat_app.as_deref(), origin_ns)?;
        let has_device_log_file = metadata
            .files
            .logcat_device
            .as_deref()
            .and_then(|rel| legacy_file_path(&root_dir, rel).ok())
            .is_some_and(|path| path.is_file());
        let (logs, log_digest) = if !device_logs.is_empty() || has_device_log_file {
            (device_logs, device_digest)
        } else {
            (app_logs.clone(), app_digest)
        };
        let taps = load_legacy_taps(&root_dir, metadata.files.touches.as_deref(), origin_ns)?;
        let discontinuities = derive_legacy_discontinuities(&metadata);

        let unanchored_count = count_unanchored(&frames, &logs, &taps);
        let summary = build_legacy_summary(&metadata, &frames, &logs, &taps, unanchored_count);

        Ok(Self {
            root_dir,
            metadata,
            summary,
            frames,
            discontinuities,
            logs,
            log_digest,
            app_logs,
            taps,
        })
    }

    /// Returns the legacy bundle root directory path.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Returns the parsed [`LegacyBundleMetadata`].
    #[must_use]
    pub const fn legacy_metadata(&self) -> &LegacyBundleMetadata {
        &self.metadata
    }

    /// Returns the projected app-filtered logcat rows (`logcat-app.jsonl`), if present.
    #[must_use]
    pub fn app_logs(&self) -> &[LogRecord] {
        &self.app_logs
    }

    /// Verifies any `files_sha256` entries recorded in the legacy manifest, strictly read-only.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::IntegrityMismatch`] if a listed file is missing or its
    /// SHA-256 digest differs, or [`StorageError::Io`] on unreadable files.
    pub fn verify_integrity(&self) -> Result<(), StorageError> {
        for (rel_path, expected_sha) in &self.metadata.files_sha256 {
            let file_path = legacy_file_path(&self.root_dir, rel_path)?;
            if !file_path.is_file() {
                return Err(StorageError::IntegrityMismatch {
                    path: rel_path.clone(),
                    reason: "legacy file listed in files_sha256 is missing".to_owned(),
                });
            }
            let (_bytes, actual_sha) = stream_hash_file(&file_path)?;
            if actual_sha != expected_sha.to_ascii_lowercase() {
                return Err(StorageError::IntegrityMismatch {
                    path: rel_path.clone(),
                    reason: format!("sha256 mismatch: expected {expected_sha}, got {actual_sha}"),
                });
            }
        }
        Ok(())
    }
}

impl BundleReader for LegacyBundleReader {
    fn count_logs(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
    ) -> Result<usize, StorageError> {
        if bucket.is_none() && range == TimeRange::all() {
            return Ok(self.logs.len());
        }
        Ok(self
            .logs
            .iter()
            .filter(|row| {
                bucket.is_none_or(|b| b == row.bucket)
                    && range.matches(alignment_query_timestamp(&row.alignment))
            })
            .count())
    }

    fn legacy_log_digest(&self) -> Option<&str> {
        Some(&self.log_digest)
    }

    fn manifest_summary(&self) -> &BundleManifestSummary {
        &self.summary
    }

    fn frames(&self) -> Result<Vec<FrameRecord>, StorageError> {
        Ok(self.frames.clone())
    }

    fn discontinuities(&self) -> Result<Vec<DiscontinuityEvent>, StorageError> {
        Ok(self.discontinuities.clone())
    }

    fn query_logs_after(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<LogRecord>, StorageError> {
        let mut results = Vec::new();
        let start = after_seq.map_or(0, |seq| self.logs.partition_point(|row| row.seq <= seq));
        for record in &self.logs[start..] {
            if let Some(wanted) = bucket
                && record.bucket != wanted
            {
                continue;
            }
            if !range.matches(alignment_query_timestamp(&record.alignment)) {
                continue;
            }
            results.push(record.clone());
            if let Some(max) = limit
                && results.len() >= max
            {
                break;
            }
        }
        Ok(results)
    }

    fn query_taps(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        limit: Option<usize>,
    ) -> Result<Vec<TapRecord>, StorageError> {
        let mut results = Vec::new();
        for record in &self.taps {
            if let Some(wanted) = bucket
                && record.bucket != wanted
            {
                continue;
            }
            if !range.matches(alignment_query_timestamp(&record.alignment)) {
                continue;
            }
            results.push(record.clone());
            if let Some(max) = limit
                && results.len() >= max
            {
                break;
            }
        }
        Ok(results)
    }

    fn video_path(&self) -> Option<PathBuf> {
        if self.metadata.video_excluded || self.metadata.save_kind == LegacySaveKind::LogsOnly {
            return None;
        }
        let video_name = self.metadata.files.video.as_deref()?;
        let path = legacy_file_path(&self.root_dir, video_name).ok()?;
        path.is_file().then_some(path)
    }

    fn audio_path(&self) -> Option<PathBuf> {
        None
    }

    fn is_legacy(&self) -> bool {
        true
    }

    fn crashes(&self) -> Result<Vec<CrashMarker>, StorageError> {
        Ok(extract_crash_markers(&self.logs, &self.frames))
    }

    fn screens(&self) -> Result<Vec<ScreenSpan>, StorageError> {
        Ok(extract_screen_spans(&self.logs, &self.frames))
    }
}

/// Reads the original JSON manifest of a v2 or legacy bundle.
///
/// # Errors
/// Returns an error for an unknown format, invalid JSON or a path escaping the bundle.
pub fn read_bundle_manifest(dir: &Path) -> Result<serde_json::Value, StorageError> {
    let root = fs::canonicalize(dir).map_err(|e| StorageError::io("resolve_bundle", dir, e))?;
    for name in [
        MANIFEST_V2_FILE,
        LEGACY_SCHEMA4_MANIFEST,
        LEGACY_SCHEMA3_MANIFEST,
    ] {
        let path = confined_path(&root, name)?;
        if path.is_file() {
            let bytes = fs::read(&path).map_err(|e| StorageError::io("read_manifest", &path, e))?;
            return serde_json::from_slice(&bytes)
                .map_err(|e| StorageError::json("parse_manifest", e));
        }
    }
    Err(StorageError::InvalidBundle {
        path: root,
        reason: "missing manifest".into(),
    })
}

/// Auto-detects whether `bundle_dir` is a WORM v2 bundle or a legacy schema 3/4
/// bundle and opens it strictly read-only.
///
/// # Errors
///
/// Returns [`StorageError::InvalidBundle`] if the directory does not match any
/// supported bundle format, or underlying parse/SQLite errors.
pub fn open_bundle(bundle_dir: impl AsRef<Path>) -> Result<Box<dyn BundleReader>, StorageError> {
    let dir = bundle_dir.as_ref();
    if dir.join(MANIFEST_V2_FILE).is_file() {
        return Ok(Box::new(V2BundleReader::open(dir)?));
    }
    if dir.join(LEGACY_SCHEMA4_MANIFEST).is_file() || dir.join(LEGACY_SCHEMA3_MANIFEST).is_file() {
        return Ok(Box::new(LegacyBundleReader::open(dir)?));
    }
    Err(StorageError::InvalidBundle {
        path: dir.to_path_buf(),
        reason: "no manifest.json, replay.json, or session.json found".to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Staging & Hashing Helpers
// ---------------------------------------------------------------------------

fn validate_share_sidecar_name(file_name: &str) -> Result<(), StorageError> {
    if !matches!(file_name, SHARE_MP4_FILE | SHARE_TAPS_MP4_FILE) {
        return Err(StorageError::InvalidArgument {
            name: "share_mp4.file_name",
            reason: format!("invalid share MP4 sidecar filename `{file_name}`"),
        });
    }
    Ok(())
}

/// Formats `epoch_ns` (Unix epoch nanoseconds) as `YYYY-MM-DDTHH:MM:SS.mmmZ` without external crates.
fn format_epoch_ns_iso_utc(epoch_ns: i64) -> String {
    let secs = epoch_ns.div_euclid(1_000_000_000);
    let nanos = epoch_ns.rem_euclid(1_000_000_000);
    let millis = nanos / 1_000_000;

    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;

    // Howard Hinnant's civil_from_days algorithm (proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn format_ns_offset(prefix: &str, ns: i64) -> String {
    let sign = if ns < 0 { "-" } else { "" };
    let abs = ns.unsigned_abs();
    let secs = abs / 1_000_000_000;
    let millis = (abs % 1_000_000_000) / 1_000_000;
    format!("{prefix}{sign}{secs}.{millis:03}s")
}

fn format_log_timestamp(log: &LogRecord) -> String {
    if let Some(epoch_ns) = log.header_epoch_ns.filter(|&ns| ns > 0) {
        return format_epoch_ns_iso_utc(epoch_ns);
    }
    match &log.alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => format_ns_offset("bt+", *boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns, ..
        } => format_ns_offset("bt~", *min_boottime_ns),
        TimeAlignment::Unanchored {
            pc_ingest_cont_ns, ..
        } => format_ns_offset("pc+", *pc_ingest_cont_ns),
        _ => format_ns_offset("pc+", log.pc_ingest_cont_ns),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogTextFormat {
    Standard,
    WithUnanchoredReason,
}

fn format_log_lines(log: &LogRecord, format: LogTextFormat, out: &mut String) {
    use std::fmt::Write as _;

    let ts = format_log_timestamp(log);
    let pid = log.pid.map_or_else(|| "-".to_owned(), |v| v.to_string());
    let tid = log.tid.map_or_else(|| "-".to_owned(), |v| v.to_string());
    let level = log
        .level
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("I");
    let tag = log
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("-");
    let reason_tag = match format {
        LogTextFormat::WithUnanchoredReason => match &log.alignment {
            TimeAlignment::Unanchored { reason, .. } => format!(" [unanchored:{reason:?}]"),
            TimeAlignment::Bounded { reason, .. } => format!(" [bounded:{reason:?}]"),
            _ => String::new(),
        },
        LogTextFormat::Standard => String::new(),
    };

    if log.message.is_empty() {
        let _ = writeln!(out, "{ts} {pid} {tid} {level} {tag}{reason_tag}:");
        return;
    }
    for line in log.message.lines() {
        let clean = line.trim_end_matches('\r');
        let _ = writeln!(out, "{ts} {pid} {tid} {level} {tag}{reason_tag}: {clean}");
    }
}

fn stage_logs_text(
    staging: &AtomicStagingDir,
    rel_name: &str,
    logs: &[&LogRecord],
    format: LogTextFormat,
) -> Result<FileDigest, StorageError> {
    let mut text = String::with_capacity(logs.len().saturating_mul(96));
    for log in logs {
        format_log_lines(log, format, &mut text);
    }
    let bytes = text.as_bytes();
    let _staged_path = staging.write_file_synced(rel_name, bytes)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(FileDigest {
        path: rel_name.to_owned(),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        sha256: hex_lower(&hasher.finalize()),
    })
}

fn swipe_direction_label(tap: &TapRecord) -> Option<&'static str> {
    if tap.gesture_kind != GestureKind::Swipe {
        return None;
    }
    let (sx, sy, ex, ey) = (tap.x?, tap.y?, tap.end_x?, tap.end_y?);
    let dx = ex - sx;
    let dy = ey - sy;
    if dx.abs() >= dy.abs() {
        Some(if dx >= 0.0 { "right" } else { "left" })
    } else {
        Some(if dy >= 0.0 { "down" } else { "up" })
    }
}

fn draft_crashes(draft: &V2BundleDraft) -> Vec<CrashMarker> {
    let trigger = match draft.requested_window {
        RequestedWindow::RealElapsedSec {
            duration_sec,
            post_roll_ms,
            ..
        } if post_roll_ms > 0 => draft.epochs.first().map(|e| {
            e.start_boottime_ns.saturating_add(
                i64::try_from(duration_sec.saturating_mul(1_000_000_000)).unwrap_or(i64::MAX),
            )
        }),
        _ => None,
    };
    arb_core::analytics::extract_crash_markers_for_window(&draft.logs, &draft.frames, trigger)
}

#[expect(
    clippy::too_many_lines,
    reason = "serializes taps, screen spans, crashes, and discontinuities into one chronologically sorted events.jsonl"
)]
fn stage_events_jsonl(
    staging: &AtomicStagingDir,
    draft: &V2BundleDraft,
    clock: Option<&arb_core::bundle::FrameMediaClock>,
) -> Result<FileDigest, StorageError> {
    let screens = extract_screen_spans(&draft.logs, &draft.frames);
    let crashes = draft_crashes(draft);

    let total = draft.taps.len() + screens.len() + crashes.len() + draft.discontinuities.len();

    let mut entries: Vec<(i64, u8, u64, serde_json::Value)> = Vec::with_capacity(total);

    for tap in &draft.taps {
        let t_ns = alignment_query_timestamp(&tap.alignment).unwrap_or(tap.pc_ingest_cont_ns);
        let kind = match tap.gesture_kind {
            GestureKind::Swipe => "swipe",
            _ => "tap",
        };
        let mut obj = serde_json::Map::new();
        obj.insert("kind".to_owned(), serde_json::json!(kind));
        obj.insert("seq".to_owned(), serde_json::json!(tap.seq));
        obj.insert("t_ns".to_owned(), serde_json::json!(t_ns));
        obj.insert(
            "bucket".to_owned(),
            serde_json::json!(record_bucket_to_str(tap.bucket)),
        );
        if tap.bucket == RecordBucket::Main
            && let Some(clock) = clock
        {
            obj.insert(
                "relative_time_ns".into(),
                serde_json::json!(clock.relative_time(t_ns)),
            );
        }
        if let Some(x) = tap.x {
            obj.insert("x".to_owned(), serde_json::json!(x));
        }
        if let Some(y) = tap.y {
            obj.insert("y".to_owned(), serde_json::json!(y));
        }
        if let Some(ex) = tap.end_x {
            obj.insert("end_x".to_owned(), serde_json::json!(ex));
        }
        if let Some(ey) = tap.end_y {
            obj.insert("end_y".to_owned(), serde_json::json!(ey));
        }
        if let Some(dt) = tap.end_t_offset_ns {
            obj.insert("end_t_offset_ns".to_owned(), serde_json::json!(dt));
        }
        if let Some(dir) = swipe_direction_label(tap) {
            obj.insert("direction".to_owned(), serde_json::json!(dir));
        }
        if let Some(pid) = tap.pointer_id {
            obj.insert("pointer_id".to_owned(), serde_json::json!(pid));
        }
        if !tap.path.is_empty() {
            obj.insert("path".to_owned(), serde_json::json!(tap.path));
        }
        entries.push((t_ns, 1, tap.seq, serde_json::Value::Object(obj)));
    }

    for (idx, span) in screens.iter().enumerate() {
        entries.push((
            span.start_ns,
            0,
            u64::try_from(idx).unwrap_or(u64::MAX),
            serde_json::json!({
                "kind": "screen",
                "start_ns": span.start_ns,
                "end_ns": span.end_ns,
                "package": span.package_name,
                "activity": span.activity_name,
                "short_name": span.short_name,
                "start_frame": span.start_frame_index,
                "end_frame": span.end_frame_index,
            }),
        ));
    }

    for crash in &crashes {
        let t_ns = crash.boottime_ns.unwrap_or(0);
        entries.push((
            t_ns,
            2,
            crash.source_log_seq,
            serde_json::json!({
                "kind": "crash",
                "t_ns": crash.boottime_ns,
                "frame_index_hint": crash.frame_index_hint,
                "process": crash.process_name,
                "package": crash.package_name,
                "pid": crash.pid,
                "exception": crash.exception_class,
                "message": crash.message,
                "source_log_seq": crash.source_log_seq,
            }),
        ));
    }

    for (idx, disc) in draft.discontinuities.iter().enumerate() {
        let t_ns = disc.start_boottime_ns.unwrap_or(disc.pc_cont_start_ns);
        entries.push((
            t_ns,
            3,
            u64::try_from(idx).unwrap_or(u64::MAX),
            serde_json::json!({
                "kind": "discontinuity",
                "discontinuity_kind": disc.kind,
                "stream": disc.stream,
                "boot_id": disc.boot_id,
                "start_ns": disc.start_boottime_ns,
                "end_ns": disc.end_boottime_ns,
                "detail": disc.detail,
            }),
        ));
    }

    entries.sort_by_key(|(t, order, seq, _)| (*t, *order, *seq));

    let mut bytes = Vec::with_capacity(entries.len().saturating_mul(128));
    for (_, _, _, val) in entries {
        let line = serde_json::to_string(&val)
            .map_err(|source| StorageError::json("serialize_events_jsonl", source))?;
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }

    let _staged_path = staging.write_file_synced(EVENTS_JSONL_FILE, &bytes)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(FileDigest {
        path: EVENTS_JSONL_FILE.to_owned(),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        sha256: hex_lower(&hasher.finalize()),
    })
}

fn trim_clock(draft: &V2BundleDraft) -> Option<arb_core::bundle::FrameMediaClock> {
    match draft.requested_window {
        RequestedWindow::TrimFrameRange {
            in_time_ns: Some(_),
            ..
        } => draft.epochs.first().map(|e| {
            arb_core::bundle::FrameMediaClock::new(&draft.frames, &e.boot_id, e.start_boottime_ns)
        }),
        _ => None,
    }
}

fn stage_logs_jsonl(
    staging: &AtomicStagingDir,
    rel_name: &str,
    logs: &[LogRecord],
    bucket: RecordBucket,
    clock: Option<&arb_core::bundle::FrameMediaClock>,
) -> Result<FileDigest, StorageError> {
    #[derive(serde::Serialize)]
    struct TimedLog<'a> {
        #[serde(flatten)]
        log: &'a LogRecord,
        #[serde(skip_serializing_if = "Option::is_none")]
        relative_time_ns: Option<i64>,
    }

    let mut bytes = Vec::new();
    for log in logs.iter().filter(|log| log.bucket == bucket) {
        let row = TimedLog {
            log,
            relative_time_ns: alignment_query_timestamp(&log.alignment)
                .zip(clock)
                .map(|(t, clock)| clock.relative_time(t)),
        };
        serde_json::to_writer(&mut bytes, &row)
            .map_err(|e| StorageError::json("serialize_log_jsonl", e))?;
        bytes.push(b'\n');
    }
    let _staged_path = staging.write_file_synced(rel_name, &bytes)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(FileDigest {
        path: rel_name.to_owned(),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        sha256: hex_lower(&hasher.finalize()),
    })
}

fn stage_media_file(
    staging: &AtomicStagingDir,
    rel_name: &str,
    payload: &MediaPayload,
) -> Result<FileDigest, StorageError> {
    let mut writer = staging.create_file_writer(rel_name)?;
    let mut hasher = Sha256::new();
    let mut total_bytes: u64 = 0;

    match payload {
        MediaPayload::Bytes(bytes) => {
            writer
                .write_all(bytes)
                .map_err(|source| StorageError::io("write_staged_media", rel_name, source))?;
            hasher.update(bytes);
            total_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        }
        MediaPayload::File(src_path) => {
            let mut src = File::open(src_path)
                .map_err(|source| StorageError::io("open_source_media", src_path, source))?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = src
                    .read(&mut buf)
                    .map_err(|source| StorageError::io("read_source_media", src_path, source))?;
                if n == 0 {
                    break;
                }
                let slice = &buf[..n];
                writer
                    .write_all(slice)
                    .map_err(|source| StorageError::io("copy_staged_media", rel_name, source))?;
                hasher.update(slice);
                total_bytes = total_bytes.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
            }
        }
    }

    fsync_file(&writer)?;
    Ok(FileDigest {
        path: rel_name.to_owned(),
        bytes: total_bytes,
        sha256: hex_lower(&hasher.finalize()),
    })
}

fn hash_and_sync_file(path: &Path, rel_name: &str) -> Result<FileDigest, StorageError> {
    // Windows `FlushFileBuffers` rejects read-only handles (ERROR_ACCESS_DENIED), so the
    // fsync handle must be write-capable. POSIX fsync accepts any fd for the inode.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| StorageError::io("open_for_fsync", path, source))?;
    fsync_file(&file)?;
    drop(file);

    let (bytes, sha256) = stream_hash_file(path)?;
    Ok(FileDigest {
        path: rel_name.to_owned(),
        bytes,
        sha256,
    })
}

/// Returns the file's byte count and lowercase SHA-256 using bounded memory.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read.
pub fn stream_hash_file(path: &Path) -> Result<(u64, String), StorageError> {
    let mut file =
        File::open(path).map_err(|source| StorageError::io("open_for_hash", path, source))?;
    let mut hasher = Sha256::new();
    let mut total_bytes: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|source| StorageError::io("read_for_hash", path, source))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total_bytes = total_bytes.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
    }
    Ok((total_bytes, hex_lower(&hasher.finalize())))
}

// ---------------------------------------------------------------------------
// timeline.sqlite Read/Write Helpers
// ---------------------------------------------------------------------------

fn open_readonly_sqlite(path: &Path) -> Result<Connection, StorageError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)
        .map_err(|source| StorageError::sqlite("open_timeline_readonly", source))?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .map_err(|source| StorageError::sqlite("set_query_only", source))?;
    Ok(conn)
}

fn write_timeline_sqlite(path: &Path, draft: &V2BundleDraft) -> Result<(), StorageError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut conn = Connection::open_with_flags(path, flags)
        .map_err(|source| StorageError::sqlite("create_timeline_db", source))?;

    // Use rollback journal (DELETE) so the committed WORM bundle has zero -wal/-shm sidecars
    // and can be opened cleanly with SQLITE_OPEN_READ_ONLY even on read-only directories.
    conn.execute_batch(
        "PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;",
    )
    .map_err(|source| StorageError::sqlite("configure_timeline_pragmas", source))?;
    conn.execute_batch(TIMELINE_DDL)
        .map_err(|source| StorageError::sqlite("create_timeline_schema", source))?;

    let tx = conn
        .transaction()
        .map_err(|source| StorageError::sqlite("begin_timeline_tx", source))?;

    insert_frames(&tx, &draft.frames)?;
    insert_discontinuities(&tx, &draft.discontinuities)?;
    insert_logs(&tx, &draft.logs)?;
    insert_taps(&tx, &draft.taps)?;
    insert_clock_anchors(&tx, &draft.clock_anchors)?;
    let crashes = draft_crashes(draft);
    insert_crashes(&tx, &crashes)?;

    tx.commit()
        .map_err(|source| StorageError::sqlite("commit_timeline_tx", source))?;
    drop(conn);
    Ok(())
}

#[derive(Debug)]
struct AlignmentSqlCols {
    aligned_boottime_ns: Option<i64>,
    half_width_ns: Option<i64>,
    min_boottime_ns: Option<i64>,
    max_boottime_ns: Option<i64>,
    certainty: &'static str,
    semantics: Option<String>,
    uncertainty_reason: Option<String>,
}

fn encode_alignment(alignment: &TimeAlignment) -> Result<AlignmentSqlCols, StorageError> {
    match alignment {
        TimeAlignment::Anchored {
            boottime_ns,
            half_width_ns,
            semantics,
            ..
        } => Ok(AlignmentSqlCols {
            aligned_boottime_ns: Some(*boottime_ns),
            half_width_ns: Some(i64::try_from(*half_width_ns).unwrap_or(i64::MAX)),
            min_boottime_ns: None,
            max_boottime_ns: None,
            certainty: "anchored",
            semantics: Some(serde_enum_to_string(semantics)?),
            uncertainty_reason: None,
        }),
        TimeAlignment::Bounded {
            min_boottime_ns,
            max_boottime_ns,
            reason,
            ..
        } => Ok(AlignmentSqlCols {
            aligned_boottime_ns: Some(*min_boottime_ns),
            half_width_ns: None,
            min_boottime_ns: Some(*min_boottime_ns),
            max_boottime_ns: Some(*max_boottime_ns),
            certainty: "bounded",
            semantics: None,
            uncertainty_reason: Some(serde_enum_to_string(reason)?),
        }),
        TimeAlignment::Unanchored { reason, .. } => Ok(AlignmentSqlCols {
            aligned_boottime_ns: None,
            half_width_ns: None,
            min_boottime_ns: None,
            max_boottime_ns: None,
            certainty: "unanchored",
            semantics: None,
            uncertainty_reason: Some(serde_enum_to_string(reason)?),
        }),
        other => Err(StorageError::CorruptData {
            reason: format!("unhandled TimeAlignment variant: {other:?}"),
        }),
    }
}

#[derive(Debug)]
struct AlignmentDecodeInput {
    certainty: String,
    boot_id: Option<String>,
    aligned_boottime_ns: Option<i64>,
    half_width_ns: Option<i64>,
    min_boottime_ns: Option<i64>,
    max_boottime_ns: Option<i64>,
    pc_ingest_cont_ns: i64,
    semantics: Option<String>,
    uncertainty_reason: Option<String>,
    default_semantics: TimestampSemantics,
}

fn decode_alignment(input: AlignmentDecodeInput) -> Result<TimeAlignment, StorageError> {
    match input.certainty.as_str() {
        "anchored" => {
            let boottime_ns =
                input
                    .aligned_boottime_ns
                    .ok_or_else(|| StorageError::CorruptData {
                        reason: "anchored row missing aligned_boottime_ns".to_owned(),
                    })?;
            let half_width_ns = input
                .half_width_ns
                .and_then(|v| u64::try_from(v).ok())
                .unwrap_or(0);
            let semantics = match input.semantics {
                Some(raw) => serde_enum_from_str(&raw)?,
                None => input.default_semantics,
            };
            Ok(TimeAlignment::Anchored {
                boot_id: input.boot_id.unwrap_or_default(),
                boottime_ns,
                half_width_ns,
                semantics,
            })
        }
        "bounded" => {
            let min_boottime_ns = input
                .min_boottime_ns
                .or(input.aligned_boottime_ns)
                .ok_or_else(|| StorageError::CorruptData {
                    reason: "bounded row missing min_boottime_ns".to_owned(),
                })?;
            let max_boottime_ns = input.max_boottime_ns.unwrap_or(min_boottime_ns);
            let reason = match input.uncertainty_reason {
                Some(raw) => serde_enum_from_str(&raw)?,
                None => UncertaintyReason::NoClockAnchor,
            };
            Ok(TimeAlignment::Bounded {
                boot_id: input.boot_id.unwrap_or_default(),
                min_boottime_ns,
                max_boottime_ns,
                reason,
            })
        }
        "unanchored" => {
            let reason = match input.uncertainty_reason {
                Some(raw) => serde_enum_from_str(&raw)?,
                None => UncertaintyReason::NoClockAnchor,
            };
            Ok(TimeAlignment::Unanchored {
                pc_ingest_cont_ns: input.pc_ingest_cont_ns,
                reason,
            })
        }
        other => Err(StorageError::CorruptData {
            reason: format!("unknown certainty value `{other}`"),
        }),
    }
}

fn insert_frames(
    tx: &rusqlite::Transaction<'_>,
    frames: &[FrameRecord],
) -> Result<(), StorageError> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO frames (
                frame_index, boot_id, pts_device_mono_ns, aligned_boottime_ns,
                half_width_ns, certainty, is_keyframe, gop_id,
                mp4_sample_offset, mp4_sample_size, pc_ingest_cont_ns,
                min_boottime_ns, max_boottime_ns, semantics, uncertainty_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )
        .map_err(|source| StorageError::sqlite("prepare_insert_frames", source))?;

    for f in frames {
        let cols = encode_alignment(&f.alignment)?;
        let frame_index = u64_to_i64(f.frame_index, "frame_index")?;
        let gop_id = u64_to_i64(f.gop_id, "gop_id")?;
        let mp4_offset = u64_to_i64(f.mp4_sample_offset, "mp4_sample_offset")?;
        let mp4_size = i64::from(f.mp4_sample_size);
        let is_key: i64 = i64::from(f.is_keyframe);
        stmt.execute(params![
            frame_index,
            f.boot_id,
            f.pts_device_mono_ns,
            cols.aligned_boottime_ns,
            cols.half_width_ns,
            cols.certainty,
            is_key,
            gop_id,
            mp4_offset,
            mp4_size,
            f.pc_ingest_cont_ns,
            cols.min_boottime_ns,
            cols.max_boottime_ns,
            cols.semantics,
            cols.uncertainty_reason,
        ])
        .map_err(|source| StorageError::sqlite("insert_frame", source))?;
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct DiscontinuityDetailPayload {
    delta_ns: Option<i64>,
    detail: Option<String>,
}

fn insert_discontinuities(
    tx: &rusqlite::Transaction<'_>,
    events: &[DiscontinuityEvent],
) -> Result<(), StorageError> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO discontinuities (
                kind, stream, boot_id, start_boottime_ns, end_boottime_ns,
                pc_cont_start_ns, pc_cont_end_ns, detail_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .map_err(|source| StorageError::sqlite("prepare_insert_discontinuities", source))?;

    for ev in events {
        let kind_str = serde_enum_to_string(&ev.kind)?;
        let stream_str = ev.stream.as_ref().map(serde_enum_to_string).transpose()?;
        let detail_json = if ev.delta_ns.is_some() || ev.detail.is_some() {
            Some(
                serde_json::to_string(&DiscontinuityDetailPayload {
                    delta_ns: ev.delta_ns,
                    detail: ev.detail.clone(),
                })
                .map_err(|source| StorageError::json("serialize_discontinuity_detail", source))?,
            )
        } else {
            None
        };

        stmt.execute(params![
            kind_str,
            stream_str,
            ev.boot_id,
            ev.start_boottime_ns,
            ev.end_boottime_ns,
            ev.pc_cont_start_ns,
            ev.pc_cont_end_ns,
            detail_json,
        ])
        .map_err(|source| StorageError::sqlite("insert_discontinuity", source))?;
    }
    Ok(())
}

fn insert_logs(tx: &rusqlite::Transaction<'_>, logs: &[LogRecord]) -> Result<(), StorageError> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO logs (
                seq, boot_id, header_epoch_ns, device_read_boottime_ns,
                pc_ingest_cont_ns, aligned_boottime_ns, min_boottime_ns,
                max_boottime_ns, certainty, bucket, level, tag, pid, tid,
                uid, package_name,
                message, half_width_ns, semantics, uncertainty_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        )
        .map_err(|source| StorageError::sqlite("prepare_insert_logs", source))?;

    for log in logs {
        let cols = encode_alignment(&log.alignment)?;
        let seq = u64_to_i64(log.seq, "log.seq")?;
        let uid = log.uid.map(i64::from);
        stmt.execute(params![
            seq,
            log.boot_id,
            log.header_epoch_ns,
            log.device_read_boottime_ns,
            log.pc_ingest_cont_ns,
            cols.aligned_boottime_ns,
            cols.min_boottime_ns,
            cols.max_boottime_ns,
            cols.certainty,
            record_bucket_to_str(log.bucket),
            log.level,
            log.tag,
            log.pid,
            log.tid,
            uid,
            log.package_name,
            log.message,
            cols.half_width_ns,
            cols.semantics,
            cols.uncertainty_reason,
        ])
        .map_err(|source| StorageError::sqlite("insert_log", source))?;
    }
    Ok(())
}

fn insert_taps(tx: &rusqlite::Transaction<'_>, taps: &[TapRecord]) -> Result<(), StorageError> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO taps (
                seq, boot_id, event_mono_ns, pc_ingest_cont_ns,
                aligned_boottime_ns, certainty, bucket, action, x, y,
                pointer_id, half_width_ns, min_boottime_ns, max_boottime_ns,
                semantics, uncertainty_reason,
                gesture_kind, end_x, end_y, end_t_offset_ns, path_json
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20, ?21
            )",
        )
        .map_err(|source| StorageError::sqlite("prepare_insert_taps", source))?;

    for tap in taps {
        let cols = encode_alignment(&tap.alignment)?;
        let seq = u64_to_i64(tap.seq, "tap.seq")?;
        let path_json = if tap.path.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&tap.path)
                    .map_err(|source| StorageError::json("encode_tap_path", source))?,
            )
        };
        stmt.execute(params![
            seq,
            tap.boot_id,
            tap.event_mono_ns,
            tap.pc_ingest_cont_ns,
            cols.aligned_boottime_ns,
            cols.certainty,
            record_bucket_to_str(tap.bucket),
            tap.action,
            tap.x,
            tap.y,
            tap.pointer_id,
            cols.half_width_ns,
            cols.min_boottime_ns,
            cols.max_boottime_ns,
            cols.semantics,
            cols.uncertainty_reason,
            gesture_kind_to_str(tap.gesture_kind),
            tap.end_x,
            tap.end_y,
            tap.end_t_offset_ns,
            path_json,
        ])
        .map_err(|source| StorageError::sqlite("insert_tap", source))?;
    }
    Ok(())
}

fn insert_clock_anchors(
    tx: &rusqlite::Transaction<'_>,
    anchors: &[ClockAnchorRecord],
) -> Result<(), StorageError> {
    let mut stmt = tx
        .prepare("INSERT INTO clock_anchors (kind, boot_id, payload_json) VALUES (?1, ?2, ?3)")
        .map_err(|source| StorageError::sqlite("prepare_insert_clock_anchors", source))?;

    for anchor in anchors {
        match anchor {
            ClockAnchorRecord::DeviceTriple(triple) => {
                let json = serde_json::to_string(triple)
                    .map_err(|source| StorageError::json("serialize_device_triple", source))?;
                stmt.execute(params!["device_triple", triple.boot_id, json])
                    .map_err(|source| StorageError::sqlite("insert_clock_anchor", source))?;
            }
            ClockAnchorRecord::PcSyncProbe(probe) => {
                let json = serde_json::to_string(probe)
                    .map_err(|source| StorageError::json("serialize_pc_sync_probe", source))?;
                stmt.execute(params!["pc_sync_probe", probe.boot_id, json])
                    .map_err(|source| StorageError::sqlite("insert_clock_anchor", source))?;
            }
        }
    }
    Ok(())
}

fn insert_crashes(
    tx: &rusqlite::Transaction<'_>,
    crashes: &[CrashMarker],
) -> Result<(), StorageError> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO crashes (
                boottime_ns, frame_index_hint, process_name, package_name,
                pid, exception_class, message, source_log_seq
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .map_err(|source| StorageError::sqlite("prepare_insert_crashes", source))?;

    for crash in crashes {
        let frame_hint = crash
            .frame_index_hint
            .map(|idx| u64_to_i64(idx, "crash.frame_index_hint"))
            .transpose()?;
        let source_seq = u64_to_i64(crash.source_log_seq, "crash.source_log_seq")?;
        stmt.execute(params![
            crash.boottime_ns,
            frame_hint,
            crash.process_name,
            crash.package_name,
            crash.pid,
            crash.exception_class,
            crash.message,
            source_seq,
        ])
        .map_err(|source| StorageError::sqlite("insert_crash", source))?;
    }
    Ok(())
}

fn sqlite_table_exists(conn: &Connection, table_name: &str) -> Result<bool, StorageError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table_name],
            |row| row.get(0),
        )
        .map_err(|source| StorageError::sqlite("check_table_exists", source))?;
    Ok(count > 0)
}

fn sqlite_table_columns(
    conn: &Connection,
    table_name: &str,
) -> Result<std::collections::BTreeSet<String>, StorageError> {
    let sql = format!("PRAGMA table_info({table_name})");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|source| StorageError::sqlite("check_column_exists", source))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|source| StorageError::sqlite("query_column_exists", source))?;
    let mut cols = std::collections::BTreeSet::new();
    for name in names {
        cols.insert(name.map_err(|source| StorageError::sqlite("read_column_name", source))?);
    }
    Ok(cols)
}

fn sqlite_column_exists(
    conn: &Connection,
    table_name: &str,
    column_name: &str,
) -> Result<bool, StorageError> {
    Ok(sqlite_table_columns(conn, table_name)?.contains(column_name))
}

fn read_persisted_crashes(conn: &Connection) -> Result<Vec<CrashMarker>, StorageError> {
    let mut stmt = conn
        .prepare(
            "SELECT boottime_ns, frame_index_hint, process_name, package_name,
                    pid, exception_class, message, source_log_seq
             FROM crashes
             ORDER BY id ASC",
        )
        .map_err(|source| StorageError::sqlite("prepare_query_crashes", source))?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i32>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })
        .map_err(|source| StorageError::sqlite("query_crashes", source))?;

    let mut crashes = Vec::new();
    for row in rows {
        let (
            boottime_ns,
            frame_hint_raw,
            process_name,
            package_name,
            pid,
            exception_class,
            message,
            seq_raw,
        ) = row.map_err(|source| StorageError::sqlite("read_crash_row", source))?;
        let frame_index_hint = frame_hint_raw
            .map(|v| {
                u64::try_from(v).map_err(|_| StorageError::CorruptData {
                    reason: format!("negative crash frame_index_hint {v}"),
                })
            })
            .transpose()?;
        let source_log_seq = u64::try_from(seq_raw).map_err(|_| StorageError::CorruptData {
            reason: format!("negative crash source_log_seq {seq_raw}"),
        })?;
        crashes.push(CrashMarker {
            boottime_ns,
            frame_index_hint,
            process_name,
            package_name,
            pid,
            exception_class,
            message,
            source_log_seq,
        });
    }
    Ok(crashes)
}

struct RawFrameSqlRow {
    frame_index: i64,
    boot_id: String,
    pts_device_mono_ns: i64,
    aligned_boottime_ns: Option<i64>,
    half_width_ns: Option<i64>,
    certainty: String,
    is_keyframe: i64,
    gop_id: i64,
    mp4_sample_offset: i64,
    mp4_sample_size: i64,
    pc_ingest_cont_ns: i64,
    min_boottime_ns: Option<i64>,
    max_boottime_ns: Option<i64>,
    semantics: Option<String>,
    uncertainty_reason: Option<String>,
}

fn decode_frame_row(raw: RawFrameSqlRow) -> Result<FrameRecord, StorageError> {
    let alignment = decode_alignment(AlignmentDecodeInput {
        certainty: raw.certainty,
        boot_id: Some(raw.boot_id.clone()),
        aligned_boottime_ns: raw.aligned_boottime_ns,
        half_width_ns: raw.half_width_ns,
        min_boottime_ns: raw.min_boottime_ns,
        max_boottime_ns: raw.max_boottime_ns,
        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
        semantics: raw.semantics,
        uncertainty_reason: raw.uncertainty_reason,
        default_semantics: TimestampSemantics::VideoEncoderInputEstimate,
    })?;
    Ok(FrameRecord {
        frame_index: i64_to_u64(raw.frame_index, "frame_index")?,
        boot_id: raw.boot_id,
        pts_device_mono_ns: raw.pts_device_mono_ns,
        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
        alignment,
        is_keyframe: raw.is_keyframe != 0,
        gop_id: i64_to_u64(raw.gop_id, "gop_id")?,
        mp4_sample_offset: i64_to_u64(raw.mp4_sample_offset, "mp4_sample_offset")?,
        mp4_sample_size: u32::try_from(raw.mp4_sample_size).map_err(|_| {
            StorageError::CorruptData {
                reason: format!("invalid mp4_sample_size {}", raw.mp4_sample_size),
            }
        })?,
    })
}

struct RawDiscontinuitySqlRow {
    kind: String,
    stream: Option<String>,
    boot_id: Option<String>,
    start_boottime_ns: Option<i64>,
    end_boottime_ns: Option<i64>,
    pc_cont_start_ns: i64,
    pc_cont_end_ns: Option<i64>,
    detail_json: Option<String>,
}

fn decode_discontinuity_row(
    raw: RawDiscontinuitySqlRow,
) -> Result<DiscontinuityEvent, StorageError> {
    let kind: DiscontinuityKind = serde_enum_from_str(&raw.kind)?;
    let stream: Option<StreamKind> = raw.stream.as_deref().map(serde_enum_from_str).transpose()?;
    let (delta_ns, detail) = match raw.detail_json {
        Some(json) => {
            let parsed: DiscontinuityDetailPayload = serde_json::from_str(&json)
                .map_err(|source| StorageError::json("parse_discontinuity_detail", source))?;
            (parsed.delta_ns, parsed.detail)
        }
        None => (None, None),
    };
    Ok(DiscontinuityEvent {
        kind,
        stream,
        boot_id: raw.boot_id,
        start_boottime_ns: raw.start_boottime_ns,
        end_boottime_ns: raw.end_boottime_ns,
        pc_cont_start_ns: raw.pc_cont_start_ns,
        pc_cont_end_ns: raw.pc_cont_end_ns,
        delta_ns,
        detail,
    })
}

struct RawLogSqlRow {
    seq: i64,
    boot_id: Option<String>,
    header_epoch_ns: Option<i64>,
    device_read_boottime_ns: Option<i64>,
    pc_ingest_cont_ns: i64,
    aligned_boottime_ns: Option<i64>,
    min_boottime_ns: Option<i64>,
    max_boottime_ns: Option<i64>,
    certainty: String,
    bucket: String,
    level: Option<String>,
    tag: Option<String>,
    pid: Option<i32>,
    tid: Option<i32>,
    message: String,
    half_width_ns: Option<i64>,
    semantics: Option<String>,
    uncertainty_reason: Option<String>,
    uid: Option<i64>,
    package_name: Option<String>,
}

fn decode_log_row(raw: RawLogSqlRow) -> Result<LogRecord, StorageError> {
    let alignment = decode_alignment(AlignmentDecodeInput {
        certainty: raw.certainty,
        boot_id: raw.boot_id.clone(),
        aligned_boottime_ns: raw.aligned_boottime_ns,
        half_width_ns: raw.half_width_ns,
        min_boottime_ns: raw.min_boottime_ns,
        max_boottime_ns: raw.max_boottime_ns,
        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
        semantics: raw.semantics,
        uncertainty_reason: raw.uncertainty_reason,
        default_semantics: TimestampSemantics::LogcatHeaderWall,
    })?;
    Ok(LogRecord {
        seq: i64_to_u64(raw.seq, "log.seq")?,
        boot_id: raw.boot_id,
        header_epoch_ns: raw.header_epoch_ns,
        device_read_boottime_ns: raw.device_read_boottime_ns,
        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
        alignment,
        bucket: parse_record_bucket(&raw.bucket)?,
        level: raw.level,
        tag: raw.tag,
        pid: raw.pid,
        tid: raw.tid,
        uid: raw.uid.and_then(|v| u32::try_from(v).ok()),
        package_name: raw.package_name,
        message: raw.message,
    })
}

struct RawTapSqlRow {
    seq: i64,
    boot_id: Option<String>,
    event_mono_ns: Option<i64>,
    pc_ingest_cont_ns: i64,
    aligned_boottime_ns: Option<i64>,
    certainty: String,
    bucket: String,
    action: String,
    x: Option<f64>,
    y: Option<f64>,
    pointer_id: Option<i32>,
    half_width_ns: Option<i64>,
    min_boottime_ns: Option<i64>,
    max_boottime_ns: Option<i64>,
    semantics: Option<String>,
    uncertainty_reason: Option<String>,
    // T-6H item 2: `None` when reading a bundle saved before the gesture columns existed.
    gesture_kind: Option<String>,
    end_x: Option<f64>,
    end_y: Option<f64>,
    end_t_offset_ns: Option<i64>,
    path_json: Option<String>,
}

fn gesture_kind_to_str(kind: GestureKind) -> &'static str {
    match kind {
        GestureKind::Swipe => "swipe",
        _ => "tap",
    }
}

fn parse_gesture_kind(raw: Option<&str>) -> GestureKind {
    match raw {
        Some("swipe") => GestureKind::Swipe,
        _ => GestureKind::Tap,
    }
}

fn decode_tap_row(raw: RawTapSqlRow) -> Result<TapRecord, StorageError> {
    let alignment = decode_alignment(AlignmentDecodeInput {
        certainty: raw.certainty,
        boot_id: raw.boot_id.clone(),
        aligned_boottime_ns: raw.aligned_boottime_ns,
        half_width_ns: raw.half_width_ns,
        min_boottime_ns: raw.min_boottime_ns,
        max_boottime_ns: raw.max_boottime_ns,
        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
        semantics: raw.semantics,
        uncertainty_reason: raw.uncertainty_reason,
        default_semantics: TimestampSemantics::InputEventTime,
    })?;
    let path: Vec<TouchPoint> = raw
        .path_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Ok(TapRecord {
        seq: i64_to_u64(raw.seq, "tap.seq")?,
        boot_id: raw.boot_id,
        event_mono_ns: raw.event_mono_ns,
        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
        alignment,
        bucket: parse_record_bucket(&raw.bucket)?,
        action: raw.action,
        x: raw.x,
        y: raw.y,
        pointer_id: raw.pointer_id,
        gesture_kind: parse_gesture_kind(raw.gesture_kind.as_deref()),
        end_x: raw.end_x,
        end_y: raw.end_y,
        end_t_offset_ns: raw.end_t_offset_ns,
        path,
    })
}

// ---------------------------------------------------------------------------
// Legacy Read-Only Projection Helpers
// ---------------------------------------------------------------------------

fn confined_path(root: &Path, name: &str) -> Result<PathBuf, StorageError> {
    let mut target = root.to_owned();
    for component in Path::new(name).components() {
        let std::path::Component::Normal(part) = component else {
            return Err(StorageError::InvalidBundle {
                path: root.into(),
                reason: format!("{name} outside bundle"),
            });
        };
        target.push(part);
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                target = fs::canonicalize(&target)
                    .map_err(|e| StorageError::io("resolve_bundle_file", &target, e))?;
                if !target.starts_with(root) {
                    return Err(StorageError::InvalidBundle {
                        path: root.into(),
                        reason: format!("{name} resolves outside bundle"),
                    });
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(StorageError::io("stat_bundle_file", &target, e)),
        }
    }
    Ok(target)
}

/// Lexical rule for every file name taken from an untrusted legacy manifest (R2 S16):
/// a single non-empty path segment inside the bundle folder (legacy layouts are flat), so
/// no `..`/`.`, no `/` or `\`, no drive prefix or NTFS stream (`:`), and no NUL.
fn is_safe_legacy_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
}

/// Resolves a legacy manifest file name to a path confined to `root_dir`: the name must be
/// lexically safe and, when it exists, must canonicalise (through symlinks) inside the
/// canonical bundle root.
fn legacy_file_path(root_dir: &Path, name: &str) -> Result<PathBuf, StorageError> {
    let reject = |reason: String| StorageError::InvalidBundle {
        path: root_dir.to_path_buf(),
        reason,
    };
    if !is_safe_legacy_file_name(name) {
        return Err(reject(format!(
            "legacy manifest file name `{name}` escapes the bundle folder"
        )));
    }
    let path = root_dir.join(name);
    if let Ok(canonical) = fs::canonicalize(&path) {
        let canonical_root = fs::canonicalize(root_dir)
            .map_err(|source| StorageError::io("canonicalize_legacy_root", root_dir, source))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(reject(format!(
                "legacy manifest file `{name}` resolves outside the bundle folder"
            )));
        }
    }
    Ok(path)
}

/// Validates every file name referenced by a legacy `replay.json` / `session.json` before
/// anything is read, hashed, or handed to the media pipeline.
fn validate_legacy_manifest_paths(
    root_dir: &Path,
    meta: &LegacyBundleMetadata,
) -> Result<(), StorageError> {
    let files = &meta.files;
    let named = [
        Some(files.manifest.as_str()),
        files.readme.as_deref(),
        files.video.as_deref(),
        files.video_frames.as_deref(),
        files.video_source.as_deref(),
        files.logcat_device.as_deref(),
        files.logcat_app.as_deref(),
        files.touches.as_deref(),
    ];
    for name in named.into_iter().flatten() {
        legacy_file_path(root_dir, name)?;
    }
    for name in meta.files_sha256.keys() {
        legacy_file_path(root_dir, name)?;
    }
    Ok(())
}

fn load_legacy_frames(
    root_dir: &Path,
    meta: &LegacyBundleMetadata,
    origin_ns: Option<i64>,
) -> Result<Vec<FrameRecord>, StorageError> {
    let Some(frames_file) = meta.files.video_frames.as_deref() else {
        return Ok(Vec::new());
    };
    let path = legacy_file_path(root_dir, frames_file)?;
    let text = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(StorageError::io("read_legacy_frames", &path, source)),
    };

    let rows = match meta.schema {
        LegacySchemaVersion::Schema4 => parse_schema4_frames_jsonl(&text)?,
        LegacySchemaVersion::Schema3 => parse_schema3_frames_jsonl(&text)?,
        other => {
            return Err(StorageError::CorruptData {
                reason: format!("unsupported legacy schema version: {other:?}"),
            });
        }
    };

    let mut gop_id: u64 = 0;
    let mut frames = Vec::with_capacity(rows.len());
    for (idx, row) in rows.into_iter().enumerate() {
        let is_keyframe = row.output_key || row.source_key;
        if idx > 0 && is_keyframe {
            gop_id = gop_id.saturating_add(1);
        }
        let boot_id = row
            .boot_segment
            .clone()
            .unwrap_or_else(|| DEFAULT_LEGACY_BOOT_ID.to_owned());
        let alignment = row.to_time_alignment_with_origin(&boot_id, origin_ns, 0);
        let pts_device_mono_ns = row
            .source_pts_us
            .or(row.media_pts_us)
            .or(row.movie_pts_us)
            .map_or(0, |us| us.saturating_mul(1_000));

        frames.push(FrameRecord {
            frame_index: u64::try_from(idx).unwrap_or(u64::MAX),
            boot_id,
            pts_device_mono_ns,
            pc_ingest_cont_ns: 0,
            alignment,
            is_keyframe,
            gop_id,
            mp4_sample_offset: 0,
            mp4_sample_size: 0,
        });
    }
    Ok(frames)
}

fn load_legacy_logs(
    root_dir: &Path,
    rel_file: Option<&str>,
    origin_ns: Option<i64>,
) -> Result<(Vec<LogRecord>, String), StorageError> {
    let Some(file_name) = rel_file else {
        return Ok((Vec::new(), String::new()));
    };
    let path = legacy_file_path(root_dir, file_name)?;
    let text = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok((Vec::new(), String::new()));
        }
        Err(source) => return Err(StorageError::io("read_legacy_logs", &path, source)),
    };

    let rows = parse_schema4_logs_jsonl(&text)?;
    let mut records = Vec::with_capacity(rows.len());
    for (idx, row) in rows.into_iter().enumerate() {
        let boot_id = row
            .boot_segment
            .clone()
            .unwrap_or_else(|| DEFAULT_LEGACY_BOOT_ID.to_owned());
        let alignment = row.to_time_alignment_with_origin(&boot_id, origin_ns, 0);
        let bucket = match alignment {
            TimeAlignment::Unanchored { .. } => RecordBucket::Unanchored,
            _ => RecordBucket::Main,
        };
        records.push(LogRecord {
            seq: u64::try_from(idx).unwrap_or(u64::MAX),
            boot_id: Some(boot_id),
            header_epoch_ns: row.epoch_ns,
            device_read_boottime_ns: row.elapsed_ns,
            pc_ingest_cont_ns: 0,
            alignment,
            bucket,
            level: row.priority.map(android_priority_label),
            tag: row.tag,
            pid: row.pid,
            tid: row.tid,
            uid: None,
            package_name: None,
            message: row.message,
        });
    }
    Ok((records, crate::sha256_hex(text.as_bytes())))
}

fn load_legacy_taps(
    root_dir: &Path,
    rel_file: Option<&str>,
    origin_ns: Option<i64>,
) -> Result<Vec<TapRecord>, StorageError> {
    let Some(file_name) = rel_file else {
        return Ok(Vec::new());
    };
    let path = legacy_file_path(root_dir, file_name)?;
    let text = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(StorageError::io("read_legacy_taps", &path, source)),
    };

    let rows = parse_schema4_touches_jsonl(&text)?;
    let mut records = Vec::with_capacity(rows.len());
    for (idx, row) in rows.into_iter().enumerate() {
        let boot_id = row
            .boot_segment
            .clone()
            .unwrap_or_else(|| DEFAULT_LEGACY_BOOT_ID.to_owned());
        let alignment = row.to_time_alignment_with_origin(&boot_id, origin_ns, 0);
        let bucket = match alignment {
            TimeAlignment::Unanchored { .. } => RecordBucket::Unanchored,
            _ => RecordBucket::Main,
        };
        records.push(TapRecord {
            seq: u64::try_from(idx).unwrap_or(u64::MAX),
            boot_id: Some(boot_id),
            event_mono_ns: row.device_mono_ns,
            pc_ingest_cont_ns: 0,
            alignment,
            bucket,
            action: row.action,
            x: row.x.or(row.screen_x),
            y: row.y.or(row.screen_y),
            pointer_id: row.pointer,
            // Legacy schema 3/4 bundles only ever recorded one row per raw touch sample, so
            // there is no stroke to classify; default to `Tap` (T-6H item 2 backward compat).
            gesture_kind: GestureKind::default(),
            end_x: None,
            end_y: None,
            end_t_offset_ns: None,
            path: Vec::new(),
        });
    }
    Ok(records)
}

fn derive_legacy_discontinuities(meta: &LegacyBundleMetadata) -> Vec<DiscontinuityEvent> {
    let origin_ns = meta.timebase.device_boottime_origin_ns();
    let mut events = Vec::new();
    for pair in meta.parts.windows(2) {
        let prev = &pair[0];
        let next = &pair[1];
        if let (Some(prev_end), Some(next_start)) = (prev.t_end_ns, next.t_start_ns)
            && next_start > prev_end
        {
            events.push(DiscontinuityEvent {
                kind: DiscontinuityKind::StreamGap,
                stream: Some(StreamKind::Video),
                boot_id: Some(DEFAULT_LEGACY_BOOT_ID.to_owned()),
                start_boottime_ns: origin_ns.and_then(|o| o.checked_add(prev_end)),
                end_boottime_ns: origin_ns.and_then(|o| o.checked_add(next_start)),
                pc_cont_start_ns: prev_end,
                pc_cont_end_ns: Some(next_start),
                delta_ns: i64::try_from(i128::from(next_start) - i128::from(prev_end)).ok(),
                detail: Some(format!(
                    "gap between legacy video segments {} and {}",
                    prev.segment_index, next.segment_index
                )),
            });
        }
    }
    events
}

fn count_unanchored(frames: &[FrameRecord], logs: &[LogRecord], taps: &[TapRecord]) -> u64 {
    let f_count = frames
        .iter()
        .filter(|f| matches!(f.alignment, TimeAlignment::Unanchored { .. }))
        .count();
    let l_count = logs
        .iter()
        .filter(|l| l.bucket == RecordBucket::Unanchored)
        .count();
    let t_count = taps
        .iter()
        .filter(|t| t.bucket == RecordBucket::Unanchored)
        .count();
    u64::try_from(f_count + l_count + t_count).unwrap_or(u64::MAX)
}

fn build_legacy_summary(
    meta: &LegacyBundleMetadata,
    frames: &[FrameRecord],
    logs: &[LogRecord],
    taps: &[TapRecord],
    unanchored_count: u64,
) -> BundleManifestSummary {
    let format = match meta.schema {
        LegacySchemaVersion::Schema4 => BundleFormat::LegacySchema4,
        _ => BundleFormat::LegacySchema3,
    };
    let video_status = if meta.video_excluded || meta.save_kind == LegacySaveKind::LogsOnly {
        StreamStatus::Excluded
    } else if !frames.is_empty() || meta.files.video.is_some() {
        StreamStatus::Present
    } else {
        StreamStatus::Absent
    };

    let streams = StreamsSummary {
        video: Some(VideoStreamMeta {
            status: video_status,
            frame_count: u64::try_from(frames.len()).unwrap_or(u64::MAX),
            tail_may_be_incomplete: false,
            semantics: TimestampSemantics::VideoEncoderInputEstimate,
            display_width: None,
            display_height: None,
            carried_in: None,
        }),
        audio: None,
        logcat: Some(LogcatStreamMeta {
            dropped_count: None,
            status: if logs.is_empty() {
                StreamStatus::Absent
            } else {
                StreamStatus::Present
            },
            completeness: LogCompleteness::Unverified,
            overflow_events: 0,
            tail_may_be_incomplete: false,
        }),
        input: Some(InputStreamMeta {
            input_events: None,
            status: if taps.is_empty() {
                StreamStatus::Absent
            } else {
                StreamStatus::Present
            },
            tail_may_be_incomplete: false,
        }),
    };

    let mut epochs = Vec::new();
    if let (Some(start), Some(dur)) = (
        meta.timebase.device_boottime_origin_ns(),
        meta.timebase.window_duration_ns,
    ) && dur >= 0
    {
        epochs.push(EpochSpan {
            boot_id: DEFAULT_LEGACY_BOOT_ID.to_owned(),
            start_boottime_ns: start,
            end_boottime_ns: start.saturating_add(dur),
        });
    }

    BundleManifestSummary {
        bundle_id: meta.save_id.clone(),
        format,
        bundle_kind: if meta.save_kind == LegacySaveKind::LogsOnly {
            BundleKind::LogsOnly
        } else {
            BundleKind::Full
        },
        created_at_utc: meta.created_at.clone(),
        provenance: Provenance::Original,
        device: None,
        streams,
        unanchored_summary: UnanchoredSummary {
            included_count: unanchored_count,
            truncated_count: 0,
        },
        epochs,
        app_logs: Vec::new(),
        manifest_v2: None,
        legacy_metadata: Some(meta.clone()),
    }
}

fn alignment_query_timestamp(alignment: &TimeAlignment) -> Option<i64> {
    match alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns, ..
        } => Some(*min_boottime_ns),
        TimeAlignment::Unanchored {
            pc_ingest_cont_ns, ..
        } => Some(*pc_ingest_cont_ns),
        _ => None,
    }
}

fn android_priority_label(priority: i32) -> String {
    match priority {
        2 => "V".to_owned(),
        3 => "D".to_owned(),
        4 => "I".to_owned(),
        5 => "W".to_owned(),
        6 => "E".to_owned(),
        7 => "F".to_owned(),
        other => other.to_string(),
    }
}

const fn record_bucket_to_str(bucket: RecordBucket) -> &'static str {
    match bucket {
        RecordBucket::Main => "main",
        _ => "unanchored",
    }
}

fn parse_record_bucket(value: &str) -> Result<RecordBucket, StorageError> {
    match value {
        "main" => Ok(RecordBucket::Main),
        "unanchored" => Ok(RecordBucket::Unanchored),
        other => Err(StorageError::CorruptData {
            reason: format!("unknown record bucket `{other}`"),
        }),
    }
}

fn serde_enum_to_string<T: Serialize>(value: &T) -> Result<String, StorageError> {
    let json = serde_json::to_string(value)
        .map_err(|source| StorageError::json("serialize_enum", source))?;
    Ok(json.trim_matches('"').to_owned())
}

fn serde_enum_from_str<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, StorageError> {
    let quoted = serde_json::to_string(value)
        .map_err(|source| StorageError::json("quote_enum_str", source))?;
    serde_json::from_str(&quoted).map_err(|source| StorageError::json("deserialize_enum", source))
}

fn u64_to_i64(value: u64, field: &'static str) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| StorageError::InvalidArgument {
        name: field,
        reason: format!("{field} value {value} exceeds SQLite i64::MAX"),
    })
}

fn i64_to_u64(value: i64, field: &'static str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::CorruptData {
        reason: format!("negative {field} value {value} in timeline.sqlite"),
    })
}

#[cfg(test)]
mod tests {
    use arb_core::bundle::{
        BundleKind, DeviceInfo, EpochSpan, FrameRecord, InputStreamMeta, LogCompleteness,
        LogRecord, LogcatStreamMeta, Provenance, RecordBucket, RequestedWindow, StreamStatus,
        StreamsSummary, TapRecord, UnanchoredSummary, VideoStreamMeta,
    };
    use arb_core::gesture::{GestureKind, TouchPoint};
    use arb_core::time::{
        DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind, StreamKind, TimeAlignment,
        TimestampSemantics, UncertaintyReason,
    };
    use tempfile::TempDir;

    use super::{
        BundleReader, ClockAnchorRecord, MediaPayload, TimeRange, V2BundleDraft, V2BundleReader,
        V2BundleWriter,
    };
    use crate::error::StorageError;

    #[test]
    fn share_sidecar_requires_a_portable_share_filename() {
        assert!(super::validate_share_sidecar_name("Video.MP4").is_err());
    }

    fn sample_logs() -> Vec<LogRecord> {
        vec![
            LogRecord {
                seq: 0,
                boot_id: Some("boot-1".to_owned()),
                header_epoch_ns: Some(1_700_000_000_000),
                device_read_boottime_ns: Some(1_550),
                pc_ingest_cont_ns: 5_050,
                alignment: TimeAlignment::Anchored {
                    boot_id: "boot-1".to_owned(),
                    boottime_ns: 1_550,
                    half_width_ns: 10,
                    semantics: TimestampSemantics::LogcatHeaderWall,
                },
                bucket: RecordBucket::Main,
                level: Some("I".to_owned()),
                tag: Some("Main".to_owned()),
                pid: Some(100),
                tid: Some(101),
                uid: None,
                package_name: None,
                message: "anchored log".to_owned(),
            },
            LogRecord {
                seq: 1,
                boot_id: None,
                header_epoch_ns: None,
                device_read_boottime_ns: None,
                pc_ingest_cont_ns: 5_200,
                alignment: TimeAlignment::Unanchored {
                    pc_ingest_cont_ns: 5_200,
                    reason: UncertaintyReason::NoClockAnchor,
                },
                bucket: RecordBucket::Unanchored,
                level: Some("W".to_owned()),
                tag: Some("Net".to_owned()),
                pid: Some(200),
                tid: Some(201),
                uid: None,
                package_name: None,
                message: "unanchored log".to_owned(),
            },
        ]
    }

    #[expect(
        clippy::too_many_lines,
        reason = "test fixture builder lists every bundle struct field explicitly"
    )]
    fn sample_draft() -> V2BundleDraft {
        V2BundleDraft {
            memo: None,
            bundle_kind: BundleKind::Full,
            bundle_id: "01JTEST0000000000000000001".to_owned(),
            created_at_utc: "2026-11-22T12:00:00Z".to_owned(),
            provenance: Provenance::Original,
            device: DeviceInfo {
                serial: "emulator-5554".to_owned(),
                model: "Pixel 9".to_owned(),
                api_level: 35,
                is_emulator: true,
            },
            requested_window: RequestedWindow::RealElapsedSec {
                post_roll_ms: 0,
                actual_post_roll_ms: 0,
                post_roll_truncated: false,
                duration_sec: 30,
                trigger_pc_wall_utc: "2026-11-22T12:00:00Z".to_owned(),
            },
            epochs: vec![EpochSpan {
                boot_id: "boot-1".to_owned(),
                start_boottime_ns: 1_000,
                end_boottime_ns: 2_000,
            }],
            streams: StreamsSummary {
                video: Some(VideoStreamMeta {
                    status: StreamStatus::Present,
                    frame_count: 1,
                    tail_may_be_incomplete: false,
                    semantics: TimestampSemantics::VideoEncoderInputEstimate,
                    display_width: None,
                    display_height: None,
                    carried_in: None,
                }),
                audio: None,
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
                included_count: 1,
                truncated_count: 0,
            },
            video_mp4: Some(MediaPayload::Bytes(b"fake-mp4-data".to_vec())),
            audio_m4a: Some(MediaPayload::Bytes(b"fake-m4a-data".to_vec())),
            share_mp4: None,
            frames: vec![FrameRecord {
                frame_index: 0,
                boot_id: "boot-1".to_owned(),
                pts_device_mono_ns: 1_000,
                pc_ingest_cont_ns: 5_000,
                alignment: TimeAlignment::Anchored {
                    boot_id: "boot-1".to_owned(),
                    boottime_ns: 1_500,
                    half_width_ns: 50,
                    semantics: TimestampSemantics::VideoEncoderInputEstimate,
                },
                is_keyframe: true,
                gop_id: 0,
                mp4_sample_offset: 32,
                mp4_sample_size: 128,
            }],
            discontinuities: vec![DiscontinuityEvent {
                kind: DiscontinuityKind::StreamGap,
                stream: Some(StreamKind::Video),
                boot_id: Some("boot-1".to_owned()),
                start_boottime_ns: Some(1_600),
                end_boottime_ns: Some(1_900),
                pc_cont_start_ns: 5_100,
                pc_cont_end_ns: Some(5_400),
                delta_ns: Some(300),
                detail: Some("synthetic gap".to_owned()),
            }],
            logs: sample_logs(),
            taps: vec![
                TapRecord {
                    seq: 0,
                    boot_id: Some("boot-1".to_owned()),
                    event_mono_ns: Some(1_520),
                    pc_ingest_cont_ns: 5_020,
                    alignment: TimeAlignment::Anchored {
                        boot_id: "boot-1".to_owned(),
                        boottime_ns: 1_520,
                        half_width_ns: 5,
                        semantics: TimestampSemantics::InputEventTime,
                    },
                    bucket: RecordBucket::Main,
                    action: "TAP".to_owned(),
                    x: Some(120.5),
                    y: Some(340.25),
                    pointer_id: Some(0),
                    gesture_kind: GestureKind::Tap,
                    end_x: None,
                    end_y: None,
                    end_t_offset_ns: None,
                    path: Vec::new(),
                },
                // Swipe round-trip (T-6H item 2): non-null end_x/end_y/end_t_offset_ns and a
                // decimated path must survive the `taps` sqlite table losslessly.
                TapRecord {
                    seq: 1,
                    boot_id: Some("boot-1".to_owned()),
                    event_mono_ns: Some(1_600),
                    pc_ingest_cont_ns: 5_100,
                    alignment: TimeAlignment::Anchored {
                        boot_id: "boot-1".to_owned(),
                        boottime_ns: 1_600,
                        half_width_ns: 5,
                        semantics: TimestampSemantics::InputEventTime,
                    },
                    bucket: RecordBucket::Main,
                    action: "SWIPE".to_owned(),
                    x: Some(50.0),
                    y: Some(900.0),
                    pointer_id: Some(1),
                    gesture_kind: GestureKind::Swipe,
                    end_x: Some(50.0),
                    end_y: Some(200.0),
                    end_t_offset_ns: Some(180_000_000),
                    path: vec![
                        TouchPoint {
                            x: 50.0,
                            y: 900.0,
                            t_offset_ns: 0,
                        },
                        TouchPoint {
                            x: 50.0,
                            y: 550.0,
                            t_offset_ns: 90_000_000,
                        },
                        TouchPoint {
                            x: 50.0,
                            y: 200.0,
                            t_offset_ns: 180_000_000,
                        },
                    ],
                },
            ],
            clock_anchors: vec![ClockAnchorRecord::DeviceTriple(DeviceClockTriple {
                boot_id: "boot-1".to_owned(),
                boottime_ns: 1_500,
                monotonic_ns: 1_000,
                realtime_ns: 1_700_000_000_000,
            })],
        }
    }

    #[test]
    fn v2_bundle_write_read_roundtrip_and_tamper_detection() {
        let tmp = TempDir::new().expect("temp dir must be created");
        let dest = tmp.path().join("bundle-01");
        let draft = sample_draft();

        let manifest =
            V2BundleWriter::write(&dest, "job-01", &draft).expect("v2 write must succeed");
        assert_eq!(
            manifest.files.len(),
            6,
            "manifest must list audio.m4a, events.jsonl, logcat.log, timeline.sqlite, unanchored.log, and video.mp4"
        );

        let reader = V2BundleReader::open(&dest).expect("v2 read-only open must succeed");
        reader
            .verify_integrity()
            .expect("freshly committed bundle must pass SHA-256 verification");
        assert!(!reader.is_legacy(), "v2 reader must not report legacy");
        assert!(reader.video_path().is_some(), "video.mp4 must exist");
        assert!(reader.audio_path().is_some(), "audio.m4a must exist");

        assert_eq!(
            reader.frames().expect("frames query must succeed"),
            draft.frames,
            "frames must round-trip losslessly"
        );
        assert_eq!(
            reader
                .discontinuities()
                .expect("discontinuities query must succeed"),
            draft.discontinuities,
            "discontinuities must round-trip losslessly"
        );
        assert_eq!(
            reader
                .query_logs(
                    Some(RecordBucket::Main),
                    TimeRange::between(1_500, 1_600),
                    None
                )
                .expect("main log query must succeed")
                .len(),
            1,
            "main bucket filter must return 1 anchored log"
        );
        assert_eq!(
            reader
                .query_taps(Some(RecordBucket::Main), TimeRange::all(), Some(5))
                .expect("taps query must succeed"),
            draft.taps,
            "taps must round-trip losslessly"
        );
        assert_eq!(
            reader
                .clock_anchors()
                .expect("clock_anchors query must succeed"),
            draft.clock_anchors,
            "clock_anchors must round-trip losslessly"
        );

        // Tamper with video.mp4 and verify integrity check fails cleanly.
        std::fs::write(dest.join("video.mp4"), b"corrupted-mp4")
            .expect("tamper write in test must succeed");
        let err = reader
            .verify_integrity()
            .expect_err("tampered video.mp4 must fail integrity verification");
        assert!(
            matches!(err, StorageError::IntegrityMismatch { .. }),
            "expected IntegrityMismatch, got {err:?}"
        );
    }

    #[test]
    fn logs_only_v2_bundle_writes_jsonl_sidecars_persists_crashes_and_verifies_sha256() {
        let tmp = TempDir::new().expect("tempdir");
        let dest = tmp.path().join("bundle-logs-only");
        let mut draft = sample_draft();
        draft.bundle_kind = BundleKind::LogsOnly;
        draft.bundle_id = "01JLOGS0000000000000000001".to_owned();
        draft.video_mp4 = None;
        draft.audio_m4a = None;
        draft.frames.clear();
        draft.taps.clear();
        draft.streams.video = Some(VideoStreamMeta {
            status: StreamStatus::Absent,
            frame_count: 0,
            tail_may_be_incomplete: false,
            semantics: TimestampSemantics::VideoEncoderInputEstimate,
            display_width: None,
            display_height: None,
            carried_in: None,
        });
        draft.streams.audio = Some(arb_core::bundle::AudioStreamMeta {
            status: StreamStatus::Absent,
            av_offset_status: arb_core::bundle::AvOffsetStatus::Uncalibrated,
            av_offset_ms: 0,
            audio_lead_in_offset_ns: 0,
            tail_may_be_incomplete: false,
        });
        draft.logs.push(LogRecord {
            seq: 2,
            boot_id: Some("boot-1".to_owned()),
            header_epoch_ns: Some(1_700_000_002_000),
            device_read_boottime_ns: Some(1_700),
            pc_ingest_cont_ns: 5_300,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-1".to_owned(),
                boottime_ns: 1_700,
                half_width_ns: 10,
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            bucket: RecordBucket::Main,
            level: Some("E".to_owned()),
            tag: Some("am_crash".to_owned()),
            pid: Some(1000),
            tid: Some(1000),
            uid: None,
            package_name: None,
            message:
                "[0,4321,com.example.demo,8929190,java.lang.NullPointerException,boom,Main.kt,14,0]"
                    .to_owned(),
        });

        let manifest =
            V2BundleWriter::write(&dest, "job-logs", &draft).expect("logs_only write must succeed");
        assert_eq!(
            manifest.bundle_kind,
            BundleKind::LogsOnly,
            "manifest must mark logs_only"
        );
        assert!(
            !dest.join("video.mp4").exists(),
            "logs_only bundle must not create video.mp4"
        );
        assert!(
            dest.join(super::LOGS_JSONL_FILE).is_file(),
            "logs.jsonl must be written"
        );
        assert!(
            dest.join(super::UNANCHORED_JSONL_FILE).is_file(),
            "unanchored.jsonl must be written"
        );

        let reader = V2BundleReader::open(&dest).expect("logs_only open must succeed");
        reader
            .verify_integrity()
            .expect("logs_only SHA-256 verification must pass");
        assert!(
            reader.video_path().is_none(),
            "logs_only must expose no video path"
        );
        let crashes = reader.crashes().expect("crashes query must succeed");
        assert_eq!(crashes.len(), 1, "must persist and return 1 crash marker");
        assert_eq!(
            crashes[0].exception_class, "java.lang.NullPointerException",
            "crash exception must match"
        );

        // Tampering logs.jsonl must fail integrity check.
        std::fs::write(dest.join(super::LOGS_JSONL_FILE), b"tampered\n").expect("tamper logs");
        assert!(
            matches!(
                reader.verify_integrity(),
                Err(StorageError::IntegrityMismatch { .. })
            ),
            "tampered logs.jsonl must fail SHA-256 check"
        );
    }

    #[test]
    fn pre_t6h_readonly_sqlite_without_gesture_columns_reads_taps_cleanly() {
        let tmp = TempDir::new().expect("tempdir");
        let dest = tmp.path().join("bundle-pre-t6h");
        let draft = sample_draft();
        let mut manifest =
            V2BundleWriter::write(&dest, "job-pre-t6h", &draft).expect("seed bundle write");

        // Rebuild `taps` with ONLY the pre-T-6H 16-column schema (no gesture_kind/end_x/end_y/
        // end_t_offset_ns/path_json) to simulate an already-saved WORM bundle opened strictly
        // SQLITE_OPEN_READ_ONLY after upgrading to T-6H.
        let timeline_path = dest.join(super::TIMELINE_DB_FILE);
        {
            let conn = rusqlite::Connection::open(&timeline_path).expect("open sqlite rw");
            conn.execute_batch(
                "DROP TABLE taps;
                 CREATE TABLE taps (
                   seq INTEGER PRIMARY KEY,
                   boot_id TEXT,
                   event_mono_ns INTEGER,
                   pc_ingest_cont_ns INTEGER NOT NULL,
                   aligned_boottime_ns INTEGER,
                   certainty TEXT NOT NULL,
                   bucket TEXT NOT NULL,
                   action TEXT NOT NULL,
                   x REAL,
                   y REAL,
                   pointer_id INTEGER,
                   half_width_ns INTEGER,
                   min_boottime_ns INTEGER,
                   max_boottime_ns INTEGER,
                   semantics TEXT,
                   uncertainty_reason TEXT
                 );
                 INSERT INTO taps (
                   seq, boot_id, event_mono_ns, pc_ingest_cont_ns,
                   aligned_boottime_ns, certainty, bucket, action, x, y,
                   pointer_id, half_width_ns, min_boottime_ns, max_boottime_ns,
                   semantics, uncertainty_reason
                 ) VALUES (
                   0, 'boot-1', 1520, 5020, 1520, 'anchored', 'main', 'DOWN',
                   120.5, 340.25, 0, 5, NULL, NULL, 'input_event_time', NULL
                 );",
            )
            .expect("rewrite pre-t6h taps table");
        }

        // Refresh manifest digest for timeline.sqlite so verify_integrity also succeeds.
        let (bytes, sha256) = super::stream_hash_file(&timeline_path).expect("rehash timeline");
        for entry in &mut manifest.files {
            if entry.path == super::TIMELINE_DB_FILE {
                entry.bytes = bytes;
                entry.sha256 = sha256.clone();
            }
        }
        std::fs::write(
            dest.join(super::MANIFEST_V2_FILE),
            serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
        )
        .expect("write updated manifest");

        let reader = V2BundleReader::open(&dest).expect("pre-t6h readonly open must succeed");
        reader
            .verify_integrity()
            .expect("pre-t6h integrity must pass");
        let taps = reader
            .query_taps(Some(RecordBucket::Main), TimeRange::all(), None)
            .expect("pre-t6h query_taps must succeed without no-such-column error");
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0].action, "DOWN");
        assert_eq!(taps[0].gesture_kind, GestureKind::Tap);
        assert_eq!(taps[0].end_x, None);
        assert_eq!(taps[0].end_y, None);
        assert_eq!(taps[0].end_t_offset_ns, None);
        assert_eq!(taps[0].path, Vec::new());

        // Partial schema (only gesture_kind present, missing the remaining 4 columns) must also
        // fall back cleanly instead of failing with `no such column: end_x`.
        {
            let conn = rusqlite::Connection::open(&timeline_path).expect("open sqlite rw");
            conn.execute_batch("ALTER TABLE taps ADD COLUMN gesture_kind TEXT;")
                .expect("add single partial column");
        }
        let partial_taps = reader
            .query_taps(Some(RecordBucket::Main), TimeRange::all(), None)
            .expect("partial-schema query_taps must fall back cleanly");
        assert_eq!(partial_taps.len(), 1);
        assert_eq!(partial_taps[0].gesture_kind, GestureKind::Tap);
    }
}
