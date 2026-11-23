//! Versioned JSON-RPC 2.0 / NDJSON control-plane protocol schemas (`SPEC-03`).

use std::path::PathBuf;

use arb_adb::adb::AdbDeviceInfo;
use arb_core::analytics::{CrashMarker, ScreenSpan};
use arb_core::bundle::{
    BundleKind, DeviceInfo, EpochSpan, LogRecord, Provenance, StreamsSummary, TapRecord,
    UnanchoredSummary,
};
use arb_core::config::ArbConfig;
use arb_core::selection::SelectionPolicy;
use arb_core::state::{BundleLifecycleState, SaveJobState, SaveTriggerKind};
use arb_core::time::DiscontinuityEvent;
use arb_media::trim::{PngExportReport, TrimReport};
use arb_storage::bundle_io::BundleFormat;
use arb_storage::catalog::{CatalogFormatVersion, LeaseHolderKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Control-plane JSON-RPC 2.0 protocol version exposed by `system.hello`.
pub const IPC_PROTOCOL_VERSION: u32 = 1;

/// Default `limit` applied to `timeline.query_logs` / `timeline.query_taps` when omitted.
pub const DEFAULT_QUERY_LIMIT: usize = 10_000;

/// Maximum accepted `limit` for `timeline.query_logs` / `timeline.query_taps`.
pub const MAX_QUERY_LIMIT: usize = 100_000;

/// Maximum number of concurrently open SHM viewports across all clients.
pub const MAX_VIEWPORTS: usize = 8;

/// JSON-RPC 2.0 version literal.
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC 2.0 request envelope sent over length-capped NDJSON IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    /// Must equal `"2.0"`.
    pub jsonrpc: String,
    /// Client correlation identifier (number, string, or null).
    #[serde(default)]
    pub id: Value,
    /// Method name (e.g., `"system.hello"`, `"save.trigger"`, `"media.seek_frame"`).
    pub method: String,
    /// Optional structured parameters object.
    #[serde(default)]
    pub params: Value,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcErrorObject {
    /// Standard or application error code.
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured diagnostic details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 2.0 response envelope returned over NDJSON IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Echoed request correlation identifier.
    pub id: Value,
    /// Present on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Present on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorObject>,
}

impl RpcResponse {
    /// Constructs a JSON-RPC 2.0 success response.
    #[must_use]
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Constructs a JSON-RPC 2.0 error response.
    #[must_use]
    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(RpcErrorObject {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// ---------------- System ----------------

/// Response payload for `system.hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemHelloResponse {
    /// Control-plane IPC protocol version.
    pub protocol_version: u32,
    /// Bundle manifest schema version (`2`).
    pub schema_version: u32,
    /// Daemon host OS process ID.
    pub pid: u32,
    /// Daemon data directory.
    pub data_dir: PathBuf,
    /// Daemon cache directory.
    pub cache_dir: PathBuf,
    /// Daemon runtime directory.
    pub runtime_dir: PathBuf,
    /// Daemon crate version string.
    pub daemon_version: String,
}

/// Live capture telemetry inside [`SystemStatusResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStatusView {
    /// Whether capture is currently active.
    pub active: bool,
    /// Whether transport is currently connected.
    pub connected: bool,
    /// Active device serial, if capturing.
    pub serial: Option<String>,
    /// Active device `boot_id`, if observed.
    pub boot_id: Option<String>,
    /// Whether capture stopped due to host storage pressure (`StoragePressureStop`).
    pub storage_pressure_stopped: bool,
    /// Count of captured video frames currently buffered.
    pub video_frames: usize,
    /// Count of captured audio frames currently buffered.
    pub audio_frames: usize,
    /// Count of captured log records currently buffered.
    pub log_records: usize,
    /// Count of captured tap records currently buffered.
    pub tap_records: usize,
    /// Count of recorded discontinuities in the live session.
    pub discontinuities: usize,
    /// Count of `StaticHold` detections in the live session.
    pub static_hold_events: usize,
    /// Count of Zone-1 ring chunk writes that failed with an I/O error (each recorded as a
    /// `stream_gap` discontinuity).
    #[serde(default)]
    pub ring_write_errors: usize,
}

/// Startup recovery summary inside [`SystemStatusResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverySummaryView {
    /// Count of orphan `.staging-*` directories removed at startup.
    pub removed_staging_dirs: usize,
    /// Count of dead-PID leases reaped at startup.
    pub reaped_leases: usize,
    /// Count of interrupted save jobs transitioned to `FailedRetryable` at startup.
    pub recovered_jobs: usize,
    /// Count of previously `Purging` bundles finalized at startup.
    pub completed_purging_bundles: usize,
    /// Count of pin journals released at startup (job completed/discarded/unknown).
    #[serde(default)]
    pub released_pin_journals: usize,
    /// Count of pin journals re-created at startup for active/retryable jobs.
    #[serde(default)]
    pub repinned_jobs: usize,
}

/// Crash auto-save telemetry inside [`SystemStatusResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CrashAutoSaveStatusView {
    /// Whether live `am_crash` auto-save is currently enabled in config.
    pub enabled: bool,
    /// Configured debounce cooldown in seconds.
    pub cooldown_secs: u32,
    /// Total live fatal `am_crash` events observed since daemon start.
    pub detected_crashes: u64,
    /// Total background auto-saves triggered since daemon start.
    pub triggered_saves: u64,
    /// Crashes suppressed due to the debounce cooldown window.
    pub suppressed_cooldown: u64,
    /// Crashes suppressed because `STORAGE_PRESSURE_STOP` was active.
    pub suppressed_storage_pressure: u64,
    /// `true` while a background crash auto-save is actively running.
    pub in_flight: bool,
    /// Process name of the most recently detected fatal crash, if any.
    pub last_crash_process: Option<String>,
    /// Exception class of the most recently detected fatal crash, if any.
    pub last_crash_exception: Option<String>,
    /// Save job ID of the most recently triggered crash auto-save, if any.
    pub last_job_id: Option<String>,
    /// Completed bundle ID of the most recent successful crash auto-save, if any.
    pub last_bundle_id: Option<String>,
    /// Diagnostic error from the last crash auto-save attempt, if it failed.
    pub last_error: Option<String>,
}

