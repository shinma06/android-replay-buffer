//! Single-writer local daemon (`arbd`) for Android Replay Buffer (ARB) v2.
//!
//! Coordinates:
//! - [`arb_os::fs::SingleInstanceLock`] and [`arb_os::ipc::LocalIpcListener`]
//! - [`arb_storage::catalog::Catalog`] startup recovery, leases, edit sessions, and GC
//! - [`capture::CapturePipeline`] Zone-1 ring buffer & two-phase save engine
//! - [`live_adb`] live Android device-agent deployment & reconnect supervision
//! - [`media_engine::MediaEngine`] SHM triple-buffered viewport & frame-accurate trim/PNG export

pub mod capture;
pub mod error;
pub mod live_adb;
pub mod media_engine;
pub mod protocol;
pub mod time_util;

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arb_adb::adb::AdbClient;
use arb_adb::mock::DeviceEventSource;
use arb_core::BUNDLE_SCHEMA_VERSION;
use arb_core::bundle::RecordBucket;
use arb_core::selection::{DrainOutcome, SelectionPolicy, evaluate_stream_drain};
use arb_core::state::RingBudgetPolicy;
use arb_os::fs::SingleInstanceLock;
use arb_os::ipc::{
    DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, LocalIpcStream, read_frame_line, write_frame_line,
};
use arb_os::paths::ArbPaths;
use arb_storage::bundle_io::{TimeRange, open_bundle};
use arb_storage::catalog::{
    BundleEntry, Catalog, CatalogFormatVersion, EditSession, LeaseHolderKind, NewBundleEntry,
    RecoveryReport,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::capture::{
    CapturePipeline, SharedDiskSpaceProvider, TailCompleteness, save_job_to_view,
};
use crate::error::{DaemonError, RPC_ERR_INVALID_REQUEST, RPC_ERR_PARSE};
use crate::live_adb::{LiveCaptureHandle, start_live_adb_capture};
use crate::media_engine::{MediaEngine, build_timeline_summary};
use crate::protocol::{
    BundleEntryView, BundleGcParams, BundleGcResponse, BundleIdParams, BundleListParams,
    BundleListResponse, BundleOpenParams, BundleOpenResponse, BundleRefParams, CaptureStartParams,
    CaptureStartResponse, CaptureStopResponse, DeviceListResponse, EditGetSessionParams,
    EditSessionView, EditUpdateSessionParams, ExportPngParams, ExportTrimParams,
    IPC_PROTOCOL_VERSION, JSONRPC_VERSION, LeaseIdParams, MediaCloseViewportParams,
    MediaFilmstripParams, MediaFrameRequestParams, MediaOpenViewportParams, QueryBucket,
    RecoverySummaryView, RpcRequest, RpcResponse, SaveJobIdParams, SaveListJobsResponse,
    SaveRetargetParams, SaveTriggerParams, SystemHelloResponse, SystemStatusResponse,
    TimelineDiscontinuitiesResponse, TimelineLogsResponse, TimelineQueryParams,
    TimelineTapsResponse,
};
use crate::time_util::utc_now_iso8601;

const DEFAULT_SAVE_DURATION_SEC: u64 = 30;
const DRAIN_POLL_INTERVAL_MS: u64 = 20;

/// Startup configuration for [`DaemonServer`].
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Resolved data, cache, and runtime directories.
    pub paths: ArbPaths,
    /// Zone-1 ring buffer byte, duration, and free-disk safety floor policy.
    pub ring_policy: RingBudgetPolicy,
    /// Injectable filesystem disk-space provider (defaults to host OS `statvfs` / Win32).
    pub disk_provider: SharedDiskSpaceProvider,
}

impl DaemonConfig {
    /// Creates a daemon configuration using `paths` and default SPEC-02 ring policies.
    #[must_use]
    pub fn new(paths: ArbPaths) -> Self {
        Self {
            paths,
            ring_policy: RingBudgetPolicy::default(),
            disk_provider: SharedDiskSpaceProvider::os_default(),
        }
    }

    /// Overrides the Zone-1 [`RingBudgetPolicy`].
    #[must_use]
    pub const fn with_ring_policy(mut self, policy: RingBudgetPolicy) -> Self {
        self.ring_policy = policy;
        self
    }

