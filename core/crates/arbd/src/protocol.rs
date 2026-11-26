//! Versioned JSON-RPC 2.0 / NDJSON control-plane protocol schemas (`SPEC-03`).

use std::path::PathBuf;

use arb_adb::adb::AdbDeviceInfo;
use arb_core::analytics::{CrashMarker, ScreenSpan};
use arb_core::bundle::{
    AppLogSummary, BundleKind, DeviceInfo, EpochSpan, LogCompleteness, LogRecord, Provenance,
    StreamStatus, StreamsSummary, TapRecord, UnanchoredSummary,
};
use arb_core::config::{ArbConfig, CaptureMode};
use arb_core::gesture::GestureKind;
use arb_core::selection::SelectionPolicy;
use arb_core::state::{BundleLifecycleState, SaveJobState, SaveTriggerKind};
use arb_core::time::DiscontinuityEvent;
use arb_media::mux_av::AvMuxReport;
use arb_media::trim::{PngExportReport, TrimReport};
use arb_storage::bundle_io::BundleFormat;
use arb_storage::catalog::{BundleDiskState, CatalogFormatVersion, LeaseHolderKind};
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
    /// Optional build/binary fingerprint (`version` + executable mtime/size) for same-path upgrade detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    /// Optional Unix timestamp (seconds) of the running `arbd` executable's last modification time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_mtime_secs: Option<u64>,
    /// `true` when this daemon was started with `--exit-on-stdin-eof` and exits when the parent
    /// holding its stdin pipe dies (parent-bound lifecycle). Absent (= `false`) on older daemons.
    #[serde(default)]
    pub parent_bound: bool,
}

/// Live audio capture state and its machine-readable cause, both decided by
/// [`arb_core::audio_policy`] (PDD G8) and reported in [`CaptureStatusView`].
pub use arb_core::audio_policy::{AudioCaptureState, AudioCause};

/// Live capture telemetry inside [`SystemStatusResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStatusView {
    /// Whether capture is currently active.
    pub active: bool,
    /// Whether transport is currently connected.
    pub connected: bool,
    /// Active (or configured) continuous capture mode (`video_and_logs` or `logs_only`).
    #[serde(default)]
    pub capture_mode: CaptureMode,
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
    /// Optional honest diagnostic when video encoder initialization fails (e.g., HW encoder
    /// contention with Android Studio Running Devices mirroring).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_video_error: Option<String>,
    /// Honest live audio capture availability state (`unknown`, `available`, `disabled`, `unavailable`).
    #[serde(default)]
    pub audio_state: AudioCaptureState,
    /// Audio source selected for this capture attempt.
    #[serde(default)]
    pub audio_capture_mode: arb_core::config::AudioCaptureMode,
    /// Machine-readable cause behind `audio_state` (`api_below_min`, `device_capture_failed`,
    /// `encoder_fatal`, ...). Clients pick their wording from this, never from `audio_reason`;
    /// only `api_below_min` means the device OS is unsupported. Absent from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_cause: Option<AudioCause>,
    /// Optional honest diagnostic explaining why audio is unavailable (e.g., Android 9 / API 28,
    /// or a runtime failure with the tail of `scrcpy-server` stderr).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_reason: Option<String>,
    /// Detected Android API level of the capturing device, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_api_level: Option<u32>,
    /// Optional non-blocking warning when the connected device is below the official
    /// Android 9 (API 28) support floor (best-effort capture continues without hard-blocking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_api_warning: Option<String>,
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
    /// Count of interrupted `Purging` bundles that failed to finalize at startup.
    #[serde(default)]
    pub failed_purging_bundles: usize,
    /// Bundle IDs of interrupted `Purging` bundles that failed to finalize at startup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_purging_bundle_ids: Vec<String>,
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
    /// Whether capture should resume when an eligible device appears.
    #[serde(default)]
    pub capture_enabled: bool,
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
    /// Honest real elapsed duration currently retained in the Zone-1 ring buffer in seconds.
    #[serde(default)]
    pub buffered_secs: u64,
    /// Number of currently open SHM viewports.
    pub active_viewports: usize,
    /// Number of non-terminal save jobs in the catalog.
    pub active_save_jobs: usize,
    /// Startup crash recovery report summary.
    pub recovery: RecoverySummaryView,
    /// Live `am_crash` debounced auto-save telemetry.
    #[serde(default)]
    pub crash_autosave: CrashAutoSaveStatusView,
    /// Set when startup found an invalid `config.json`, set it aside and fell back to defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_warning: Option<String>,
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
    /// Wait for a confirmed device connection.
    #[serde(default)]
    pub wait_connected: bool,
    /// Connection deadline including ADB discovery, limited to 30 seconds.
    pub wait_timeout_ms: Option<u64>,
    /// Optional ADB device serial (auto-selects sole online device if omitted).
    pub serial: Option<String>,
    /// Optional continuous capture mode (`video_and_logs` or `logs_only`; defaults to config).
    #[serde(default, alias = "mode")]
    pub capture_mode: Option<CaptureMode>,
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
    /// Enable audio capture with the configured source (defaults to the saved toggle).
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
    /// Confirmed transport state for a waiting caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected: Option<bool>,
    /// Whether discovery or connection exceeded the requested wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_timed_out: Option<bool>,
    /// Audio status of this capture generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_state: Option<AudioCaptureState>,
    /// Audio cause, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_cause: Option<AudioCause>,
    /// Target device serial.
    pub serial: String,
    /// `true` once live capture has been started.
    pub started: bool,
    /// Effective continuous capture mode (`video_and_logs` or `logs_only`).
    #[serde(default)]
    pub capture_mode: CaptureMode,
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
    /// Optional explicit absolute PNG destination file path (mutually exclusive with `output_dir`).
    pub output_path: Option<PathBuf>,
    /// Optional explicit absolute destination directory (e.g., inherited from Android Studio
    /// Logcat screenshot settings); writes `<output_dir>/screenshot-*.png`.
    #[serde(default)]
    pub output_dir: Option<PathBuf>,
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
    /// Evidence present in a committed recording, including a pending catalog retry.
    #[serde(flatten)]
    pub evidence: SaveEvidence,
    /// Requested lookback, also available when the write failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_duration_secs: Option<u64>,
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