/// Response payload for `system.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemStatusResponse {
    /// Daemon host OS process ID.
    pub pid: u32,
    /// Daemon uptime in milliseconds.
    pub uptime_ms: u64,
    /// Live capture status snapshot.
    pub capture: CaptureStatusView,
    /// Number of Zone-1 ring chunks currently stored on disk.
    pub ring_chunk_count: usize,
    /// Total bytes across all Zone-1 ring chunks.
    pub ring_total_bytes: u64,
    /// Number of pinned Zone-1 ring chunks protected by active save jobs.
    pub ring_pinned_chunk_count: usize,
    /// Number of currently open SHM viewports.
    pub active_viewports: usize,
    /// Number of non-terminal save jobs in the catalog.
    pub active_save_jobs: usize,
    /// Startup crash recovery report summary.
    pub recovery: RecoverySummaryView,
    /// Live `am_crash` debounced auto-save telemetry.
    #[serde(default)]
    pub crash_autosave: CrashAutoSaveStatusView,
}

// ---------------- Persisted Config ----------------

/// Response payload for `config.get` and `config.update`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigResponse {
    /// Active validated user configuration.
    pub config: ArbConfig,
    /// Filesystem path of the persisted `config.json` file.
    pub config_path: PathBuf,
}

// ---------------- Device & Capture ----------------

/// Response payload for `device.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceListResponse {
    /// Devices discovered via `adb devices -l`.
    pub devices: Vec<AdbDeviceInfo>,
}

/// Request parameters for `capture.start`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptureStartParams {
    /// Optional ADB device serial (auto-selects sole online device if omitted).
    pub serial: Option<String>,
    /// Optional explicit path to `arb-companion.dex.jar`.
    pub companion_jar: Option<PathBuf>,
    /// Optional explicit path to `replay-scrcpy-audio.dex.jar`.
    pub scrcpy_jar: Option<PathBuf>,
    /// Maximum encoded video dimension (`1024` or `1920`, default `1024`).
    pub max_size: Option<u32>,
    /// Target maximum frame rate (`15` or `30`, default `30`).
    pub max_fps: Option<u32>,
    /// Target H.264 bitrate in bits per second (default `4_000_000`).
    pub video_bit_rate: Option<u32>,
    /// Enable playback audio capture on API >= 33 (default `true`).
    pub audio_enabled: Option<bool>,
    /// Enable companion logcat stream (default `true`).
    pub logcat_enabled: Option<bool>,
    /// Enable companion touch/input stream (default `true`).
    pub input_enabled: Option<bool>,
    /// Companion beacon heartbeat interval in milliseconds (default `250`).
    pub heartbeat_ms: Option<u32>,
}

