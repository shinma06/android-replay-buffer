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
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use arb_core::BUNDLE_SCHEMA_VERSION;
use arb_core::bundle::{
    DeviceInfo, EpochSpan, FileDigest, FrameRecord, InputStreamMeta, LogCompleteness, LogRecord,
    LogcatStreamMeta, ManifestV2, Provenance, RecordBucket, RequestedWindow, StreamStatus,
    StreamsSummary, TapRecord, UnanchoredSummary, VideoStreamMeta,
};
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
  uncertainty_reason TEXT
);

CREATE TABLE clock_anchors (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  boot_id TEXT NOT NULL,
  payload_json TEXT NOT NULL
);

CREATE INDEX idx_frames_boottime_ns ON frames(aligned_boottime_ns);
CREATE INDEX idx_frames_gop_id ON frames(gop_id);
CREATE INDEX idx_discontinuities_boottime_ns ON discontinuities(start_boottime_ns);
CREATE INDEX idx_logs_bucket_boottime_ns ON logs(bucket, aligned_boottime_ns);
CREATE INDEX idx_logs_pc_ingest_cont_ns ON logs(pc_ingest_cont_ns);
CREATE INDEX idx_taps_bucket_boottime_ns ON taps(bucket, aligned_boottime_ns);
CREATE INDEX idx_clock_anchors_boot_id ON clock_anchors(boot_id);
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
    /// Full v2 manifest when `format == BundleFormat::V2`.
    pub manifest_v2: Option<ManifestV2>,
    /// Full legacy metadata when `format` is schema 3 or 4.
    pub legacy_metadata: Option<LegacyBundleMetadata>,
}

/// Unified read-only bundle reader interface implemented for v2 and legacy bundles.
pub trait BundleReader: fmt::Debug + Send + Sync {
    /// Returns the unified manifest summary for this bundle.
    fn manifest_summary(&self) -> &BundleManifestSummary;

    /// Returns all video frame index records ordered by `frame_index`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if reading or decoding frame rows fails.
    fn frames(&self) -> Result<Vec<FrameRecord>, StorageError>;

    /// Returns all recorded timeline discontinuities ordered chronologically.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if reading or decoding discontinuity rows fails.
    fn discontinuities(&self) -> Result<Vec<DiscontinuityEvent>, StorageError>;

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
    ) -> Result<Vec<LogRecord>, StorageError>;

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
        let mut files = Vec::new();

        if let Some(video) = &draft.video_mp4 {
            let digest = stage_media_file(&staging, VIDEO_MP4_FILE, video)?;
            files.push(digest);
        }

        if let Some(audio) = &draft.audio_m4a {
            let digest = stage_media_file(&staging, AUDIO_M4A_FILE, audio)?;
            files.push(digest);
        }

        let timeline_path = staging.staging_path().join(TIMELINE_DB_FILE);
        write_timeline_sqlite(&timeline_path, draft)?;
        let timeline_digest = hash_and_sync_file(&timeline_path, TIMELINE_DB_FILE)?;
        files.push(timeline_digest);

        files.sort_by(|a, b| a.path.cmp(&b.path));

        let manifest = ManifestV2 {
            schema_version: BUNDLE_SCHEMA_VERSION,
            bundle_id: draft.bundle_id.clone(),
            created_at_utc: draft.created_at_utc.clone(),
            provenance: draft.provenance.clone(),
            device: draft.device.clone(),
            requested_window: draft.requested_window.clone(),
            epochs: draft.epochs.clone(),
            streams: draft.streams.clone(),
            unanchored_summary: draft.unanchored_summary,
            files,
        };
        manifest.validate()?;

        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|source| StorageError::json("serialize_manifest_v2", source))?;
        staging.write_file_synced(MANIFEST_V2_FILE, &manifest_bytes)?;

        staging.commit()?;
        Ok(manifest)
    }
}

/// Removes `<parent>/.staging-<job_id>` left behind by a crashed earlier attempt (managed
/// roots are swept by `Catalog::recover_on_startup`; external destinations are not).
fn remove_stale_staging_sibling(final_dir: &Path, job_id: &str) -> Result<(), StorageError> {
    let Some(parent) = final_dir.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(());
    };
    let stale = parent.join(format!(".staging-{job_id}"));
    match fs::symlink_metadata(&stale) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(&stale)
            .map_err(|source| StorageError::io("remove_stale_staging_dir", &stale, source)),
        Ok(_) => fs::remove_file(&stale)
            .map_err(|source| StorageError::io("remove_stale_staging_entry", &stale, source)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StorageError::io("stat_stale_staging_dir", &stale, source)),
    }
}