/// Optional evidence measurements from the published bundle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveEvidence {
    /// Encoded MP4 duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_video_duration_ms: Option<u64>,
    /// Anchored main log span in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_log_duration_ms: Option<u64>,
    /// Indexed video frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_frame_count: Option<u64>,
    /// Total saved log rows including quarantine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_count: Option<usize>,
    /// Quarantined log rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unanchored_log_count: Option<usize>,
    /// Quarantined records omitted by selection limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_unanchored_count: Option<u64>,
    /// Saved timeline discontinuities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discontinuity_count: Option<usize>,
    /// Requested evidence stream falls short by more than one second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_shortfall: Option<bool>,
    /// Warnings about evidence completeness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
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
    /// Summary read from the immutable payload, if available.
    #[serde(flatten)]
    pub summary: CatalogSummary,
    /// UTC creation time (alias for `created_at_utc`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_utc: Option<String>,
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
    /// Observed on-disk availability of `path` (`present`, `missing`, or `storage_unavailable`).
    #[serde(default)]
    pub disk_state: BundleDiskState,
}

pub use arb_storage::catalog::CatalogSummary;

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
    /// Inspect eligibility without changing files or catalog.
    #[serde(default)]
    pub dry_run: bool,
    /// Optional UTC cutoff timestamp (defaults to current UTC time).
    pub now_utc: Option<String>,
    /// When `true` (explicit user "Empty Trash" action), bypasses `gc_grace_secs` while still
    /// enforcing active-lease, active-job, managed-root, and legacy protections.
    #[serde(default)]
    pub ignore_grace: bool,
}

/// Request parameters for `bundle.forget`.
///
/// Specify either `bundle_id` (forget a single missing entry) or `all_missing: true` (forget all
/// catalog entries currently missing on disk). Never modifies or deletes files on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BundleForgetParams {
    /// Specific catalog bundle identifier to remove if missing on disk.
    pub bundle_id: Option<String>,
    /// When `true`, removes every catalog entry whose directory is missing on disk.
    #[serde(default)]
    pub all_missing: bool,
}