/// Response payload for `capture.start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStartResponse {
    /// Target device serial.
    pub serial: String,
    /// `true` once live capture has been started.
    pub started: bool,
}

/// Response payload for `capture.stop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStopResponse {
    /// `true` if an active capture session was stopped.
    pub stopped: bool,
    /// Final buffered event counts after stopping.
    pub capture: CaptureStatusView,
}

// ---------------- Save Jobs ----------------

/// Payload mode for `save.trigger` / `save.logs`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveMode {
    /// Full bundle including video, optional audio, logcat, and input.
    #[default]
    Full,
    /// Log-only WORM v2 bundle (`timeline.sqlite` + `logs.jsonl` + `unanchored.jsonl`, no video/audio).
    LogsOnly,
}

/// Request parameters for `save.trigger`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SaveTriggerParams {
    /// Requested lookback duration in real elapsed seconds (defaults to `config.save_window_secs`).
    #[serde(alias = "window_secs")]
    pub duration_sec: Option<u64>,
    /// Optional destination parent directory (defaults to `config.output_dir_override` or `<data_dir>/bundles`).
    pub dest_dir: Option<PathBuf>,
    /// Optional boundary drain timeout in milliseconds (default `500`).
    pub drain_timeout_ms: Option<u64>,
    /// Optional explicit selection policy overrides (useful in tests).
    pub selection_policy: Option<SelectionPolicy>,
    /// Save payload mode (`full` default, or `logs_only`).
    #[serde(default)]
    pub mode: SaveMode,
}

/// Request parameters for `save.logs` (dedicated log-only WORM bundle save).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SaveLogsParams {
    /// Requested lookback duration in real elapsed seconds (defaults to `config.save_window_secs`).
    #[serde(alias = "duration_sec")]
    pub window_secs: Option<u64>,
    /// Optional destination parent directory (defaults to `config.output_dir_override` or `<data_dir>/bundles`).
    pub dest_dir: Option<PathBuf>,
    /// Optional boundary drain timeout in milliseconds (default `500`).
    pub drain_timeout_ms: Option<u64>,
    /// Optional explicit selection policy overrides (useful in tests).
    pub selection_policy: Option<SelectionPolicy>,
}

/// Request parameters for `save.screenshot`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SaveScreenshotParams {
    /// Optional ADB serial (defaults to active capture serial or sole online ADB device).
    pub serial: Option<String>,
    /// Optional explicit absolute PNG destination path (defaults to `<output_dir>/screenshot-*.png`).
    pub output_path: Option<PathBuf>,
    /// Whether an existing destination file may be atomically replaced (default `false`).
    #[serde(default)]
    pub overwrite: bool,
}

/// Response payload for `save.screenshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveScreenshotResponse {
    /// Written PNG file path.
    pub path: PathBuf,
    /// Validated PNG width in pixels.
    pub width: u32,
    /// Validated PNG height in pixels.
    pub height: u32,
    /// Lowercase 64-character hexadecimal SHA-256 digest of the PNG file.
    pub sha256: String,
    /// Host Unix epoch milliseconds when the screenshot was captured.
    pub captured_at_unix_ms: u64,
    /// Android device ADB serial from which the screenshot was captured.
    pub serial: String,
}

/// Serialized view of a persisted save job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveJobView {
    /// Unique save job identifier.
    pub job_id: String,
    /// Lifecycle state (`pinned`, `draining`, `materializing`, `failed_retryable`, `completed`, `discarded`).
    pub state: SaveJobState,
    /// Origin that initiated this save job (`manual` or `crash_auto_save`).
    #[serde(default)]
    pub trigger: SaveTriggerKind,
    /// Target bundle directory path.
    pub destination_path: PathBuf,
    /// Ring chunk IDs currently pinned by this job.
    pub pinned_chunk_ids: Vec<String>,
    /// Last error message when in `failed_retryable`.
    pub last_error: Option<String>,
    /// Creation UTC timestamp.
    pub created_at_utc: String,
    /// Last update UTC timestamp.
    pub updated_at_utc: String,
}

