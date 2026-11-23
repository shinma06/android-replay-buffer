//! Live capture ingestion pipeline, Zone-1 ring chunk persistence, and two-phase save engine.
//!
//! Bridges [`DeviceEventSource`] (both live ADB sessions and deterministic
//! [`arb_adb::mock::MockDeviceSource`]) into [`ClockTimeline`], [`RingStore`],
//! [`arb_core::selection::select_records`], [`arb_media::mp4::mux_raw_h264_to_mp4`],
//! and [`V2BundleWriter`].

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arb_adb::mock::{CaptureEvent, NormalizedTouchEvent, TouchAction};
use arb_adb::scrcpy::VideoPacketKind;
use arb_adb::wire::{StreamId, StreamLifecycleState, TouchEventPayload};
use arb_core::bundle::{
    AudioStreamMeta, AvOffsetStatus, DeviceInfo, EpochSpan, FrameRecord, InputStreamMeta,
    LogCompleteness, LogRecord, LogcatStreamMeta, Provenance, RecordBucket, RequestedWindow,
    StreamStatus, StreamsSummary, TapRecord, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::selection::{SelectionPolicy, WindowSpec, select_records};
use arb_core::state::{RingBudgetPolicy, SaveJobState};
use arb_core::time::{
    ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind,
    StreamKind, TimeAlignment, TimestampSemantics,
};
use arb_media::mp4::{Mp4VideoTrack, RawH264Packet, mux_raw_h264_to_mp4};
use arb_os::OsError;
use arb_os::fs::DiskSpaceInfo;
use arb_storage::StorageError;
use arb_storage::bundle_io::{ClockAnchorRecord, MediaPayload, V2BundleDraft, V2BundleWriter};
use arb_storage::catalog::{Catalog, CatalogFormatVersion, CatalogSaveJob, NewBundleEntry};
use arb_storage::ring::{DiskSpaceProvider, OsDiskSpaceProvider, RingChunkKind, RingStore};
use serde::{Deserialize, Serialize};

use crate::error::DaemonError;
use crate::protocol::{CaptureStatusView, SaveJobView, SaveOutcomeResponse};
use crate::time_util::{next_id, utc_now_iso8601};

const NANOS_PER_SEC: i64 = 1_000_000_000;
const MP4_TIMESCALE: u32 = 90_000;
const MP4_DEFAULT_FRAME_DURATION_TICKS: u32 = 3_000; // 30 fps
const MAX_BATCH_EVENTS: usize = 32;
const MAX_BATCH_BYTES: usize = 128 * 1024;
const EV_SYN: u16 = 0x0000;
const EV_KEY: u16 = 0x0001;
const EV_ABS: u16 = 0x0003;
const SYN_REPORT: u16 = 0x0000;
const BTN_TOUCH: u16 = 0x014a;
const ABS_X: u16 = 0x0000;
const ABS_Y: u16 = 0x0001;
const ABS_MT_SLOT: u16 = 0x002f;
const ABS_MT_POSITION_X: u16 = 0x0035;
const ABS_MT_POSITION_Y: u16 = 0x0036;
const ABS_MT_TRACKING_ID: u16 = 0x0039;

/// Wrapper allowing trait-object [`DiskSpaceProvider`] injection into [`RingStore`].
#[derive(Debug, Clone)]
pub struct SharedDiskSpaceProvider(Arc<dyn DiskSpaceProvider>);

impl SharedDiskSpaceProvider {
    /// Wraps a concrete [`DiskSpaceProvider`] inside an `Arc` handle.
    #[must_use]
    pub fn new(provider: Arc<dyn DiskSpaceProvider>) -> Self {
        Self(provider)
    }

    /// Creates the default OS-backed [`DiskSpaceProvider`].
    #[must_use]
    pub fn os_default() -> Self {
        Self(Arc::new(OsDiskSpaceProvider))
    }
}

impl DiskSpaceProvider for SharedDiskSpaceProvider {
    fn disk_space(&self, path: &Path) -> Result<DiskSpaceInfo, OsError> {
        self.0.disk_space(path)
    }
}

/// Whether post-trigger boundary drain confirmed a heartbeat or timed out.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailCompleteness {
    /// Post-trigger heartbeat arrived within `drain_timeout_ms` (or capture was idle).
    Complete,
    /// Boundary drain timed out before confirming post-trigger heartbeat.
    MayBeIncomplete,
}

impl TailCompleteness {
    /// Returns `true` when `tail_may_be_incomplete` should be flagged in the manifest.
    #[must_use]
    pub const fn may_be_incomplete(self) -> bool {
        matches!(self, Self::MayBeIncomplete)
    }
}

/// Persisted batch stored inside a Zone-1 immutable ring chunk file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RingChunkBatch {
    events: Vec<CaptureEvent>,
}

/// Durable Phase-1 parameters saved alongside `<data_dir>/save_jobs/<job-id>.json`
/// so `save.retry` / `save.retarget` can rematerialize identically even across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SaveJobMaterializeParams {
    bundle_id: String,
    duration_sec: u64,
    trigger_utc: String,
    window: WindowSpec,
    policy: SelectionPolicy,
    tail_may_be_incomplete: bool,
    device: DeviceInfo,
    video_width: u16,
    video_height: u16,
    fallback_video_config: Option<Vec<u8>>,
}

/// Tracks multi-touch `getevent -lt` raw Linux input events into normalized taps.
#[derive(Debug, Default)]
struct TouchNormalizer {
    current_slot: i32,
    slots: BTreeMap<i32, TouchSlotState>,
}

#[derive(Debug, Default, Clone, Copy)]
struct TouchSlotState {
    active: bool,
    x: Option<i32>,
    y: Option<i32>,
    pending_action: Option<TouchAction>,
}