/// Response payload for `bundle.forget`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleForgetResponse {
    /// Missing bundle IDs removed from the catalog (zero disk writes).
    pub forgotten_bundle_ids: Vec<String>,
    /// Requested bundle IDs skipped because their directories still exist on disk.
    pub skipped_present_bundle_ids: Vec<String>,
    /// Bundle IDs skipped because their parent storage is currently unavailable.
    pub skipped_storage_unavailable_bundle_ids: Vec<String>,
    /// Bundle IDs skipped because active leases were present.
    pub skipped_leased_bundle_ids: Vec<String>,
    /// Bundle IDs skipped because active save jobs targeted their path.
    pub skipped_active_job_bundle_ids: Vec<String>,
    /// Expired or dead-PID leases reaped before evaluating forget eligibility.
    #[serde(default)]
    pub reaped_lease_ids: Vec<String>,
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
    /// True for a read-only plan.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    /// Candidate IDs and bytes for a preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<arb_storage::catalog::GcPreview>,
    /// Per-bucket counts in a preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<std::collections::BTreeMap<String, usize>>,
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
    /// Decimal exclusive log sequence; valid with the same filters and immutable bundle.
    pub cursor: Option<String>,
    /// Match the entire keyword/tag token (Unicode alphanumerics and underscore delimit words).
    #[serde(default)]
    pub whole_word: bool,
    /// Exclusive log sequence cursor for continuing an immutable bundle query.
    pub after_seq: Option<u64>,
    /// Catalog `bundle_id` or filesystem path.
    #[serde(alias = "bundle_id")]
    pub bundle_id_or_path: String,
    /// Bucket filter (`main`, `unanchored`, or `all`; default `all`).
    pub bucket: Option<QueryBucket>,
    /// Optional inclusive start timestamp in nanoseconds.
    pub start_ns: Option<i64>,
    /// Optional inclusive end timestamp in nanoseconds.
    pub end_ns: Option<i64>,
    /// Optional inclusive starting video frame index (translated to `DeviceBoottime` bounds).
    #[serde(default)]
    pub start_frame: Option<u64>,
    /// Optional inclusive ending video frame index (translated to `DeviceBoottime` bounds).
    #[serde(default)]
    pub end_frame: Option<u64>,
    /// Optional case-insensitive substring filter on `LogRecord::tag` (`timeline.query_logs`).
    #[serde(default)]
    pub tag: Option<String>,
    /// Optional log priority filter (`V`, `D`, `I`, `W`, `E`, `F` or their full names; `timeline.query_logs`).
    #[serde(default)]
    pub level: Option<String>,
    /// Optional case-insensitive substring filter on `LogRecord::message` or `tag`.
    #[serde(default)]
    pub keyword: Option<String>,
    /// Optional Android application package name filter (`timeline.query_logs`, e.g. `com.example.app`).
    #[serde(default)]
    pub package: Option<String>,
    /// Optional maximum row count (default [`DEFAULT_QUERY_LIMIT`], capped at
    /// [`MAX_QUERY_LIMIT`]).
    pub limit: Option<usize>,
}

/// Per-epoch frame index and `DeviceBoottime` bounds for multi-epoch (cross-reboot) timelines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochFrameRange {
    /// Zero-based sequential epoch index within this bundle.
    pub epoch_index: usize,
    /// Device boot UUID (`boot_id`) for this epoch.
    pub boot_id: String,
    /// Inclusive starting zero-based video frame index belonging to this epoch.
    pub start_frame_index: u64,
    /// Inclusive ending zero-based video frame index belonging to this epoch.
    pub end_frame_index: u64,
    /// Earliest observed `DeviceBoottime` in nanoseconds among frames in this epoch.
    pub start_boottime_ns: i64,
    /// Latest observed `DeviceBoottime` in nanoseconds among frames in this epoch.
    pub end_boottime_ns: i64,
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
    /// Per-epoch video frame index and boottime bounds for exact cross-reboot event mapping.
    #[serde(default)]
    pub epoch_frame_ranges: Vec<EpochFrameRange>,
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
    /// Per-app extracted foreground logcat sidecars (`app-logs/<package>.jsonl`).
    #[serde(default)]
    pub app_logs: Vec<AppLogSummary>,
}