/// Response payload for `save.trigger`, `save.retry`, and `save.retarget`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveOutcomeResponse {
    /// Save job snapshot after execution.
    pub job: SaveJobView,
    /// Materialized bundle ID when `job.state == SaveJobState::Completed`.
    pub bundle_id: Option<String>,
    /// Materialized bundle directory path when completed.
    pub bundle_path: Option<PathBuf>,
    /// `true` when post-trigger boundary drain timed out before receiving a heartbeat.
    pub tail_may_be_incomplete: bool,
    /// Summary of quarantined `unanchored` records when completed.
    pub unanchored_summary: Option<UnanchoredSummary>,
    /// Non-fatal warning, e.g. the bundle was committed but its parent directory `fsync`
    /// failed (`CommittedParentSyncFailed`): the bundle is published and cataloged anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Response payload for `save.list_jobs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveListJobsResponse {
    /// All tracked save jobs.
    pub jobs: Vec<SaveJobView>,
}

/// Request parameters for `save.retry` and `save.discard`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveJobIdParams {
    /// Save job identifier.
    pub job_id: String,
}

/// Request parameters for `save.retarget`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveRetargetParams {
    /// Save job identifier.
    pub job_id: String,
    /// Replacement destination parent directory (or full bundle directory path).
    pub dest_dir: PathBuf,
}

// ---------------- Bundles & Leases ----------------

/// Serialized catalog bundle entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleEntryView {
    /// Globally unique bundle identifier.
    pub bundle_id: String,
    /// On-disk bundle directory path.
    pub path: PathBuf,
    /// Storage format version (`v2` or `legacy_ro`).
    pub format_version: CatalogFormatVersion,
    /// Lifecycle state (`active`, `trashed`, `purging`).
    pub state: BundleLifecycleState,
    /// UTC timestamp when moved to trash, if trashed.
    pub trashed_at_utc: Option<String>,
    /// Parent bundle ID for trimmed bundles, if any.
    pub parent_bundle_id: Option<String>,
    /// Creation UTC timestamp.
    pub created_at_utc: String,
    /// Count of active leases currently referencing this bundle.
    pub active_leases: usize,
}

/// Request parameters for `bundle.list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BundleListParams {
    /// Optional lifecycle state filter (`active` or `trashed`).
    pub state: Option<BundleLifecycleState>,
}

/// Response payload for `bundle.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleListResponse {
    /// Matching catalog entries.
    pub bundles: Vec<BundleEntryView>,
}

/// Request parameters for `bundle.open`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleOpenParams {
    /// Catalog `bundle_id` or filesystem path to a v2 or legacy schema 3/4 bundle folder.
    pub bundle_id_or_path: String,
    /// Lease holder category (default `cli`).
    pub holder_kind: Option<LeaseHolderKind>,
    /// Lease holder PID. Required: it must be the live client process (never the daemon),
    /// so a crashed client's lease is reaped by `bundle.gc` / startup recovery.
    pub holder_pid: Option<u32>,
}

/// Response payload for `bundle.open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleOpenResponse {
    /// Catalog `bundle_id`.
    pub bundle_id: String,
    /// Resolved on-disk directory path.
    pub path: PathBuf,
    /// Detected bundle format (`v2`, `legacy_schema4`, or `legacy_schema3`).
    pub format: BundleFormat,
    /// `true` for read-only legacy bundles.
    pub is_legacy: bool,
    /// Acquired viewer/CLI lease ID protecting this bundle from GC.
    pub lease_id: String,
    /// Timeline summary for the opened bundle.
    pub summary: TimelineSummaryResponse,
}

/// Request parameters for `bundle.lease_renew` and `bundle.close`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseIdParams {
    /// Active lease identifier.
    pub lease_id: String,
}

/// Request parameters for `bundle.trash` and `bundle.restore`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleIdParams {
    /// Catalog bundle identifier.
    pub bundle_id: String,
}