impl TouchNormalizer {
    fn ingest(
        &mut self,
        payload: &TouchEventPayload,
        viewport: Option<(u32, u32)>,
    ) -> Option<NormalizedTouchEvent> {
        match (payload.ev_type, payload.ev_code) {
            (EV_ABS, ABS_MT_SLOT) => {
                self.current_slot = payload.ev_value;
                None
            }
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let slot = self.slots.entry(self.current_slot).or_default();
                if payload.ev_value >= 0 {
                    slot.active = true;
                    slot.pending_action = Some(TouchAction::Down);
                } else {
                    slot.active = false;
                    slot.pending_action = Some(TouchAction::Up);
                }
                None
            }
            (EV_ABS, ABS_MT_POSITION_X | ABS_X) => {
                let slot = self.slots.entry(self.current_slot).or_default();
                slot.x = Some(payload.ev_value);
                if slot.active && slot.pending_action.is_none() {
                    slot.pending_action = Some(TouchAction::Move);
                }
                None
            }
            (EV_ABS, ABS_MT_POSITION_Y | ABS_Y) => {
                let slot = self.slots.entry(self.current_slot).or_default();
                slot.y = Some(payload.ev_value);
                if slot.active && slot.pending_action.is_none() {
                    slot.pending_action = Some(TouchAction::Move);
                }
                None
            }
            (EV_KEY, BTN_TOUCH) => {
                let slot = self.slots.entry(self.current_slot).or_default();
                let action = if payload.ev_value != 0 {
                    slot.active = true;
                    TouchAction::Down
                } else {
                    slot.active = false;
                    TouchAction::Up
                };
                slot.pending_action = Some(action);
                None
            }
            (EV_SYN, SYN_REPORT) => {
                let slot_id = self.current_slot;
                let slot = self.slots.entry(slot_id).or_default();
                let action = slot.pending_action.take()?;
                Some(NormalizedTouchEvent {
                    device_mono_ns: payload.event_monotonic_ns(),
                    device_read_boottime_ns: payload.device_read_boottime_ns,
                    action,
                    pointer_id: slot_id,
                    x: slot.x.unwrap_or(0),
                    y: slot.y.unwrap_or(0),
                    viewport,
                })
            }
            _ => None,
        }
    }
}

/// Single-owner live capture state and Zone-1 ring buffer manager.
#[derive(Debug)]
pub struct CapturePipeline {
    data_dir: PathBuf,
    ring: RingStore<SharedDiskSpaceProvider>,
    active: bool,
    connected: bool,
    storage_pressure_stopped: bool,
    serial: Option<String>,
    device: DeviceInfo,
    video_width: u16,
    video_height: u16,
    latest_video_config: Option<Vec<u8>>,
    timeline: ClockTimeline,
    pending_events: Vec<CaptureEvent>,
    pending_bytes: usize,
    video_frames_seen: usize,
    audio_frames_seen: usize,
    log_records_seen: usize,
    tap_records_seen: usize,
    discontinuities_seen: usize,
    static_hold_seen: usize,
    latest_heartbeat_boottime_ns: Option<i64>,
    latest_heartbeat_pc_cont_ns: Option<i64>,
    latest_pc_cont_ns: Option<i64>,
    last_discontinuities: Vec<DiscontinuityEvent>,
}