    /// Overrides the [`SharedDiskSpaceProvider`] (for deterministic storage-pressure tests).
    #[must_use]
    pub fn with_disk_provider(mut self, provider: SharedDiskSpaceProvider) -> Self {
        self.disk_provider = provider;
        self
    }
}

struct DaemonState {
    paths: ArbPaths,
    started_at: Instant,
    recovery: RecoveryReport,
    catalog: Catalog,
    capture: Arc<Mutex<CapturePipeline>>,
    live_capture: Mutex<Option<LiveCaptureHandle>>,
    media: Mutex<MediaEngine>,
    shutdown_flag: Arc<AtomicBool>,
    endpoint: PathBuf,
}

impl DaemonState {
    fn trigger_shutdown(&self) {
        if !self.shutdown_flag.swap(true, Ordering::AcqRel) {
            // Wake the blocking `LocalIpcListener::accept()` loop cleanly on all OSes.
            let _ = LocalIpcStream::connect(&self.endpoint);
        }
    }

    fn stop_active_capture(&self) -> CaptureStopResponse {
        let mut live_guard = self
            .live_capture
            .lock()
            .expect("live capture lock poisoned");
        let was_running = live_guard.is_some();
        if let Some(mut handle) = live_guard.take() {
            handle.stop();
        }
        let mut cap = self.capture.lock().expect("capture pipeline lock poisoned");
        let was_active = was_running || cap.is_active();
        cap.mark_stopped();
        CaptureStopResponse {
            stopped: was_active,
            capture: cap.status_view(),
        }
    }

    fn resolve_bundle_path(
        &self,
        bundle_id_or_path: &str,
    ) -> Result<(PathBuf, Option<BundleEntry>), DaemonError> {
        if let Some(entry) = self.catalog.bundle(bundle_id_or_path)? {
            return Ok((entry.path.clone(), Some(entry)));
        }
        let path = PathBuf::from(bundle_id_or_path);
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
        match method {
            "system.hello" => Ok(serde_json::to_value(SystemHelloResponse {
                protocol_version: IPC_PROTOCOL_VERSION,
                schema_version: BUNDLE_SCHEMA_VERSION,
                pid: std::process::id(),
                data_dir: self.paths.data_dir.clone(),
                cache_dir: self.paths.cache_dir.clone(),
                runtime_dir: self.paths.runtime_dir.clone(),
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            })?),
            "system.status" => self.handle_system_status(),
            "system.shutdown" => Ok(json!({ "ok": true })),
            "device.list" => Self::handle_device_list(),
            "capture.start" => self.handle_capture_start(params),
            "capture.stop" => Ok(serde_json::to_value(self.stop_active_capture())?),
            "save.trigger" => self.handle_save_trigger(params),
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
            "timeline.query_logs" => self.handle_timeline_query_logs(params),
            "timeline.query_taps" => self.handle_timeline_query_taps(params),
            "timeline.discontinuities" => self.handle_timeline_discontinuities(params),
            "edit.get_session" => self.handle_edit_get_session(params),
            "edit.update_session" => self.handle_edit_update_session(params),
            "export.trim" => self.handle_export_trim(params),
            "export.png" => self.handle_export_png(params),
            "media.open_viewport" => self.handle_media_open_viewport(params),
            "media.filmstrip" => self.handle_media_filmstrip(params),
            "media.scrub" => self.handle_media_scrub(params),
            "media.seek_frame" => self.handle_media_seek_frame(params),
            "media.close_viewport" => self.handle_media_close_viewport(params),
            other => Err(DaemonError::MethodNotFound(other.to_owned())),
        }
    }

    fn handle_system_status(&self) -> Result<Value, DaemonError> {
        let (capture_view, ring_chunks, ring_bytes, ring_pinned) = {
            let cap = self.capture.lock().expect("capture pipeline lock poisoned");
            let (chunks, bytes, pinned) = cap.ring_stats()?;
            (cap.status_view(), chunks, bytes, pinned)
        };
        let active_viewports = self
            .media
            .lock()
            .expect("media engine lock poisoned")
            .active_viewport_count();
        let active_save_jobs = self
            .catalog
            .list_save_jobs()?
            .into_iter()
            .filter(|j| !j.job.state.is_terminal())
            .count();
        let uptime_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(serde_json::to_value(SystemStatusResponse {
            pid: std::process::id(),
            uptime_ms,
            capture: capture_view,
            ring_chunk_count: ring_chunks,
            ring_total_bytes: ring_bytes,
            ring_pinned_chunk_count: ring_pinned,
            active_viewports,
            active_save_jobs,
            recovery: RecoverySummaryView {
                removed_staging_dirs: self.recovery.removed_staging_dirs.len(),
                reaped_leases: self.recovery.reaped_lease_ids.len(),
                recovered_jobs: self.recovery.recovered_job_ids.len(),
                completed_purging_bundles: self.recovery.completed_purging_bundle_ids.len(),
            },
        })?)
    }