/// Request parameters for `bundle.gc`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BundleGcParams {
    /// Optional UTC cutoff timestamp (defaults to current UTC time).
    pub now_utc: Option<String>,
}

/// One bundle whose purge failed during `bundle.gc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleGcFailure {
    /// Bundle identifier left in `purging` for retry.
    pub bundle_id: String,
    /// Human-readable failure description.
    pub error: String,
}

/// Response payload for `bundle.gc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleGcResponse {
    /// Managed trashed bundle IDs physically deleted.
    pub purged_bundle_ids: Vec<String>,
    /// Trashed bundles preserved due to active leases.
    pub skipped_leased_bundle_ids: Vec<String>,
    /// Trashed bundles preserved due to active save jobs.
    pub skipped_active_job_bundle_ids: Vec<String>,
    /// Trashed bundles skipped because legacy or unmanaged.
    pub skipped_unmanaged_or_legacy_bundle_ids: Vec<String>,
    /// Trashed bundles still within grace period.
    pub skipped_in_grace_bundle_ids: Vec<String>,
    /// Bundles whose physical purge failed this run (e.g. a Windows sharing violation).
    /// They stay `purging` and are retried by the next `bundle.gc`.
    #[serde(default)]
    pub failed: Vec<BundleGcFailure>,
    /// Leases reaped by this GC run because their holder process was dead.
    #[serde(default)]
    pub reaped_lease_ids: Vec<String>,
}

// ---------------- Timeline ----------------

/// Request parameters identifying a bundle by ID or path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleRefParams {
    /// Catalog `bundle_id` or filesystem path.
    pub bundle_id_or_path: String,
}

/// Bucket filter selector for timeline log/tap queries.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryBucket {
    /// Main synchronized timeline bucket.
    Main,
    /// Quarantined `unanchored` sidecar bucket.
    Unanchored,
    /// Return records from both buckets.
    All,
}

/// Request parameters for `timeline.query_logs` and `timeline.query_taps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineQueryParams {
    /// Catalog `bundle_id` or filesystem path.
    pub bundle_id_or_path: String,
    /// Bucket filter (`main`, `unanchored`, or `all`; default `all`).
    pub bucket: Option<QueryBucket>,
    /// Optional inclusive start timestamp in nanoseconds.
    pub start_ns: Option<i64>,
    /// Optional inclusive end timestamp in nanoseconds.
    pub end_ns: Option<i64>,
    /// Optional maximum row count (default [`DEFAULT_QUERY_LIMIT`], capped at
    /// [`MAX_QUERY_LIMIT`]).
    pub limit: Option<usize>,
}

/// Response payload for `timeline.summary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSummaryResponse {
    /// Bundle identifier.
    pub bundle_id: String,
    /// Resolved on-disk directory path.
    pub path: PathBuf,
    /// Detected bundle format.
    pub format: BundleFormat,
    /// High-level bundle payload kind (`full` or `logs_only`).
    #[serde(default)]
    pub bundle_kind: BundleKind,
    /// `true` for legacy schema 3/4 bundles.
    pub is_legacy: bool,
    /// Creation timestamp in UTC, if known.
    pub created_at_utc: Option<String>,
    /// Bundle provenance (original or trimmed).
    pub provenance: Provenance,
    /// Device metadata, if recorded.
    pub device: Option<DeviceInfo>,
    /// Per-stream presence and honesty summary.
    pub streams: StreamsSummary,
    /// Quarantined `unanchored` record summary.
    pub unanchored_summary: UnanchoredSummary,
    /// Covered device boot epochs.
    pub epochs: Vec<EpochSpan>,
    /// Total indexed video frames.
    pub frame_count: usize,
    /// Total video IDR keyframes.
    pub keyframe_count: usize,
    /// Count of main-timeline log records.
    pub log_main_count: usize,
    /// Count of quarantined `unanchored` log records.
    pub log_unanchored_count: usize,
    /// Count of main-timeline tap records.
    pub tap_main_count: usize,
    /// Count of quarantined `unanchored` tap records.
    pub tap_unanchored_count: usize,
    /// Count of recorded discontinuities.
    pub discontinuity_count: usize,
    /// Path to `video.mp4` if present.
    pub video_path: Option<PathBuf>,
    /// Path to `audio.m4a` if present.
    pub audio_path: Option<PathBuf>,
}