impl CapturePipeline {
    /// Opens the Zone-1 ring store under `data_dir` and initializes an idle capture pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] if the Zone-1 ring store cannot be opened.
    pub fn open(
        data_dir: impl AsRef<Path>,
        policy: RingBudgetPolicy,
        disk_provider: SharedDiskSpaceProvider,
    ) -> Result<Self, DaemonError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let ring = RingStore::open_with_disk_provider(&data_dir, policy, disk_provider)?;
        Ok(Self {
            data_dir,
            ring,
            active: false,
            connected: false,
            storage_pressure_stopped: false,
            serial: None,
            device: DeviceInfo {
                serial: "unknown".to_owned(),
                model: "unknown".to_owned(),
                api_level: 0,
                is_emulator: false,
            },
            video_width: 1920,
            video_height: 1080,
            latest_video_config: None,
            timeline: ClockTimeline::new(ClockTimelineConfig::new()),
            pending_events: Vec::new(),
            pending_bytes: 0,
            video_frames_seen: 0,
            audio_frames_seen: 0,
            log_records_seen: 0,
            tap_records_seen: 0,
            discontinuities_seen: 0,
            static_hold_seen: 0,
            latest_heartbeat_boottime_ns: None,
            latest_heartbeat_pc_cont_ns: None,
            latest_pc_cont_ns: None,
            last_discontinuities: Vec::new(),
        })
    }

    /// Marks the start of a new capture session for `serial`.
    pub fn begin_session(&mut self, serial: impl Into<String>) {
        let serial = serial.into();
        self.active = true;
        self.connected = false;
        self.storage_pressure_stopped = false;
        self.serial = Some(serial.clone());
        self.device.serial = serial;
    }

    /// Marks the active capture session as stopped and flushes buffered events to Zone-1 ring.
    pub fn mark_stopped(&mut self) {
        self.active = false;
        self.connected = false;
        let _ = self.flush_pending_chunk();
    }

    /// Returns whether capture is currently active and not stopped by storage pressure.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active && !self.storage_pressure_stopped
    }

    /// Returns whether the transport is currently connected.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// Returns the most recently observed device heartbeat boottime in nanoseconds.
    #[must_use]
    pub const fn latest_heartbeat_boottime_ns(&self) -> Option<i64> {
        self.latest_heartbeat_boottime_ns
    }

    /// Returns the most recently observed host continuous monotonic timestamp of a heartbeat.
    #[must_use]
    pub const fn latest_heartbeat_pc_cont_ns(&self) -> Option<i64> {
        self.latest_heartbeat_pc_cont_ns
    }

    /// Returns a snapshot of live capture telemetry for `system.status`.
    #[must_use]
    pub fn status_view(&self) -> CaptureStatusView {
        CaptureStatusView {
            active: self.active,
            connected: self.connected,
            serial: self.serial.clone(),
            boot_id: self.timeline.current_boot_id().map(str::to_owned),
            storage_pressure_stopped: self.storage_pressure_stopped,
            video_frames: self.video_frames_seen,
            audio_frames: self.audio_frames_seen,
            log_records: self.log_records_seen,
            tap_records: self.tap_records_seen,
            discontinuities: self.discontinuities_seen,
            static_hold_events: self.static_hold_seen,
        }
    }

    /// Returns `(chunk_count, total_bytes, pinned_chunk_count)` for Zone-1 ring telemetry.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] if pin journals cannot be read.
    pub fn ring_stats(&self) -> Result<(usize, u64, usize), DaemonError> {
        let chunks = self.ring.chunks();
        let total_bytes = chunks
            .iter()
            .fold(0_u64, |acc, c| acc.saturating_add(c.bytes));
        let pinned_count = self.ring.pinned_chunk_ids()?.len();
        Ok((chunks.len(), total_bytes, pinned_count))
    }

    /// Returns a shared reference to the Zone-1 [`RingStore`].
    #[must_use]
    pub const fn ring_store(&self) -> &RingStore<SharedDiskSpaceProvider> {
        &self.ring
    }

    /// Returns all discontinuities recorded so far in the live capture session.
    #[must_use]
    pub fn discontinuities(&self) -> &[DiscontinuityEvent] {
        &self.last_discontinuities
    }

    /// Ingests a single [`CaptureEvent`] from either live ADB or [`arb_adb::mock::MockDeviceSource`].
    ///
    /// If host disk free space drops below `min_free_disk_bytes`, records
    /// [`DiscontinuityKind::StoragePressureStop`], halts capture writes, and leaves all
    /// pinned chunks and previously saved bundles untouched.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] for non-pressure storage errors.
    pub fn ingest_event(&mut self, event: CaptureEvent) -> Result<(), DaemonError> {
        if self.storage_pressure_stopped {
            return Ok(());
        }

        let event_pc_ns = capture_event_pc_cont_ns(&event);
        if let Some(ts) = event_pc_ns {
            self.latest_pc_cont_ns = Some(self.latest_pc_cont_ns.map_or(ts, |prev| prev.max(ts)));
        }

        let mut should_flush = false;
        let approx_bytes = estimate_event_bytes(&event);

        match &event {
            CaptureEvent::Hello { payload, .. } => {
                self.connected = true;
                self.device.model.clone_from(&payload.model);
                self.device.api_level = payload.api_level;
                self.device.is_emulator = payload.is_emulator;
                should_flush = true;
            }
            CaptureEvent::Heartbeat {
                pc_cont_ns,
                boot_id,
                payload,
            } => {
                self.connected = true;
                self.latest_heartbeat_boottime_ns = Some(payload.boottime_ns);
                self.latest_heartbeat_pc_cont_ns = Some(*pc_cont_ns);
                let triple = DeviceClockTriple {
                    boot_id: boot_id.clone(),
                    boottime_ns: payload.boottime_ns,
                    monotonic_ns: payload.monotonic_ns,
                    realtime_ns: payload.realtime_ns,
                };
                let _ = self.timeline.ingest_device_triple(triple, *pc_cont_ns);
                should_flush = true;
            }
            CaptureEvent::SyncProbe { probe, .. } => {
                let _ = self.timeline.ingest_pc_sync_probe(probe.clone());
            }
            CaptureEvent::VideoSession { meta, .. } => {
                self.video_width = u16::try_from(meta.width).unwrap_or(1920);
                self.video_height = u16::try_from(meta.height).unwrap_or(1080);
                should_flush = true;
            }
            CaptureEvent::VideoFrame { packet, .. } => {
                if packet.is_config() {
                    self.latest_video_config = Some(packet.data.clone());
                    should_flush = true;
                } else {
                    self.video_frames_seen += 1;
                }
            }
            CaptureEvent::AudioFrame { .. } => {
                self.audio_frames_seen += 1;
            }
            CaptureEvent::LogRecord { .. } => {
                self.log_records_seen += 1;
            }
            CaptureEvent::TouchEvent { .. } => {
                self.tap_records_seen += 1;
            }
            CaptureEvent::StaticHold { .. } => {
                self.static_hold_seen += 1;
            }
            CaptureEvent::Discontinuity(disc) => {
                self.discontinuities_seen += 1;
                self.last_discontinuities.push(disc.clone());
                if disc.kind == DiscontinuityKind::ConnectionLost {
                    self.connected = false;
                }
                should_flush = true;
            }
            CaptureEvent::ConnectionRestored { .. } => {
                self.connected = true;
                should_flush = true;
            }
            CaptureEvent::Shutdown { .. } => {
                self.connected = false;
                should_flush = true;
            }
            _ => {}
        }

        self.pending_bytes = self.pending_bytes.saturating_add(approx_bytes);
        self.pending_events.push(event);

        if should_flush
            || self.pending_events.len() >= MAX_BATCH_EVENTS
            || self.pending_bytes >= MAX_BATCH_BYTES
        {
            self.flush_pending_chunk()?;
        }

        Ok(())
    }

    /// Flushes any buffered events into an immutable closed Zone-1 ring chunk.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] on unexpected I/O failure. Storage pressure
    /// (`StorageError::StoragePressureStop` or `StorageError::RingFullPinned`) is handled
    /// gracefully by recording [`DiscontinuityKind::StoragePressureStop`] and halting capture.
    pub fn flush_pending_chunk(&mut self) -> Result<(), DaemonError> {
        if self.pending_events.is_empty() {
            return Ok(());
        }

        let batch = RingChunkBatch {
            events: std::mem::take(&mut self.pending_events),
        };
        self.pending_bytes = 0;

        let payload = serde_json::to_vec(&batch)?;
        let created_cont_ns = batch
            .events
            .first()
            .and_then(capture_event_pc_cont_ns)
            .or(self.latest_pc_cont_ns)
            .unwrap_or(0);
        let end_cont_ns = batch
            .events
            .last()
            .and_then(capture_event_pc_cont_ns)
            .unwrap_or(created_cont_ns);
        let duration_ns =
            u64::try_from(end_cont_ns.saturating_sub(created_cont_ns).max(0)).unwrap_or(0);
        let chunk_id = next_id("chunk");

        match self.ring.write_closed_chunk(
            chunk_id,
            RingChunkKind::Video,
            created_cont_ns,
            duration_ns,
            &payload,
        ) {
            Ok((_desc, _evicted)) => Ok(()),
            Err(StorageError::StoragePressureStop {
                host_free_bytes,
                min_free_disk_bytes,
                ..
            }) => {
                self.storage_pressure_stopped = true;
                self.active = false;
                self.discontinuities_seen += 1;
                self.last_discontinuities.push(DiscontinuityEvent {
                    kind: DiscontinuityKind::StoragePressureStop,
                    stream: None,
                    boot_id: self.timeline.current_boot_id().map(str::to_owned),
                    start_boottime_ns: self.latest_heartbeat_boottime_ns,
                    end_boottime_ns: self.latest_heartbeat_boottime_ns,
                    pc_cont_start_ns: created_cont_ns,
                    pc_cont_end_ns: Some(created_cont_ns),
                    delta_ns: None,
                    detail: Some(format!(
                        "host free space ({host_free_bytes} bytes) below floor ({min_free_disk_bytes} bytes); pinned & saved data protected"
                    )),
                });
                Ok(())
            }
            Err(StorageError::RingFullPinned {
                remaining_bytes,
                max_ring_bytes,
                ..
            }) => {
                self.storage_pressure_stopped = true;
                self.active = false;
                self.discontinuities_seen += 1;
                self.last_discontinuities.push(DiscontinuityEvent {
                    kind: DiscontinuityKind::StoragePressureStop,
                    stream: None,
                    boot_id: self.timeline.current_boot_id().map(str::to_owned),
                    start_boottime_ns: self.latest_heartbeat_boottime_ns,
                    end_boottime_ns: self.latest_heartbeat_boottime_ns,
                    pc_cont_start_ns: created_cont_ns,
                    pc_cont_end_ns: Some(created_cont_ns),
                    delta_ns: None,
                    detail: Some(format!(
                        "ring full of pinned chunks ({remaining_bytes}/{max_ring_bytes} bytes); capture halted to protect pins"
                    )),
                });
                Ok(())
            }
            Err(other) => Err(DaemonError::Storage(other)),
        }
    }

    /// Executes Phase-1 pinning: flushes pending chunks, durably pins all live ring chunks
    /// in `<data_dir>/save_jobs/<job-id>.json`, writes durable job materialization parameters,
    /// and inserts a `Pinned` row into `catalog`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Conflict`] if no ring chunks exist to save, or storage errors.
    pub fn phase1_pin_save(
        &mut self,
        catalog: &Catalog,
        duration_sec: u64,
        dest_parent_dir: &Path,
        policy: SelectionPolicy,
        tail_completeness: TailCompleteness,
    ) -> Result<CatalogSaveJob, DaemonError> {
        // Flush any in-memory buffered events into closed immutable chunks first.
        // Ignore storage pressure here if we already have chunks to save.
        let _ = self.flush_pending_chunk();

        let chunks = self.ring.chunks();
        if chunks.is_empty() {
            return Err(DaemonError::Conflict(
                "no captured ring chunks available to save".to_owned(),
            ));
        }

        let chunk_ids: Vec<String> = chunks.into_iter().map(|c| c.chunk_id).collect();
        let job_id = next_id("job");
        let bundle_id = next_id("bundle");
        let now_utc = utc_now_iso8601();
        let dest_bundle_dir = dest_parent_dir.join(&bundle_id);

        let window = self.build_save_window(duration_sec);

        let _pin_record =
            self.ring
                .pin_chunks(&job_id, &dest_bundle_dir, chunk_ids.clone(), &now_utc)?;

        let params = SaveJobMaterializeParams {
            bundle_id,
            duration_sec,
            trigger_utc: now_utc.clone(),
            window,
            policy,
            tail_may_be_incomplete: tail_completeness.may_be_incomplete(),
            device: self.device.clone(),
            video_width: self.video_width,
            video_height: self.video_height,
            fallback_video_config: self.latest_video_config.clone(),
        };
        write_job_params(&self.data_dir, &job_id, &params)?;

        let created = catalog.create_pinned_job(&job_id, &dest_bundle_dir, &chunk_ids, &now_utc)?;
        Ok(created)
    }

    /// Executes Phase-2 materialization for `job_id` from its durably pinned Zone-1 chunks.
    ///
    /// On write/commit failure (e.g., unwritable destination), cleans staging automatically,
    /// preserves pins in both [`PinJournal`] and [`Catalog`], and transitions the job to
    /// [`SaveJobState::FailedRetryable`] so `save.retry` / `save.retarget` / `save.discard`
    /// can recover cleanly.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the job does not exist or its catalog transitions fail.
    pub fn phase2_materialize(
        &self,
        catalog: &Catalog,
        job_id: &str,
    ) -> Result<SaveOutcomeResponse, DaemonError> {
        let now_utc = utc_now_iso8601();
        let job_record = catalog
            .save_job(job_id)?
            .ok_or_else(|| DaemonError::NotFound(format!("save job `{job_id}` not found")))?;

        let job_record = match job_record.job.state {
            SaveJobState::Pinned => {
                let _ = catalog.mark_draining(job_id, &now_utc)?;
                catalog.mark_materializing(job_id, &now_utc)?
            }
            SaveJobState::Draining => catalog.mark_materializing(job_id, &now_utc)?,
            SaveJobState::Materializing => job_record,
            other => {
                return Err(DaemonError::Conflict(format!(
                    "save job `{job_id}` cannot materialize from state {other:?}"
                )));
            }
        };

        let params = read_job_params(&self.data_dir, job_id)?;
        let dest_path = PathBuf::from(&job_record.job.destination_path);
        let pinned_chunks = &job_record.job.pinned_chunk_ids;

        match self.materialize_from_pinned_chunks(job_id, &dest_path, pinned_chunks, &params) {
            Ok((bundle_id, unanchored_summary)) => {
                let done_utc = utc_now_iso8601();
                catalog.register_bundle(&NewBundleEntry {
                    bundle_id: bundle_id.clone(),
                    path: dest_path.clone(),
                    format_version: CatalogFormatVersion::V2,
                    parent_bundle_id: None,
                    created_at_utc: params.trigger_utc.clone(),
                })?;
                let completed = catalog.complete_job(job_id, &done_utc)?;
                let _ = self.ring.unpin_job(job_id);
                let _ = remove_job_params(&self.data_dir, job_id);

                Ok(SaveOutcomeResponse {
                    job: save_job_to_view(&completed),
                    bundle_id: Some(bundle_id),
                    bundle_path: Some(dest_path),
                    tail_may_be_incomplete: params.tail_may_be_incomplete,
                    unanchored_summary: Some(unanchored_summary),
                })
            }
            Err(err) => {
                let err_msg = err.to_string();
                let fail_utc = utc_now_iso8601();
                let failed = catalog.mark_failed_retryable(job_id, &err_msg, &fail_utc)?;
                Ok(SaveOutcomeResponse {
                    job: save_job_to_view(&failed),
                    bundle_id: None,
                    bundle_path: None,
                    tail_may_be_incomplete: params.tail_may_be_incomplete,
                    unanchored_summary: None,
                })
            }
        }
    }

    /// Retries a `FailedRetryable` save job to its existing destination path.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the job is missing or not in `FailedRetryable`.
    pub fn retry_save_job(
        &self,
        catalog: &Catalog,
        job_id: &str,
    ) -> Result<SaveOutcomeResponse, DaemonError> {
        let now_utc = utc_now_iso8601();
        let _ = catalog.retry_job(job_id, &now_utc)?;
        self.phase2_materialize(catalog, job_id)
    }

    /// Retargets a `FailedRetryable` save job to `new_dest_dir` and rematerializes it
    /// from its preserved pinned ring chunks.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the job is missing, invalid, or catalog update fails.
    pub fn retarget_save_job(
        &self,
        catalog: &Catalog,
        job_id: &str,
        new_dest_dir: &Path,
    ) -> Result<SaveOutcomeResponse, DaemonError> {
        if new_dest_dir.as_os_str().is_empty() {
            return Err(DaemonError::InvalidParams(
                "dest_dir must not be empty".to_owned(),
            ));
        }
        let params = read_job_params(&self.data_dir, job_id)?;
        let new_bundle_dir = new_dest_dir.join(&params.bundle_id);
        let now_utc = utc_now_iso8601();

        let _ = self
            .ring
            .pin_journal()
            .update_destination(job_id, &new_bundle_dir)?;
        let _ = catalog.retarget_job(job_id, &new_bundle_dir, &now_utc)?;
        self.phase2_materialize(catalog, job_id)
    }

    /// Discards a `Pinned`, `Draining`, or `FailedRetryable` save job and releases its pins.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the job is missing or already terminal.
    pub fn discard_save_job(
        &self,
        catalog: &Catalog,
        job_id: &str,
    ) -> Result<SaveJobView, DaemonError> {
        let now_utc = utc_now_iso8601();
        let discarded = catalog.discard_job(job_id, &now_utc)?;
        let _ = self.ring.unpin_job(job_id);
        let _ = remove_job_params(&self.data_dir, job_id);
        Ok(save_job_to_view(&discarded))
    }

    fn build_save_window(&self, duration_sec: u64) -> WindowSpec {
        let effective_sec = duration_sec.max(1);
        let duration_ns = i64::try_from(effective_sec)
            .unwrap_or(30)
            .saturating_mul(NANOS_PER_SEC);
        let boot_id = self
            .timeline
            .current_boot_id()
            .unwrap_or("unknown")
            .to_owned();
        let end_boottime_ns = self.latest_heartbeat_boottime_ns.unwrap_or(duration_ns);
        let start_boottime_ns = end_boottime_ns.saturating_sub(duration_ns);
        let ingest_end_cont_ns = self.latest_pc_cont_ns.unwrap_or(duration_ns);
        let ingest_start_cont_ns = ingest_end_cont_ns.saturating_sub(duration_ns);

        WindowSpec::RealElapsed {
            boot_id,
            start_boottime_ns,
            end_boottime_ns,
            ingest_start_cont_ns,
            ingest_end_cont_ns,
        }
    }

    fn materialize_from_pinned_chunks(
        &self,
        job_id: &str,
        dest_bundle_dir: &Path,
        pinned_chunk_ids: &[String],
        params: &SaveJobMaterializeParams,
    ) -> Result<(String, UnanchoredSummary), DaemonError> {
        let mut all_events = Vec::new();
        for chunk_id in pinned_chunk_ids {
            let bytes = self.ring.read_chunk(chunk_id)?;
            let batch: RingChunkBatch = serde_json::from_slice(&bytes)?;
            all_events.extend(batch.events);
        }

        let mut replayed = ReplayCollector::new(params);
        replayed.ingest_all(&all_events);
        let draft = replayed.build_draft(job_id, params, &self.data_dir)?;
        let unanchored_summary = draft.unanchored_summary;
        let bundle_id = draft.bundle_id.clone();

        // Write atomically via V2BundleWriter (AtomicStagingDir cleans up on error).
        let _manifest = V2BundleWriter::write(dest_bundle_dir, job_id, &draft)?;
        Ok((bundle_id, unanchored_summary))
    }
}