/// Response payload for `timeline.query_logs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineLogsResponse {
    /// Matching log records.
    pub logs: Vec<QueryLogRow>,
    /// Count matching the filters across all pages.
    pub total_matched: usize,
    /// Number of rows in this page.
    pub returned_count: usize,
    /// More matching rows follow.
    pub has_more: bool,
    /// Exclusive continuation sequence, only supplied for non-final pages.
    pub next_cursor: Option<String>,
}

/// A stored row and its derived presentation coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryLogRow {
    /// Original log record.
    #[serde(flatten)]
    pub record: LogRecord,
    /// Logcat UTC header timestamp.
    pub timestamp_utc: Option<String>,
    /// Elapsed time from this epoch's first frame or log timeline start.
    pub offset_ms: Option<i64>,
    /// Display frame at this instant on the same boot.
    pub frame_index: Option<u64>,
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
    /// Optional destination parent directory (defaults to `output_dir_override` or `<data_dir>/bundles`).
    pub dest_dir: Option<PathBuf>,
    /// Optional client PID for the temporary `ExportJob` lease.
    pub holder_pid: Option<u32>,
    /// When `true`, burns recorded Tap/Swipe overlays into the emitted `share-taps.mp4`
    /// (implies `include_share_mp4`; lossless `video.mp4` stays raw inside the trimmed bundle).
    #[serde(default)]
    pub burn_taps: bool,
    /// When `true`, also muxes a ready-to-share faststart zero-`elst` `share.mp4` (or
    /// `share-taps.mp4` when `burn_taps` is enabled) directly inside the trimmed bundle directory.
    #[serde(default)]
    pub include_share_mp4: bool,
    /// Optional additional standalone copy of the verified share MP4.
    pub share_output_path: Option<PathBuf>,
    /// When `true`, moves the source bundle to non-destructive Trash strictly AFTER the trimmed
    /// bundle is committed and verified (refused for read-only legacy or uncataloged bundles).
    #[serde(default)]
    pub trash_original: bool,
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
    /// Path to `share.mp4` or `share-taps.mp4` inside the trimmed bundle when generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_mp4_path: Option<PathBuf>,
    /// Verified mux metrics when a share MP4 was produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub av_mux_report: Option<AvMuxReport>,
    /// Number of per-app `app-logs/<package>.log` files emitted in the trimmed bundle.
    #[serde(default)]
    pub app_logs_count: usize,
    /// `true` if `trash_original` succeeded and the source bundle is now logically trashed.
    #[serde(default)]
    pub original_trashed: bool,
    /// `true` if Tap/Swipe overlays were rendered and re-encoded into `share-taps.mp4`.
    #[serde(default)]
    pub burn_taps_applied: bool,
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
    /// Exclusive media endpoint. Available even when the sample table exceeds the wire limit.
    #[serde(default)]
    pub timeline_duration_ns: i64,
    /// Duration of the final MP4 sample, including its hold after the final timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_frame_duration_ns: Option<u64>,
    /// Nanosecond presentation timestamps relative to frame zero, in sample order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_pts_ns: Vec<i64>,
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

// ---------------- Audio PCM Decode (IDE Playback Sync) ----------------

/// Request parameters for `media.decode_audio_pcm`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaDecodeAudioPcmParams {
    /// Catalog `bundle_id` or filesystem path.
    #[serde(alias = "bundle_id_or_path")]
    pub bundle_id: String,
}

