#![forbid(unsafe_code)]

//! Single-writer local daemon (`arbd`) for Android Replay Buffer (ARB).
//!
//! Coordinates:
//! - [`arb_os::fs::SingleInstanceLock`] and [`arb_os::ipc::LocalIpcListener`]
//! - [`arb_storage::catalog::Catalog`] startup recovery, leases, edit sessions, and GC
//! - [`capture::CapturePipeline`] Zone-1 ring buffer & two-phase save engine
//! - [`live_adb`] live Android device-agent deployment & reconnect supervision
//! - [`media_engine::MediaEngine`] SHM triple-buffered viewport & frame-accurate trim/PNG export

mod adb_resources;
pub mod capture;
pub mod error;
pub mod inspect_export;
pub mod live_adb;
pub mod media_engine;
pub mod protocol;
pub mod screenshot;
pub mod time_util;
mod touch_calibration;
mod trim_range;

use std::collections::BTreeMap;
use std::io::BufReader;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

use arb_adb::adb::AdbClient;
use arb_adb::mock::DeviceEventSource;
use arb_core::BUNDLE_SCHEMA_VERSION;
use arb_core::analytics::{CrashAutoSaveDecision, CrashAutoSaveEval, CrashAutoSaveGate};
use arb_core::bundle::RecordBucket;
use arb_core::config::{
    ArbConfig, ArbConfigPatch, CaptureMode, MAX_SAVE_WINDOW_SECS, MIN_SAVE_WINDOW_SECS,
};
use arb_core::device::{DeviceSelectionOutcome, validate_adb_serial};
use arb_core::selection::{DrainOutcome, SelectionPolicy, evaluate_stream_drain};
use arb_core::state::{RingBudgetPolicy, SaveTriggerKind};
use arb_media::codec::default_codec_backend;
use arb_os::fs::{SingleInstanceLock, is_pid_alive};
use arb_os::ipc::{
    DEFAULT_MAX_IPC_LINE_BYTES, FrameLineReader, LocalIpcListener, LocalIpcStream, write_frame_line,
};
use arb_os::paths::ArbPaths;
use arb_storage::StorageError;
use arb_storage::bundle_io::{TimeRange, open_bundle};
use arb_storage::catalog::{
    BundleEntry, Catalog, CatalogFormatVersion, EditSession, LeaseHolderKind, NewBundleEntry,
    PinReconcileReport, RecoveryReport,
};
use arb_storage::config::ConfigStore;
use arb_storage::ring::{DiskSpaceProvider, PinJournal};
use arb_storage::sha256_hex;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::capture::{
    CapturePipeline, CrashAutoSaveNotice, FaultInjection, PendingSave, SharedDiskSpaceProvider,
    TailCompleteness, phase2_materialize, save_job_to_view,
};
use crate::error::{
    DaemonError, NotFoundEntity, RPC_ERR_INVALID_REQUEST, RPC_ERR_PARSE, RPC_ERR_RESPONSE_TOO_LARGE,
};
use crate::live_adb::{LiveCaptureHandle, select_live_target, start_selected_capture};
use crate::media_engine::{ExportContext, MediaEngine, build_timeline_summary};
use crate::protocol::{
    BundleEntryView, BundleForgetParams, BundleForgetResponse, BundleGcFailure, BundleGcParams,
    BundleGcResponse, BundleIdParams, BundleListParams, BundleListResponse, BundleOpenParams,
    BundleOpenResponse, BundleRefParams, CaptureConfigChanges, CaptureStartParams,
    CaptureStartResponse, CaptureStopResponse, ConfigResponse, CrashAutoSaveStatusView,
    DEFAULT_QUERY_LIMIT, DeviceListResponse, EditGetSessionParams, EditSessionView,
    EditUpdateSessionParams, ExportPngParams, ExportShareMp4Params, ExportTrimParams,
    IPC_PROTOCOL_VERSION, JSONRPC_VERSION, LeaseIdParams, MAX_QUERY_LIMIT,
    MediaCloseViewportParams, MediaDecodeAudioPcmParams, MediaFilmstripParams,
    MediaFrameRequestParams, MediaInspectFramesParams, MediaOpenViewportParams, QueryBucket,
    RecoverySummaryView, RpcRequest, RpcResponse, SaveJobIdParams, SaveListJobsResponse,
    SaveLogsParams, SaveMode, SaveOutcomeResponse, SaveRetargetParams, SaveScreenshotParams,
    SaveScreenshotResponse, SaveTriggerParams, SerialSource, SystemHelloResponse,
    SystemStatusResponse, TimelineCrashesResponse, TimelineDiscontinuitiesResponse,
    TimelineOverviewParams, TimelineQueryParams, TimelineScreensResponse, TimelineTapsResponse,
};
use crate::screenshot::{ScreenshotOverwrite, SharedScreenshotProvider, write_screenshot_atomic};
use crate::time_util::{current_unix_millis, next_id, utc_now_iso8601};

const DRAIN_POLL_INTERVAL_MS: u64 = 20;
/// Upper bound on how long shutdown waits for in-flight client requests to finish.
const CLIENT_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound on how long shutdown waits for an in-flight crash auto-save.
const CRASH_WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Read timeout on client IPC streams (where the OS supports it) so idle client threads wake
/// promptly on shutdown, release their SHM viewports, and join cleanly (R3 S11).
const CLIENT_READ_POLL_TIMEOUT: Duration = Duration::from_millis(250);

/// Cloneable handle allowing an external watcher (such as a signal handler in `arbd`'s
/// `main.rs`) to request a graceful daemon shutdown (R3 S11).
#[derive(Debug, Clone)]
pub struct DaemonShutdownHandle {
    state: Arc<DaemonState>,
}

impl DaemonShutdownHandle {
    /// Signals the daemon to stop accepting requests and exit cleanly.
    pub fn trigger(&self) {
        self.state.trigger_shutdown();
    }

    /// Returns `true` once a shutdown has been requested.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.state.is_shutting_down()
    }
}

/// Startup configuration for [`DaemonServer`].
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Resolved data, cache, and runtime directories.
    pub paths: ArbPaths,
    /// Zone-1 ring buffer byte, duration, and free-disk safety floor policy.
    pub ring_policy: RingBudgetPolicy,
    /// Injectable filesystem disk-space provider (defaults to host OS `statvfs` / Win32).
    pub disk_provider: SharedDiskSpaceProvider,
    /// Injectable screenshot provider (defaults to `adb exec-out screencap -p`).
    pub screenshot_provider: SharedScreenshotProvider,
    /// One-shot fault injection switches (all off by default; used by the E2E suite).
    pub faults: Arc<FaultInjection>,
    ring_policy_overridden: bool,
    parent_bound: bool,
}

impl DaemonConfig {
    /// Creates a daemon configuration using `paths` and default SPEC-02 ring policies.
    #[must_use]
    pub fn new(paths: ArbPaths) -> Self {
        Self {
            paths,
            ring_policy: RingBudgetPolicy::default(),
            disk_provider: SharedDiskSpaceProvider::os_default(),
            screenshot_provider: SharedScreenshotProvider::adb_default(),
            faults: Arc::new(FaultInjection::default()),
            ring_policy_overridden: false,
            parent_bound: false,
        }
    }

    /// Marks this daemon as parent-bound (`--exit-on-stdin-eof`): it exits when the parent that
    /// holds its stdin pipe dies, and reports `parent_bound: true` in `system.hello` so clients
    /// can tell a daemon owned by a live parent (another IDE) from an unmanaged one.
    #[must_use]
    pub const fn bound_to_parent(mut self) -> Self {
        self.parent_bound = true;
        self
    }

    /// Installs shared [`FaultInjection`] switches (deterministic crash/failure tests).
    #[must_use]
    pub fn with_fault_injection(mut self, faults: Arc<FaultInjection>) -> Self {
        self.faults = faults;
        self
    }

    /// Overrides the Zone-1 [`RingBudgetPolicy`].
    #[must_use]
    pub const fn with_ring_policy(mut self, policy: RingBudgetPolicy) -> Self {
        self.ring_policy = policy;
        self.ring_policy_overridden = true;
        self
    }

    /// Overrides the [`SharedDiskSpaceProvider`] (for deterministic storage-pressure tests).
    #[must_use]
    pub fn with_disk_provider(mut self, provider: SharedDiskSpaceProvider) -> Self {
        self.disk_provider = provider;
        self
    }

    /// Overrides the [`SharedScreenshotProvider`] (for deterministic screenshot tests).
    #[must_use]
    pub fn with_screenshot_provider(mut self, provider: SharedScreenshotProvider) -> Self {
        self.screenshot_provider = provider;
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureIntent {
    Armed,
    Stopped,
}

#[derive(Default)]
struct LiveCapture {
    handle: Option<LiveCaptureHandle>,
    reservation: Option<Arc<()>>,
    request: CaptureStartParams,
    /// Bumped by every stop (`capture.stop`, a device reselection, a pressure disarm). A start
    /// remembers it on entry, so a stop also cancels starts still queued or waiting.
    stop_epoch: u64,
}

struct DaemonState {
    paths: ArbPaths,
    started_at: Instant,
    recovery: RecoveryReport,
    pin_reconcile: PinReconcileReport,
    config_store: ConfigStore,
    user_config: Mutex<ArbConfig>,
    /// Serialises whole `config.update` transactions (read, patch, persist, swap, apply ring
    /// policy). Lock order: `config_update_lock` -> `user_config` -> `capture`; `user_config`
    /// itself is only ever held briefly and never across I/O.
    config_update_lock: Mutex<()>,
    /// Set when startup had to set an invalid `config.json` aside and fall back to defaults.
    config_load_warning: Option<String>,
    /// `true` when `config.json` exists but could not be read: `config.update` is refused so the
    /// user's real file is never replaced by a patch built on defaults.
    config_unreadable: bool,
    disk_provider: SharedDiskSpaceProvider,
    catalog: Catalog,
    capture: Arc<Mutex<CapturePipeline>>,
    live_capture: Mutex<LiveCapture>,
    media: MediaEngine,
    screenshot_provider: SharedScreenshotProvider,
    crash_autosave_status: Mutex<CrashAutoSaveStatusView>,
    crash_autosave_gate: Mutex<CrashAutoSaveGate>,
    handover_gate: RwLock<()>,
    crash_jobs: std::sync::atomic::AtomicUsize,
    crash_tx: Mutex<Option<mpsc::Sender<PendingSave>>>,
    faults: Arc<FaultInjection>,
    shutdown_flag: Arc<AtomicBool>,
    endpoint: PathBuf,
    build_id: Option<String>,
    binary_mtime_secs: Option<u64>,
    parent_bound: bool,
    handover_owner: Option<String>,
}

/// Computes an optional `(build_id, binary_mtime_secs)` fingerprint of the currently running
/// `arbd` executable so clients can detect same-path in-place daemon upgrades.
fn compute_daemon_binary_identity() -> (Option<String>, Option<u64>) {
    static IDENTITY: std::sync::OnceLock<(Option<String>, Option<u64>)> =
        std::sync::OnceLock::new();
    IDENTITY
        .get_or_init(|| {
            let Ok(exe) = std::env::current_exe() else {
                return (None, None);
            };
            let mtime_secs = std::fs::metadata(&exe)
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            let build_id = arb_storage::bundle_io::stream_hash_file(&exe)
                .ok()
                .map(|(_, digest)| format!("{}:sha256:{digest}", env!("CARGO_PKG_VERSION")));
            (build_id, mtime_secs)
        })
        .clone()
}

/// Locks `mutex`, mapping poisoning (a panic while it was held) to an error instead of a
/// panic in every later request (R3 S15). The caller triggers shutdown.
fn lock_or_poisoned<'a, T>(
    mutex: &'a Mutex<T>,
    what: &str,
) -> Result<MutexGuard<'a, T>, DaemonError> {
    mutex.lock().map_err(|_| {
        DaemonError::Internal(format!(
            "{what} lock was poisoned by an earlier panic; arbd is shutting down so a client can restart it"
        ))
    })
}

/// Canonicalizes the longest existing ancestor of `path` and re-appends the rest, so paths
/// that do not exist yet can still be compared against canonical directories.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        if let Ok(canon) = std::fs::canonicalize(cursor) {
            let mut out = canon;
            for part in missing.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                cursor = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn is_bundle_directory(dir: &Path) -> bool {
    #[derive(serde::Deserialize)]
    struct LegacyHeader {
        schema: u32,
    }
    dir.join("manifest.json").is_file()
        || [("session.json", 3), ("replay.json", 4)]
            .into_iter()
            .any(|(name, version)| {
                std::fs::File::open(dir.join(name)).is_ok_and(|file| {
                    serde_json::from_reader::<_, LegacyHeader>(std::io::BufReader::new(file))
                        .is_ok_and(|h| h.schema == version)
                })
            })
}

/// `true` when `raw` is clearly meant as a filesystem path rather than a bundle ID.
fn looks_like_path(raw: &str) -> bool {
    raw.contains('/') || raw.contains('\\') || raw.starts_with('.') || Path::new(raw).is_absolute()
}

impl DaemonState {
    fn trigger_shutdown(&self) {
        if !self.shutdown_flag.swap(true, Ordering::AcqRel) {
            // Wake the blocking `LocalIpcListener::accept()` loop cleanly on all OSes.
            let _ = LocalIpcStream::connect(&self.endpoint);
        }
    }

    fn is_shutting_down(&self) -> bool {
        self.shutdown_flag.load(Ordering::Acquire)
    }