/// Replays pinned [`CaptureEvent`] records through [`ClockTimeline`] and the
/// 3-valued [`arb_core::selection`] engine to build a [`V2BundleDraft`].
struct ReplayCollector {
    timeline: ClockTimeline,
    clock_anchors: Vec<ClockAnchorRecord>,
    video_width: u16,
    video_height: u16,
    video_config: Option<Vec<u8>>,
    raw_video: Vec<RawCapturedVideo>,
    audio_frames_count: usize,
    audio_unavailable: bool,
    audio_gap: bool,
    raw_logs: Vec<RawCapturedLog>,
    raw_taps: Vec<RawCapturedTap>,
    discontinuities: Vec<DiscontinuityEvent>,
    log_completeness: LogCompleteness,
    log_overflow_events: u64,
    touch_normalizer: TouchNormalizer,
    epochs_map: BTreeMap<String, (i64, i64)>,
}

struct RawCapturedVideo {
    boot_id: String,
    pts_mono_ns: i64,
    pc_ingest_cont_ns: i64,
    is_keyframe: bool,
    annex_b: Vec<u8>,
}

struct RawCapturedLog {
    boot_id: String,
    header_epoch_ns: Option<i64>,
    device_read_boottime_ns: Option<i64>,
    pc_ingest_cont_ns: i64,
    level: Option<String>,
    tag: Option<String>,
    pid: Option<i32>,
    tid: Option<i32>,
    message: String,
}