    fn handle_device_list() -> Result<Value, DaemonError> {
        let adb = AdbClient::discover()?;
        let devices = adb.devices()?;
        Ok(serde_json::to_value(DeviceListResponse { devices })?)
    }

    fn handle_capture_start(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: CaptureStartParams = parse_optional_params(params)?;
        let mut live_guard = self
            .live_capture
            .lock()
            .expect("live capture lock poisoned");
        if live_guard.is_some() {
            return Err(DaemonError::Conflict(
                "capture session is already active; call capture.stop first".to_owned(),
            ));
        }
        let (serial, handle) = start_live_adb_capture(&parsed, &self.capture)?;
        *live_guard = Some(handle);
        Ok(serde_json::to_value(CaptureStartResponse {
            serial,
            started: true,
        })?)
    }

    fn handle_save_trigger(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveTriggerParams = parse_optional_params(params)?;
        let duration_sec = parsed.duration_sec.unwrap_or(DEFAULT_SAVE_DURATION_SEC);
        let dest_dir = parsed
            .dest_dir
            .unwrap_or_else(|| self.catalog.managed_bundles_root().to_path_buf());
        let mut policy = parsed.selection_policy.unwrap_or_default();
        if let Some(drain_ms) = parsed.drain_timeout_ms {
            policy.drain_timeout_ms = drain_ms;
        }

        // Bounded boundary drain grace window (`SPEC-02` §2):
        // When live capture is actively connected, wait up to `drain_timeout_ms` for a
        // post-trigger heartbeat so tail frames are cleanly bracketed.
        let tail_completeness = self.wait_for_boundary_drain(&policy);

        // Phase 1: Durable chunk pin + catalog job row.
        let pinned_job = {
            let mut cap = self.capture.lock().expect("capture pipeline lock poisoned");
            cap.phase1_pin_save(
                &self.catalog,
                duration_sec,
                &dest_dir,
                policy,
                tail_completeness,
            )?
        };

        // Phase 2: Materialize from pinned chunks only (releases lock during muxing if needed,
        // here ring reads are read-only once pinned).
        let cap = self.capture.lock().expect("capture pipeline lock poisoned");
        let outcome = cap.phase2_materialize(&self.catalog, &pinned_job.job.job_id)?;
        Ok(serde_json::to_value(outcome)?)
    }