    /// Locks `mutex`; on poisoning, starts a graceful shutdown and returns an error (S15).
    fn lock<'a, T>(
        &self,
        mutex: &'a Mutex<T>,
        what: &str,
    ) -> Result<MutexGuard<'a, T>, DaemonError> {
        lock_or_poisoned(mutex, what).inspect_err(|_| self.trigger_shutdown())
    }

    fn lock_capture(&self) -> Result<MutexGuard<'_, CapturePipeline>, DaemonError> {
        self.lock(&self.capture, "capture pipeline")
    }

    /// Stops the live capture session. The handle is taken out of `L` and `L` is released
    /// **before** joining the supervisor thread (R3 S12), and only the session that was
    /// current when the stop began is marked stopped.
    fn stop_active_capture(&self) -> Result<CaptureStopResponse, DaemonError> {
        let (handle, generation) = {
            let mut live_guard = self.lock(&self.live_capture, "live capture")?;
            let generation = self.lock_capture()?.session_generation();
            live_guard.reservation = None;
            live_guard.request = CaptureStartParams::default();
            live_guard.stop_epoch = live_guard.stop_epoch.wrapping_add(1);
            (live_guard.handle.take(), generation)
        };
        let was_running = handle.is_some();
        if let Some(mut handle) = handle {
            handle.stop();
        }
        let mut cap = self.lock_capture()?;
        let was_active = was_running || cap.is_active();
        cap.mark_stopped_if_current(generation);
        Ok(CaptureStopResponse {
            config_changed: CaptureConfigChanges::default(),
            stopped: was_active,
            capture: cap.status_view(),
        })
    }

    /// `true` when free space on the volume receiving the save is already below
    /// `min_free_disk_bytes`.
    ///
    /// Crash auto-save checks this *before* pinning a window so a crash loop cannot fill the
    /// disk before the ring's latched `STORAGE_PRESSURE_STOP` ever trips. A failed probe is
    /// treated as "not below" (fail-open): the ring's own guard remains the backstop and crash
    /// evidence is worth keeping.
    fn host_free_below_floor(&self, target_dir: &Path, min_free_disk_bytes: u64) -> bool {
        // Measure the volume the save will actually write to: the nearest existing ancestor
        // of `target_dir` (it may not have been created yet).
        let probe = target_dir
            .ancestors()
            .find(|dir| dir.exists())
            .unwrap_or(&self.paths.data_dir);
        self.disk_provider
            .disk_space(probe)
            .is_ok_and(|info| info.available_bytes < min_free_disk_bytes)
    }

    /// Validates a client-supplied output location (R3 M8 / C3 / S7): it must be absolute
    /// and must not lie inside arbd's private directories (ring, tmp, journals, cache,
    /// runtime), inside any cataloged bundle, inside any directory holding a bundle
    /// `manifest.json`, or inside `also_forbidden` (e.g. the export source bundle).
    /// Rejects a *file* target inside `data_dir` unless it is under `data_dir/screenshots`, so a
    /// caller can never replace `catalog.sqlite`, `config.json`, locks, journals or bundles
    /// with arbitrary bytes (`validate_output_location` only guards whole directories).
    fn reject_data_dir_state_file(&self, path: &Path, what: &str) -> Result<(), DaemonError> {
        let canon = canonicalize_lenient(path);
        let data_dir = canonicalize_lenient(&self.paths.data_dir);
        let allowed_subdir = canon.starts_with(data_dir.join("screenshots"))
            || canon.starts_with(data_dir.join("exports"));
        if canon.starts_with(&data_dir) && !allowed_subdir {
            return Err(DaemonError::InvalidParams(format!(
                "{what} `{}` is inside arbd's data directory `{}`; only `{}` or `{}` is allowed there",
                path.display(),
                self.paths.data_dir.display(),
                self.paths.data_dir.join("screenshots").display(),
                self.paths.data_dir.join("exports").display()
            )));
        }
        Ok(())
    }

    fn validate_output_location(
        &self,
        path: &Path,
        what: &str,
        also_forbidden: Option<&Path>,
    ) -> Result<(), DaemonError> {
        if !path.is_absolute() {
            return Err(DaemonError::InvalidParams(format!(
                "{what} must be an absolute path (got `{}`); arbd does not resolve paths against its own working directory",
                path.display()
            )));
        }
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(DaemonError::InvalidParams(format!(
                "{what} `{}` must not contain `..` segments",
                path.display()
            )));
        }
        let canon = canonicalize_lenient(path);
        let data_dir = &self.paths.data_dir;
        let private_dirs = [
            data_dir.join("ring"),
            data_dir.join("adb-sessions"),
            data_dir.join("tmp"),
            data_dir.join("save_jobs"),
            data_dir.join("save_job_params"),
            self.paths.cache_dir.clone(),
            self.paths.runtime_dir.clone(),
        ];
        for dir in &private_dirs {
            if canon.starts_with(canonicalize_lenient(dir)) {
                return Err(DaemonError::InvalidParams(format!(
                    "{what} `{}` is inside arbd's private directory `{}`",
                    path.display(),
                    dir.display()
                )));
            }
        }
        let mut bundle_dirs: Vec<PathBuf> = self
            .catalog
            .list_bundles(None)?
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        bundle_dirs.extend(also_forbidden.map(Path::to_path_buf));
        for dir in &bundle_dirs {
            if canon.starts_with(canonicalize_lenient(dir)) {
                return Err(DaemonError::InvalidParams(format!(
                    "{what} `{}` is inside the immutable bundle `{}`",
                    path.display(),
                    dir.display()
                )));
            }
        }
        if let Some(bundle_root) = canon.ancestors().find(|dir| is_bundle_directory(dir)) {
            return Err(DaemonError::InvalidParams(format!(
                "{what} `{}` is inside the bundle directory `{}`",
                path.display(),
                bundle_root.display()
            )));
        }
        Ok(())
    }

    fn resolve_bundle_path(
        &self,
        bundle_id_or_path: &str,
    ) -> Result<(PathBuf, Option<BundleEntry>), DaemonError> {
        if let Some(entry) = self.catalog.bundle(bundle_id_or_path)? {
            if self.catalog.bundle_disk_state(&entry.path)
                == arb_storage::catalog::BundleDiskState::Missing
            {
                return Err(DaemonError::NotFound {
                    entity: NotFoundEntity::Bundle,
                    detail: format!("bundle `{bundle_id_or_path}` is missing"),
                });
            }
            return Ok((entry.path.clone(), Some(entry)));
        }
        let path = PathBuf::from(bundle_id_or_path);
        if !path.is_absolute() {
            if looks_like_path(bundle_id_or_path) {
                return Err(DaemonError::InvalidParams(format!(
                    "bundle path `{bundle_id_or_path}` must be absolute; arbd does not resolve paths against its own working directory"
                )));
            }
            return Err(DaemonError::NotFound {
                entity: NotFoundEntity::Bundle,
                detail: format!("bundle `{bundle_id_or_path}` not found in catalog"),
            });
        }
        if path.is_dir() {
            // Check if already cataloged under this path.
            let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            for entry in self.catalog.list_bundles(None)? {
                let entry_canon =
                    std::fs::canonicalize(&entry.path).unwrap_or_else(|_| entry.path.clone());
                if entry_canon == canon {
                    return Ok((entry.path.clone(), Some(entry)));
                }
            }
            return Ok((path, None));
        }
        Err(DaemonError::NotFound {
            entity: NotFoundEntity::Bundle,
            detail: format!("bundle `{bundle_id_or_path}` not found in catalog or filesystem"),
        })
    }

    fn handle_system_hello(&self) -> Result<Value, DaemonError> {
        Ok(serde_json::to_value(SystemHelloResponse {
            protocol_version: IPC_PROTOCOL_VERSION,
            log_min_level: true,
            log_exclude_tags: true,
            gc_dry_run: true,
            standalone_share_trim: true,
            realtime_trim: true,
            schema_version: BUNDLE_SCHEMA_VERSION,
            pid: std::process::id(),
            data_dir: self.paths.data_dir.clone(),
            cache_dir: self.paths.cache_dir.clone(),
            runtime_dir: self.paths.runtime_dir.clone(),
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            build_id: self.build_id.clone(),
            binary_mtime_secs: self.binary_mtime_secs,
            parent_bound: self.parent_bound,
        })?)
    }

    fn dispatch(self: &Arc<Self>, method: &str, params: Value) -> Result<Value, DaemonError> {
        if !params.is_null() && !params.is_object() {
            return Err(DaemonError::InvalidParams(
                "params must be an object".into(),
            ));
        }
        if method == "system.shutdown_for_handover" {
            #[derive(serde::Deserialize, Default)]
            struct HandoverParams {
                owner: Option<String>,
            }
            let request: HandoverParams = parse_optional_params(params)?;
            let Ok(_idle) = self.handover_gate.try_write() else {
                return Ok(json!({ "ok": false }));
            };
            let _crash_admission = self.lock(&self.crash_autosave_status, "crash status")?;
            let owner_matches = self
                .handover_owner
                .as_deref()
                .is_some_and(|owner| request.owner.as_deref() == Some(owner));
            let idle = (!self.parent_bound || owner_matches)
                && self.crash_jobs.load(Ordering::Acquire) == 0
                && self.media.active_viewport_count() == 0;
            if idle {
                self.trigger_shutdown();
            }
            return Ok(json!({ "ok": idle }));
        }
        let _request = (!method.starts_with("system.")).then(|| {
            self.handover_gate
                .read()
                .unwrap_or_else(PoisonError::into_inner)
        });
        // R3 S11: once shutdown started, only `system.*` is served, so no client can start a
        // capture or open a viewport after the subsystems were cleaned up.
        if self.is_shutting_down() && !method.starts_with("system.") {
            return Err(DaemonError::Conflict(format!(
                "arbd is shutting down; `{method}` rejected"
            )));
        }
        match method {
            "system.hello" => self.handle_system_hello(),
            "system.status" => self.handle_system_status(),
            "system.shutdown" => Ok(json!({ "ok": true })),
            "config.get" => self.handle_config_get(),
            "config.update" => self.handle_config_update(params),
            "device.list" => Self::handle_device_list(),
            "capture.start" => self.handle_capture_start(params),
            "capture.stop" => {
                let _guard = self.lock(&self.config_update_lock, "config update")?;
                let changes = self.persist_capture_intent(CaptureIntent::Stopped, None)?;
                let mut response = self.stop_active_capture()?;
                response.config_changed = changes;
                Ok(serde_json::to_value(response)?)
            }
            "save.trigger" => self.handle_save_trigger(params),
            "save.logs" => self.handle_save_logs(params),
            "save.screenshot" => self.handle_save_screenshot(params),
            "save.list_jobs" => self.handle_save_list_jobs(),
            "save.retry" => self.handle_save_retry(params),
            "save.retarget" => self.handle_save_retarget(params),
            "save.discard" => self.handle_save_discard(params),
            "bundle.manifest" => self.handle_bundle_manifest(params),
            "bundle.list" => self.handle_bundle_list(params),
            "bundle.open" => self.handle_bundle_open(params),
            "bundle.lease_renew" => self.handle_bundle_lease_renew(params),
            "bundle.close" => self.handle_bundle_close(params),
            "bundle.trash" => self.handle_bundle_trash(params),
            "bundle.restore" => self.handle_bundle_restore(params),
            "bundle.gc" => self.handle_bundle_gc(params),
            "bundle.forget" => self.handle_bundle_forget(params),
            "timeline.summary" => self.handle_timeline_summary(params),
            "timeline.overview" => self.handle_timeline_overview(params),
            "timeline.query_logs" | "bundle.query_logs" => self.handle_timeline_query_logs(params),
            "timeline.query_taps" => self.handle_timeline_query_taps(params),
            "timeline.discontinuities" => self.handle_timeline_discontinuities(params),
            "timeline.crashes" => self.handle_timeline_crashes(params),
            "timeline.screens" => self.handle_timeline_screens(params),
            "edit.get_session" => self.handle_edit_get_session(params),
            "edit.update_session" => self.handle_edit_update_session(params),
            "export.trim" => self.handle_export_trim(params),
            "export.png" | "export.frame_png" => self.handle_export_png(params),
            "export.share_mp4" => self.handle_export_share_mp4(params),
            "media.open_viewport" => self.handle_media_open_viewport(params),
            "media.filmstrip" => self.handle_media_filmstrip(params),
            "media.scrub" => self.handle_media_scrub(params),
            "media.seek_frame" => self.handle_media_seek_frame(params),
            "media.close_viewport" => self.handle_media_close_viewport(params),
            "media.decode_audio_pcm" => self.handle_media_decode_audio_pcm(params),
            "media.inspect_frames" => self.handle_media_inspect_frames(params),
            other => Err(DaemonError::MethodNotFound(other.to_owned())),
        }
    }

    fn handle_system_status(&self) -> Result<Value, DaemonError> {
        let (cfg, capture_view, ring_chunks, ring_bytes, ring_pinned, buffered_secs) = {
            // Keep the configured preference stable until the capture snapshot is taken.
            let _guard = self.lock(&self.config_update_lock, "config update")?;
            let config = self.lock(&self.user_config, "user config")?.clone();
            let cap = self.lock_capture()?;
            let (chunks, bytes, pinned) = cap.ring_stats()?;
            (
                config,
                cap.status_view(),
                chunks,
                bytes,
                pinned,
                cap.buffered_secs(),
            )
        };
        let active_viewports = self.media.active_viewport_count();
        let active_save_jobs = self
            .catalog
            .list_save_jobs()?
            .into_iter()
            .filter(|j| !j.job.state.is_terminal())
            .count();
        let uptime_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut crash_autosave = self
            .lock(&self.crash_autosave_status, "crash autosave status")?
            .clone();
        crash_autosave.enabled = cfg.crash_autosave_enabled;
        crash_autosave.cooldown_secs = cfg.crash_autosave_cooldown_secs;

        Ok(serde_json::to_value(SystemStatusResponse {
            capture_enabled: cfg.capture_enabled,
            serial_mismatch: capture_view.active
                && capture_view.connected
                && cfg
                    .device_serial
                    .as_ref()
                    .zip(capture_view.serial.as_ref())
                    .is_some_and(|(configured, actual)| configured != actual),
            configured_device_serial: cfg.device_serial,
            pid: std::process::id(),
            uptime_ms,
            capture: capture_view,
            ring_chunk_count: ring_chunks,
            ring_total_bytes: ring_bytes,
            ring_pinned_chunk_count: ring_pinned,
            buffered_secs,
            active_viewports,
            active_save_jobs,
            recovery: RecoverySummaryView {
                removed_staging_dirs: self.recovery.removed_staging_dirs.len(),
                reaped_leases: self.recovery.reaped_lease_ids.len(),
                recovered_jobs: self.recovery.recovered_job_ids.len(),
                completed_purging_bundles: self.recovery.completed_purging_bundle_ids.len(),
                failed_purging_bundles: self.recovery.failed.len(),
                failed_purging_bundle_ids: self
                    .recovery
                    .failed
                    .iter()
                    .map(|f| f.bundle_id.clone())
                    .collect(),
                released_pin_journals: self.pin_reconcile.released_job_ids.len(),
                repinned_jobs: self.pin_reconcile.repinned_job_ids.len(),
            },
            crash_autosave,
            config_warning: self.config_load_warning.clone(),
        })?)
    }

    fn handle_config_get(&self) -> Result<Value, DaemonError> {
        let config = self.lock(&self.user_config, "user config")?.clone();
        Ok(serde_json::to_value(ConfigResponse {
            config,
            config_path: self.config_store.path(),
        })?)
    }

    fn handle_config_update(&self, params: Value) -> Result<Value, DaemonError> {
        let patch: ArbConfigPatch = parse_params(params)?;
        if self.config_unreadable {
            return Err(DaemonError::Conflict(
                "config.json was not loaded at startup (see system.status config_warning); restore or remove it and restart arbd before changing settings"
                    .to_owned(),
            ));
        }
        let _update_guard = self.lock(&self.config_update_lock, "config update")?;
        let candidate = {
            let current = self.lock(&self.user_config, "user config")?;
            current
                .apply_patch(&patch)
                .map_err(|err| DaemonError::InvalidParams(err.to_string()))?
        };
        if let Some(dir) = &candidate.output_dir_override {
            self.validate_output_location(dir, "output_dir_override", None)?;
        }
        if let Some(dir) = &candidate.screenshot_dir_override {
            self.validate_output_location(dir, "screenshot_dir_override", None)?;
            self.reject_data_dir_state_file(&dir.join("probe.png"), "screenshot_dir_override")?;
        }
        self.config_store.save_atomic(&candidate)?;
        {
            let mut guard = self.lock(&self.user_config, "user config")?;
            *guard = candidate.clone();
        }
        {
            let mut cap = self.lock_capture()?;
            cap.select_audio_mode(candidate.audio_capture_mode);
            cap.set_ring_policy(candidate.to_ring_budget_policy());
            if patch.capture_enabled == Some(true) {
                cap.clear_storage_pressure_stop();
            }
        }
        if patch.capture_enabled == Some(false)
            || patch.device_serial != arb_core::config::DeviceSerialPatch::Unchanged
        {
            self.stop_active_capture()?;
        }
        Ok(serde_json::to_value(ConfigResponse {
            config: candidate,
            config_path: self.config_store.path(),
        })?)
    }

    fn handle_device_list() -> Result<Value, DaemonError> {
        let adb = AdbClient::discover()?;
        let devices = adb.devices()?;
        Ok(serde_json::to_value(DeviceListResponse { devices })?)
    }

    /// `capture.start` (#583). An explicit `serial` is only a tentative target: nothing reaches
    /// `config.json` or `live.request` until that device is `Selected` and its capture started,
    /// and then `(capture_enabled=true, device_serial)` is committed in one atomic write. A start
    /// without `serial` keeps the always-on arming contract (DS-02/DS-03).
    fn handle_capture_start(self: &Arc<Self>, params: Value) -> Result<Value, DaemonError> {
        let request: CaptureStartParams = parse_optional_params(params)?;
        let explicit = request
            .serial
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let wait = Duration::from_millis(request.wait_timeout_ms.unwrap_or(5000).min(30_000));
        // A reservation is a single discovery (auto-attach) or another caller's bounded wait;
        // queue behind it instead of cancelling it, and report a conflict if it never ends.
        let reservation_deadline = Instant::now()
            + START_RESERVATION_WAIT.max(if request.wait_connected {
                wait
            } else {
                Duration::ZERO
            });
        // The connection wait starts once this call holds the slot, so queuing never uses it up.
        let deadline_from_now = || request.wait_connected.then(|| Instant::now() + wait);
        let epoch = self.lock(&self.live_capture, "live capture")?.stop_epoch;
        let (reservation, parsed, mut response) = loop {
            match self.admit_capture_start(&request, explicit.as_deref(), epoch)? {
                StartAdmission::Reserved(ticket, parsed, response) => {
                    break (ticket, parsed, *response);
                }
                StartAdmission::AlreadyActive(response, generation) => {
                    return self.wait_for_connection(
                        serde_json::to_value(*response)?,
                        Some(generation),
                        deadline_from_now(),
                    );
                }
                StartAdmission::Busy if Instant::now() < reservation_deadline => {
                    thread::sleep(Duration::from_millis(DRAIN_POLL_INTERVAL_MS));
                }
                StartAdmission::Busy => {
                    return Err(DaemonError::CaptureConflict {
                        reason: "attach_in_progress",
                        active_serial: None,
                        requested_serial: explicit,
                    });
                }
            }
        };
        let deadline = deadline_from_now();
        let observed = Mutex::new(None);
        // A deadline always goes through the bounded worker, even `wait_timeout_ms = 0`, so a
        // hung `adb devices` can never hold the reply past it.
        let outcome = if let Some(end) = deadline {
            self.attach_with_deadline(&reservation, parsed, end, &observed, epoch)?
        } else {
            Some(self.attach_reserved(&reservation, &parsed, None, Some(&observed))?)
        };
        // `None`: the deadline passed and this call cancelled its own reservation. The last
        // discovery may still read `Selected` (the worker saw the device but never started it),
        // so a timed-out attach is reported as a timeout, never as a started session.
        let timed_out = outcome.is_none();
        let (selection, generation) = outcome.unwrap_or_else(|| {
            let seen = observed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            (
                seen.unwrap_or(DeviceSelectionOutcome::NoOnlineDevices),
                None,
            )
        });
        let waited_out = deadline.is_some()
            && (timed_out
                || matches!(
                    selection,
                    DeviceSelectionOutcome::NoOnlineDevices
                        | DeviceSelectionOutcome::WaitingForPreferredDevice { .. }
                ));
        if !timed_out && let DeviceSelectionOutcome::Selected { serial } = selection {
            if let Some(explicit) = explicit.as_deref() {
                response.config_changed =
                    self.commit_explicit_start(&request, explicit, generation, epoch)?;
            }
            response.started = true;
            response.serial = serial;
            return self.wait_for_connection(serde_json::to_value(response)?, generation, deadline);
        }
        if let Some(serial) = explicit {
            return Err(DaemonError::StartTargetNotFound {
                serial: serial.clone(),
                // Still waiting for this serial at the deadline (it may have been seen online
                // while the attach never got to start it).
                selection: match selection {
                    DeviceSelectionOutcome::NoOnlineDevices
                    | DeviceSelectionOutcome::Selected { .. } => {
                        DeviceSelectionOutcome::WaitingForPreferredDevice {
                            preferred_serial: serial,
                            other_online_serials: Vec::new(),
                        }
                    }
                    other => other,
                },
                wait_timed_out: waited_out,
            });
        }
        response.wait_timed_out = Some(waited_out);
        response.warnings = vec![waiting_for_device_warning(&selection)];
        response.selection = Some(selection);
        Ok(serde_json::to_value(response)?)
    }

    /// Decides, under `config_update_lock` and the live lock, whether a start may reserve the
    /// attach slot. Only a start without `serial` persists anything here (the armed intent).
    fn admit_capture_start(
        &self,
        request: &CaptureStartParams,
        explicit: Option<&str>,
        epoch: u64,
    ) -> Result<StartAdmission, DaemonError> {
        let _guard = self.lock(&self.config_update_lock, "config update")?;
        if self.is_shutting_down() {
            return Err(DaemonError::Conflict("arbd is shutting down".to_owned()));
        }
        let mut live = self.lock(&self.live_capture, "live capture")?;
        if live.stop_epoch != epoch {
            // A stop arrived while this start was queued: it must not resume recording.
            return Err(start_cancelled());
        }
        let parsed = self.capture_options(request.clone())?;
        let serial_source = if explicit.is_some() {
            SerialSource::Argument
        } else if parsed.serial.is_some() {
            SerialSource::Config
        } else {
            SerialSource::Auto
        };
        let mut response = CaptureStartResponse {
            serial_source: Some(serial_source),
            config_changed: CaptureConfigChanges::default(),
            selection: None,
            serial: parsed.serial.clone().unwrap_or_default(),
            capture_mode: parsed.capture_mode.unwrap_or_default(),
            started: false,
            connected: Some(false),
            wait_timed_out: Some(false),
            audio_state: Some(protocol::AudioCaptureState::Unknown),
            audio_cause: None,
            already_active: false,
            warnings: Vec::new(),
        };
        if live.handle.as_ref().is_some_and(|h| !h.is_finished()) {
            let (status, generation) = {
                let cap = self.lock_capture()?;
                (cap.status_view(), cap.session_generation())
            };
            if let Some(requested) = explicit
                && status.serial.as_deref() != Some(requested)
            {
                return Err(DaemonError::CaptureConflict {
                    reason: "different_active_serial",
                    active_serial: status.serial,
                    requested_serial: Some(requested.to_owned()),
                });
            }
            // The same device keeps recording: an idempotent start, never a restart (CI-08).
            response.config_changed =
                self.persist_capture_intent(CaptureIntent::Armed, explicit)?;
            response.started = true;
            response.already_active = true;
            response.connected = Some(status.connected);
            response.serial = status.serial.unwrap_or_default();
            response.capture_mode = status.capture_mode;
            response.audio_state = Some(status.audio_state);
            response.audio_cause = status.audio_cause;
            return Ok(StartAdmission::AlreadyActive(
                Box::new(response),
                generation,
            ));
        }
        if live.reservation.is_some() {
            return Ok(StartAdmission::Busy);
        }
        if explicit.is_none() {
            response.config_changed = self.persist_capture_intent(CaptureIntent::Armed, None)?;
            self.lock_capture()?.clear_storage_pressure_stop();
            live.request = request.clone();
        } else if self.config_unreadable {
            return Err(DaemonError::Conflict(
                "config.json could not be read".into(),
            ));
        }
        let ticket = Arc::new(());
        live.reservation = Some(Arc::clone(&ticket));
        Ok(StartAdmission::Reserved(ticket, parsed, Box::new(response)))
    }

    /// Commits an explicit start once its device is `Selected` and capturing: one atomic write
    /// of `(capture_enabled=true, device_serial)`. A stop or reselection that replaced the
    /// session in between wins, and nothing is persisted.
    fn commit_explicit_start(
        &self,
        request: &CaptureStartParams,
        serial: &str,
        generation: Option<u64>,
        epoch: u64,
    ) -> Result<CaptureConfigChanges, DaemonError> {
        let (committed, orphan) = {
            let _guard = self.lock(&self.config_update_lock, "config update")?;
            let mut live = self.lock(&self.live_capture, "live capture")?;
            let current = live.stop_epoch == epoch
                && live.handle.as_ref().is_some_and(|h| !h.is_finished())
                && generation == Some(self.lock_capture()?.session_generation());
            if !current {
                return Err(start_cancelled());
            }
            let committed = self.persist_capture_intent(CaptureIntent::Armed, Some(serial));
            let orphan = if committed.is_ok() {
                live.request = request.clone();
                live.request.serial = Some(serial.to_owned());
                None
            } else {
                // Detach exactly this tentative session while it is still provably ours.
                live.handle.take()
            };
            (committed, orphan)
        };
        if let Some(mut handle) = orphan {
            handle.stop();
            if let Some(generation) = generation {
                self.lock_capture()?.mark_stopped_if_current(generation);
            }
        }
        committed
    }

    fn attach_with_deadline(
        self: &Arc<Self>,
        reservation: &Arc<()>,
        parsed: CaptureStartParams,
        deadline: Instant,
        observed: &Mutex<Option<DeviceSelectionOutcome>>,
        epoch: u64,
    ) -> Result<Option<AttachResult>, DaemonError> {
        let (tx, rx) = mpsc::channel();
        let state = Arc::clone(self);
        let worker_ticket = Arc::clone(reservation);
        let seen = Arc::new(Mutex::new(None));
        let worker_seen = Arc::clone(&seen);
        let spawned = thread::Builder::new()
            .name("arbd-connect-wait".into())
            .spawn(move || {
                let _ = tx.send(state.attach_reserved(
                    &worker_ticket,
                    &parsed,
                    Some(deadline),
                    Some(&*worker_seen),
                ));
            });
        if let Err(err) = spawned {
            self.release_reservation(reservation, epoch)?;
            return Err(err.into());
        }
        let result = self.await_attach(&rx, deadline);
        observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone_from(&seen.lock().unwrap_or_else(PoisonError::into_inner));
        if !matches!(result, Ok(None)) {
            return result;
        }
        // Deadline passed. A reservation still ours is cancelled, so a late attach cannot start
        // a session after this caller was told it timed out. A stop that took it reports the
        // cancellation now, without waiting for a discovery that may still be blocked. Only when
        // the worker itself consumed it (under the live lock) is its result the answer.
        match self.release_reservation(reservation, epoch)? {
            ReservationFate::Released => return Ok(None),
            ReservationFate::Cancelled => return Err(start_cancelled()),
            ReservationFate::Consumed => {}
        }
        loop {
            if self.is_shutting_down() {
                return Err(DaemonError::Conflict("arbd is shutting down".to_owned()));
            }
            match rx.recv_timeout(Duration::from_millis(DRAIN_POLL_INTERVAL_MS)) {
                Ok(r) => return r.map(Some),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }

    /// Drops `reservation` if it is still the active one, and otherwise tells whether a stop
    /// (epoch changed) or the attach worker took it.
    fn release_reservation(
        &self,
        reservation: &Arc<()>,
        epoch: u64,
    ) -> Result<ReservationFate, DaemonError> {
        let mut live = self.lock(&self.live_capture, "live capture")?;
        if live
            .reservation
            .as_ref()
            .is_some_and(|ticket| Arc::ptr_eq(ticket, reservation))
        {
            live.reservation = None;
            return Ok(ReservationFate::Released);
        }
        Ok(if live.stop_epoch == epoch {
            ReservationFate::Consumed
        } else {
            ReservationFate::Cancelled
        })
    }

    /// Waits for the attach worker; `None` means the deadline passed first.
    fn await_attach(
        &self,
        rx: &Receiver<AttachOutcome>,
        deadline: Instant,
    ) -> Result<Option<AttachResult>, DaemonError> {
        loop {
            if self.is_shutting_down() {
                return Err(DaemonError::Conflict("arbd is shutting down".to_owned()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            match rx.recv_timeout(remaining.min(Duration::from_millis(DRAIN_POLL_INTERVAL_MS))) {
                Ok(r) => return r.map(Some),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(DaemonError::Internal("capture start worker exited".into()));
                }
            }
        }
    }

    fn wait_for_connection(
        &self,
        mut result: Value,
        generation: Option<u64>,
        deadline: Option<Instant>,
    ) -> Result<Value, DaemonError> {
        loop {
            let cap = self.lock_capture()?;
            let current = generation == Some(cap.session_generation());
            let status = cap.status_view();
            let connected = current && status.connected;
            let pending = current && !connected && cap.is_active() && !self.is_shutting_down();
            drop(cap);
            let remaining = deadline.map_or(Duration::ZERO, |d| {
                d.saturating_duration_since(Instant::now())
            });
            if !pending || remaining.is_zero() {
                if deadline.is_some() {
                    result["started"] = json!(connected);
                }
                result["connected"] = json!(connected);
                result["wait_timed_out"] = json!(deadline.is_some() && pending);
                result["audio_state"] = if current {
                    json!(status.audio_state)
                } else {
                    json!("unknown")
                };
                result["audio_cause"] = if current {
                    json!(status.audio_cause)
                } else {
                    Value::Null
                };
                return Ok(result);
            }
            thread::sleep(remaining.min(Duration::from_millis(DRAIN_POLL_INTERVAL_MS)));
        }
    }

    fn persist_capture_intent(
        &self,
        intent: CaptureIntent,
        explicit: Option<&str>,
    ) -> Result<CaptureConfigChanges, DaemonError> {
        let enabled = intent == CaptureIntent::Armed;
        let mut candidate = self.lock(&self.user_config, "user config")?.clone();
        let changes = CaptureConfigChanges {
            capture_enabled: (candidate.capture_enabled != enabled)
                .then_some((candidate.capture_enabled, enabled)),
            device_serial: explicit
                .filter(|serial| Some(*serial) != candidate.device_serial.as_deref())
                .map(|serial| (candidate.device_serial.clone(), Some(serial.to_owned()))),
        };
        if changes != CaptureConfigChanges::default() {
            if self.config_unreadable {
                return Err(DaemonError::Conflict(
                    "config.json could not be read".into(),
                ));
            }
            candidate.capture_enabled = enabled;
            if let Some((_, new)) = &changes.device_serial {
                candidate.device_serial.clone_from(new);
            }
            self.config_store.save_atomic(&candidate)?;
            *self.lock(&self.user_config, "user config")? = candidate;
        }
        Ok(changes)
    }

    fn capture_options(
        &self,
        mut parsed: CaptureStartParams,
    ) -> Result<CaptureStartParams, DaemonError> {
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();
        let effective_mode = parsed.capture_mode.unwrap_or(user_cfg.capture_mode);
        parsed.capture_mode = Some(effective_mode);
        if parsed.serial.as_deref().is_none_or(|s| s.trim().is_empty()) {
            parsed.serial.clone_from(&user_cfg.device_serial);
        }
        if let Some(serial) = parsed.serial.as_deref().map(str::trim)
            && !serial.is_empty()
        {
            validate_adb_serial(serial)
                .map_err(|err| DaemonError::InvalidParams(err.to_string()))?;
            parsed.serial = Some(serial.to_owned());
        }
        if parsed.max_size.is_none() {
            parsed.max_size = Some(user_cfg.video_max_size);
        }
        if parsed.max_fps.is_none() {
            parsed.max_fps = Some(user_cfg.video_max_fps);
        }
        if effective_mode == CaptureMode::LogsOnly {
            parsed.audio_enabled = Some(false);
            parsed.input_enabled = Some(false);
        } else if parsed.audio_enabled.is_none() {
            parsed.audio_enabled = Some(user_cfg.audio_enabled);
        }
        for (name, jar) in [
            ("companion_jar", parsed.companion_jar.as_deref()),
            ("scrcpy_jar", parsed.scrcpy_jar.as_deref()),
        ] {
            if let Some(path) = jar
                && !path.is_absolute()
            {
                return Err(DaemonError::InvalidParams(format!(
                    "{name} must be an absolute path (got `{}`)",
                    path.display()
                )));
            }
        }
        Ok(parsed)
    }

    fn attach_reserved(
        &self,
        ticket: &Arc<()>,
        parsed: &CaptureStartParams,
        deadline: Option<Instant>,
        observed: Option<&Mutex<Option<DeviceSelectionOutcome>>>,
    ) -> AttachOutcome {
        use arb_core::device::DeviceSelectionOutcome::{
            NoOnlineDevices, WaitingForPreferredDevice,
        };
        loop {
            let discovery = AdbClient::discover()
                .map_err(DaemonError::from)
                .and_then(|adb| {
                    let outcome = select_live_target(&adb, parsed.serial.as_deref())?;
                    Ok((adb, outcome))
                });
            if let (Some(observed), Ok((_, outcome))) = (observed, &discovery) {
                *observed.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome.clone());
            }
            let mut live = self.lock(&self.live_capture, "live capture")?;
            if self.is_shutting_down()
                || !live
                    .reservation
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(active, ticket))
            {
                return Err(start_cancelled());
            }
            // Keep this request's reservation across discovery attempts. A stop or replacement
            // invalidates the ticket, and the background supervisor cannot claim this generation.
            if let Some(end) = deadline
                && matches!(
                    &discovery,
                    Ok((_, NoOnlineDevices | WaitingForPreferredDevice { .. }))
                )
            {
                let remaining = end.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    drop(live);
                    thread::sleep(remaining.min(Duration::from_millis(100)));
                    continue;
                }
            }
            live.reservation = None;
            let (adb, selection) = discovery?;
            if let DeviceSelectionOutcome::Selected { serial } = selection {
                // A start that reached its device may record again after a pressure stop; a
                // start that never got here leaves the latch (and the generation) untouched.
                self.lock_capture()?.clear_storage_pressure_stop();
                let (serial, handle) = start_selected_capture(parsed, &self.capture, adb, serial)?;
                live.handle = Some(handle);
                let generation = self.lock_capture()?.session_generation();
                return Ok((
                    DeviceSelectionOutcome::Selected { serial },
                    Some(generation),
                ));
            }
            return Ok((selection, None));
        }
    }

    fn auto_attach(&self) -> Result<(), DaemonError> {
        let (ticket, parsed) = {
            let _guard = self.lock(&self.config_update_lock, "config update")?;
            if !self.lock(&self.user_config, "user config")?.capture_enabled
                || self.is_shutting_down()
            {
                return Ok(());
            }
            if self.lock_capture()?.is_storage_pressure_stopped() {
                self.persist_capture_intent(CaptureIntent::Stopped, None)?;
                self.stop_active_capture()?;
                return Ok(());
            }
            let mut live = self.lock(&self.live_capture, "live capture")?;
            if live.reservation.is_some() || live.handle.as_ref().is_some_and(|h| !h.is_finished())
            {
                return Ok(());
            }
            let parsed = self.capture_options(live.request.clone())?;
            let ticket = Arc::new(());
            live.reservation = Some(Arc::clone(&ticket));
            (ticket, parsed)
        };
        self.attach_reserved(&ticket, &parsed, None, None)?;
        Ok(())
    }

    fn handle_save_trigger(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveTriggerParams = parse_optional_params(params)?;
        Ok(serde_json::to_value(self.execute_save(parsed)?)?)
    }

    fn handle_save_logs(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveLogsParams = parse_optional_params(params)?;
        let outcome = self.execute_save(SaveTriggerParams {
            note: parsed.note,
            duration_sec: parsed.window_secs,
            dest_dir: parsed.dest_dir,
            drain_timeout_ms: parsed.drain_timeout_ms,
            selection_policy: parsed.selection_policy,
            mode: SaveMode::LogsOnly,
        })?;
        Ok(serde_json::to_value(outcome)?)
    }

    fn execute_save(&self, params: SaveTriggerParams) -> Result<SaveOutcomeResponse, DaemonError> {
        let SaveTriggerParams {
            duration_sec,
            dest_dir,
            drain_timeout_ms,
            selection_policy,
            mode,
            note,
        } = params;
        let trigger_kind = SaveTriggerKind::Manual;
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();
        let duration_sec = duration_sec.unwrap_or_else(|| u64::from(user_cfg.save_window_secs));
        let window_range = u64::from(MIN_SAVE_WINDOW_SECS)..=u64::from(MAX_SAVE_WINDOW_SECS);
        if !window_range.contains(&duration_sec) {
            return Err(DaemonError::InvalidParams(format!(
                "save window {duration_sec}s is outside {MIN_SAVE_WINDOW_SECS}..={MAX_SAVE_WINDOW_SECS} seconds"
            )));
        }
        let mut policy = selection_policy.unwrap_or_default();
        policy.drain_timeout_ms = drain_timeout_ms.unwrap_or(policy.drain_timeout_ms);
        if policy.drain_timeout_ms > 5_000 {
            return Err(DaemonError::InvalidParams(
                "drain_timeout_ms は 0～5000 ミリ秒で指定してください".into(),
            ));
        }
        let dest_dir = self.resolve_output_dir(
            dest_dir.as_deref(),
            user_cfg.output_dir_override.as_deref(),
            self.catalog.managed_bundles_root(),
            None,
        )?;
        let limits = SelectionPolicy::default();
        policy.unanchored_ingest_margin_ns = policy
            .unanchored_ingest_margin_ns
            .min(limits.unanchored_ingest_margin_ns);
        policy.max_unanchored_records = policy
            .max_unanchored_records
            .min(limits.max_unanchored_records);
        policy.max_unanchored_bytes = policy.max_unanchored_bytes.min(limits.max_unanchored_bytes);

        // Phase 1 (R3 S1): fix T_trigger and pin the window chunks immediately, under `C`.
        // When the active/last capture session is `CaptureMode::LogsOnly`, coerce `SaveMode::Full`
        // to `SaveMode::LogsOnly` so `save.trigger` and crash auto-save write a valid WORM
        // `logs_only` bundle cleanly without failing on missing video frames.
        let pending = {
            let mut cap = self.lock_capture()?;
            let effective_mode =
                if mode == SaveMode::Full && cap.capture_mode() == CaptureMode::LogsOnly {
                    SaveMode::LogsOnly
                } else {
                    mode
                };
            cap.pin_request(
                duration_sec,
                &dest_dir,
                policy,
                effective_mode,
                trigger_kind,
                note,
            )?
        };

        self.finish_pinned_save(pending, policy, trigger_kind)
    }

    fn finish_pinned_save(
        &self,
        pending: PendingSave,
        policy: SelectionPolicy,
        trigger_kind: SaveTriggerKind,
    ) -> Result<SaveOutcomeResponse, DaemonError> {
        // Bounded boundary drain (`SPEC-02` §2) with `C` released between polls.
        let tail = match pending.drain_trigger_boottime_ns() {
            Some(trigger_bt) => {
                self.wait_for_boundary_drain(&policy, trigger_bt, pending.generation)?
            }
            None => pending.tail_without_drain(),
        };

        // Phase 1 commit: extend pins to tail chunks, persist params, create the catalog job.
        let (job, plan) = self
            .lock_capture()?
            .phase1_commit(&self.catalog, pending, tail)?;
        // From here the catalog job owns the ring pins; earlier failures roll the pins back.

        if trigger_kind == SaveTriggerKind::CrashAutoSave && self.is_shutting_down() {
            // Do not start a long bundle write that would outlive this daemon instance; the
            // journaled job is re-materialised by startup recovery of the next arbd.
            return Err(DaemonError::Conflict(
                "daemon is shutting down; crash auto-save left for startup recovery".to_owned(),
            ));
        }

        // Phase 2 without `C` (R3 M2): live readers, heartbeats and status never stall.
        phase2_materialize(&plan, &self.catalog, &job.job.job_id)
    }

    fn screenshot_output_path(
        &self,
        config: &ArbConfig,
        explicit_path: Option<&Path>,
        output_dir: Option<&Path>,
        source_bundle: Option<&Path>,
    ) -> Result<PathBuf, DaemonError> {
        if let Some(path) = explicit_path {
            if output_dir.is_some() {
                return Err(DaemonError::InvalidParams(
                    "pass either output_path or output_dir, not both".to_owned(),
                ));
            }
            self.validate_output_location(path, "output_path", source_bundle)?;
            self.reject_data_dir_state_file(path, "output_path")?;
            return Ok(path.to_owned());
        }

        let name = format!("{}.png", next_id("screenshot"));
        let path = if let Some(dir) = output_dir.or(config.screenshot_dir_override.as_deref()) {
            let path = dir.join(name);
            self.reject_data_dir_state_file(&path, "output_dir")?;
            path
        } else if let Some(dir) = &config.output_dir_override {
            // The configured bundle output root may itself be under data_dir. Only an
            // automatically named, no-replace PNG may use this fallback.
            dir.join(name)
        } else {
            // The daemon's own --data-dir may be relative. RPC-supplied paths must be absolute.
            let path = std::fs::canonicalize(&self.paths.data_dir)?
                .join("screenshots")
                .join(name);
            self.reject_data_dir_state_file(&path, "output_dir")?;
            path
        };
        self.validate_output_location(&path, "output_dir", source_bundle)?;
        Ok(path)
    }

    fn handle_save_screenshot(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveScreenshotParams = parse_optional_params(params)?;
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();
        let dest_path = self.screenshot_output_path(
            &user_cfg,
            parsed.output_path.as_deref(),
            parsed.output_dir.as_deref(),
            None,
        )?;
        let active_serial = self.lock_capture()?.active_serial();
        let requested_serial = parsed
            .serial
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or(active_serial)
            .or(user_cfg.device_serial);
        if let Some(serial) = requested_serial.as_deref() {
            validate_adb_serial(serial)
                .map_err(|err| DaemonError::InvalidParams(err.to_string()))?;
        }

        let (serial, png_bytes) = self
            .screenshot_provider
            .capture_png(requested_serial.as_deref())?;
        let captured_at_unix_ms = current_unix_millis();
        let overwrite = if parsed.output_path.is_some() && parsed.overwrite {
            ScreenshotOverwrite::Replace
        } else {
            ScreenshotOverwrite::NoReplace
        };
        let (width, height) = write_screenshot_atomic(&dest_path, &png_bytes, overwrite)?;
        let sha256 = sha256_hex(&png_bytes);

        Ok(serde_json::to_value(SaveScreenshotResponse {
            path: dest_path,
            width,
            height,
            sha256,
            captured_at_unix_ms,
            serial,
        })?)
    }

    fn wait_for_boundary_drain(
        &self,
        policy: &SelectionPolicy,
        trigger_boottime_ns: i64,
        generation: u64,
    ) -> Result<TailCompleteness, DaemonError> {
        let start = Instant::now();
        loop {
            let latest_hb = {
                let cap = self.lock_capture()?;
                if cap.session_generation() != generation || !cap.is_connected() {
                    return Ok(TailCompleteness::MayBeIncomplete);
                }
                cap.drain_watermark_ns()
            };
            let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            match evaluate_stream_drain(trigger_boottime_ns, latest_hb, elapsed_ms, policy) {
                DrainOutcome::DrainedComplete => return Ok(TailCompleteness::Complete),
                DrainOutcome::TimedOutTailIncomplete => {
                    return Ok(TailCompleteness::MayBeIncomplete);
                }
                DrainOutcome::Waiting | _ => {
                    thread::sleep(Duration::from_millis(DRAIN_POLL_INTERVAL_MS));
                }
            }
        }
    }

    fn handle_save_list_jobs(&self) -> Result<Value, DaemonError> {
        let jobs = self
            .catalog
            .list_save_jobs()?
            .iter()
            .map(save_job_to_view)
            .collect();
        Ok(serde_json::to_value(SaveListJobsResponse { jobs })?)
    }

    fn handle_save_retry(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveJobIdParams = parse_params(params)?;
        let plan = self
            .lock_capture()?
            .prepare_retry(&self.catalog, &parsed.job_id)?;
        let outcome = phase2_materialize(&plan, &self.catalog, &parsed.job_id)?;
        Ok(serde_json::to_value(outcome)?)
    }

    fn handle_save_retarget(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveRetargetParams = parse_params(params)?;
        self.validate_output_location(&parsed.dest_dir, "dest_dir", None)?;
        let plan = self.lock_capture()?.prepare_retarget(
            &self.catalog,
            &parsed.job_id,
            &parsed.dest_dir,
        )?;
        let outcome = phase2_materialize(&plan, &self.catalog, &parsed.job_id)?;
        Ok(serde_json::to_value(outcome)?)
    }

    fn handle_save_discard(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveJobIdParams = parse_params(params)?;
        let view = self
            .lock_capture()?
            .discard_save_job(&self.catalog, &parsed.job_id)?;
        Ok(serde_json::to_value(view)?)
    }

    fn handle_bundle_manifest(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleRefParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        Ok(arb_storage::bundle_io::read_bundle_manifest(&path)?)
    }
    fn handle_bundle_list(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleListParams = parse_optional_params(params)?;
        let entries = self.catalog.list_bundles(parsed.state)?;
        let bundles = entries
            .into_iter()
            .map(|entry| self.bundle_entry_view(entry))
            .collect::<Result<_, _>>()?;
        Ok(serde_json::to_value(BundleListResponse { bundles })?)
    }

    fn bundle_entry_view(&self, entry: BundleEntry) -> Result<BundleEntryView, DaemonError> {
        let active_leases = self
            .catalog
            .active_leases_for_bundle(&entry.bundle_id)?
            .len();
        let disk_state = self.catalog.bundle_disk_state(&entry.path);
        Ok(BundleEntryView {
            created_utc: Some(entry.created_at_utc.clone()),
            summary: match self.catalog.bundle_summary(&entry.bundle_id)? {
                Some(summary) => summary,
                None if disk_state == arb_storage::catalog::BundleDiskState::Present => {
                    let measured = open_bundle(&entry.path)
                        .ok()
                        .and_then(|r| inspect_export::catalog_summary(r.as_ref()).ok());
                    if let Some(summary) = &measured {
                        self.catalog
                            .record_bundle_summary(&entry.bundle_id, summary)?;
                    }
                    measured.unwrap_or_default()
                }
                None => protocol::CatalogSummary::default(),
            },
            bundle_id: entry.bundle_id,
            path: entry.path,
            format_version: entry.format_version,
            state: entry.state,
            trashed_at_utc: entry.trashed_at_utc,
            parent_bundle_id: entry.parent_bundle_id,
            created_at_utc: entry.created_at_utc,
            active_leases,
            disk_state,
        })
    }

    fn handle_bundle_open(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleOpenParams = parse_params(params)?;
        let holder_kind = parsed.holder_kind.unwrap_or(LeaseHolderKind::Cli);
        // R3 S6: a lease must belong to a live client process; defaulting to the always-alive
        // daemon PID made a crashed client's lease unreapable.
        let holder_pid = parsed.holder_pid.ok_or_else(|| {
            DaemonError::InvalidParams("missing required field 'holder_pid'".to_owned())
        })?;
        if !is_pid_alive(holder_pid) {
            return Err(DaemonError::InvalidParams(format!(
                "holder_pid {holder_pid} is not a running process"
            )));
        }
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let summary = build_timeline_summary(reader.as_ref(), &path)?;
        let now_utc = utc_now_iso8601();

        let bundle_id = if let Some(entry) = existing {
            entry.bundle_id
        } else {
            let format_version = if reader.is_legacy() {
                CatalogFormatVersion::LegacyRo
            } else {
                CatalogFormatVersion::V2
            };
            let candidate_id = summary.bundle_id.clone();
            let reg_id = if self.catalog.bundle(&candidate_id)?.is_some() {
                crate::time_util::next_id("ext")
            } else {
                candidate_id.clone()
            };
            let new_entry = NewBundleEntry {
                bundle_id: reg_id.clone(),
                path: path.clone(),
                format_version,
                parent_bundle_id: match &summary.provenance {
                    arb_core::bundle::Provenance::Trimmed {
                        parent_bundle_id, ..
                    } => Some(parent_bundle_id.clone()),
                    _ => None,
                },
                created_at_utc: summary
                    .created_at_utc
                    .clone()
                    .unwrap_or_else(|| now_utc.clone()),
            };
            let _ = if format_version == CatalogFormatVersion::V2 && reg_id != candidate_id {
                self.catalog
                    .register_bundle_with_manifest_id(&new_entry, &candidate_id)?
            } else {
                self.catalog.register_bundle(&new_entry)?
            };
            reg_id
        };

        if self.catalog.bundle_summary(&bundle_id)?.is_none()
            && let Ok(measured) = inspect_export::catalog_summary(reader.as_ref())
        {
            self.catalog.record_bundle_summary(&bundle_id, &measured)?;
        }
        let lease_id = self
            .catalog
            .acquire_lease(&bundle_id, holder_kind, holder_pid, &now_utc)?;

        Ok(serde_json::to_value(BundleOpenResponse {
            bundle_id,
            path,
            format: summary.format,
            is_legacy: summary.is_legacy,
            lease_id,
            summary,
        })?)
    }

    fn handle_bundle_lease_renew(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: LeaseIdParams = parse_params(params)?;
        let now_utc = utc_now_iso8601();
        self.catalog.renew_lease(&parsed.lease_id, &now_utc)?;
        Ok(json!({ "renewed": true, "lease_id": parsed.lease_id }))
    }

    fn handle_bundle_close(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: LeaseIdParams = parse_params(params)?;
        let released = self.catalog.release_lease(&parsed.lease_id)?;
        Ok(json!({ "released": released, "lease_id": parsed.lease_id }))
    }

    fn handle_bundle_trash(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleIdParams = parse_params(params)?;
        let now_utc = utc_now_iso8601();
        let entry = self.catalog.trash_bundle(&parsed.bundle_id, &now_utc)?;
        Ok(serde_json::to_value(self.bundle_entry_view(entry)?)?)
    }

    fn handle_bundle_restore(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleIdParams = parse_params(params)?;
        let entry = self.catalog.restore_bundle(&parsed.bundle_id)?;
        Ok(serde_json::to_value(self.bundle_entry_view(entry)?)?)
    }

    fn handle_bundle_gc(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleGcParams = parse_optional_params(params)?;
        let now_utc = utc_now_iso8601();
        let cutoff = parsed.now_utc.as_deref().unwrap_or(&now_utc);
        let grace = if parsed.ignore_grace {
            0
        } else {
            self.lock(&self.user_config, "user config")?.gc_grace_secs
        };
        let report = self.catalog.collect_garbage(
            cutoff,
            grace,
            &now_utc,
            if parsed.dry_run {
                arb_storage::catalog::GcMode::Preview
            } else {
                arb_storage::catalog::GcMode::Execute
            },
        )?;
        let counts = report.preview.as_ref().map(|p| {
            [
                ("candidate", p.candidate_bundle_ids.len()),
                ("missing", p.missing_bundle_ids.len()),
                ("legacy", p.legacy_bundle_ids.len()),
                ("unmanaged", p.unmanaged_bundle_ids.len()),
                ("expired_leases", p.expired_lease_ids.len()),
                ("leased", report.skipped_leased_bundle_ids.len()),
                ("active_job", report.skipped_active_job_bundle_ids.len()),
                ("in_grace", report.skipped_in_grace_bundle_ids.len()),
                ("failed", report.failed.len()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect()
        });
        Ok(serde_json::to_value(BundleGcResponse {
            dry_run: parsed.dry_run,
            preview: report.preview,
            counts,
            purged_bundle_ids: report.purged_bundle_ids,
            skipped_leased_bundle_ids: report.skipped_leased_bundle_ids,
            skipped_active_job_bundle_ids: report.skipped_active_job_bundle_ids,
            skipped_unmanaged_or_legacy_bundle_ids: report.skipped_unmanaged_or_legacy_bundle_ids,
            skipped_in_grace_bundle_ids: report.skipped_in_grace_bundle_ids,
            reaped_lease_ids: report.reaped_lease_ids,
            failed: report
                .failed
                .into_iter()
                .map(|f| BundleGcFailure {
                    bundle_id: f.bundle_id,
                    error: f.error,
                })
                .collect(),
        })?)
    }

    fn handle_bundle_forget(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleForgetParams = parse_optional_params(params)?;
        let target_id = parsed
            .bundle_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if parsed.all_missing && parsed.bundle_id.is_some() {
            return Err(DaemonError::InvalidParams(
                "specify either `bundle_id` or `all_missing: true`, not both".to_owned(),
            ));
        }
        let now_utc = utc_now_iso8601();
        let report = if parsed.all_missing {
            self.catalog.prune_missing_bundles(&now_utc)?
        } else if let Some(bundle_id) = target_id {
            self.catalog.forget_missing_bundle(bundle_id, &now_utc)?
        } else {
            return Err(DaemonError::InvalidParams(
                "`bundle.forget` requires `bundle_id` or `all_missing: true`".to_owned(),
            ));
        };
        Ok(serde_json::to_value(BundleForgetResponse {
            forgotten_bundle_ids: report.forgotten_bundle_ids,
            skipped_present_bundle_ids: report.skipped_present_bundle_ids,
            skipped_storage_unavailable_bundle_ids: report.skipped_storage_unavailable_bundle_ids,
            skipped_leased_bundle_ids: report.skipped_leased_bundle_ids,
            skipped_active_job_bundle_ids: report.skipped_active_job_bundle_ids,
            reaped_lease_ids: report.reaped_lease_ids,
        })?)
    }

    fn handle_timeline_summary(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleRefParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let summary = build_timeline_summary(reader.as_ref(), &path)?;
        Ok(serde_json::to_value(summary)?)
    }

    fn handle_timeline_overview(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: TimelineOverviewParams = parse_params(params)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id)?;
        let catalog_id = existing.as_ref().map(|e| e.bundle_id.as_str());
        let overview = inspect_export::build_timeline_overview(&path, catalog_id, &parsed)?;
        Ok(serde_json::to_value(overview)?)
    }

    fn handle_timeline_query_logs(&self, params: Value) -> Result<Value, DaemonError> {
        let mut parsed: TimelineQueryParams = parse_params(params)?;
        if parsed.limit == Some(0) {
            return Err(DaemonError::InvalidParams("limit must be positive".into()));
        }
        if parsed.limit.is_some_and(|limit| limit > MAX_QUERY_LIMIT) {
            return Err(DaemonError::InvalidParams(format!(
                "limit must not exceed {MAX_QUERY_LIMIT}"
            )));
        }
        if let Some(cursor) = &parsed.cursor {
            if parsed.after_seq.is_some() {
                return Err(DaemonError::InvalidParams(
                    "cursor and after_seq cannot be combined".into(),
                ));
            }
            parsed.after_seq = Some(cursor.parse().map_err(|_| {
                DaemonError::InvalidParams("cursor must be a decimal sequence".into())
            })?);
        }
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let bucket = match parsed.bucket.unwrap_or(QueryBucket::All) {
            QueryBucket::Main => Some(RecordBucket::Main),
            QueryBucket::Unanchored => Some(RecordBucket::Unanchored),
            QueryBucket::All => None,
        };
        let response = inspect_export::query_log_response(
            reader.as_ref(),
            bucket,
            &parsed,
            effective_query_limit(parsed.limit),
        )?;
        Ok(serde_json::to_value(response)?)
    }

    fn handle_timeline_query_taps(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: TimelineQueryParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let bucket = match parsed.bucket.unwrap_or(QueryBucket::All) {
            QueryBucket::Main => Some(RecordBucket::Main),
            QueryBucket::Unanchored => Some(RecordBucket::Unanchored),
            QueryBucket::All => None,
        };
        let range = TimeRange {
            start_ns: parsed.start_ns,
            end_ns: parsed.end_ns,
        };
        let taps = reader.query_taps(bucket, range, Some(effective_query_limit(parsed.limit)))?;
        Ok(serde_json::to_value(TimelineTapsResponse { taps })?)
    }

    fn handle_timeline_discontinuities(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleRefParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let discontinuities = reader.discontinuities()?;
        Ok(serde_json::to_value(TimelineDiscontinuitiesResponse {
            discontinuities,
        })?)
    }

    fn handle_timeline_crashes(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleRefParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let crashes = reader.crashes()?;
        Ok(serde_json::to_value(TimelineCrashesResponse { crashes })?)
    }

    fn handle_timeline_screens(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleRefParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let screens =
            inspect_export::screens_with_offsets(reader.as_ref(), &reader.display_frames()?)?;
        Ok(serde_json::to_value(TimelineScreensResponse { screens })?)
    }

    fn handle_edit_get_session(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: EditGetSessionParams = parse_params(params)?;
        let session = self
            .catalog
            .get_edit_session(&parsed.session_id)?
            .map(|s| EditSessionView {
                session_id: s.session_id,
                bundle_id: s.bundle_id,
                in_frame_index: s.in_frame_index,
                in_time_ns: s.in_time_ns,
                out_time_ns: s.out_time_ns,
                out_frame_index: s.out_frame_index,
                playhead_frame_index: s.playhead_frame_index,
                av_offset_override_ms: s.av_offset_override_ms,
                updated_at_utc: s.updated_at_utc,
            });
        Ok(json!({ "session": session }))
    }

    fn handle_edit_update_session(&self, params: Value) -> Result<Value, DaemonError> {
        let mut parsed: EditUpdateSessionParams = parse_params(params)?;
        if let Some(times) = trim_range::time_pair(parsed.in_time_ns, parsed.out_time_ns)? {
            let (path, _) = self.resolve_bundle_path(&parsed.bundle_id)?;
            let reader = arb_storage::bundle_io::open_bundle(path)?;
            let video = reader.video_path().ok_or_else(|| {
                DaemonError::InvalidParams("動画のないバンドルには範囲を指定できません".into())
            })?;
            let track = arb_media::mp4::Mp4VideoTrack::open(video)?;
            let range = trim_range::TrimRange::resolve(&track, (None, None), Some(times))?;
            parsed.in_frame_index = Some(range.first);
            parsed.out_frame_index = Some(range.last);
        }
        let now_utc = utc_now_iso8601();
        let session = EditSession {
            session_id: parsed.session_id,
            bundle_id: parsed.bundle_id,
            in_frame_index: parsed.in_frame_index,
            in_time_ns: parsed.in_time_ns,
            out_time_ns: parsed.out_time_ns,
            out_frame_index: parsed.out_frame_index,
            playhead_frame_index: parsed.playhead_frame_index,
            av_offset_override_ms: parsed.av_offset_override_ms,
            updated_at_utc: now_utc,
        };
        self.catalog.upsert_edit_session(&session)?;
        Ok(json!({
            "session": EditSessionView {
                session_id: session.session_id,
                bundle_id: session.bundle_id,
                in_frame_index: session.in_frame_index,
                in_time_ns: session.in_time_ns,
                out_time_ns: session.out_time_ns,
                out_frame_index: session.out_frame_index,
                playhead_frame_index: session.playhead_frame_index,
                av_offset_override_ms: session.av_offset_override_ms,
                updated_at_utc: session.updated_at_utc,
            }
        }))
    }

    fn resolve_output_dir(
        &self,
        explicit: Option<&Path>,
        configured: Option<&Path>,
        fallback: &Path,
        source: Option<&Path>,
    ) -> Result<PathBuf, DaemonError> {
        if let Some((dir, name)) = explicit
            .map(|p| (p, "dest_dir"))
            .or_else(|| configured.map(|p| (p, "output_dir_override")))
        {
            self.validate_output_location(dir, name, source)?;
            Ok(dir.to_path_buf())
        } else {
            Ok(fallback.to_path_buf())
        }
    }

    fn handle_export_trim(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: ExportTrimParams = parse_params(params)?;
        trim_range::time_pair(parsed.in_time_ns, parsed.out_time_ns)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();
        let dest_parent = self.resolve_output_dir(
            parsed.dest_dir.as_deref(),
            user_cfg.output_dir_override.as_deref(),
            self.catalog.managed_bundles_root(),
            Some(&path),
        )?;
        let source_id = existing.as_ref().map(|e| e.bundle_id.as_str());
        if let Some(output) = &parsed.share_output_path {
            self.validate_output_location(output, "output_path", Some(&path))?;
            self.reject_data_dir_state_file(output, "output_path")?;
            if !(parsed.include_share_mp4 || parsed.burn_taps) || output.exists() {
                return Err(DaemonError::InvalidParams(
                    "共有 MP4 の出力先を作成できません".into(),
                ));
            }
        }
        // R3 S9: exports run outside the media mutex `M` with their own codec backend.
        let backend = default_codec_backend();
        let ctx = ExportContext {
            catalog: &self.catalog,
            data_dir: &self.paths.data_dir,
            backend: backend.as_ref(),
            faults: &self.faults,
        };
        let resp = media_engine::export_trim(&ctx, &path, source_id, &dest_parent, &parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_export_png(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: ExportPngParams = parse_params(params)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        // R3 S7: validate before anything is created; never write into a WORM bundle.
        let config = self.lock(&self.user_config, "user config")?.clone();
        let output_path = self.screenshot_output_path(
            &config,
            parsed
                .output_path
                .as_deref()
                .filter(|p| !p.as_os_str().is_empty()),
            parsed.output_dir.as_deref(),
            Some(&path),
        )?;
        let source_id = existing.as_ref().map(|e| e.bundle_id.as_str());
        let backend = default_codec_backend();
        let ctx = ExportContext {
            catalog: &self.catalog,
            data_dir: &self.paths.data_dir,
            backend: backend.as_ref(),
            faults: &self.faults,
        };
        let resp =
            media_engine::export_png(&ctx, &path, source_id, parsed.frame_index, &output_path)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_export_share_mp4(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: ExportShareMp4Params = parse_params(params)?;
        trim_range::time_pair(parsed.in_time_ns, parsed.out_time_ns)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id)?;
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();

        let dest_parent = self.resolve_output_dir(
            parsed.dest_dir.as_deref(),
            user_cfg.output_dir_override.as_deref(),
            self.catalog.managed_bundles_root(),
            Some(&path),
        )?;
        let share_mp4_path = if let Some(explicit) = &parsed.output_path {
            self.validate_output_location(explicit, "output_path", Some(&path))?;
            self.reject_data_dir_state_file(explicit, "output_path")?;
            explicit.clone()
        } else {
            let base_dir = self.resolve_output_dir(
                parsed.dest_dir.as_deref(),
                user_cfg.output_dir_override.as_deref(),
                &self.paths.data_dir.join("exports"),
                Some(&path),
            )?;
            let stem = next_id("share");
            let file_name = if parsed.burn_taps {
                format!("{stem}-taps.mp4")
            } else {
                format!("{stem}.mp4")
            };
            let output = base_dir.join(file_name);
            self.reject_data_dir_state_file(&output, "dest_dir")?;
            output
        };

        let backend = default_codec_backend();
        let ctx = ExportContext {
            catalog: &self.catalog,
            data_dir: &self.paths.data_dir,
            backend: backend.as_ref(),
            faults: &self.faults,
        };
        let resp = inspect_export::export_share_mp4(
            &ctx,
            &path,
            existing.as_ref(),
            &dest_parent,
            &share_mp4_path,
            &parsed,
        )?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_decode_audio_pcm(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaDecodeAudioPcmParams = parse_params(params)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id)?;
        let source_id = existing.as_ref().map(|e| e.bundle_id.as_str());
        let resp = inspect_export::decode_audio_pcm(
            &self.catalog,
            &self.paths.cache_dir,
            &path,
            source_id,
        )?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_inspect_frames(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaInspectFramesParams = parse_params(params)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id)?;
        let source_id = existing.as_ref().map(|e| e.bundle_id.as_str());
        if parsed.metadata_only {
            return inspect_export::inspect_metadata(&self.catalog, &path, source_id, &parsed);
        }
        let backend = default_codec_backend();
        let ctx = ExportContext {
            catalog: &self.catalog,
            data_dir: &self.paths.data_dir,
            backend: backend.as_ref(),
            faults: &self.faults,
        };
        let resp =
            inspect_export::inspect_frames(&ctx, &self.paths.cache_dir, &path, source_id, &parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_open_viewport(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaOpenViewportParams = parse_params(params)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let id_override = existing.as_ref().map(|e| e.bundle_id.as_str());
        if self.is_shutting_down() {
            return Err(DaemonError::Conflict("arbd is shutting down".to_owned()));
        }
        let resp = self.media.open_viewport(&path, id_override)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_filmstrip(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFilmstripParams = parse_params(params)?;
        let resp = self.media.filmstrip(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_scrub(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFrameRequestParams = parse_params(params)?;
        let resp = self.media.scrub(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_seek_frame(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFrameRequestParams = parse_params(params)?;
        let resp = self.media.seek_frame(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_close_viewport(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaCloseViewportParams = parse_params(params)?;
        let closed = self.media.close_viewport(&parsed.viewport_id);
        Ok(json!({ "closed": closed, "viewport_id": parsed.viewport_id }))
    }

    /// Closes viewports a client opened but never closed, once its connection ends (R3 M7).
    fn close_client_viewports(&self, viewport_ids: &[String]) {
        if viewport_ids.is_empty() {
            return;
        }
        for id in viewport_ids {
            let _ = self.media.close_viewport(id);
        }
    }

    /// Runs one request, converting a panic into a `-32603` error and a graceful shutdown
    /// instead of a silently dropped connection and a zombie daemon (R3 S15).
    fn dispatch_guarded(
        self: &Arc<Self>,
        method: &str,
        params: Value,
    ) -> Result<Value, DaemonError> {
        if let Ok(result) = catch_unwind(AssertUnwindSafe(|| self.dispatch(method, params))) {
            result
        } else {
            self.trigger_shutdown();
            Err(DaemonError::Internal(format!(
                "`{method}` panicked; arbd is shutting down"
            )))
        }
    }
}

/// Running local `arbd` daemon server holding single-instance lock and IPC accept loop.
#[derive(Debug)]
pub struct DaemonServer {
    state: Arc<DaemonState>,
    _data_lock: SingleInstanceLock,
    _lock: SingleInstanceLock,
    accept_thread: Option<JoinHandle<()>>,
    attach_thread: Option<JoinHandle<()>>,
    crash_worker_thread: Option<JoinHandle<()>>,
}

impl DaemonServer {
    /// Starts `arbd` with the given [`DaemonConfig`]:
    /// 1. Acquires [`SingleInstanceLock`] at `<runtime_dir>/arbd.lock`.
    /// 2. Removes stale `*.shm` viewport files left by a crashed daemon (R3 M7).
    /// 3. Loads persisted `config.json` (falling back to defaults if absent).
    /// 4. Opens [`Catalog`] and runs [`Catalog::recover_on_startup_with_pins`].
    /// 5. Opens Zone-1 [`CapturePipeline`] (`RingStore`) and [`MediaEngine`].
    /// 6. Binds [`LocalIpcListener`] and spawns the concurrent accept & crash-autosave loops.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if locking, config loading, storage recovery, or IPC binding fails.
    pub fn start(config: DaemonConfig) -> Result<Self, DaemonError> {
        std::fs::create_dir_all(&config.paths.data_dir)?;
        std::fs::create_dir_all(&config.paths.cache_dir)?;
        std::fs::create_dir_all(&config.paths.runtime_dir)?;
        std::fs::create_dir_all(config.paths.shm_dir())?;

        let lock = SingleInstanceLock::try_acquire(config.paths.lock_file())?;
        let data_lock = SingleInstanceLock::try_acquire(config.paths.data_lock_file())?;
        // Safe only now: the lock guarantees no other daemon owns these viewport files.
        let _removed_shm = MediaEngine::sweep_stale_shm_files(&config.paths.shm_dir());

        let config_store = ConfigStore::new(&config.paths.data_dir);
        let disk_provider = config.disk_provider.clone();
        let (user_config, config_load_warning, config_unreadable) =
            load_user_config_or_defaults(&config_store);
        let effective_ring_policy = if config.ring_policy_overridden {
            config.ring_policy
        } else {
            user_config.to_ring_budget_policy()
        };

        let catalog_db = config.paths.data_dir.join("catalog.sqlite");
        let managed_bundles = config.paths.data_dir.join("bundles");
        let catalog = Catalog::open(&catalog_db, &managed_bundles)?;
        // Catalog recovery AND pin-journal reconciliation (S13): journals of completed /
        // discarded / unknown jobs are released and journals missing for active jobs are
        // re-created, so a crash between `complete_job` and `unpin_job` cannot leak pins.
        let pin_journal = PinJournal::open(&config.paths.data_dir)?;
        capture::recover_provisional_jobs(&config.paths.data_dir, &catalog, &pin_journal)?;
        let (recovery, pin_reconcile) =
            catalog.recover_on_startup_with_pins(&utc_now_iso8601(), &pin_journal)?;

        let (crash_tx, crash_rx) = mpsc::channel();
        let mut capture = CapturePipeline::open(
            &config.paths.data_dir,
            effective_ring_policy,
            config.disk_provider,
        )?;
        capture.select_audio_mode(user_config.audio_capture_mode);
        capture.set_fault_injection(Arc::clone(&config.faults));

        let media = MediaEngine::new(config.paths.cache_dir.clone(), config.paths.shm_dir());

        let endpoint = config.paths.ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint)?;
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let recovered_crashes: Vec<_> = catalog
            .list_save_jobs()?
            .into_iter()
            .filter(|job| {
                job.trigger_kind == SaveTriggerKind::CrashAutoSave
                    && recovery.recovered_job_ids.contains(&job.job.job_id)
            })
            .map(|job| job.job.job_id)
            .collect();
        let initial_crash_status = CrashAutoSaveStatusView {
            enabled: user_config.crash_autosave_enabled,
            cooldown_secs: user_config.crash_autosave_cooldown_secs,
            ..CrashAutoSaveStatusView::default()
        };

        let (build_id, binary_mtime_secs) = compute_daemon_binary_identity();

        let state = Arc::new(DaemonState {
            paths: config.paths,
            started_at: Instant::now(),
            recovery,
            pin_reconcile,
            config_store,
            user_config: Mutex::new(user_config),
            config_update_lock: Mutex::new(()),
            config_load_warning,
            config_unreadable,
            disk_provider,
            catalog,
            capture: Arc::new(Mutex::new(capture)),
            live_capture: Mutex::new(LiveCapture::default()),
            media,
            screenshot_provider: config.screenshot_provider,
            crash_autosave_status: Mutex::new(initial_crash_status),
            crash_autosave_gate: Mutex::new(CrashAutoSaveGate::new()),
            handover_gate: RwLock::new(()),
            crash_jobs: std::sync::atomic::AtomicUsize::new(recovered_crashes.len()),
            crash_tx: Mutex::new(Some(crash_tx)),
            faults: config.faults,
            shutdown_flag: Arc::clone(&shutdown_flag),
            endpoint,
            build_id,
            binary_mtime_secs,
            parent_bound: config.parent_bound,
            handover_owner: std::env::var("ARB_HANDOVER_OWNER")
                .ok()
                .filter(|s| !s.is_empty()),
        });

        state.lock_capture()?.crash_daemon = Arc::downgrade(&state);
        let attach_thread = spawn_auto_attach(Arc::clone(&state))?;
        let accept_state = Arc::clone(&state);
        let accept_thread = thread::Builder::new()
            .name("arbd-ipc-accept".to_owned())
            .spawn(move || {
                run_accept_loop(&listener, &accept_state);
            })?;

        let crash_worker_thread =
            spawn_crash_worker(Arc::clone(&state), crash_rx, recovered_crashes)?;

        Ok(Self {
            state,
            _data_lock: data_lock,
            _lock: lock,
            accept_thread: Some(accept_thread),
            attach_thread: Some(attach_thread),
            crash_worker_thread: Some(crash_worker_thread),
        })
    }

    /// Returns the bound local IPC endpoint path (UDS path or Windows Named Pipe).
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.state.endpoint
    }

    /// Returns the resolved [`ArbPaths`] owned by this daemon.
    #[must_use]
    pub fn paths(&self) -> &ArbPaths {
        &self.state.paths
    }

    /// Returns the startup [`RecoveryReport`] executed when the daemon booted.
    #[must_use]
    pub fn recovery_report(&self) -> &RecoveryReport {
        &self.state.recovery
    }

    /// Returns `true` once a shutdown was requested (`system.shutdown`, a poisoned lock, or
    /// a panicking request).
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.state.is_shutting_down()
    }

    /// Returns a cloneable [`DaemonShutdownHandle`] for triggering graceful shutdown from a
    /// signal watcher or test harness (R3 S11).
    #[must_use]
    pub fn shutdown_handle(&self) -> DaemonShutdownHandle {
        DaemonShutdownHandle {
            state: Arc::clone(&self.state),
        }
    }

    /// Drains all events from any [`DeviceEventSource`] (such as a deterministic
    /// [`arb_adb::mock::MockDeviceSource`]) directly into the live capture pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if reading from `source` or persisting ring chunks fails, or
    /// if the capture pipeline lock is poisoned.
    pub fn ingest_device_source(
        &self,
        source: &mut dyn DeviceEventSource,
    ) -> Result<usize, DaemonError> {
        self.ingest_device_source_with_mode(source, CaptureMode::VideoAndLogs)
    }

    /// Drains all events from any [`DeviceEventSource`] into the capture pipeline using the
    /// requested [`CaptureMode`] (useful for deterministic `logs_only` E2E tests).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if reading from `source` or persisting ring chunks fails, or
    /// if the capture pipeline lock is poisoned.
    pub fn ingest_device_source_with_mode(
        &self,
        source: &mut dyn DeviceEventSource,
        mode: CaptureMode,
    ) -> Result<usize, DaemonError> {
        let mut cap = self.state.lock_capture()?;
        let _generation = cap.begin_session_with_mode(source.serial().to_owned(), mode);
        let mut count = 0_usize;
        while let Some(event) = source.next_event()? {
            cap.ingest_event(event)?;
            count += 1;
        }
        cap.flush_pending_chunk()?;
        Ok(count)
    }

    /// Installs a UID -> Android package name map on the current capture pipeline session
    /// (for deterministic foreground app-log extraction tests without a physical ADB device).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the capture pipeline lock is poisoned.
    pub fn set_capture_package_uids(
        &self,
        uid_packages: BTreeMap<u32, String>,
    ) -> Result<(), DaemonError> {
        self.state.lock_capture()?.set_package_uids(uid_packages);
        Ok(())
    }

    /// Flushes any pending live events into Zone-1 ring chunks.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] on unexpected I/O failure, or an internal error if
    /// the capture pipeline lock is poisoned.
    pub fn flush_capture_ring(&self) -> Result<(), DaemonError> {
        self.state.lock_capture()?.flush_pending_chunk()
    }

    /// Returns a snapshot of live capture discontinuities recorded so far.
    #[must_use]
    pub fn live_discontinuities(&self) -> Vec<arb_core::time::DiscontinuityEvent> {
        self.state
            .capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .discontinuities()
            .to_vec()
    }

    /// Blocks until the daemon exits after receiving `system.shutdown` or [`Self::shutdown`].
    pub fn wait(mut self) {
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        self.cleanup_subsystems();
    }

    /// Requests a graceful shutdown and waits for the IPC accept loop and active capture to finish.
    pub fn shutdown(mut self) {
        self.state.trigger_shutdown();
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        self.cleanup_subsystems();
    }

    /// Stops capture and closes every viewport. Tolerates poisoned locks so a daemon that
    /// is shutting down because of a panic still releases its devices and SHM files.
    fn cleanup_subsystems(&mut self) {
        if let Some(attach) = self.attach_thread.take() {
            attach.thread().unpark();
            let deadline = Instant::now() + CRASH_WORKER_JOIN_TIMEOUT;
            while !attach.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if attach.is_finished() {
                let _ = attach.join();
            }
        }
        let handle = self
            .state
            .live_capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handle
            .take();
        if let Some(mut handle) = handle {
            handle.stop();
        }
        {
            let mut cap = self
                .state
                .capture
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            cap.crash_daemon = std::sync::Weak::new();
            self.state
                .crash_tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            cap.mark_stopped();
        }
        if let Some(worker) = self.crash_worker_thread.take() {
            // The worker may be inside a long auto-save (drain + bundle write): wait only a
            // bounded time, like client threads, and detach it otherwise.
            let deadline = Instant::now() + CRASH_WORKER_JOIN_TIMEOUT;
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
        self.state.media.close_all();
    }
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        self.state.trigger_shutdown();
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        self.cleanup_subsystems();
    }
}

/// Loads `config.json`, never letting a bad file keep arbd from starting.
///
/// Returns `(config, warning, unreadable)`. A parse / validation error sets the file aside and
/// runs on defaults; a transient I/O error leaves the file untouched (and `unreadable = true`
/// so `config.update` is refused rather than replacing the user's real settings).
fn load_user_config_or_defaults(config_store: &ConfigStore) -> (ArbConfig, Option<String>, bool) {
    match config_store.load_or_default() {
        Ok(cfg) => (cfg, None, false),
        // A newer arbd's file must survive a downgrade: leave it, run on defaults, block updates.
        Err(
            err @ StorageError::ConfigValidation(
                arb_core::config::ConfigValidationError::UnsupportedVersion { found },
            ),
        ) if found > arb_core::config::CONFIG_SCHEMA_VERSION => (
            ArbConfig::default(),
            Some(format!(
                "config.json was written by a different arbd version ({err}); defaults are in use and the file was left untouched"
            )),
            true,
        ),
        // Parse failure of a file that claims a newer schema (unknown enum value / fields):
        // same treatment as `UnsupportedVersion`, not corruption.
        Err(err @ StorageError::Json { .. })
            if config_store
                .stored_config_version()
                .is_some_and(|v| v > arb_core::config::CONFIG_SCHEMA_VERSION) =>
        {
            (
                ArbConfig::default(),
                Some(format!(
                    "config.json was written by a newer arbd version ({err}); defaults are in use and the file was left untouched"
                )),
                true,
            )
        }
        Err(err @ (StorageError::Json { .. } | StorageError::ConfigValidation(_))) => {
            let warning = match config_store.quarantine_invalid(&utc_now_iso8601()) {
                Ok(path) => format!(
                    "invalid config.json ({err}); moved to `{}` and defaults are in use",
                    path.display()
                ),
                Err(move_err) => format!(
                    "invalid config.json ({err}); could not move it aside ({move_err}); defaults are in use"
                ),
            };
            (ArbConfig::default(), Some(warning), false)
        }
        Err(err) => (
            ArbConfig::default(),
            Some(format!(
                "config.json could not be read ({err}); defaults are in use and the file was left untouched"
            )),
            true,
        ),
    }
}

fn spawn_auto_attach(attach_state: Arc<DaemonState>) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("arbd-auto-attach".into())
        .spawn(move || {
            while !attach_state.is_shutting_down() {
                adb_resources::reap(&attach_state.paths.data_dir.join("adb-sessions"));
                let _ = attach_state.auto_attach();
                thread::park_timeout(Duration::from_secs(2));
            }
        })
}

fn spawn_crash_worker(
    crash_state: Arc<DaemonState>,
    crash_rx: mpsc::Receiver<PendingSave>,
    recovered_crashes: Vec<String>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("arbd-crash-autosave".to_owned())
        .spawn(move || {
            for id in recovered_crashes {
                let result = crash_state
                    .lock_capture()
                    .and_then(|cap| cap.prepare_retry(&crash_state.catalog, &id))
                    .and_then(|plan| phase2_materialize(&plan, &crash_state.catalog, &id));
                record_crash_outcome(&crash_state, result);
            }
            run_crash_autosave_worker(&crash_state, &crash_rx);
        })
}

fn run_crash_autosave_worker(state: &DaemonState, rx: &Receiver<PendingSave>) {
    while let Ok(pending) = rx.recv() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.finish_pinned_save(
                pending,
                SelectionPolicy::default(),
                SaveTriggerKind::CrashAutoSave,
            )
        }))
        .unwrap_or_else(|_| {
            Err(DaemonError::Internal(
                "crash auto-save worker panicked".into(),
            ))
        });
        record_crash_outcome(state, result);
    }
}

fn record_crash_outcome(state: &DaemonState, result: Result<SaveOutcomeResponse, DaemonError>) {
    let mut status = state
        .crash_autosave_status
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    status.in_flight = state.crash_jobs.fetch_sub(1, Ordering::AcqRel) > 1;
    match result {
        Ok(outcome) => {
            status.last_job_id = Some(outcome.job.job_id);
            status.last_bundle_id = outcome.bundle_id;
            status.last_error = outcome.job.last_error;
        }
        Err(e) => status.last_error = Some(e.to_string()),
    }
}

fn pin_crash_notice(state: &DaemonState, cap: &mut CapturePipeline, notice: &CrashAutoSaveNotice) {
    if state.is_shutting_down() {
        return;
    }
    let cfg = state
        .user_config
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let save_target = cfg
        .output_dir_override
        .clone()
        .unwrap_or_else(|| state.catalog.managed_bundles_root().to_path_buf());
    let storage_pressure_active = notice.storage_pressure_stopped
        || state.host_free_below_floor(&save_target, cap.min_free_disk_bytes());
    let decision = {
        let mut gate = state
            .crash_autosave_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        gate.evaluate(CrashAutoSaveEval {
            enabled: cfg.crash_autosave_enabled,
            storage_pressure_active,
            cooldown_secs: cfg.crash_autosave_cooldown_secs,
            now_ns: notice.pc_cont_ns,
        })
    };

    {
        let mut status = state
            .crash_autosave_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        status.enabled = cfg.crash_autosave_enabled;
        status.cooldown_secs = cfg.crash_autosave_cooldown_secs;
        if state.is_shutting_down() {
            return;
        }
        status.detected_crashes = status.detected_crashes.saturating_add(1);
        status.last_crash_process = Some(notice.process_name.clone());
        status.last_crash_exception = Some(notice.exception_class.clone());
        match decision {
            CrashAutoSaveDecision::Trigger => {
                status.triggered_saves = status.triggered_saves.saturating_add(1);
                state.crash_jobs.fetch_add(1, Ordering::AcqRel);
                status.in_flight = true;
                status.last_error = None;
            }
            CrashAutoSaveDecision::SuppressedCooldown => {
                status.suppressed_cooldown = status.suppressed_cooldown.saturating_add(1);
                return;
            }
            CrashAutoSaveDecision::SuppressedStoragePressure => {
                status.suppressed_storage_pressure =
                    status.suppressed_storage_pressure.saturating_add(1);
                return;
            }
            CrashAutoSaveDecision::Disabled | _ => return,
        }
    }

    let result = (|| {
        if cfg.output_dir_override.is_some() {
            state.validate_output_location(&save_target, "dest_dir", None)?;
        }
        let pending = cap.pin_crash(notice, u64::from(cfg.save_window_secs), &save_target)?;
        state
            .lock(&state.crash_tx, "crash queue")?
            .as_ref()
            .ok_or_else(|| DaemonError::Conflict("daemon is shutting down".into()))?
            .send(pending)
            .map_err(|_| DaemonError::Internal("crash worker unavailable".into()))
    })();
    if let Err(err) = result {
        state
            .crash_autosave_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .abort_last_trigger();
        let mut status = state
            .crash_autosave_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        status.in_flight = state.crash_jobs.fetch_sub(1, Ordering::AcqRel) > 1;
        status.last_error = Some(err.to_string());
    }
}

fn run_accept_loop(listener: &LocalIpcListener, state: &Arc<DaemonState>) {
    let mut client_threads: Vec<JoinHandle<()>> = Vec::new();
    loop {
        if state.is_shutting_down() {
            break;
        }
        let Ok(stream) = listener.accept() else {
            if state.is_shutting_down() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        if state.is_shutting_down() {
            break;
        }
        // Reap completed client threads periodically.
        client_threads.retain(|h| !h.is_finished());

        let client_state = Arc::clone(state);
        if let Ok(handle) = thread::Builder::new()
            .name("arbd-ipc-client".to_owned())
            .spawn(move || handle_client_connection(stream, &client_state))
        {
            client_threads.push(handle);
        }
    }

    // R3 S11: give in-flight requests a bounded time to finish (new non-`system.*` requests
    // are already rejected), join the threads that did, and leave only idle connections
    // blocked in a read detached until process teardown.
    let save_deadline = Instant::now() + Duration::from_secs(15);
    while matches!(
        state.handover_gate.try_write(),
        Err(std::sync::TryLockError::WouldBlock)
    ) && Instant::now() < save_deadline
    {
        thread::sleep(Duration::from_millis(20));
    }
    let deadline = Instant::now() + CLIENT_JOIN_TIMEOUT;
    while Instant::now() < deadline && client_threads.iter().any(|h| !h.is_finished()) {
        thread::sleep(Duration::from_millis(20));
    }
    for handle in client_threads {
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
}

/// Serializes `response`, replacing it with a structured error when it would exceed the
/// IPC frame limit instead of dropping the connection (R3 S10).
fn encode_response(response: &RpcResponse) -> Option<String> {
    let text = serde_json::to_string(response).ok()?;
    if text.len() <= DEFAULT_MAX_IPC_LINE_BYTES {
        return Some(text);
    }
    let too_large = RpcResponse::err(
        response.id.clone(),
        RPC_ERR_RESPONSE_TOO_LARGE,
        format!(
            "response of {} bytes exceeds the {DEFAULT_MAX_IPC_LINE_BYTES}-byte IPC frame limit; narrow the request (limit / start_ns / end_ns)",
            text.len()
        ),
    );
    serde_json::to_string(&too_large).ok()
}

fn handle_client_connection(stream: LocalIpcStream, state: &Arc<DaemonState>) {
    // Poll read timeout (supported on both Unix sockets and overlapped Windows Named Pipes)
    // so idle client threads wake and exit promptly during shutdown (S11). `FrameLineReader`
    // preserves partial frame bytes across poll timeouts so split frames are never lost.
    let _ = stream.set_read_timeout(Some(CLIENT_READ_POLL_TIMEOUT));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut frame_reader = FrameLineReader::new();
    // R3 M7: viewports opened over this connection are closed when it ends.
    let mut owned_viewports: Vec<String> = Vec::new();

    loop {
        if state.is_shutting_down() {
            break;
        }
        let line = match frame_reader.read_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES) {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(arb_os::OsError::Io { source, .. })
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(err) => {
                let resp = RpcResponse::err(Value::Null, RPC_ERR_INVALID_REQUEST, err.to_string());
                if let Ok(text) = serde_json::to_string(&resp) {
                    let _ = write_frame_line(&mut writer, &text, DEFAULT_MAX_IPC_LINE_BYTES);
                    let _ = writer.drain();
                }
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let request: RpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(err) => {
                let resp = RpcResponse::err(Value::Null, RPC_ERR_PARSE, err.to_string());
                if let Ok(text) = serde_json::to_string(&resp) {
                    if write_frame_line(&mut writer, &text, DEFAULT_MAX_IPC_LINE_BYTES).is_err() {
                        break;
                    }
                    let _ = writer.drain();
                }
                continue;
            }
        };

        let is_shutdown = request.method == "system.shutdown";
        let closed_viewport = (request.method == "media.close_viewport")
            .then(|| {
                request
                    .params
                    .get("viewport_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten();
        let response = if request.jsonrpc == JSONRPC_VERSION {
            match state.dispatch_guarded(&request.method, request.params) {
                Ok(result) => {
                    if request.method == "media.open_viewport"
                        && let Some(id) = result.get("viewport_id").and_then(Value::as_str)
                    {
                        owned_viewports.push(id.to_owned());
                    }
                    if let Some(id) = &closed_viewport {
                        owned_viewports.retain(|v| v != id);
                    }
                    RpcResponse::ok(request.id, result)
                }
                Err(err) => err.into_response(request.id),
            }
        } else {
            RpcResponse::err(
                request.id,
                RPC_ERR_INVALID_REQUEST,
                format!("unsupported jsonrpc version `{}`", request.jsonrpc),
            )
        };

        let is_shutdown = is_shutdown && response.error.is_none();
        if is_shutdown {
            state.trigger_shutdown();
        }
        if let Some(text) = encode_response(&response) {
            if write_frame_line(&mut writer, &text, DEFAULT_MAX_IPC_LINE_BYTES).is_err() {
                break;
            }
            let _ = writer.drain();
        }

        if is_shutdown {
            break;
        }
    }

    state.close_client_viewports(&owned_viewports);
}

type AttachResult = (DeviceSelectionOutcome, Option<u64>);

/// How long a `capture.start` queues behind another device reservation before `CONFLICT`.
const START_RESERVATION_WAIT: Duration = Duration::from_secs(5);

/// Admission decision for `capture.start`, taken under the live lock.
enum StartAdmission {
    /// The attach slot is reserved for this call.
    Reserved(Arc<()>, CaptureStartParams, Box<CaptureStartResponse>),
    /// The same session keeps recording; carries its generation.
    AlreadyActive(Box<CaptureStartResponse>, u64),
    /// Auto-attach or another start holds the reservation.
    Busy,
}

/// What happened to a start's reservation when its deadline passed.
enum ReservationFate {
    /// Still ours; this call released it.
    Released,
    /// A stop cleared it.
    Cancelled,
    /// The attach worker consumed it and decided under the live lock.
    Consumed,
}

fn start_cancelled() -> DaemonError {
    DaemonError::Conflict("capture start was cancelled".into())
}

/// `waiting_for_device`: an armed start without `serial` is waiting for its target (DS-02/03).
fn waiting_for_device_warning(selection: &DeviceSelectionOutcome) -> protocol::WarningDetail {
    let message = match selection {
        DeviceSelectionOutcome::MultipleDevicesNeedSelection { .. } => {
            "複数の端末が接続されています。録画する端末を選ぶまで待機します。"
        }
        DeviceSelectionOutcome::WaitingForPreferredDevice { .. } => {
            "設定済みの端末の接続を待っています。接続すると自動で録画を始めます。"
        }
        _ => "端末の接続を待っています。接続すると自動で録画を始めます。",
    };
    let details = serde_json::to_value(selection)
        .ok()
        .and_then(|v| v.as_object().cloned());
    protocol::WarningDetail {
        code: "waiting_for_device".to_owned(),
        message: message.to_owned(),
        details,
    }
}
type AttachOutcome = Result<AttachResult, DaemonError>;

/// Applies the default and maximum `limit` of `timeline.query_*` (R3 S10).
fn effective_query_limit(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_QUERY_LIMIT)
        .min(MAX_QUERY_LIMIT)
}

fn parse_params<T: DeserializeOwned>(value: Value) -> Result<T, DaemonError> {
    let value = if value.is_null() { json!({}) } else { value };
    serde_path_to_error::deserialize(&value).map_err(|err| {
        let detail = err.inner().to_string();
        let path = err.path().to_string();
        let message = if let Some(field) = detail
            .strip_prefix("missing field `")
            .and_then(|s| s.strip_suffix('`'))
        {
            format!("missing required field '{field}'")
        } else if detail.starts_with("invalid type: null") {
            format!("missing required field '{path}'")
        } else if let Some((actual, expected)) = detail
            .strip_prefix("invalid type: ")
            .and_then(|s| s.split_once(", expected "))
        {
            let got = match actual.split_ascii_whitespace().next().unwrap_or(actual) {
                "integer" | "floating" => "number",
                "map" => "object",
                "sequence" => "array",
                "boolean" => "boolean",
                "string" => "string",
                other => other,
            };
            let expected = match expected {
                "a string" | "path string" | "a path string" => "string",
                "a boolean" => "boolean",
                other => other,
            };
            format!("invalid type for '{path}': expected {expected}, got {got}")
        } else {
            format!("{path}: {detail}")
        };
        DaemonError::InvalidParams(message)
    })
}

fn parse_optional_params<T: DeserializeOwned + Default>(value: Value) -> Result<T, DaemonError> {
    if value.is_null() {
        return Ok(T::default());
    }
    parse_params(value)
}

impl std::fmt::Debug for DaemonState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonState")
            .field("endpoint", &self.endpoint)
            .field("data_dir", &self.paths.data_dir)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod supervisor_hardening_tests {
    use super::*;
    use arb_os::error::OsError;
    use arb_os::fs::DiskSpaceInfo;
    use arb_os::paths::ArbPaths;
    use arb_storage::ring::DiskSpaceProvider;
    use std::sync::atomic::AtomicU64;

    #[derive(Debug)]
    struct ConfigurableDisk(Arc<AtomicU64>);
    impl DiskSpaceProvider for ConfigurableDisk {
        fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
            let free = self.0.load(Ordering::Acquire);
            Ok(DiskSpaceInfo {
                available_bytes: free,
                total_bytes: free.max(10_000_000_000),
            })
        }
    }

    fn test_paths(tmp: &tempfile::TempDir) -> ArbPaths {
        ArbPaths {
            data_dir: tmp.path().join("d"),
            cache_dir: tmp.path().join("c"),
            runtime_dir: tmp.path().join("r"),
        }
    }

    #[test]
    fn start_wait_returns_promptly_when_daemon_shuts_down_during_discovery() {
        let tmp = tempfile::tempdir().expect("temp");
        let server = DaemonServer::start(DaemonConfig::new(test_paths(&tmp))).expect("daemon");
        let (_worker, rx) = mpsc::channel();
        let state = Arc::clone(&server.state);
        let waiter = thread::spawn(move || {
            let started = Instant::now();
            let outcome = state.await_attach(&rx, started + Duration::from_secs(30));
            (outcome.is_err(), started.elapsed())
        });
        thread::sleep(Duration::from_millis(100));
        server.state.trigger_shutdown();
        let (failed, elapsed) = waiter.join().expect("waiter");
        assert!(failed, "shutdown must cancel the wait");
        assert!(elapsed < Duration::from_secs(2), "woke after {elapsed:?}");
        server.shutdown();
    }

    #[test]
    fn start_wait_distinguishes_connection_timeout_and_replaced_generation() {
        let tmp = tempfile::tempdir().expect("temp");
        let server = DaemonServer::start(DaemonConfig::new(test_paths(&tmp))).expect("daemon");
        let generation = server
            .state
            .lock_capture()
            .expect("capture")
            .begin_session("mock");
        let wait = |g| {
            server
                .state
                .wait_for_connection(json!({"serial":"mock"}), Some(g), Some(Instant::now()))
                .expect("wait")
        };
        let before = wait(generation);
        assert_eq!(before["connected"], false, "connection is pending");
        assert_eq!(before["wait_timed_out"], true, "deadline reached");
        let mut source = arb_adb::mock::MockDeviceSource::from_scenario(
            arb_adb::mock::FaultScenario::NormalFhd30,
        );
        while let Some(event) = source.next_event().expect("mock event") {
            let mut cap = server.state.lock_capture().expect("capture");
            cap.ingest_event(event).expect("event");
            if cap.is_connected() {
                break;
            }
        }
        assert_eq!(wait(generation)["connected"], true, "confirmed");
        assert_eq!(
            wait(generation)["wait_timed_out"],
            false,
            "connected at deadline"
        );
        assert_eq!(
            wait(generation + 1)["connected"],
            false,
            "unrelated generation"
        );
        server.shutdown();
    }

    #[test]
    fn p1_second_runtime_cannot_recover_same_data() {
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = test_paths(&tmp);
        let first = DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap();
        paths.runtime_dir = tmp.path().join("r2");
        assert!(DaemonServer::start(DaemonConfig::new(paths.clone())).is_err());
        first.shutdown();
        DaemonServer::start(DaemonConfig::new(paths))
            .unwrap()
            .shutdown();
    }

    #[test]
    fn p1_audit_shutdown_releases_data_lock_even_with_detached_idle_client() {
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = test_paths(&tmp);
        // Also verify same-directory runtime_dir == data_dir never self-deadlocks.
        paths.runtime_dir = paths.data_dir.clone();
        let first = DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap();
        // Hold an idle client stream open so `run_accept_loop` detaches its worker thread
        // (which keeps an `Arc<DaemonState>` alive past `DaemonServer::shutdown()`).
        let _idle_stream = arb_os::ipc::LocalIpcStream::connect(first.endpoint()).unwrap();
        let _held_handle = first.shutdown_handle();
        first.shutdown();

        let mut second_paths = paths;
        second_paths.runtime_dir = tmp.path().join("r2");
        let second = DaemonServer::start(DaemonConfig::new(second_paths))
            .expect("second daemon must acquire .arbd-data.lock immediately after shutdown");
        second.shutdown();
    }

    #[test]
    fn p1_shutdown_waits_for_save_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let server = DaemonServer::start(DaemonConfig::new(test_paths(&tmp))).unwrap();
        let state = Arc::clone(&server.state);
        let save = state.handover_gate.read().unwrap();
        let shutdown = thread::spawn(move || server.shutdown());
        thread::sleep(CLIENT_JOIN_TIMEOUT + Duration::from_millis(200));
        assert!(!shutdown.is_finished());
        drop(save);
        shutdown.join().unwrap();
    }

    #[test]
    fn handover_waits_for_requests_and_queued_crash_saves() {
        let tmp = tempfile::tempdir().unwrap();
        let server = DaemonServer::start(DaemonConfig::new(test_paths(&tmp))).unwrap();
        let state = &server.state;
        let request = state.handover_gate.read().unwrap();
        let handover = || {
            state
                .dispatch("system.shutdown_for_handover", json!({}))
                .unwrap()["ok"]
                .clone()
        };
        assert_eq!(handover(), false);
        state.crash_jobs.store(1, Ordering::Release);
        drop(request);
        assert_eq!(handover(), false);
        state.crash_jobs.store(0, Ordering::Release);
        assert_eq!(handover(), true);
        assert!(state.dispatch("save.trigger", json!({})).is_err());
        server.shutdown();
    }

    #[test]
    fn rearming_after_storage_pressure_stop_survives_auto_attach() {
        let tmp = tempfile::tempdir().unwrap();
        let free = Arc::new(AtomicU64::new(1024));
        let server = DaemonServer::start(DaemonConfig::new(test_paths(&tmp)).with_disk_provider(
            SharedDiskSpaceProvider::new(Arc::new(ConfigurableDisk(Arc::clone(&free)))),
        ))
        .unwrap();

        server
            .state
            .dispatch(
                "config.update",
                json!({ "capture_enabled": true, "device_serial": "absent-test-device" }),
            )
            .unwrap();
        {
            let mut cap = server.state.lock_capture().unwrap();
            cap.begin_session("A");
            let mut source = arb_adb::mock::MockDeviceSource::from_scenario(
                arb_adb::mock::FaultScenario::NormalFhd30,
            );
            while let Some(ev) = source.next_event().unwrap() {
                cap.ingest_event(ev).unwrap();
                if cap.is_storage_pressure_stopped() {
                    break;
                }
            }
            assert!(cap.is_storage_pressure_stopped());
        }

        // auto_attach must disarm capture_enabled when storage pressure stops capture.
        let _ = server.state.auto_attach();
        assert_eq!(
            server.state.dispatch("system.status", json!({})).unwrap()["capture_enabled"],
            false
        );

        // User frees disk space and explicitly re-arms while target device is absent.
        free.store(10 * 1024 * 1024 * 1024, Ordering::Release);
        server
            .state
            .dispatch("config.update", json!({ "capture_enabled": true }))
            .unwrap();

        // Subsequent auto_attach ticks must NOT silently disarm capture_enabled back to false.
        let _ = server.state.auto_attach();
        let status = server.state.dispatch("system.status", json!({})).unwrap();
        assert_eq!(
            status["capture_enabled"], true,
            "re-armed capture_enabled must survive auto_attach after pressure clears"
        );
        assert_eq!(status["capture"]["storage_pressure_stopped"], false);

        // Also verify explicit capture.start re-arms cleanly after a second storage-pressure stop.
        free.store(1024, Ordering::Release);
        {
            let mut cap = server.state.lock_capture().unwrap();
            cap.begin_session("A");
            let mut source = arb_adb::mock::MockDeviceSource::from_scenario(
                arb_adb::mock::FaultScenario::NormalFhd30,
            );
            while let Some(ev) = source.next_event().unwrap() {
                cap.ingest_event(ev).unwrap();
                if cap.is_storage_pressure_stopped() {
                    break;
                }
            }
            assert!(cap.is_storage_pressure_stopped());
        }
        let _ = server.state.auto_attach();
        assert_eq!(
            server.state.dispatch("system.status", json!({})).unwrap()["capture_enabled"],
            false
        );

        free.store(10 * 1024 * 1024 * 1024, Ordering::Release);
        // An explicit serial that is not online re-arms nothing (#583); the stored pin does.
        let _ = server
            .state
            .handle_capture_start(json!({ "serial": "absent-test-device" }));
        assert_eq!(
            server.state.dispatch("system.status", json!({})).unwrap()["capture_enabled"],
            false,
            "a failed explicit start must not persist capture_enabled"
        );
        let _ = server.state.handle_capture_start(json!({}));
        let _ = server.state.auto_attach();
        let status = server.state.dispatch("system.status", json!({})).unwrap();
        assert_eq!(
            status["capture_enabled"], true,
            "capture.start re-arm must survive auto_attach after pressure clears"
        );
        assert_eq!(status["capture"]["storage_pressure_stopped"], false);

        server.shutdown();
    }

    #[test]
    fn capture_start_during_shutdown_does_not_persist_capture_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let server = DaemonServer::start(DaemonConfig::new(test_paths(&tmp))).unwrap();
        let state = Arc::clone(&server.state);
        state.trigger_shutdown();
        let err = state
            .handle_capture_start(json!({ "serial": "absent-test-device" }))
            .expect_err("capture.start during shutdown must fail");
        assert!(err.to_string().contains("shutting down"), "{err}");
        let cfg = state.config_store.load_or_default().unwrap();
        assert!(
            !cfg.capture_enabled,
            "failed capture.start during shutdown must not persist capture_enabled"
        );
        drop(state);
        server.shutdown();
    }

    #[test]
    fn shutdown_joins_auto_attach_thread_without_leaking_state() {
        let tmp = tempfile::tempdir().unwrap();
        let server = DaemonServer::start(DaemonConfig::new(test_paths(&tmp))).unwrap();
        let weak = Arc::downgrade(&server.state);
        // Wait briefly so arbd-auto-attach enters park_timeout.
        thread::sleep(Duration::from_millis(50));
        server.shutdown();
        assert!(
            weak.upgrade().is_none(),
            "arbd-auto-attach thread must be joined before shutdown returns"
        );
    }
}