struct RawCapturedTap {
    boot_id: String,
    event_mono_ns: Option<i64>,
    pc_ingest_cont_ns: i64,
    action: String,
    x: Option<f64>,
    y: Option<f64>,
    pointer_id: Option<i32>,
}

impl ReplayCollector {
    fn new(params: &SaveJobMaterializeParams) -> Self {
        Self {
            timeline: ClockTimeline::new(ClockTimelineConfig::new()),
            clock_anchors: Vec::new(),
            video_width: params.video_width,
            video_height: params.video_height,
            video_config: params.fallback_video_config.clone(),
            raw_video: Vec::new(),
            audio_frames_count: 0,
            audio_unavailable: false,
            audio_gap: false,
            raw_logs: Vec::new(),
            raw_taps: Vec::new(),
            discontinuities: Vec::new(),
            log_completeness: LogCompleteness::Unverified,
            log_overflow_events: 0,
            touch_normalizer: TouchNormalizer::default(),
            epochs_map: BTreeMap::new(),
        }
    }

    fn record_epoch_boottime(&mut self, boot_id: &str, boottime_ns: i64) {
        self.epochs_map
            .entry(boot_id.to_owned())
            .and_modify(|(min_bt, max_bt)| {
                *min_bt = (*min_bt).min(boottime_ns);
                *max_bt = (*max_bt).max(boottime_ns);
            })
            .or_insert((boottime_ns, boottime_ns));
    }

    fn ingest_all(&mut self, events: &[CaptureEvent]) {
        for event in events {
            self.ingest_one(event);
        }
    }

    fn ingest_one(&mut self, event: &CaptureEvent) {
        match event {
            CaptureEvent::Heartbeat {
                pc_cont_ns,
                boot_id,
                payload,
            } => {
                let triple = DeviceClockTriple {
                    boot_id: boot_id.clone(),
                    boottime_ns: payload.boottime_ns,
                    monotonic_ns: payload.monotonic_ns,
                    realtime_ns: payload.realtime_ns,
                };
                self.record_epoch_boottime(boot_id, payload.boottime_ns);
                let _ = self
                    .timeline
                    .ingest_device_triple(triple.clone(), *pc_cont_ns);
                self.clock_anchors
                    .push(ClockAnchorRecord::DeviceTriple(triple));
            }
            CaptureEvent::SyncProbe { probe, .. } => {
                self.record_epoch_boottime(&probe.boot_id, probe.dev_boottime_ns);
                let _ = self.timeline.ingest_pc_sync_probe(probe.clone());
                self.clock_anchors
                    .push(ClockAnchorRecord::PcSyncProbe(probe.clone()));
            }
            CaptureEvent::VideoSession { meta, .. } => {
                self.video_width = u16::try_from(meta.width).unwrap_or(self.video_width);
                self.video_height = u16::try_from(meta.height).unwrap_or(self.video_height);
            }
            CaptureEvent::VideoFrame {
                pc_cont_ns,
                boot_id,
                packet,
            } => {
                if packet.kind == VideoPacketKind::Config {
                    self.video_config = Some(packet.data.clone());
                } else {
                    let pts_mono_ns = packet.pts_monotonic_ns().unwrap_or(0);
                    self.raw_video.push(RawCapturedVideo {
                        boot_id: boot_id.clone(),
                        pts_mono_ns,
                        pc_ingest_cont_ns: *pc_cont_ns,
                        is_keyframe: packet.is_keyframe(),
                        annex_b: packet.data.clone(),
                    });
                }
            }
            CaptureEvent::AudioFrame { .. } => self.audio_frames_count += 1,
            CaptureEvent::AudioUnavailable { .. } => self.audio_unavailable = true,
            _ => self.ingest_non_media_event(event),
        }
    }