/// Response payload for `media.decode_audio_pcm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDecodeAudioPcmResponse {
    /// `true` when the bundle has an AAC-LC track decoded to PCM WAV.
    pub available: bool,
    /// Path to the cached signed 16-bit LE PCM RIFF/WAV file, when `available == true`.
    pub wav_path: Option<PathBuf>,
    /// Decoded PCM sampling rate in Hz (`0` when unavailable).
    pub sample_rate_hz: u32,
    /// Decoded PCM channel count (`0` when unavailable).
    pub channels: u16,
    /// Decoded audio duration in nanoseconds (`0` when unavailable).
    pub duration_ns: u64,
    /// Recorded A/V sync offset in milliseconds from bundle metadata.
    pub av_offset_ms: i64,
    /// Exact measured lead-in skew in nanoseconds from bundle metadata.
    pub audio_lead_in_offset_ns: i64,
    /// `true` when served directly from `<cache_dir>/audio-pcm/<bundle_id>.wav`.
    pub cached: bool,
}

// ---------------- Single-File Share MP4 Export ----------------

/// Request parameters for `export.share_mp4`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportShareMp4Params {
    /// Source catalog `bundle_id` or filesystem path.
    #[serde(alias = "bundle_id_or_path")]
    pub bundle_id: String,
    /// Optional inclusive integer starting frame index (must be paired with `out_frame_index`).
    pub in_frame_index: Option<u64>,
    /// Optional inclusive integer ending frame index (must be paired with `in_frame_index`).
    pub out_frame_index: Option<u64>,
    /// Optional explicit absolute output `.mp4` path (defaults to `<output_dir>/share-*.mp4`).
    pub output_path: Option<PathBuf>,
    /// When `true`, moves the source bundle to non-destructive Trash strictly AFTER the new
    /// output bundle & `share.mp4` are fsynced, verified, and committed (refused for `LegacyRo`).
    #[serde(default)]
    pub trash_original: bool,
    /// Optional destination parent directory for a trimmed WORM bundle (when trimming).
    pub dest_dir: Option<PathBuf>,
    /// Optional client PID for the temporary `ExportJob` lease.
    pub holder_pid: Option<u32>,
    /// When `true`, composites Tap/Swipe overlays onto frames overlapping touch events before muxing.
    #[serde(default)]
    pub burn_taps: bool,
}

/// Response payload for `export.share_mp4`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportShareMp4Response {
    /// Written standalone faststart zero-`elst` `share.mp4` file path.
    pub share_mp4_path: PathBuf,
    /// Newly created immutable WORM trimmed bundle ID, when a narrower frame range was trimmed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trimmed_bundle_id: Option<String>,
    /// Filesystem path of the newly committed WORM trimmed bundle, when trimmed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trimmed_bundle_path: Option<PathBuf>,
    /// `true` when an AAC-LC audio track was muxed into `share.mp4`.
    pub audio_included: bool,
    /// Total H.264 frames muxed into `share.mp4`.
    pub video_frame_count: u64,
    /// Count of `edts` boxes written (always `0`).
    pub edts_box_count: usize,
    /// Count of `elst` boxes written (always `0`).
    pub elst_box_count: usize,
    /// Total byte size of the published `share.mp4`.
    pub output_bytes: u64,
    /// `true` if `trash_original` succeeded and the source bundle is now logically trashed.
    pub original_trashed: bool,
    /// Detailed mux telemetry from `arb_media::mux_share_mp4`.
    pub av_mux_report: AvMuxReport,
    /// Non-fatal warning, if any (e.g. parent-directory `fsync` warning on trimmed bundle commit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

// ---------------- AI Timeline Overview & Frame Inspection ----------------

/// Default maximum `E`/`F` error log highlights returned by `timeline.overview`.
pub const DEFAULT_OVERVIEW_ERROR_LOGS: usize = 50;

/// Maximum accepted `max_error_logs` cap for `timeline.overview`.
pub const MAX_OVERVIEW_ERROR_LOGS: usize = 200;

/// Request parameters for `timeline.overview`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineOverviewParams {
    /// Catalog `bundle_id` or filesystem path.
    #[serde(alias = "bundle_id_or_path")]
    pub bundle_id: String,
    /// Optional cap on `error_log_highlights` (default `50`, capped at `200`).
    pub max_error_logs: Option<usize>,
}