/// Returns the manifest of an already committed, intact bundle with `bundle_id` at
/// `final_dir`, if that is what is there.
fn existing_committed_manifest(final_dir: &Path, bundle_id: &str) -> Option<ManifestV2> {
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
        let root_dir = bundle_dir.as_ref().to_path_buf();
        let manifest_path = root_dir.join(MANIFEST_V2_FILE);
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

        let timeline_path = root_dir.join(TIMELINE_DB_FILE);
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
            created_at_utc: Some(manifest.created_at_utc.clone()),
            provenance: manifest.provenance.clone(),
            device: Some(manifest.device.clone()),
            streams: manifest.streams.clone(),
            unanchored_summary: manifest.unanchored_summary,
            epochs: manifest.epochs.clone(),
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
        for rel in required {
            if !self.manifest_lists(rel) {
                return Err(StorageError::IntegrityMismatch {
                    path: rel.to_owned(),
                    reason: "required bundle file is not listed in manifest.files".to_owned(),
                });
            }
        }

        for entry in &self.manifest.files {
            let file_path = self.root_dir.join(&entry.path);
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
        open_readonly_sqlite(&self.root_dir.join(TIMELINE_DB_FILE))
    }
}

impl BundleReader for V2BundleReader {
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

    fn query_logs(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        limit: Option<usize>,
    ) -> Result<Vec<LogRecord>, StorageError> {
        let conn = self.open_timeline_ro()?;
        let bucket_str = bucket.map(record_bucket_to_str);
        let limit_i64 = match limit {
            Some(n) => i64::try_from(n).unwrap_or(i64::MAX),
            None => -1,
        };

        let mut stmt = conn
            .prepare(
                "SELECT seq, boot_id, header_epoch_ns, device_read_boottime_ns,
                        pc_ingest_cont_ns, aligned_boottime_ns, min_boottime_ns,
                        max_boottime_ns, certainty, bucket, level, tag, pid, tid,
                        message, half_width_ns, semantics, uncertainty_reason
                 FROM logs
                 WHERE (?1 IS NULL OR bucket = ?1)
                   AND (?2 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) >= ?2)
                   AND (?3 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) <= ?3)
                 ORDER BY seq ASC
                 LIMIT ?4",
            )
            .map_err(|source| StorageError::sqlite("prepare_query_logs", source))?;

        let rows = stmt
            .query_map(
                params![bucket_str, range.start_ns, range.end_ns, limit_i64],
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

        let mut stmt = conn
            .prepare(
                "SELECT seq, boot_id, event_mono_ns, pc_ingest_cont_ns,
                        aligned_boottime_ns, certainty, bucket, action, x, y,
                        pointer_id, half_width_ns, min_boottime_ns, max_boottime_ns,
                        semantics, uncertainty_reason
                 FROM taps
                 WHERE (?1 IS NULL OR bucket = ?1)
                   AND (?2 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) >= ?2)
                   AND (?3 IS NULL OR COALESCE(aligned_boottime_ns, pc_ingest_cont_ns) <= ?3)
                 ORDER BY seq ASC
                 LIMIT ?4",
            )
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
                    })
                },
            )
            .map_err(|source| StorageError::sqlite("query_taps", source))?;

        let mut records = Vec::new();
        for row in rows {
            let raw = row.map_err(|source| StorageError::sqlite("read_tap_row", source))?;
            records.push(decode_tap_row(raw)?);
        }
        Ok(records)
    }

    fn video_path(&self) -> Option<PathBuf> {
        // Never serve a media file the manifest (and thus integrity checks) does not cover.
        if !self.manifest_lists(VIDEO_MP4_FILE) {
            return None;
        }
        let path = self.root_dir.join(VIDEO_MP4_FILE);
        path.is_file().then_some(path)
    }

    fn audio_path(&self) -> Option<PathBuf> {
        if !self.manifest_lists(AUDIO_M4A_FILE) {
            return None;
        }
        let path = self.root_dir.join(AUDIO_M4A_FILE);
        path.is_file().then_some(path)
    }

    fn is_legacy(&self) -> bool {
        false
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
        let device_logs = load_legacy_logs(
            &root_dir,
            metadata.files.logcat_device.as_deref(),
            origin_ns,
        )?;
        let app_logs =
            load_legacy_logs(&root_dir, metadata.files.logcat_app.as_deref(), origin_ns)?;
        let has_device_log_file = metadata
            .files
            .logcat_device
            .as_deref()
            .and_then(|rel| legacy_file_path(&root_dir, rel).ok())
            .is_some_and(|path| path.is_file());
        let logs = if !device_logs.is_empty() || has_device_log_file {
            device_logs
        } else {
            app_logs.clone()
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
    fn manifest_summary(&self) -> &BundleManifestSummary {
        &self.summary
    }

    fn frames(&self) -> Result<Vec<FrameRecord>, StorageError> {
        Ok(self.frames.clone())
    }

    fn discontinuities(&self) -> Result<Vec<DiscontinuityEvent>, StorageError> {
        Ok(self.discontinuities.clone())
    }

    fn query_logs(
        &self,
        bucket: Option<RecordBucket>,
        range: TimeRange,
        limit: Option<usize>,
    ) -> Result<Vec<LogRecord>, StorageError> {
        let mut results = Vec::new();
        for record in &self.logs {
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
    let file =
        File::open(path).map_err(|source| StorageError::io("open_for_fsync", path, source))?;
    fsync_file(&file)?;
    drop(file);

    let (bytes, sha256) = stream_hash_file(path)?;
    Ok(FileDigest {
        path: rel_name.to_owned(),
        bytes,
        sha256,
    })
}

fn stream_hash_file(path: &Path) -> Result<(u64, String), StorageError> {
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
                message, half_width_ns, semantics, uncertainty_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        )
        .map_err(|source| StorageError::sqlite("prepare_insert_logs", source))?;

    for log in logs {
        let cols = encode_alignment(&log.alignment)?;
        let seq = u64_to_i64(log.seq, "log.seq")?;
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
                semantics, uncertainty_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )
        .map_err(|source| StorageError::sqlite("prepare_insert_taps", source))?;

    for tap in taps {
        let cols = encode_alignment(&tap.alignment)?;
        let seq = u64_to_i64(tap.seq, "tap.seq")?;
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
    })
}

// ---------------------------------------------------------------------------
// Legacy Read-Only Projection Helpers
// ---------------------------------------------------------------------------

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
) -> Result<Vec<LogRecord>, StorageError> {
    let Some(file_name) = rel_file else {
        return Ok(Vec::new());
    };
    let path = legacy_file_path(root_dir, file_name)?;
    let text = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
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
            message: row.message,
        });
    }
    Ok(records)
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
        }),
        audio: None,
        logcat: Some(LogcatStreamMeta {
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
        created_at_utc: meta.created_at.clone(),
        provenance: Provenance::Original,
        device: None,
        streams,
        unanchored_summary: UnanchoredSummary {
            included_count: unanchored_count,
            truncated_count: 0,
        },
        epochs,
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
        DeviceInfo, EpochSpan, FrameRecord, InputStreamMeta, LogCompleteness, LogRecord,
        LogcatStreamMeta, Provenance, RecordBucket, RequestedWindow, StreamStatus, StreamsSummary,
        TapRecord, UnanchoredSummary, VideoStreamMeta,
    };
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
                message: "unanchored log".to_owned(),
            },
        ]
    }

    fn sample_draft() -> V2BundleDraft {
        V2BundleDraft {
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
                }),
                audio: None,
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
                included_count: 1,
                truncated_count: 0,
            },
            video_mp4: Some(MediaPayload::Bytes(b"fake-mp4-data".to_vec())),
            audio_m4a: Some(MediaPayload::Bytes(b"fake-m4a-data".to_vec())),
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
            taps: vec![TapRecord {
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
                action: "DOWN".to_owned(),
                x: Some(120.5),
                y: Some(340.25),
                pointer_id: Some(0),
            }],
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
            3,
            "manifest must list audio.m4a, timeline.sqlite, and video.mp4"
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
}