    fn ingest_non_media_event(&mut self, event: &CaptureEvent) {
        match event {
            CaptureEvent::LogRecord {
                pc_cont_ns,
                boot_id,
                payload,
                ..
            } => {
                let (tag, message) = payload.parse_text_tag_and_message().unwrap_or_else(|| {
                    (
                        String::new(),
                        format!("<binary {} bytes>", payload.payload.len()),
                    )
                });
                let level = Some(log_priority_label(payload.priority).to_owned());
                self.raw_logs.push(RawCapturedLog {
                    boot_id: boot_id.clone(),
                    header_epoch_ns: payload.header_epoch_ns(),
                    device_read_boottime_ns: Some(payload.device_read_boottime_ns),
                    pc_ingest_cont_ns: *pc_cont_ns,
                    level,
                    tag: (!tag.is_empty()).then_some(tag),
                    pid: Some(payload.pid),
                    tid: Some(payload.tid),
                    message,
                });
            }
            CaptureEvent::LogOverflowStats { payload, .. } => {
                let next_comp = payload.completeness.to_core_completeness();
                if next_comp == LogCompleteness::OverflowDetected {
                    self.log_completeness = LogCompleteness::OverflowDetected;
                    self.log_overflow_events = self.log_overflow_events.saturating_add(1);
                } else if self.log_completeness != LogCompleteness::OverflowDetected {
                    self.log_completeness = next_comp;
                }
            }
            CaptureEvent::TouchEvent {
                pc_cont_ns,
                boot_id,
                payload,
                normalized,
                ..
            } => {
                let viewport = Some((u32::from(self.video_width), u32::from(self.video_height)));
                let norm = normalized
                    .clone()
                    .or_else(|| self.touch_normalizer.ingest(payload, viewport));
                if let Some(t) = norm {
                    self.raw_taps.push(RawCapturedTap {
                        boot_id: boot_id.clone(),
                        event_mono_ns: t.device_mono_ns,
                        pc_ingest_cont_ns: *pc_cont_ns,
                        action: touch_action_label(t.action).to_owned(),
                        x: Some(f64::from(t.x)),
                        y: Some(f64::from(t.y)),
                        pointer_id: Some(t.pointer_id),
                    });
                }
            }
            CaptureEvent::StreamState { payload, .. } => {
                if payload.target_stream_id == StreamId::Audio
                    && matches!(
                        payload.state,
                        StreamLifecycleState::Error | StreamLifecycleState::Stopped
                    )
                {
                    self.audio_gap = true;
                }
            }
            CaptureEvent::Discontinuity(disc) => {
                if disc.kind == DiscontinuityKind::StreamGap
                    && disc.stream == Some(StreamKind::Audio)
                {
                    self.audio_gap = true;
                }
                self.discontinuities.push(disc.clone());
            }
            _ => {}
        }
    }

    fn select_video(
        timeline: &ClockTimeline,
        raw_video: Vec<RawCapturedVideo>,
        params: &SaveJobMaterializeParams,
    ) -> Vec<AlignedVideoItem> {
        let aligned: Vec<AlignedVideoItem> = raw_video
            .into_iter()
            .enumerate()
            .map(|(idx, raw)| {
                let alignment = timeline.map_device_monotonic(
                    &raw.boot_id,
                    raw.pts_mono_ns,
                    TimestampSemantics::VideoEncoderInputEstimate,
                    raw.pc_ingest_cont_ns,
                );
                AlignedVideoItem {
                    record: FrameRecord {
                        frame_index: u64::try_from(idx).unwrap_or(0),
                        boot_id: raw.boot_id,
                        pts_device_mono_ns: raw.pts_mono_ns,
                        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
                        alignment,
                        is_keyframe: raw.is_keyframe,
                        gop_id: 0,
                        mp4_sample_offset: 0,
                        mp4_sample_size: u32::try_from(raw.annex_b.len()).unwrap_or(u32::MAX),
                    },
                    annex_b: raw.annex_b,
                }
            })
            .collect();
        let part = select_records(aligned, &params.window, &params.policy);
        if let Some(first_kf) = part.main.iter().position(|v| v.record.is_keyframe) {
            part.main.into_iter().skip(first_kf).collect()
        } else {
            part.main
        }
    }

    fn select_logs(
        timeline: &ClockTimeline,
        raw_logs: Vec<RawCapturedLog>,
        params: &SaveJobMaterializeParams,
    ) -> (Vec<LogRecord>, usize, usize) {
        let aligned: Vec<LogRecord> = raw_logs
            .into_iter()
            .enumerate()
            .map(|(idx, raw)| {
                let alignment = match raw.header_epoch_ns {
                    Some(header_ns) => timeline.map_logcat_wall(
                        &raw.boot_id,
                        header_ns,
                        raw.device_read_boottime_ns,
                        raw.pc_ingest_cont_ns,
                    ),
                    None => TimeAlignment::Unanchored {
                        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
                        reason: arb_core::time::UncertaintyReason::PcIngestOnly,
                    },
                };
                LogRecord {
                    seq: u64::try_from(idx + 1).unwrap_or(u64::MAX),
                    boot_id: Some(raw.boot_id),
                    header_epoch_ns: raw.header_epoch_ns,
                    device_read_boottime_ns: raw.device_read_boottime_ns,
                    pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
                    alignment,
                    bucket: RecordBucket::Main,
                    level: raw.level,
                    tag: raw.tag,
                    pid: raw.pid,
                    tid: raw.tid,
                    message: raw.message,
                }
            })
            .collect();
        let part = select_records(aligned, &params.window, &params.policy);
        let unanchored_count = part.unanchored.len();
        let truncated = part.unanchored_truncated_count;
        let mut final_logs = Vec::with_capacity(part.main.len() + unanchored_count);
        let mut seq = 1_u64;
        for mut rec in part.main {
            rec.seq = seq;
            rec.bucket = RecordBucket::Main;
            final_logs.push(rec);
            seq += 1;
        }
        for mut rec in part.unanchored {
            rec.seq = seq;
            rec.bucket = RecordBucket::Unanchored;
            final_logs.push(rec);
            seq += 1;
        }
        (final_logs, unanchored_count, truncated)
    }