    fn wait_for_boundary_drain(&self, policy: &SelectionPolicy) -> TailCompleteness {
        let (should_wait, trigger_boottime_ns) = {
            let cap = self.capture.lock().expect("capture pipeline lock poisoned");
            let connected = cap.is_active() && cap.is_connected();
            let trigger_bt = cap
                .latest_heartbeat_boottime_ns()
                .unwrap_or(0)
                .saturating_add(1);
            (connected, trigger_bt)
        };
        if !should_wait {
            return TailCompleteness::Complete;
        }

        let start = Instant::now();
        loop {
            let latest_hb = {
                let cap = self.capture.lock().expect("capture pipeline lock poisoned");
                if !cap.is_connected() {
                    return TailCompleteness::MayBeIncomplete;
                }
                cap.latest_heartbeat_boottime_ns()
            };
            let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            match evaluate_stream_drain(trigger_boottime_ns, latest_hb, elapsed_ms, policy) {
                DrainOutcome::DrainedComplete => return TailCompleteness::Complete,
                DrainOutcome::TimedOutTailIncomplete => return TailCompleteness::MayBeIncomplete,
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
        let cap = self.capture.lock().expect("capture pipeline lock poisoned");
        let outcome = cap.retry_save_job(&self.catalog, &parsed.job_id)?;
        Ok(serde_json::to_value(outcome)?)
    }

    fn handle_save_retarget(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveRetargetParams = parse_params(params)?;
        let cap = self.capture.lock().expect("capture pipeline lock poisoned");
        let outcome = cap.retarget_save_job(&self.catalog, &parsed.job_id, &parsed.dest_dir)?;
        Ok(serde_json::to_value(outcome)?)
    }

    fn handle_save_discard(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: SaveJobIdParams = parse_params(params)?;
        let cap = self.capture.lock().expect("capture pipeline lock poisoned");
        let view = cap.discard_save_job(&self.catalog, &parsed.job_id)?;
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
        let holder_pid = parsed.holder_pid.unwrap_or_else(std::process::id);
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
        })?)
    }

    fn handle_timeline_summary(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: BundleRefParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let reader = open_bundle(&path)?;
        let summary = build_timeline_summary(reader.as_ref(), &path)?;
        Ok(serde_json::to_value(summary)?)
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
        let range = TimeRange {
            start_ns: parsed.start_ns,
            end_ns: parsed.end_ns,
        };
        let logs = reader.query_logs(bucket, range, parsed.limit)?;
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
        let taps = reader.query_taps(bucket, range, parsed.limit)?;
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
        let source_id = existing.as_ref().map(|e| e.bundle_id.as_str());
        let media = self.media.lock().expect("media engine lock poisoned");
        let resp = media.export_trim(
            &self.catalog,
            &self.paths.data_dir,
            &path,
            source_id,
            &parsed,
        )?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_export_png(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: ExportPngParams = parse_params(params)?;
        let (path, _) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let media = self.media.lock().expect("media engine lock poisoned");
        let resp = media.export_png(&path, &parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_open_viewport(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaOpenViewportParams = parse_params(params)?;
        let (path, existing) = self.resolve_bundle_path(&parsed.bundle_id_or_path)?;
        let id_override = existing.as_ref().map(|e| e.bundle_id.as_str());
        let mut media = self.media.lock().expect("media engine lock poisoned");
        let resp = media.open_viewport(&path, id_override)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_filmstrip(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFilmstripParams = parse_params(params)?;
        let mut media = self.media.lock().expect("media engine lock poisoned");
        let resp = media.filmstrip(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_scrub(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFrameRequestParams = parse_params(params)?;
        let mut media = self.media.lock().expect("media engine lock poisoned");
        let resp = media.scrub(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_seek_frame(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaFrameRequestParams = parse_params(params)?;
        let mut media = self.media.lock().expect("media engine lock poisoned");
        let resp = media.seek_frame(&parsed)?;
        Ok(serde_json::to_value(resp)?)
    }

    fn handle_media_close_viewport(&self, params: Value) -> Result<Value, DaemonError> {
        let parsed: MediaCloseViewportParams = parse_params(params)?;
        let mut media = self.media.lock().expect("media engine lock poisoned");
        let closed = media.close_viewport(&parsed.viewport_id);
        Ok(json!({ "closed": closed, "viewport_id": parsed.viewport_id }))
    }
}

/// Running local `arbd` daemon server holding single-instance lock and IPC accept loop.
#[derive(Debug)]
pub struct DaemonServer {
    state: Arc<DaemonState>,
    _lock: SingleInstanceLock,
    accept_thread: Option<JoinHandle<()>>,
}

impl DaemonServer {
    /// Starts `arbd` with the given [`DaemonConfig`]:
    /// 1. Acquires [`SingleInstanceLock`] at `<runtime_dir>/arbd.lock`.
    /// 2. Opens [`Catalog`] and runs [`Catalog::recover_on_startup`].
    /// 3. Opens Zone-1 [`CapturePipeline`] (`RingStore`) and [`MediaEngine`].
    /// 4. Binds [`LocalIpcListener`] and spawns the concurrent accept loop.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if locking, storage recovery, or IPC binding fails.
    pub fn start(config: DaemonConfig) -> Result<Self, DaemonError> {
        std::fs::create_dir_all(&config.paths.data_dir)?;
        std::fs::create_dir_all(&config.paths.cache_dir)?;
        std::fs::create_dir_all(&config.paths.runtime_dir)?;
        std::fs::create_dir_all(config.paths.shm_dir())?;

        let lock = SingleInstanceLock::try_acquire(config.paths.lock_file())?;
        let catalog_db = config.paths.data_dir.join("catalog.sqlite");
        let managed_bundles = config.paths.data_dir.join("bundles");
        let catalog = Catalog::open(&catalog_db, &managed_bundles)?;
        let recovery = catalog.recover_on_startup(&utc_now_iso8601())?;

        let capture = CapturePipeline::open(
            &config.paths.data_dir,
            config.ring_policy,
            config.disk_provider,
        )?;
        let media = MediaEngine::new(config.paths.cache_dir.clone(), config.paths.shm_dir());

        let endpoint = config.paths.ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint)?;
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        let state = Arc::new(DaemonState {
            paths: config.paths,
            started_at: Instant::now(),
            recovery,
            catalog,
            capture: Arc::new(Mutex::new(capture)),
            live_capture: Mutex::new(None),
            media: Mutex::new(media),
            shutdown_flag: Arc::clone(&shutdown_flag),
            endpoint,
        });

        let accept_state = Arc::clone(&state);
        let accept_thread = thread::Builder::new()
            .name("arbd-ipc-accept".to_owned())
            .spawn(move || {
                run_accept_loop(&listener, &accept_state);
            })?;

        Ok(Self {
            state,
            _lock: lock,
            accept_thread: Some(accept_thread),
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

    /// Drains all events from any [`DeviceEventSource`] (such as a deterministic
    /// [`arb_adb::mock::MockDeviceSource`]) directly into the live capture pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if reading from `source` or persisting ring chunks fails.
    ///
    /// # Panics
    ///
    /// Panics if the internal capture pipeline mutex is poisoned.
    pub fn ingest_device_source(
        &self,
        source: &mut dyn DeviceEventSource,
    ) -> Result<usize, DaemonError> {
        let mut cap = self
            .state
            .capture
            .lock()
            .expect("capture pipeline lock poisoned");
        cap.begin_session(source.serial().to_owned());
        let mut count = 0_usize;
        while let Some(event) = source.next_event()? {
            cap.ingest_event(event)?;
            count += 1;
        }
        cap.flush_pending_chunk()?;
        Ok(count)
    }

    /// Flushes any pending live events into Zone-1 ring chunks.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] on unexpected I/O failure.
    ///
    /// # Panics
    ///
    /// Panics if the internal capture pipeline mutex is poisoned.
    pub fn flush_capture_ring(&self) -> Result<(), DaemonError> {
        let mut cap = self
            .state
            .capture
            .lock()
            .expect("capture pipeline lock poisoned");
        cap.flush_pending_chunk()
    }

    /// Returns a snapshot of live capture discontinuities recorded so far.
    ///
    /// # Panics
    ///
    /// Panics if the internal capture pipeline mutex is poisoned.
    #[must_use]
    pub fn live_discontinuities(&self) -> Vec<arb_core::time::DiscontinuityEvent> {
        self.state
            .capture
            .lock()
            .expect("capture pipeline lock poisoned")
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

    fn cleanup_subsystems(&self) {
        let _ = self.state.stop_active_capture();
        self.state
            .media
            .lock()
            .expect("media engine lock poisoned")
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

fn run_accept_loop(listener: &LocalIpcListener, state: &Arc<DaemonState>) {
    let mut client_threads: Vec<JoinHandle<()>> = Vec::new();
    loop {
        if state.shutdown_flag.load(Ordering::Acquire) {
            break;
        }
        let Ok(stream) = listener.accept() else {
            if state.shutdown_flag.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        if state.shutdown_flag.load(Ordering::Acquire) {
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

    // Reap already-finished client threads cleanly; do not deadlock shutdown on
    // idle long-lived client sockets that remain open until process teardown.
    for handle in client_threads {
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
}

fn handle_client_connection(stream: LocalIpcStream, state: &Arc<DaemonState>) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);

    loop {
        let line = match read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES) {
            Ok(Some(l)) => l,
            Ok(None) => break,
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
        let response = if request.jsonrpc == JSONRPC_VERSION {
            match state.dispatch(&request.method, request.params) {
                Ok(result) => RpcResponse::ok(request.id, result),
                Err(err) => RpcResponse::err(request.id, err.rpc_code(), err.to_string()),
            }
        } else {
            RpcResponse::err(
                request.id,
                RPC_ERR_INVALID_REQUEST,
                format!("unsupported jsonrpc version `{}`", request.jsonrpc),
            )
        };

        if let Ok(text) = serde_json::to_string(&response) {
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