/// Response payload for `timeline.query_logs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineLogsResponse {
    /// Matching log records.
    pub logs: Vec<LogRecord>,
}

/// Response payload for `timeline.query_taps`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineTapsResponse {
    /// Matching tap records.
    pub taps: Vec<TapRecord>,
}

/// Response payload for `timeline.discontinuities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineDiscontinuitiesResponse {
    /// Recorded timeline discontinuities.
    pub discontinuities: Vec<DiscontinuityEvent>,
}

/// Response payload for `timeline.crashes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCrashesResponse {
    /// Structured crash markers extracted from the bundle's logcat.
    pub crashes: Vec<CrashMarker>,
}

/// Response payload for `timeline.screens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineScreensResponse {
    /// PC-derived Activity / screen transition spans from the bundle's logcat.
    pub screens: Vec<ScreenSpan>,
}

// ---------------- Edit Session ----------------

/// Serialized non-destructive editor session view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditSessionView {
    /// Unique edit session identifier.
    pub session_id: String,
    /// Referenced immutable bundle identifier.
    pub bundle_id: String,
    /// Optional inclusive trim start frame index.
    pub in_frame_index: Option<u64>,
    /// Optional inclusive trim end frame index.
    pub out_frame_index: Option<u64>,
    /// Optional playhead frame index.
    pub playhead_frame_index: Option<u64>,
    /// Optional manual A/V sync offset override in milliseconds.
    pub av_offset_override_ms: Option<i64>,
    /// Last update UTC timestamp.
    pub updated_at_utc: String,
}

/// Request parameters for `edit.get_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditGetSessionParams {
    /// Edit session identifier.
    pub session_id: String,
}

/// Request parameters for `edit.update_session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditUpdateSessionParams {
    /// Edit session identifier.
    pub session_id: String,
    /// Referenced bundle identifier.
    pub bundle_id: String,
    /// Optional inclusive trim start frame index.
    pub in_frame_index: Option<u64>,
    /// Optional inclusive trim end frame index.
    pub out_frame_index: Option<u64>,
    /// Optional playhead frame index.
    pub playhead_frame_index: Option<u64>,
    /// Optional manual A/V offset override in milliseconds.
    pub av_offset_override_ms: Option<i64>,
}

// ---------------- Export (Trim & PNG) ----------------

/// Request parameters for `export.trim`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportTrimParams {
    /// Source catalog `bundle_id` or filesystem path.
    pub bundle_id_or_path: String,
    /// Inclusive integer starting frame index.
    pub in_frame_index: u64,
    /// Inclusive integer ending frame index.
    pub out_frame_index: u64,
    /// Optional destination parent directory (defaults to `<data_dir>/bundles`).
    pub dest_dir: Option<PathBuf>,
    /// Optional client PID for the temporary `ExportJob` lease.
    pub holder_pid: Option<u32>,
}

/// Response payload for `export.trim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportTrimResponse {
    /// Newly created trimmed v2 bundle ID.
    pub bundle_id: String,
    /// Filesystem path of the newly committed trimmed v2 bundle.
    pub bundle_path: PathBuf,
    /// Video trim metrics (zero `edts`/`elst`, exact frame count).
    pub trim_report: TrimReport,
    /// Carried-over and newly-quarantined `unanchored` record summary.
    pub unanchored_summary: UnanchoredSummary,
    /// Main-timeline logs included in the trimmed bundle.
    pub main_log_count: usize,
    /// Quarantined `unanchored` logs carried into the trimmed bundle.
    pub unanchored_log_count: usize,
    /// Main-timeline taps included in the trimmed bundle.
    pub main_tap_count: usize,
    /// Quarantined `unanchored` taps carried into the trimmed bundle.
    pub unanchored_tap_count: usize,
    /// Non-fatal warning (e.g. bundle committed but parent directory `fsync` failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Request parameters for `export.png`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPngParams {
    /// Source catalog `bundle_id` or filesystem path.
    pub bundle_id_or_path: String,
    /// Exact integer frame index to export.
    pub frame_index: u64,
    /// Output PNG file path.
    pub output_path: PathBuf,
}