    fn select_taps(
        timeline: &ClockTimeline,
        raw_taps: Vec<RawCapturedTap>,
        params: &SaveJobMaterializeParams,
    ) -> (Vec<TapRecord>, usize, usize) {
        let aligned: Vec<TapRecord> = raw_taps
            .into_iter()
            .enumerate()
            .map(|(idx, raw)| {
                let alignment = match raw.event_mono_ns {
                    Some(mono_ns) => timeline.map_device_monotonic(
                        &raw.boot_id,
                        mono_ns,
                        TimestampSemantics::InputEventTime,
                        raw.pc_ingest_cont_ns,
                    ),
                    None => TimeAlignment::Unanchored {
                        pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
                        reason: arb_core::time::UncertaintyReason::PcIngestOnly,
                    },
                };
                TapRecord {
                    seq: u64::try_from(idx + 1).unwrap_or(u64::MAX),
                    boot_id: Some(raw.boot_id),
                    event_mono_ns: raw.event_mono_ns,
                    pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
                    alignment,
                    bucket: RecordBucket::Main,
                    action: raw.action,
                    x: raw.x,
                    y: raw.y,
                    pointer_id: raw.pointer_id,
                }
            })
            .collect();
        let part = select_records(aligned, &params.window, &params.policy);
        let unanchored_count = part.unanchored.len();
        let truncated = part.unanchored_truncated_count;
        let mut final_taps = Vec::with_capacity(part.main.len() + unanchored_count);
        let mut seq = 1_u64;
        for mut rec in part.main {
            rec.seq = seq;
            rec.bucket = RecordBucket::Main;
            final_taps.push(rec);
            seq += 1;
        }
        for mut rec in part.unanchored {
            rec.seq = seq;
            rec.bucket = RecordBucket::Unanchored;
            final_taps.push(rec);
            seq += 1;
        }
        (final_taps, unanchored_count, truncated)
    }

    fn build_draft(
        self,
        job_id: &str,
        params: &SaveJobMaterializeParams,
        data_dir: &Path,
    ) -> Result<V2BundleDraft, DaemonError> {
        let selected_video = Self::select_video(&self.timeline, self.raw_video, params);
        let (video_mp4, final_frames, video_status) = mux_selected_video(
            job_id,
            data_dir,
            self.video_width,
            self.video_height,
            self.video_config.as_deref(),
            selected_video,
        )?;
        let (final_logs, unanchored_logs, trunc_logs) =
            Self::select_logs(&self.timeline, self.raw_logs, params);
        let (final_taps, unanchored_taps, trunc_taps) =
            Self::select_taps(&self.timeline, self.raw_taps, params);

        let unanchored_summary = UnanchoredSummary {
            included_count: u64::try_from(unanchored_logs.saturating_add(unanchored_taps))
                .unwrap_or(u64::MAX),
            truncated_count: u64::try_from(trunc_logs.saturating_add(trunc_taps))
                .unwrap_or(u64::MAX),
        };

        let frame_count = u64::try_from(final_frames.len()).unwrap_or(0);
        let streams = build_streams_summary(&StreamSummaryCounts {
            frame_count,
            video_status,
            audio_frames_count: self.audio_frames_count,
            audio_partial: self.audio_gap || self.audio_unavailable,
            logs_count: final_logs.len(),
            log_completeness: self.log_completeness,
            log_overflow_events: self.log_overflow_events,
            taps_count: final_taps.len(),
            tail_may_be_incomplete: params.tail_may_be_incomplete,
        });

        let epochs: Vec<EpochSpan> = if self.epochs_map.is_empty() {
            vec![EpochSpan {
                boot_id: params.window.boot_id().to_owned(),
                start_boottime_ns: params.window.start_boottime_ns(),
                end_boottime_ns: params.window.end_boottime_ns(),
            }]
        } else {
            self.epochs_map
                .into_iter()
                .map(
                    |(boot_id, (start_boottime_ns, end_boottime_ns))| EpochSpan {
                        boot_id,
                        start_boottime_ns,
                        end_boottime_ns,
                    },
                )
                .collect()
        };

        Ok(V2BundleDraft {
            bundle_id: params.bundle_id.clone(),
            created_at_utc: params.trigger_utc.clone(),
            provenance: Provenance::Original,
            device: params.device.clone(),
            requested_window: RequestedWindow::RealElapsedSec {
                duration_sec: params.duration_sec.max(1),
                trigger_pc_wall_utc: params.trigger_utc.clone(),
            },
            epochs,
            streams,
            unanchored_summary,
            video_mp4,
            audio_m4a: None,
            frames: final_frames,
            discontinuities: self.discontinuities,
            logs: final_logs,
            taps: final_taps,
            clock_anchors: self.clock_anchors,
        })
    }
}

struct StreamSummaryCounts {
    frame_count: u64,
    video_status: StreamStatus,
    audio_frames_count: usize,
    audio_partial: bool,
    logs_count: usize,
    log_completeness: LogCompleteness,
    log_overflow_events: u64,
    taps_count: usize,
    tail_may_be_incomplete: bool,
}

fn build_streams_summary(counts: &StreamSummaryCounts) -> StreamsSummary {
    let tail_may_be_incomplete = counts.tail_may_be_incomplete;
    StreamsSummary {
        video: Some(VideoStreamMeta {
            status: if counts.frame_count == 0 {
                StreamStatus::Absent
            } else {
                counts.video_status
            },
            frame_count: counts.frame_count,
            tail_may_be_incomplete,
            semantics: TimestampSemantics::VideoEncoderInputEstimate,
        }),
        audio: Some(AudioStreamMeta {
            status: if counts.audio_frames_count == 0 {
                StreamStatus::Absent
            } else if counts.audio_partial {
                StreamStatus::Partial
            } else {
                StreamStatus::Present
            },
            av_offset_status: AvOffsetStatus::Uncalibrated,
            av_offset_ms: 0,
            tail_may_be_incomplete,
        }),
        logcat: Some(LogcatStreamMeta {
            status: if counts.logs_count == 0 {
                StreamStatus::Absent
            } else if counts.log_completeness == LogCompleteness::OverflowDetected {
                StreamStatus::Partial
            } else {
                StreamStatus::Present
            },
            completeness: counts.log_completeness,
            overflow_events: counts.log_overflow_events,
            tail_may_be_incomplete,
        }),
        input: Some(InputStreamMeta {
            status: if counts.taps_count == 0 {
                StreamStatus::Absent
            } else {
                StreamStatus::Present
            },
            tail_may_be_incomplete,
        }),
    }
}

struct AlignedVideoItem {
    record: FrameRecord,
    annex_b: Vec<u8>,
}

impl arb_core::selection::AlignedRecord for AlignedVideoItem {
    fn time_alignment(&self) -> &TimeAlignment {
        &self.record.alignment
    }

    fn pc_ingest_cont_ns(&self) -> i64 {
        self.record.pc_ingest_cont_ns
    }

    fn estimated_bytes(&self) -> usize {
        self.annex_b.len()
    }
}

