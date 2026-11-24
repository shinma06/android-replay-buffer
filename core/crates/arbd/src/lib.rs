//! Single-writer local daemon (`arbd`) for Android Replay Buffer (ARB).
//!
//! Coordinates:
//! - [`arb_os::fs::SingleInstanceLock`] and [`arb_os::ipc::LocalIpcListener`]
//! - [`arb_storage::catalog::Catalog`] startup recovery, leases, edit sessions, and GC
//! - [`capture::CapturePipeline`] Zone-1 ring buffer & two-phase save engine
//! - [`live_adb`] live Android device-agent deployment & reconnect supervision
//! - [`media_engine::MediaEngine`] SHM triple-buffered viewport & frame-accurate trim/PNG export

pub mod capture;
pub mod error;
pub mod inspect_export;
pub mod live_adb;
pub mod media_engine;
pub mod protocol;
pub mod screenshot;
pub mod time_util;

use std::collections::BTreeMap;
use std::io::BufReader;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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
use arb_core::device::validate_adb_serial;
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
    CapturePipeline, CrashAutoSaveNotice, FaultInjection, SharedDiskSpaceProvider,
    TailCompleteness, phase2_materialize, save_job_to_view,
};
use crate::error::{
    DaemonError, RPC_ERR_INTERNAL, RPC_ERR_INVALID_REQUEST, RPC_ERR_PARSE,
    RPC_ERR_RESPONSE_TOO_LARGE,
};
use crate::live_adb::{LiveCaptureHandle, start_live_adb_capture};
use crate::media_engine::{ExportContext, MediaEngine, build_timeline_summary};
use crate::protocol::{
    BundleEntryView, BundleGcFailure, BundleGcParams, BundleGcResponse, BundleIdParams,
    BundleListParams, BundleListResponse, BundleOpenParams, BundleOpenResponse, BundleRefParams,
    CaptureStartParams, CaptureStartResponse, CaptureStopResponse, ConfigResponse,
    CrashAutoSaveStatusView, DEFAULT_QUERY_LIMIT, DeviceListResponse, EditGetSessionParams,
    EditSessionView, EditUpdateSessionParams, ExportPngParams, ExportShareMp4Params,
    ExportTrimParams, IPC_PROTOCOL_VERSION, JSONRPC_VERSION, LeaseIdParams, MAX_QUERY_LIMIT,
    MediaCloseViewportParams, MediaDecodeAudioPcmParams, MediaFilmstripParams,
    MediaFrameRequestParams, MediaInspectFramesParams, MediaOpenViewportParams, QueryBucket,
    RecoverySummaryView, RpcRequest, RpcResponse, SaveJobIdParams, SaveListJobsResponse,
    SaveLogsParams, SaveMode, SaveOutcomeResponse, SaveRetargetParams, SaveScreenshotParams,
    SaveScreenshotResponse, SaveTriggerParams, SystemHelloResponse, SystemStatusResponse,
    TimelineCrashesResponse, TimelineDiscontinuitiesResponse, TimelineLogsResponse,
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
        }
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
    live_capture: Mutex<Option<LiveCaptureHandle>>,
    media: Mutex<MediaEngine>,
    screenshot_provider: SharedScreenshotProvider,
    crash_autosave_status: Mutex<CrashAutoSaveStatusView>,
    crash_autosave_gate: Mutex<CrashAutoSaveGate>,
    faults: Arc<FaultInjection>,
    shutdown_flag: Arc<AtomicBool>,
    endpoint: PathBuf,
    build_id: Option<String>,
    binary_mtime_secs: Option<u64>,
}