/// Compact tap summary entry returned inside [`TimelineOverviewResponse`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverviewTapItem {
    /// Zero-based tap index matching `media.inspect_frames` `tap_index`.
    pub tap_index: usize,
    /// Source `TapRecord::seq` in `timeline.sqlite`.
    pub seq: u64,
    /// Nearest video frame index, when video frames are present.
    pub frame_index: Option<u64>,
    /// Timestamp in milliseconds relative to the start of the bundle timeline.
    pub t_ms: Option<i64>,
    /// Touch X coordinate in display pixels, if available.
    pub x: Option<f64>,
    /// Touch Y coordinate in display pixels, if available.
    pub y: Option<f64>,
    /// Input action label (e.g., `"TAP"`, `"SWIPE"`; `"DOWN"`/`"UP"`/`"MOVE"` for legacy bundles
    /// saved before T-6H item 2's per-stroke gesture classification).
    pub kind: String,
    /// `Tap` vs `Swipe` classification (`#[serde(default)]`: `Tap` for legacy bundles/responses).
    #[serde(default)]
    pub gesture_kind: GestureKind,
    /// End X coordinate for a `Swipe` (`None` for `Tap` or legacy data).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_x: Option<f64>,
    /// End Y coordinate for a `Swipe` (`None` for `Tap` or legacy data).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_y: Option<f64>,
    /// End timestamp in milliseconds relative to the start of the bundle timeline (mirrors `t_ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_t_ms: Option<i64>,
}

/// Compact `E`/`F` error log highlight returned inside [`TimelineOverviewResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorLogHighlight {
    /// Source `LogRecord::seq` in `timeline.sqlite`.
    pub seq: u64,
    /// Priority level (`"E"` or `"F"`).
    pub level: String,
    /// Nearest video frame index, when video frames are present.
    pub frame_index_hint: Option<u64>,
    /// Timestamp in milliseconds relative to the start of the bundle timeline.
    pub t_ms: Option<i64>,
    /// Android logcat tag, if present.
    pub tag: Option<String>,
    /// Bounded error message excerpt.
    pub message: String,
}

/// One-shot structured bundle overview returned by `timeline.overview` for AI triage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineOverviewResponse {
    /// Resolved bundle identifier.
    pub bundle_id: String,
    /// High-level bundle payload category (`full` or `logs_only`).
    pub kind: BundleKind,
    /// `true` when the bundle is a read-only legacy schema 3/4 recording.
    pub legacy_read_only: bool,
    /// Total indexed video frames.
    pub frame_count: usize,
    /// Approximate bundle duration in milliseconds.
    pub duration_ms: u64,
    /// Video frame width in pixels, if a video track is present.
    pub width: Option<u32>,
    /// Video frame height in pixels, if a video track is present.
    pub height: Option<u32>,
    /// Physical device display width in pixels from bundle metadata, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_width: Option<u32>,
    /// Physical device display height in pixels from bundle metadata, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_height: Option<u32>,
    /// Per-epoch video frame index and boottime bounds for multi-epoch bundles.
    #[serde(default)]
    pub epoch_frame_ranges: Vec<EpochFrameRange>,
    /// Estimated average frame rate in frames per second, when measurable.
    pub fps_estimate: Option<f64>,
    /// Audio track presence status.
    pub audio_status: StreamStatus,
    /// Recorded A/V offset in milliseconds.
    pub av_offset_ms: i64,
    /// Honest logcat completeness indicator, if logcat metadata is present.
    pub logcat_completeness: Option<LogCompleteness>,
    /// Contiguous Activity / screen transition spans.
    pub screens: Vec<ScreenSpan>,
    /// Compact ordered tap records with frame hints.
    pub taps: Vec<OverviewTapItem>,
    /// Structured crash markers extracted from logcat.
    pub crashes: Vec<CrashMarker>,
    /// Recorded timeline discontinuities (gaps, reboots, sleep).
    pub discontinuities: Vec<DiscontinuityEvent>,
    /// Bounded `E`/`F` error/fatal log highlights.
    pub error_log_highlights: Vec<ErrorLogHighlight>,
    /// Count of quarantined `unanchored` records included in the bundle.
    pub unanchored_count: u64,
    /// Count of `unanchored` records truncated by quarantine caps.
    pub unanchored_truncated_count: u64,
    /// Per-app extracted foreground logcat sidecars (`app-logs/<package>.jsonl`).
    #[serde(default)]
    pub app_logs: Vec<AppLogSummary>,
}