fn mux_selected_video(
    job_id: &str,
    data_dir: &Path,
    width: u16,
    height: u16,
    video_config: Option<&[u8]>,
    selected: Vec<AlignedVideoItem>,
) -> Result<(Option<MediaPayload>, Vec<FrameRecord>, StreamStatus), DaemonError> {
    if selected.is_empty() {
        return Ok((None, Vec::new(), StreamStatus::Absent));
    }

    let mut packets: Vec<RawH264Packet> = Vec::with_capacity(selected.len());
    for (idx, item) in selected.iter().enumerate() {
        let mut annex_b = Vec::new();
        if idx == 0
            && let Some(cfg) = video_config
        {
            annex_b.extend_from_slice(cfg);
        }
        annex_b.extend_from_slice(&item.annex_b);
        packets.push(RawH264Packet {
            annex_b,
            pts_ns: item.record.pts_device_mono_ns,
            is_keyframe: item.record.is_keyframe,
        });
    }

    let tmp_dir = data_dir.join("tmp");
    fs::create_dir_all(&tmp_dir)?;
    let tmp_mp4 = tmp_dir.join(format!("mux-{job_id}.mp4"));

    if let Ok(_report) = mux_raw_h264_to_mp4(
        &tmp_mp4,
        width.max(16),
        height.max(16),
        MP4_TIMESCALE,
        MP4_DEFAULT_FRAME_DURATION_TICKS,
        &packets,
    ) {
        let track = Mp4VideoTrack::open(&tmp_mp4)?;
        let indexed = track.frames();
        let mut frames = Vec::with_capacity(selected.len());
        for (idx, item) in selected.into_iter().enumerate() {
            let mut rec = item.record;
            rec.frame_index = u64::try_from(idx).unwrap_or(0);
            if let Some(mp4_f) = indexed.get(idx) {
                rec.is_keyframe = mp4_f.is_keyframe;
                rec.gop_id = mp4_f.gop_id;
                rec.mp4_sample_offset = mp4_f.offset;
                rec.mp4_sample_size = mp4_f.size;
            }
            frames.push(rec);
        }
        let mp4_bytes = fs::read(&tmp_mp4)?;
        let _ = fs::remove_file(&tmp_mp4);
        Ok((
            Some(MediaPayload::Bytes(mp4_bytes)),
            frames,
            StreamStatus::Present,
        ))
    } else {
        // Synthetic stub streams lacking valid SPS/PPS still preserve timeline metadata
        // honestly as Partial without writing a corrupt MP4 container.
        let _ = fs::remove_file(&tmp_mp4);
        let mut gop_id = 0_u64;
        let mut frames = Vec::with_capacity(selected.len());
        for (idx, item) in selected.into_iter().enumerate() {
            let mut rec = item.record;
            if rec.is_keyframe && idx > 0 {
                gop_id += 1;
            }
            rec.frame_index = u64::try_from(idx).unwrap_or(0);
            rec.gop_id = gop_id;
            frames.push(rec);
        }
        Ok((None, frames, StreamStatus::Partial))
    }
}

/// Converts a [`CatalogSaveJob`] into its JSON-RPC [`SaveJobView`].
#[must_use]
pub fn save_job_to_view(record: &CatalogSaveJob) -> SaveJobView {
    SaveJobView {
        job_id: record.job.job_id.clone(),
        state: record.job.state,
        destination_path: PathBuf::from(&record.job.destination_path),
        pinned_chunk_ids: record.job.pinned_chunk_ids.clone(),
        last_error: record.job.last_error.clone(),
        created_at_utc: record.created_at_utc.clone(),
        updated_at_utc: record.updated_at_utc.clone(),
    }
}

fn job_params_path(data_dir: &Path, job_id: &str) -> PathBuf {
    data_dir
        .join("save_job_params")
        .join(format!("{job_id}.params.json"))
}

fn write_job_params(
    data_dir: &Path,
    job_id: &str,
    params: &SaveJobMaterializeParams,
) -> Result<(), DaemonError> {
    let path = job_params_path(data_dir, job_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(params)?;
    fs::write(&path, bytes)?;
    Ok(())
}

fn read_job_params(data_dir: &Path, job_id: &str) -> Result<SaveJobMaterializeParams, DaemonError> {
    let path = job_params_path(data_dir, job_id);
    let bytes = fs::read(&path).map_err(|_| {
        DaemonError::NotFound(format!(
            "materialization parameters missing for save job `{job_id}`"
        ))
    })?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn remove_job_params(data_dir: &Path, job_id: &str) -> Result<(), std::io::Error> {
    let path = job_params_path(data_dir, job_id);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

const fn capture_event_pc_cont_ns(event: &CaptureEvent) -> Option<i64> {
    match event {
        CaptureEvent::Hello { pc_cont_ns, .. }
        | CaptureEvent::Heartbeat { pc_cont_ns, .. }
        | CaptureEvent::SyncProbe { pc_cont_ns, .. }
        | CaptureEvent::VideoSession { pc_cont_ns, .. }
        | CaptureEvent::VideoFrame { pc_cont_ns, .. }
        | CaptureEvent::AudioConfig { pc_cont_ns, .. }
        | CaptureEvent::AudioFrame { pc_cont_ns, .. }
        | CaptureEvent::AudioUnavailable { pc_cont_ns, .. }
        | CaptureEvent::LogRecord { pc_cont_ns, .. }
        | CaptureEvent::LogOverflowStats { pc_cont_ns, .. }
        | CaptureEvent::TouchEvent { pc_cont_ns, .. }
        | CaptureEvent::StreamState { pc_cont_ns, .. }
        | CaptureEvent::StaticHold { pc_cont_ns, .. }
        | CaptureEvent::ConnectionRestored { pc_cont_ns, .. }
        | CaptureEvent::Shutdown { pc_cont_ns, .. } => Some(*pc_cont_ns),
        CaptureEvent::Discontinuity(disc) => Some(disc.pc_cont_start_ns),
        _ => None,
    }
}

fn estimate_event_bytes(event: &CaptureEvent) -> usize {
    match event {
        CaptureEvent::VideoFrame { packet, .. } => packet.data.len().saturating_add(128),
        CaptureEvent::AudioConfig { packet, .. } | CaptureEvent::AudioFrame { packet, .. } => {
            packet.data.len().saturating_add(96)
        }
        CaptureEvent::LogRecord { payload, .. } => payload.payload.len().saturating_add(128),
        _ => 256,
    }
}

const fn log_priority_label(priority: u8) -> &'static str {
    match priority {
        2 => "V",
        3 => "D",
        5 => "W",
        6 => "E",
        7 => "F",
        _ => "I",
    }
}

const fn touch_action_label(action: TouchAction) -> &'static str {
    match action {
        TouchAction::Down => "DOWN",
        TouchAction::Move => "MOVE",
        TouchAction::Up => "UP",
        TouchAction::Cancel => "CANCEL",
        _ => "UNKNOWN",
    }
}