/// Computes an optional `(build_id, binary_mtime_secs)` fingerprint of the currently running
/// `arbd` executable so clients can detect same-path in-place daemon upgrades.
fn compute_daemon_binary_identity() -> (Option<String>, Option<u64>) {
    let Ok(exe) = std::env::current_exe() else {
        return (Some(env!("CARGO_PKG_VERSION").to_owned()), None);
    };
    let Ok(meta) = std::fs::metadata(&exe) else {
        return (Some(env!("CARGO_PKG_VERSION").to_owned()), None);
    };
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let build_id = Some(format!(
        "{}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        meta.len(),
        mtime_secs.unwrap_or(0)
    ));
    (build_id, mtime_secs)
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

    fn lock_media(&self) -> Result<MutexGuard<'_, MediaEngine>, DaemonError> {
        self.lock(&self.media, "media engine")
    }

    /// Stops the live capture session. The handle is taken out of `L` and `L` is released
    /// **before** joining the supervisor thread (R3 S12), and only the session that was
    /// current when the stop began is marked stopped.
    fn stop_active_capture(&self) -> Result<CaptureStopResponse, DaemonError> {
        let (handle, generation) = {
            let mut live_guard = self.lock(&self.live_capture, "live capture")?;
            let generation = self.lock_capture()?.session_generation();
            (live_guard.take(), generation)
        };
        let was_running = handle.is_some();
        if let Some(mut handle) = handle {
            handle.stop();
        }
        let mut cap = self.lock_capture()?;
        let was_active = was_running || cap.is_active();
        cap.mark_stopped_if_current(generation);
        Ok(CaptureStopResponse {
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
        if let Some(bundle_root) = canon
            .ancestors()
            .find(|dir| dir.join("manifest.json").is_file())
        {
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
            return Ok((entry.path.clone(), Some(entry)));
        }
        let path = PathBuf::from(bundle_id_or_path);
        if !path.is_absolute() {
            if looks_like_path(bundle_id_or_path) {
                return Err(DaemonError::InvalidParams(format!(
                    "bundle path `{bundle_id_or_path}` must be absolute; arbd does not resolve paths against its own working directory"
                )));
            }
            return Err(DaemonError::NotFound(format!(
                "bundle `{bundle_id_or_path}` not found in catalog"
            )));
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
        Err(DaemonError::NotFound(format!(
            "bundle `{bundle_id_or_path}` not found in catalog or filesystem"
        )))
    }

    fn dispatch(&self, method: &str, params: Value) -> Result<Value, DaemonError> {
        // R3 S11: once shutdown started, only `system.*` is served, so no client can start a
        // capture or open a viewport after the subsystems were cleaned up.
        if self.is_shutting_down() && !method.starts_with("system.") {
            return Err(DaemonError::Conflict(format!(
                "arbd is shutting down; `{method}` rejected"
            )));
        }
        match method {
            "system.hello" => Ok(serde_json::to_value(SystemHelloResponse {
                protocol_version: IPC_PROTOCOL_VERSION,
                schema_version: BUNDLE_SCHEMA_VERSION,
                pid: std::process::id(),
                data_dir: self.paths.data_dir.clone(),
                cache_dir: self.paths.cache_dir.clone(),
                runtime_dir: self.paths.runtime_dir.clone(),
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                build_id: self.build_id.clone(),
                binary_mtime_secs: self.binary_mtime_secs,
            })?),
            "system.status" => self.handle_system_status(),
            "system.shutdown" => Ok(json!({ "ok": true })),
            "config.get" => self.handle_config_get(),
            "config.update" => self.handle_config_update(params),
            "device.list" => Self::handle_device_list(),
            "capture.start" => self.handle_capture_start(params),
            "capture.stop" => Ok(serde_json::to_value(self.stop_active_capture()?)?),
            "save.trigger" => self.handle_save_trigger(params),
            "save.logs" => self.handle_save_logs(params),
            "save.screenshot" => self.handle_save_screenshot(params),
            "save.list_jobs" => self.handle_save_list_jobs(),
            "save.retry" => self.handle_save_retry(params),
            "save.retarget" => self.handle_save_retarget(params),
            "save.discard" => self.handle_save_discard(params),
            "bundle.list" => self.handle_bundle_list(params),
            "bundle.open" => self.handle_bundle_open(params),
            "bundle.lease_renew" => self.handle_bundle_lease_renew(params),
            "bundle.close" => self.handle_bundle_close(params),
            "bundle.trash" => self.handle_bundle_trash(params),
            "bundle.restore" => self.handle_bundle_restore(params),
            "bundle.gc" => self.handle_bundle_gc(params),
            "timeline.summary" => self.handle_timeline_summary(params),
            "timeline.overview" => self.handle_timeline_overview(params),
            "timeline.query_logs" => self.handle_timeline_query_logs(params),
            "timeline.query_taps" => self.handle_timeline_query_taps(params),
            "timeline.discontinuities" => self.handle_timeline_discontinuities(params),
            "timeline.crashes" => self.handle_timeline_crashes(params),
            "timeline.screens" => self.handle_timeline_screens(params),
            "edit.get_session" => self.handle_edit_get_session(params),
            "edit.update_session" => self.handle_edit_update_session(params),
            "export.trim" => self.handle_export_trim(params),
            "export.png" => self.handle_export_png(params),
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
        let (capture_view, ring_chunks, ring_bytes, ring_pinned, buffered_secs) = {
            let cap = self.lock_capture()?;
            let (chunks, bytes, pinned) = cap.ring_stats()?;
            (
                cap.status_view(),
                chunks,
                bytes,
                pinned,
                cap.buffered_secs(),
            )
        };
        let active_viewports = self.lock_media()?.active_viewport_count();
        let active_save_jobs = self
            .catalog
            .list_save_jobs()?
            .into_iter()
            .filter(|j| !j.job.state.is_terminal())
            .count();
        let uptime_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let cfg = self.lock(&self.user_config, "user config")?.clone();
        let mut crash_autosave = self
            .lock(&self.crash_autosave_status, "crash autosave status")?
            .clone();
        crash_autosave.enabled = cfg.crash_autosave_enabled;
        crash_autosave.cooldown_secs = cfg.crash_autosave_cooldown_secs;

        Ok(serde_json::to_value(SystemStatusResponse {
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
        self.lock_capture()?
            .set_ring_policy(candidate.to_ring_budget_policy());
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

    fn handle_capture_start(&self, params: Value) -> Result<Value, DaemonError> {
        let mut parsed: CaptureStartParams = parse_optional_params(params)?;
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
        let mut live_guard = self.lock(&self.live_capture, "live capture")?;
        if self.is_shutting_down() {
            return Err(DaemonError::Conflict("arbd is shutting down".to_owned()));
        }
        // R3 S12: a supervisor that exited on its own (e.g. storage pressure) no longer blocks
        // a new capture.start.
        if live_guard.as_ref().is_some_and(|h| !h.is_finished()) {
            return Err(DaemonError::Conflict(
                "capture session is already active; call capture.stop first".to_owned(),
            ));
        }
        if let Some(mut finished) = live_guard.take() {
            finished.stop();
        }
        let (serial, handle) = start_live_adb_capture(&parsed, &self.capture)?;
        *live_guard = Some(handle);
        Ok(serde_json::to_value(CaptureStartResponse {
            serial,
            started: true,
            capture_mode: effective_mode,
        })?)
    }

    fn handle_save_trigger(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveTriggerParams = parse_optional_params(params)?;
        let outcome = self.execute_save(
            parsed.duration_sec,
            parsed.dest_dir,
            parsed.drain_timeout_ms,
            parsed.selection_policy,
            parsed.mode,
            SaveTriggerKind::Manual,
        )?;
        Ok(serde_json::to_value(outcome)?)
    }

    fn handle_save_logs(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveLogsParams = parse_optional_params(params)?;
        let outcome = self.execute_save(
            parsed.window_secs,
            parsed.dest_dir,
            parsed.drain_timeout_ms,
            parsed.selection_policy,
            SaveMode::LogsOnly,
            SaveTriggerKind::Manual,
        )?;
        Ok(serde_json::to_value(outcome)?)
    }

    fn execute_save(
        &self,
        duration_sec: Option<u64>,
        dest_dir: Option<PathBuf>,
        drain_timeout_ms: Option<u64>,
        selection_policy: Option<SelectionPolicy>,
        mode: SaveMode,
        trigger_kind: SaveTriggerKind,
    ) -> Result<SaveOutcomeResponse, DaemonError> {
        self.execute_save_tracked(
            duration_sec,
            dest_dir,
            drain_timeout_ms,
            selection_policy,
            mode,
            trigger_kind,
            &std::cell::Cell::new(false),
        )
    }

    /// Like [`Self::execute_save`], additionally setting `pinned` as soon as Phase 1 has pinned
    /// ring chunks, so callers can tell an error *before* any pin from one that left pins behind.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors execute_save plus the pin-tracking cell"
    )]
    fn execute_save_tracked(
        &self,
        duration_sec: Option<u64>,
        dest_dir: Option<PathBuf>,
        drain_timeout_ms: Option<u64>,
        selection_policy: Option<SelectionPolicy>,
        mode: SaveMode,
        trigger_kind: SaveTriggerKind,
        pinned: &std::cell::Cell<bool>,
    ) -> Result<SaveOutcomeResponse, DaemonError> {
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();
        let duration_sec = duration_sec.unwrap_or_else(|| u64::from(user_cfg.save_window_secs));
        let window_range = u64::from(MIN_SAVE_WINDOW_SECS)..=u64::from(MAX_SAVE_WINDOW_SECS);
        if !window_range.contains(&duration_sec) {
            return Err(DaemonError::InvalidParams(format!(
                "save window {duration_sec}s is outside {MIN_SAVE_WINDOW_SECS}..={MAX_SAVE_WINDOW_SECS} seconds"
            )));
        }
        let dest_dir = match dest_dir {
            Some(dest) => {
                self.validate_output_location(&dest, "dest_dir", None)?;
                dest
            }
            None => match user_cfg.output_dir_override {
                Some(override_dir) => {
                    self.validate_output_location(&override_dir, "output_dir_override", None)?;
                    override_dir
                }
                None => self.catalog.managed_bundles_root().to_path_buf(),
            },
        };
        let mut policy = selection_policy.unwrap_or_default();
        if let Some(drain_ms) = drain_timeout_ms {
            policy.drain_timeout_ms = drain_ms;
        }

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
            cap.phase1_pin_with_options(
                duration_sec,
                &dest_dir,
                policy,
                effective_mode,
                trigger_kind,
            )?
        };

        // Bounded boundary drain (`SPEC-02` §2) with `C` released between polls.
        let tail = match pending.drain_trigger_boottime_ns() {
            Some(trigger_bt) => self.wait_for_boundary_drain(&policy, trigger_bt)?,
            None => pending.tail_without_drain(),
        };

        // Phase 1 commit: extend pins to tail chunks, persist params, create the catalog job.
        let (job, plan) = self
            .lock_capture()?
            .phase1_commit(&self.catalog, pending, tail)?;
        // From here the catalog job owns the ring pins; earlier failures roll the pins back.
        pinned.set(true);
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

    fn handle_save_screenshot(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveScreenshotParams = parse_optional_params(params)?;
        if parsed.output_path.is_some() && parsed.output_dir.is_some() {
            return Err(DaemonError::InvalidParams(
                "pass either output_path or output_dir, not both".to_owned(),
            ));
        }
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();
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

        let dest_path = if let Some(explicit) = parsed.output_path {
            self.validate_output_location(&explicit, "output_path", None)?;
            self.reject_data_dir_state_file(&explicit, "output_path")?;
            explicit
        } else {
            let base_dir = if let Some(explicit_dir) = parsed.output_dir {
                self.validate_output_location(&explicit_dir, "output_dir", None)?;
                self.reject_data_dir_state_file(&explicit_dir.join("probe.png"), "output_dir")?;
                explicit_dir
            } else if let Some(screenshot_dir) = user_cfg.screenshot_dir_override {
                self.validate_output_location(&screenshot_dir, "screenshot_dir_override", None)?;
                self.reject_data_dir_state_file(
                    &screenshot_dir.join("probe.png"),
                    "screenshot_dir_override",
                )?;
                screenshot_dir
            } else if let Some(override_dir) = user_cfg.output_dir_override {
                self.validate_output_location(&override_dir, "output_dir_override", None)?;
                override_dir
            } else {
                self.paths.data_dir.join("screenshots")
            };
            base_dir.join(format!("{}.png", next_id("screenshot")))
        };

        let (serial, png_bytes) = self
            .screenshot_provider
            .capture_png(requested_serial.as_deref())?;
        let captured_at_unix_ms = current_unix_millis();
        let overwrite = if parsed.overwrite {
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
    ) -> Result<TailCompleteness, DaemonError> {
        let start = Instant::now();
        loop {
            let latest_hb = {
                let cap = self.lock_capture()?;
                if !cap.is_connected() {
                    return Ok(TailCompleteness::MayBeIncomplete);
                }
                cap.latest_heartbeat_boottime_ns()
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

    fn handle_bundle_list(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleListParams = parse_optional_params(params)?;
        let entries = self.catalog.list_bundles(parsed.state)?;
        let mut views = Vec::with_capacity(entries.len());
        for entry in entries {
            let active_leases = self
                .catalog
                .active_leases_for_bundle(&entry.bundle_id)?
                .len();
            views.push(BundleEntryView {
                bundle_id: entry.bundle_id,
                path: entry.path,
                format_version: entry.format_version,
                state: entry.state,
                trashed_at_utc: entry.trashed_at_utc,
                parent_bundle_id: entry.parent_bundle_id,
                created_at_utc: entry.created_at_utc,
                active_leases,
            });
        }
        Ok(serde_json::to_value(BundleListResponse { bundles: views })?)
    }

    fn handle_bundle_open(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleOpenParams = parse_params(params)?;
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
                candidate_id
            };
            let _ = self.catalog.register_bundle(&NewBundleEntry {
                bundle_id: reg_id.clone(),
                path: path.clone(),
                format_version,
                parent_bundle_id: None,
                created_at_utc: summary
                    .created_at_utc
                    .clone()
                    .unwrap_or_else(|| now_utc.clone()),
            })?;
            reg_id
        };

        let holder_kind = parsed.holder_kind.unwrap_or(LeaseHolderKind::Cli);
        // R3 S6: a lease must belong to a live client process; defaulting to the always-alive
        // daemon PID made a crashed client's lease unreapable.
        let holder_pid = parsed.holder_pid.ok_or_else(|| {
            DaemonError::InvalidParams(
                "holder_pid is required: pass the client's own process ID so its lease is reaped if it crashes".to_owned(),
            )
        })?;
        if !is_pid_alive(holder_pid) {
            return Err(DaemonError::InvalidParams(format!(
                "holder_pid {holder_pid} is not a running process"
            )));
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
        let active_leases = self
            .catalog
            .active_leases_for_bundle(&entry.bundle_id)?
            .len();
        Ok(serde_json::to_value(BundleEntryView {
            bundle_id: entry.bundle_id,
            path: entry.path,
            format_version: entry.format_version,
            state: entry.state,
            trashed_at_utc: entry.trashed_at_utc,
            parent_bundle_id: entry.parent_bundle_id,
            created_at_utc: entry.created_at_utc,
            active_leases,
        })?)
    }

    fn handle_bundle_restore(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleIdParams = parse_params(params)?;
        let entry = self.catalog.restore_bundle(&parsed.bundle_id)?;
        let active_leases = self
            .catalog
            .active_leases_for_bundle(&entry.bundle_id)?
            .len();
        Ok(serde_json::to_value(BundleEntryView {
            bundle_id: entry.bundle_id,
            path: entry.path,
            format_version: entry.format_version,
            state: entry.state,
            trashed_at_utc: entry.trashed_at_utc,
            parent_bundle_id: entry.parent_bundle_id,
            created_at_utc: entry.created_at_utc,
            active_leases,
        })?)
    }

    fn handle_bundle_gc(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleGcParams = parse_optional_params(params)?;
        let now_utc = parsed.now_utc.unwrap_or_else(utc_now_iso8601);
        let report = self.catalog.gc_trashed_bundles(&now_utc)?;
        Ok(serde_json::to_value(BundleGcResponse {
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
        let overview =
            inspect_export::build_timeline_overview(&path, catalog_id, parsed.max_error_logs)?;
        Ok(serde_json::to_value(overview)?)
    }

    fn handle_timeline_query_logs(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: TimelineQueryParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let bucket = match parsed.bucket.unwrap_or(QueryBucket::All) {
            QueryBucket::Main => Some(RecordBucket::Main),
            QueryBucket::Unanchored => Some(RecordBucket::Unanchored),
            QueryBucket::All => None,
        };
        let logs = inspect_export::query_logs_filtered(
            reader.as_ref(),
            bucket,
            &parsed,
            effective_query_limit(parsed.limit),
        )?;
        Ok(serde_json::to_value(TimelineLogsResponse { logs })?)
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
        let screens = reader.screens()?;
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
                out_frame_index: s.out_frame_index,
                playhead_frame_index: s.playhead_frame_index,
                av_offset_override_ms: s.av_offset_override_ms,
                updated_at_utc: s.updated_at_utc,
            });
        Ok(json!({ "session": session }))
    }

    fn handle_edit_update_session(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: EditUpdateSessionParams = parse_params(params)?;
        let now_utc = utc_now_iso8601();
        let session = EditSession {
            session_id: parsed.session_id,
            bundle_id: parsed.bundle_id,
            in_frame_index: parsed.in_frame_index,
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
                out_frame_index: session.out_frame_index,
                playhead_frame_index: session.playhead_frame_index,
                av_offset_override_ms: session.av_offset_override_ms,
                updated_at_utc: session.updated_at_utc,
            }
        }))
    }

    fn handle_export_trim(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: ExportTrimParams = parse_params(params)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();
        let dest_parent = match &parsed.dest_dir {
            Some(dest) => {
                self.validate_output_location(dest, "dest_dir", Some(&path))?;
                dest.clone()
            }
            None => match user_cfg.output_dir_override {
                Some(override_dir) => {
                    self.validate_output_location(
                        &override_dir,
                        "output_dir_override",
                        Some(&path),
                    )?;
                    override_dir
                }
                None => self.catalog.managed_bundles_root().to_path_buf(),
            },
        };
        let source_id = existing.as_ref().map(|e| e.bundle_id.as_str());
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
        self.validate_output_location(&parsed.output_path, "output_path", Some(&path))?;
        self.reject_data_dir_state_file(&parsed.output_path, "output_path")?;
        let source_id = existing.as_ref().map(|e| e.bundle_id.as_str());
        let backend = default_codec_backend();
        let ctx = ExportContext {
            catalog: &self.catalog,
            data_dir: &self.paths.data_dir,
            backend: backend.as_ref(),
            faults: &self.faults,
        };
        let resp = media_engine::export_png(&ctx, &path, source_id, &parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_export_share_mp4(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: ExportShareMp4Params = parse_params(params)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id)?;
        let user_cfg = self.lock(&self.user_config, "user config")?.clone();

        let dest_parent = match &parsed.dest_dir {
            Some(dest) => {
                self.validate_output_location(dest, "dest_dir", Some(&path))?;
                dest.clone()
            }
            None => match user_cfg.output_dir_override.clone() {
                Some(override_dir) => {
                    self.validate_output_location(
                        &override_dir,
                        "output_dir_override",
                        Some(&path),
                    )?;
                    override_dir
                }
                None => self.catalog.managed_bundles_root().to_path_buf(),
            },
        };

        let share_mp4_path = if let Some(explicit) = &parsed.output_path {
            self.validate_output_location(explicit, "output_path", Some(&path))?;
            self.reject_data_dir_state_file(explicit, "output_path")?;
            explicit.clone()
        } else {
            let base_dir = if let Some(override_dir) = user_cfg.output_dir_override {
                self.validate_output_location(&override_dir, "output_dir_override", Some(&path))?;
                override_dir
            } else {
                self.paths.data_dir.join("exports")
            };
            let stem = next_id("share");
            let file_name = if parsed.burn_taps {
                format!("{stem}-taps.mp4")
            } else {
                format!("{stem}.mp4")
            };
            base_dir.join(file_name)
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
        let mut media = self.lock_media()?;
        if self.is_shutting_down() {
            return Err(DaemonError::Conflict("arbd is shutting down".to_owned()));
        }
        let resp = media.open_viewport(&path, id_override)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_filmstrip(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFilmstripParams = parse_params(params)?;
        let resp = self.lock_media()?.filmstrip(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_scrub(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFrameRequestParams = parse_params(params)?;
        let resp = self.lock_media()?.scrub(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_seek_frame(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFrameRequestParams = parse_params(params)?;
        let resp = self.lock_media()?.seek_frame(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_close_viewport(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaCloseViewportParams = parse_params(params)?;
        let closed = self.lock_media()?.close_viewport(&parsed.viewport_id);
        Ok(json!({ "closed": closed, "viewport_id": parsed.viewport_id }))
    }

    /// Closes viewports a client opened but never closed, once its connection ends (R3 M7).
    fn close_client_viewports(&self, viewport_ids: &[String]) {
        if viewport_ids.is_empty() {
            return;
        }
        let mut media = self.media.lock().unwrap_or_else(PoisonError::into_inner);
        for id in viewport_ids {
            let _ = media.close_viewport(id);
        }
    }

    /// Runs one request, converting a panic into a `-32603` error and a graceful shutdown
    /// instead of a silently dropped connection and a zombie daemon (R3 S15).
    fn dispatch_guarded(&self, method: &str, params: Value) -> Result<Value, (i32, String)> {
        match catch_unwind(AssertUnwindSafe(|| self.dispatch(method, params))) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err((err.rpc_code(), err.to_string())),
            Err(_) => {
                self.trigger_shutdown();
                Err((
                    RPC_ERR_INTERNAL,
                    format!("internal error: `{method}` panicked; arbd is shutting down"),
                ))
            }
        }
    }
}

/// Running local `arbd` daemon server holding single-instance lock and IPC accept loop.
#[derive(Debug)]
pub struct DaemonServer {
    state: Arc<DaemonState>,
    _lock: SingleInstanceLock,
    accept_thread: Option<JoinHandle<()>>,
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
        let (recovery, pin_reconcile) =
            catalog.recover_on_startup_with_pins(&utc_now_iso8601(), &pin_journal)?;

        let (crash_tx, crash_rx) = mpsc::channel();
        let mut capture = CapturePipeline::open(
            &config.paths.data_dir,
            effective_ring_policy,
            config.disk_provider,
        )?;
        capture.set_fault_injection(Arc::clone(&config.faults));
        capture.set_crash_notifier(Some(crash_tx));
        let media = MediaEngine::new(config.paths.cache_dir.clone(), config.paths.shm_dir());

        let endpoint = config.paths.ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint)?;
        let shutdown_flag = Arc::new(AtomicBool::new(false));
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
            live_capture: Mutex::new(None),
            media: Mutex::new(media),
            screenshot_provider: config.screenshot_provider,
            crash_autosave_status: Mutex::new(initial_crash_status),
            crash_autosave_gate: Mutex::new(CrashAutoSaveGate::new()),
            faults: config.faults,
            shutdown_flag: Arc::clone(&shutdown_flag),
            endpoint,
            build_id,
            binary_mtime_secs,
        });

        let accept_state = Arc::clone(&state);
        let accept_thread = thread::Builder::new()
            .name("arbd-ipc-accept".to_owned())
            .spawn(move || {
                run_accept_loop(&listener, &accept_state);
            })?;

        let crash_state = Arc::clone(&state);
        let crash_worker_thread = thread::Builder::new()
            .name("arbd-crash-autosave".to_owned())
            .spawn(move || {
                run_crash_autosave_worker(&crash_state, &crash_rx);
            })?;

        Ok(Self {
            state,
            _lock: lock,
            accept_thread: Some(accept_thread),
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
        let handle = self
            .state
            .live_capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
            cap.set_crash_notifier(None);
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
        self.state
            .media
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .close_all();
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

fn run_crash_autosave_worker(state: &Arc<DaemonState>, rx: &Receiver<CrashAutoSaveNotice>) {
    while let Ok(notice) = rx.recv() {
        if state.is_shutting_down() {
            break;
        }
        // A panic in one auto-save must not silently kill the worker for the daemon's life.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_crash_autosave_notice(state, notice);
        }));
        if outcome.is_err() {
            let mut status = state
                .crash_autosave_status
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            status.in_flight = false;
            status.last_error = Some("crash auto-save worker panicked; continuing".to_owned());
        }
    }
}

fn handle_crash_autosave_notice(state: &Arc<DaemonState>, notice: CrashAutoSaveNotice) {
    let cfg = state
        .user_config
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let save_target = cfg
        .output_dir_override
        .clone()
        .unwrap_or_else(|| state.catalog.managed_bundles_root().to_path_buf());
    let (pipeline_pressure, floor_bytes) = {
        let cap = state.capture.lock().unwrap_or_else(PoisonError::into_inner);
        (cap.is_storage_pressure_stopped(), cap.min_free_disk_bytes())
    };
    let storage_pressure_active = notice.storage_pressure_stopped
        || pipeline_pressure
        || state.host_free_below_floor(&save_target, floor_bytes);

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
        status.detected_crashes = status.detected_crashes.saturating_add(1);
        status.last_crash_process = Some(notice.process_name);
        status.last_crash_exception = Some(notice.exception_class);
        match decision {
            CrashAutoSaveDecision::Trigger => {
                status.triggered_saves = status.triggered_saves.saturating_add(1);
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

    if state.is_shutting_down() {
        let mut status = state
            .crash_autosave_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        status.in_flight = false;
        return;
    }
    let pinned = std::cell::Cell::new(false);
    let save_res = state.execute_save_tracked(
        Some(u64::from(cfg.save_window_secs)),
        None,
        None,
        None,
        SaveMode::Full,
        SaveTriggerKind::CrashAutoSave,
        &pinned,
    );

    let mut status = state
        .crash_autosave_status
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    status.in_flight = false;
    // Roll the debounce back only when the save failed before pinning anything. Any failure
    // after Phase 1 (an `Err` from later phases, or a job that ended `FailedRetryable`) keeps its ring pins, so its cooldown must stay or a failing
    // destination plus a crash loop would pin a fresh window on every crash.
    let pre_pin_failure = save_res.is_err() && !pinned.get();
    match save_res {
        Ok(outcome) => {
            status.last_job_id = Some(outcome.job.job_id);
            status.last_bundle_id = outcome.bundle_id;
            status.last_error = outcome.job.last_error;
        }
        Err(err) => {
            status.last_error = Some(err.to_string());
        }
    }
    drop(status);
    if pre_pin_failure {
        // Nothing was pinned or saved: do not let this failed attempt debounce the next crash.
        state
            .crash_autosave_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .abort_last_trigger();
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
                    let _ = write_frame_line(&mut writer, &text, DEFAULT_MAX_IPC_LINE_BYTES);
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
                Err((code, message)) => RpcResponse::err(request.id, code, message),
            }
        } else {
            RpcResponse::err(
                request.id,
                RPC_ERR_INVALID_REQUEST,
                format!("unsupported jsonrpc version `{}`", request.jsonrpc),
            )
        };

        if let Some(text) = encode_response(&response) {
            if write_frame_line(&mut writer, &text, DEFAULT_MAX_IPC_LINE_BYTES).is_err() {
                break;
            }
            let _ = writer.drain();
        }

        if is_shutdown {
            state.trigger_shutdown();
            break;
        }
    }

    state.close_client_viewports(&owned_viewports);
}

/// Applies the default and maximum `limit` of `timeline.query_*` (R3 S10).
fn effective_query_limit(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_QUERY_LIMIT)
        .min(MAX_QUERY_LIMIT)
}

fn parse_params<T: DeserializeOwned>(value: Value) -> Result<T, DaemonError> {
    serde_json::from_value(value).map_err(|err| DaemonError::InvalidParams(err.to_string()))
}

fn parse_optional_params<T: DeserializeOwned + Default>(value: Value) -> Result<T, DaemonError> {
    if value.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(value).map_err(|err| DaemonError::InvalidParams(err.to_string()))
}

impl std::fmt::Debug for DaemonState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonState")
            .field("endpoint", &self.endpoint)
            .field("data_dir", &self.paths.data_dir)
            .finish_non_exhaustive()
    }
}