/// Visual inspection mode for `media.inspect_frames`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectFramesMode {
    /// Grid contact sheet (up to 16 tiles) with burned-in `#frame_index` and timestamps.
    ContactSheet,
    /// Single full-resolution frame PNG.
    Single,
    /// Resolution-relative 28%x12% context crop around a tap point with center crosshair.
    TapCrop,
}

/// Request parameters for `media.inspect_frames`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInspectFramesParams {
    /// Catalog `bundle_id` or filesystem path.
    #[serde(alias = "bundle_id_or_path")]
    pub bundle_id: String,
    /// Rendering mode (`contact_sheet`, `single`, or `tap_crop`).
    pub mode: InspectFramesMode,
    /// Optional inclusive start frame index for `contact_sheet`.
    pub start_frame: Option<u64>,
    /// Optional inclusive end frame index for `contact_sheet`.
    pub end_frame: Option<u64>,
    /// Optional explicit frame indices (1..=16) for `contact_sheet`.
    pub frame_indices: Option<Vec<u64>>,
    /// Optional tile cap for `contact_sheet` (default & max `16`).
    pub max_tiles: Option<usize>,
    /// Exact frame index for `single` or `tap_crop`.
    pub frame_index: Option<u64>,
    /// Zero-based tap index (matching `timeline.overview`) for `tap_crop`.
    pub tap_index: Option<usize>,
    /// Explicit tap X coordinate in source-frame pixels for `tap_crop` or marker overlay.
    pub tap_x: Option<u32>,
    /// Explicit tap Y coordinate in source-frame pixels for `tap_crop` or marker overlay.
    pub tap_y: Option<u32>,
    /// When `true`, overlays cyan tap markers on matching frames in `contact_sheet` / `single`.
    #[serde(default)]
    pub include_tap_marker: bool,
    /// Device display width in pixels; recorded taps are scaled from this space onto the video.
    #[serde(default)]
    pub display_width: Option<u32>,
    /// Device display height in pixels; recorded taps are scaled from this space onto the video.
    #[serde(default)]
    pub display_height: Option<u32>,
    /// Optional contact sheet maximum tile width in pixels.
    pub max_tile_width: Option<u32>,
    /// Optional contact sheet maximum tile height in pixels.
    pub max_tile_height: Option<u32>,
    /// Optional contact sheet column count (`1..=16`).
    pub columns: Option<u32>,
}

/// Tap-crop rectangle geometry returned by `media.inspect_frames` in `tap_crop` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapCropMeta {
    /// Left edge of the crop inside the source frame.
    pub crop_x: u32,
    /// Top edge of the crop inside the source frame.
    pub crop_y: u32,
    /// Crop width in pixels.
    pub crop_width: u32,
    /// Crop height in pixels.
    pub crop_height: u32,
    /// Tap X coordinate in the source frame.
    pub tap_x: u32,
    /// Tap Y coordinate in the source frame.
    pub tap_y: u32,
    /// Tap X coordinate relative to the cropped image.
    pub tap_rel_x: u32,
    /// Tap Y coordinate relative to the cropped image.
    pub tap_rel_y: u32,
}

/// Response payload for `media.inspect_frames`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaInspectFramesResponse {
    /// Inspection mode that was rendered.
    pub mode: InspectFramesMode,
    /// Resolved bundle identifier.
    pub bundle_id: String,
    /// Exact zero-based frame indices rendered into the PNG.
    pub frame_indices_rendered: Vec<u64>,
    /// Encoded PNG image width in pixels.
    pub width: u32,
    /// Encoded PNG image height in pixels.
    pub height: u32,
    /// Byte length of the encoded PNG.
    pub png_bytes_len: usize,
    /// Cached PNG file path under `<cache_dir>/inspect/`.
    pub png_path: PathBuf,
    /// Base64-encoded PNG bytes when small enough for inline IPC transport (`<= 2 MiB` raw).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub png_base64: Option<String>,
    /// Tap-crop geometry, populated when `mode == InspectFramesMode::TapCrop`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tap_crop: Option<TapCropMeta>,
}