/// Response payload for `export.png`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPngResponse {
    /// Written PNG file path.
    pub output_path: PathBuf,
    /// PNG export dimensions, backend, and size report.
    pub report: PngExportReport,
}

// ---------------- Media / SHM Viewport ----------------

/// Request parameters for `media.open_viewport`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaOpenViewportParams {
    /// Catalog `bundle_id` or filesystem path.
    pub bundle_id_or_path: String,
}

/// Response payload for `media.open_viewport`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaOpenViewportResponse {
    /// Unique viewport handle identifier.
    pub viewport_id: String,
    /// Resolved bundle identifier.
    pub bundle_id: String,
    /// Backing mmap file path for [`arb_os::shm::ShmFrameRingReader`].
    pub shm_path: PathBuf,
    /// Video frame width in pixels.
    pub width: u32,
    /// Video frame height in pixels.
    pub height: u32,
    /// Row stride in bytes for full-resolution `rgba8` frames.
    pub stride: u32,
    /// Pixel format identifier (`"rgba8"`).
    pub pixel_format: String,
    /// Maximum payload bytes per SHM ring slot.
    pub slot_capacity: usize,
    /// Total video frames in the opened track.
    pub frame_count: u64,
    /// Total IDR keyframes in the opened track.
    pub keyframe_count: usize,
}

/// Request parameters for `media.filmstrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFilmstripParams {
    /// Open viewport identifier.
    pub viewport_id: String,
    /// Optional thumbnail maximum width in pixels.
    pub max_width: Option<u32>,
    /// Optional thumbnail maximum height in pixels.
    pub max_height: Option<u32>,
}

/// Metadata for a single filmstrip keyframe entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilmstripEntryMeta {
    /// Zero-based keyframe index.
    pub frame_index: u64,
    /// Presentation timestamp in nanoseconds.
    pub pts_ns: i64,
    /// Thumbnail width in pixels.
    pub width: u32,
    /// Thumbnail height in pixels.
    pub height: u32,
}

/// Response payload for `media.filmstrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFilmstripResponse {
    /// `"disk_cache"` or `"generated"`.
    pub origin: String,
    /// External `<cache_dir>/<bundle_id>/filmstrip.bin` path.
    pub cache_path: PathBuf,
    /// Number of keyframe thumbnails in the filmstrip.
    pub count: usize,
    /// Elapsed wall time in milliseconds.
    pub elapsed_ms: u64,
    /// Ordered thumbnail entry metadata.
    pub entries: Vec<FilmstripEntryMeta>,
}

/// Request parameters for `media.scrub` and `media.seek_frame`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFrameRequestParams {
    /// Open viewport identifier.
    pub viewport_id: String,
    /// Target zero-based integer frame index.
    pub frame_index: u64,
}

/// Response payload for `media.scrub`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaScrubResponse {
    /// Published SHM frame ring generation number.
    pub generation: u64,
    /// Frame index requested by the caller.
    pub requested_frame_index: u64,
    /// Actual frame index published into the SHM slot.
    pub displayed_frame_index: u64,
    /// `true` when displaying an approximate keyframe thumbnail preview.
    pub is_approximate: bool,
    /// Published frame width in pixels.
    pub width: u32,
    /// Published frame height in pixels.
    pub height: u32,
    /// Elapsed wall time in milliseconds.
    pub elapsed_ms: u64,
}

/// Response payload for `media.seek_frame`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSeekFrameResponse {
    /// Published SHM frame ring generation number.
    pub generation: u64,
    /// Exact zero-based integer frame index published into the SHM slot.
    pub frame_index: u64,
    /// Always `false` for settled exact seeks.
    pub is_approximate: bool,
    /// Published frame width in pixels.
    pub width: u32,
    /// Published frame height in pixels.
    pub height: u32,
    /// Cumulative GOP-LRU cache hits on this viewport.
    pub cache_hits: u64,
    /// Cumulative GOP-LRU cache misses on this viewport.
    pub cache_misses: u64,
    /// Elapsed wall time in milliseconds.
    pub elapsed_ms: u64,
}

/// Request parameters for `media.close_viewport`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaCloseViewportParams {
    /// Open viewport identifier.
    pub viewport_id: String,
}
