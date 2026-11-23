//! Live capture ingestion pipeline, Zone-1 ring chunk persistence, and two-phase save engine.
//!
//! Bridges [`arb_adb::mock::DeviceEventSource`] (both live ADB sessions and deterministic
//! [`arb_adb::mock::MockDeviceSource`]) into [`ClockTimeline`], [`RingStore`],
//! [`arb_core::selection::select_records`], [`arb_media::mp4::mux_raw_h264_to_mp4`],
//! and [`V2BundleWriter`].
//!
//! # Save engine (R3)
//!
//! A save runs in three steps so the capture lock `C` is never held across long I/O:
//!
//! 1. [`CapturePipeline::phase1_pin`] (under `C`) fixes `T_trigger`, builds the save window and
//!    durably pins **only** the ring chunks overlapping that window (plus the nearest
//!    preceding keyframe / config / heartbeat chunk).
//! 2. The caller waits for the boundary drain **without** `C`, then
//!    [`CapturePipeline::phase1_commit`] (under `C`) extends the pins to tail chunks, writes
//!    the materialization parameters atomically, creates the catalog job and returns a
//!    [`Phase2Plan`] holding immutable chunk paths.
//! 3. [`phase2_materialize`] reads, muxes and writes the bundle with `C` released. Every
//!    outcome ends in `Completed` or `FailedRetryable`; a bundle committed with only a failed
//!    parent-directory `fsync` is treated as published.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
use arb_media::MediaError;
use arb_media::mp4::{Mp4VideoTrack, RawH264Packet, mux_raw_h264_to_mp4};
use arb_os::OsError;
use arb_os::clock::sample_host_clock;
use arb_os::fs::{DiskSpaceInfo, fsync_dir};
use arb_storage::StorageError;
use arb_storage::bundle_io::{ClockAnchorRecord, MediaPayload, V2BundleDraft, V2BundleWriter};
use arb_storage::catalog::{Catalog, CatalogFormatVersion, CatalogSaveJob, NewBundleEntry};
use arb_storage::ring::{
    DiskSpaceProvider, OsDiskSpaceProvider, PinJournal, RingChunkDescriptor, RingChunkKind,
    RingStore,
};
use serde::{Deserialize, Serialize};

use crate::error::DaemonError;
use crate::protocol::{CaptureStatusView, SaveJobView, SaveOutcomeResponse};
use crate::time_util::{next_id, utc_now_iso8601};

const NANOS_PER_SEC: i64 = 1_000_000_000;
const MP4_TIMESCALE: u32 = 90_000;
const MP4_DEFAULT_FRAME_DURATION_TICKS: u32 = 3_000; // 30 fps
const MAX_BATCH_EVENTS: usize = 32;
const MAX_BATCH_BYTES: usize = 128 * 1024;
/// Upper bound on retained carried (storage-pressure / ring-write) discontinuities.
const MAX_PRESSURE_DISCONTINUITIES: usize = 64;
/// Upper bound on live discontinuities kept in memory for `live_discontinuities()` (R3 S14).
const MAX_LIVE_DISCONTINUITIES: usize = 256;
/// Minimum `PcContinuousMono` margin added on both sides of the save window when choosing
/// which ring chunks to pin (R3 M2). The policy's unanchored ingest margin is used when larger.
const MIN_PIN_WINDOW_MARGIN_NS: i64 = 2 * NANOS_PER_SEC;
/// Magic prefix of the compact (header JSON + raw binary blobs) ring chunk encoding (R3 M2).
const RING_CHUNK_MAGIC: &[u8; 8] = b"ARBRC02\n";
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

/// Deterministic one-shot fault injection points for crash/failure regression tests.
///
/// Every flag fires at most once (it is cleared when it triggers). Production code never sets
/// them; they exist so the E2E suite can reproduce post-commit failures that cannot be
/// provoked portably through the filesystem (R3 M1).
#[derive(Debug, Default)]
pub struct FaultInjection {
    /// After a successful bundle commit, report `OsError::CommittedParentSyncFailed`
    /// (the bundle is published, only the parent-directory `fsync` "failed").
    pub fail_parent_sync_once: AtomicBool,
    /// After registering the bundle, fail `complete_job` as if the catalog write failed.
    pub fail_complete_job_once: AtomicBool,
    /// After registering the bundle, abort Phase 2 immediately as if the process crashed,
    /// leaving the job in `Materializing` for startup recovery.
    pub crash_after_register_once: AtomicBool,
}

impl FaultInjection {
    fn fire(flag: &AtomicBool) -> bool {
        flag.swap(false, Ordering::AcqRel)
    }

    /// Converts a successful commit into the injected `CommittedParentSyncFailed` error.
    pub(crate) fn maybe_fail_parent_sync(&self, dest: &Path) -> Result<(), StorageError> {
        if Self::fire(&self.fail_parent_sync_once) {
            return Err(StorageError::Os(OsError::CommittedParentSyncFailed {
                path: dest.to_path_buf(),
                source: std::io::Error::other("injected parent directory fsync failure"),
            }));
        }
        Ok(())
    }
}

/// Whether post-trigger boundary drain confirmed a heartbeat or timed out.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailCompleteness {
    /// Post-trigger heartbeat arrived within `drain_timeout_ms` (or capture was idle).
    Complete,
    /// Boundary drain timed out before confirming post-trigger heartbeat, or the capture
    /// session is active but disconnected.
    MayBeIncomplete,
}

impl TailCompleteness {
    /// Returns `true` when `tail_may_be_incomplete` should be flagged in the manifest.
    #[must_use]
    pub const fn may_be_incomplete(self) -> bool {
        matches!(self, Self::MayBeIncomplete)
    }
}

/// Legacy (pre-R3) persisted batch: plain `serde_json` with byte arrays as decimal numbers.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RingChunkBatch {
    events: Vec<CaptureEvent>,
}

/// Header of the compact ring chunk encoding: events with their binary payloads moved out,
/// plus the byte length of each event's payload (`0` for events without one).
#[derive(Debug, Serialize, Deserialize)]
struct CompactChunkHeader {
    events: Vec<CaptureEvent>,
    blob_lens: Vec<u64>,
}

/// Returns the single binary payload carried by `event`, if any.
fn event_blob_mut(event: &mut CaptureEvent) -> Option<&mut Vec<u8>> {
    match event {
        CaptureEvent::VideoFrame { packet, .. } => Some(&mut packet.data),
        CaptureEvent::AudioConfig { packet, .. } | CaptureEvent::AudioFrame { packet, .. } => {
            Some(&mut packet.data)
        }
        CaptureEvent::LogRecord { payload, .. } => Some(&mut payload.payload),
        _ => None,
    }
}

/// Encodes `events` compactly (R3 M2): `MAGIC | u64 LE header_len | header JSON | blobs`.
///
/// Binary payloads (H.264, AAC, logcat) are stored raw instead of as JSON decimal arrays,
/// which were ~3.6× larger and slow to parse.
fn encode_ring_chunk(mut events: Vec<CaptureEvent>) -> Result<Vec<u8>, DaemonError> {
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    let mut blob_lens = Vec::with_capacity(events.len());
    for event in &mut events {
        if let Some(blob) = event_blob_mut(event) {
            let data = std::mem::take(blob);
            blob_lens.push(u64::try_from(data.len()).unwrap_or(u64::MAX));
            blobs.push(data);
        } else {
            blob_lens.push(0);
        }
    }
    let header = serde_json::to_vec(&CompactChunkHeader { events, blob_lens })?;
    let blob_total: usize = blobs.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(RING_CHUNK_MAGIC.len() + 8 + header.len() + blob_total);
    out.extend_from_slice(RING_CHUNK_MAGIC);
    out.extend_from_slice(
        &u64::try_from(header.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(&header);
    for blob in blobs {
        out.extend_from_slice(&blob);
    }
    Ok(out)
}

/// Decodes a ring chunk in either the compact (R3) or the legacy plain-JSON encoding.
fn decode_ring_chunk(bytes: &[u8]) -> Result<Vec<CaptureEvent>, DaemonError> {
    let Some(rest) = bytes.strip_prefix(RING_CHUNK_MAGIC.as_slice()) else {
        let batch: RingChunkBatch = serde_json::from_slice(bytes)?;
        return Ok(batch.events);
    };
    let corrupt = |reason: &str| DaemonError::Internal(format!("corrupt ring chunk: {reason}"));
    let (len_bytes, rest) = rest
        .split_first_chunk::<8>()
        .ok_or_else(|| corrupt("truncated header length"))?;
    let header_len = usize::try_from(u64::from_le_bytes(*len_bytes))
        .map_err(|_| corrupt("header length overflow"))?;
    if header_len > rest.len() {
        return Err(corrupt("header length exceeds chunk size"));
    }
    let (header_bytes, mut blob_bytes) = rest.split_at(header_len);
    let header: CompactChunkHeader = serde_json::from_slice(header_bytes)?;
    if header.blob_lens.len() != header.events.len() {
        return Err(corrupt("blob table length mismatch"));
    }
    let mut events = header.events;
    for (event, len) in events.iter_mut().zip(header.blob_lens) {
        let len = usize::try_from(len).map_err(|_| corrupt("blob length overflow"))?;
        if len > blob_bytes.len() {
            return Err(corrupt("blob exceeds chunk size"));
        }
        let (blob, rest) = blob_bytes.split_at(len);
        blob_bytes = rest;
        match event_blob_mut(event) {
            Some(slot) => *slot = blob.to_vec(),
            None if len == 0 => {}
            None => return Err(corrupt("blob attached to an event without payload")),
        }
    }
    if !blob_bytes.is_empty() {
        return Err(corrupt("trailing bytes after blobs"));
    }
    Ok(events)
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
    /// Discontinuities that never reached a ring chunk (`StoragePressureStop`, failed ring
    /// writes) and overlap the save window. They are merged into the saved bundle.
    #[serde(default)]
    extra_discontinuities: Vec<DiscontinuityEvent>,
}

/// What a closed ring chunk contains, used to pick the nearest preceding anchor chunks.
#[derive(Debug, Default, Clone, Copy)]
struct ChunkContent {
    keyframe: bool,
    video_config: bool,
    heartbeat: bool,
}

impl ChunkContent {
    fn of(events: &[CaptureEvent]) -> Self {
        let mut content = Self::default();
        for event in events {
            match event {
                CaptureEvent::VideoFrame { packet, .. } if packet.is_config() => {
                    content.video_config = true;
                }
                CaptureEvent::VideoFrame { packet, .. } if packet.is_keyframe() => {
                    content.keyframe = true;
                }
                CaptureEvent::Heartbeat { .. } | CaptureEvent::Hello { .. } => {
                    content.heartbeat = true;
                }
                _ => {}
            }
        }
        content
    }
}

/// Clock domain of the current session's `pc_cont_ns` stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClockDomain {
    /// Replayed / mock events stamped in their own synthetic domain.
    Replayed,
    /// Live events stamped with the host continuous clock.
    HostLive,
}

/// Whether a carried discontinuity also halts capture (storage pressure) or not (I/O gap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarriedEffect {
    HaltCapture,
    KeepCapturing,
}

/// `T_trigger` captured under the capture lock at the moment `save.trigger` arrives (R3 S1).
#[derive(Debug, Clone, Copy)]
struct TriggerPoint {
    pc_cont_ns: i64,
    boottime_ns: i64,
}

/// Rolls back a Phase-1 pin (journal + params file) unless it was committed (R3 S3).
#[derive(Debug)]
struct PinRollbackGuard {
    pin_journal: PinJournal,
    data_dir: PathBuf,
    job_id: String,
    armed: bool,
}

impl Drop for PinRollbackGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.pin_journal.unpin(&self.job_id);
            let _ = remove_job_params(&self.data_dir, &self.job_id);
        }
    }
}

/// A save whose window chunks are durably pinned (Phase 1) but whose catalog job is not yet
/// created. Dropping it without [`CapturePipeline::phase1_commit`] releases the pins.
#[derive(Debug)]
pub struct PendingSave {
    guard: PinRollbackGuard,
    bundle_id: String,
    dest_bundle_dir: PathBuf,
    duration_sec: u64,
    trigger_utc: String,
    window: WindowSpec,
    policy: SelectionPolicy,
    pinned_chunk_ids: Vec<String>,
    drain_trigger_boottime_ns: i64,
    drain_decision: TailCompleteness,
    must_drain: bool,
}

impl PendingSave {
    /// Returns the save job identifier reserved for this pending save.
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.guard.job_id
    }

    /// Returns the ring chunk IDs pinned in Phase 1 (before the boundary drain).
    #[must_use]
    pub fn pinned_chunk_ids(&self) -> &[String] {
        &self.pinned_chunk_ids
    }

    /// Returns `Some(trigger_boottime_ns)` when the caller must wait for a post-trigger
    /// heartbeat (active + connected capture); otherwise `None`.
    #[must_use]
    pub const fn drain_trigger_boottime_ns(&self) -> Option<i64> {
        if self.must_drain {
            Some(self.drain_trigger_boottime_ns)
        } else {
            None
        }
    }

    /// Tail completeness when no drain wait is needed (idle → `Complete`, active but
    /// disconnected → `MayBeIncomplete`).
    #[must_use]
    pub const fn tail_without_drain(&self) -> TailCompleteness {
        self.drain_decision
    }
}

/// Immutable inputs for [`phase2_materialize`], extracted under the capture lock so the
/// lock can be released before any chunk is read, muxed or written (R3 M2).
#[derive(Debug, Clone)]
pub struct Phase2Plan {
    data_dir: PathBuf,
    pin_journal: PinJournal,
    chunk_paths: Vec<PathBuf>,
    faults: Arc<FaultInjection>,
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
    faults: Arc<FaultInjection>,
    active: bool,
    connected: bool,
    storage_pressure_stopped: bool,
    clock_domain: ClockDomain,
    session_generation: u64,
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
    ring_write_errors: usize,
    latest_heartbeat_boottime_ns: Option<i64>,
    latest_heartbeat_pc_cont_ns: Option<i64>,
    latest_pc_cont_ns: Option<i64>,
    last_discontinuities: Vec<DiscontinuityEvent>,
    pressure_discontinuities: Vec<DiscontinuityEvent>,
    chunk_contents: BTreeMap<String, ChunkContent>,
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
            faults: Arc::new(FaultInjection::default()),
            active: false,
            connected: false,
            storage_pressure_stopped: false,
            clock_domain: ClockDomain::Replayed,
            session_generation: 0,
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
            ring_write_errors: 0,
            latest_heartbeat_boottime_ns: None,
            latest_heartbeat_pc_cont_ns: None,
            latest_pc_cont_ns: None,
            last_discontinuities: Vec::new(),
            pressure_discontinuities: Vec::new(),
            chunk_contents: BTreeMap::new(),
        })
    }

    /// Installs the shared [`FaultInjection`] switches (tests only; defaults to all-off).
    pub fn set_fault_injection(&mut self, faults: Arc<FaultInjection>) {
        self.faults = faults;
    }

    /// Marks the start of a new (replayed / mock) capture session for `serial` whose event
    /// timestamps are in their own `pc_cont_ns` domain, and returns its session generation.
    pub fn begin_session(&mut self, serial: impl Into<String>) -> u64 {
        self.begin_session_inner(serial.into(), ClockDomain::Replayed)
    }

    /// Marks the start of a live capture session for `serial` whose `pc_cont_ns` stamps come
    /// from the host continuous clock, and returns its session generation.
    ///
    /// For live sessions `T_trigger` is taken from the host clock at the save request, so a
    /// save made long after a disconnect ends at the request time, not at the last heartbeat.
    pub fn begin_live_session(&mut self, serial: impl Into<String>) -> u64 {
        self.begin_session_inner(serial.into(), ClockDomain::HostLive)
    }

    fn begin_session_inner(&mut self, serial: String, clock_domain: ClockDomain) -> u64 {
        self.session_generation = self.session_generation.wrapping_add(1);
        self.active = true;
        self.connected = false;
        self.storage_pressure_stopped = false;
        self.clock_domain = clock_domain;
        self.serial = Some(serial.clone());
        self.device.serial = serial;
        self.session_generation
    }

    /// Returns the generation of the most recently started capture session.
    #[must_use]
    pub const fn session_generation(&self) -> u64 {
        self.session_generation
    }

    /// Marks the active capture session as stopped and flushes buffered events to Zone-1 ring.
    pub fn mark_stopped(&mut self) {
        self.active = false;
        self.connected = false;
        let _ = self.flush_pending_chunk();
    }

    /// Like [`Self::mark_stopped`], but only if `generation` is still the current session,
    /// so a stale supervisor thread can never stop a newer session (R3 S12).
    pub fn mark_stopped_if_current(&mut self, generation: u64) {
        if self.session_generation == generation {
            self.mark_stopped();
        }
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

    /// Returns the number of ring chunk writes that failed with an I/O error (R3 S5).
    #[must_use]
    pub const fn ring_write_errors(&self) -> usize {
        self.ring_write_errors
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
            ring_write_errors: self.ring_write_errors,
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

    /// Returns the most recent discontinuities recorded in the live capture session
    /// (bounded to the last 256 events).
    #[must_use]
    pub fn discontinuities(&self) -> &[DiscontinuityEvent] {
        &self.last_discontinuities
    }

    /// Ingests `event` only if `generation` is still the current session (R3 S12): events
    /// from a supervisor thread that is still tearing down never leak into a newer session.
    ///
    /// # Errors
    ///
    /// See [`Self::ingest_event`].
    pub fn ingest_event_for_session(
        &mut self,
        generation: u64,
        event: CaptureEvent,
    ) -> Result<(), DaemonError> {
        if generation != self.session_generation {
            return Ok(());
        }
        self.ingest_event(event)
    }

    /// Ingests a single [`CaptureEvent`] from either live ADB or [`arb_adb::mock::MockDeviceSource`].
    ///
    /// If host disk free space drops below `min_free_disk_bytes`, records
    /// [`DiscontinuityKind::StoragePressureStop`], halts capture writes, and leaves all
    /// pinned chunks and previously saved bundles untouched.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] for non-pressure storage errors (already recorded as
    /// a `StreamGap` discontinuity, see [`Self::flush_pending_chunk`]).
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
                self.push_live_discontinuity(disc.clone());
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

    fn push_live_discontinuity(&mut self, event: DiscontinuityEvent) {
        self.last_discontinuities.push(event);
        if self.last_discontinuities.len() > MAX_LIVE_DISCONTINUITIES {
            let excess = self.last_discontinuities.len() - MAX_LIVE_DISCONTINUITIES;
            self.last_discontinuities.drain(..excess);
        }
    }

    /// Flushes any buffered events into an immutable closed Zone-1 ring chunk.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] on unexpected I/O failure; the lost batch is recorded
    /// first as a [`DiscontinuityKind::StreamGap`] (R3 S5) that is shown live and carried
    /// into later bundles whose window overlaps it. Storage pressure
    /// (`StorageError::StoragePressureStop` or `StorageError::RingFullPinned`) is handled
    /// gracefully by recording [`DiscontinuityKind::StoragePressureStop`] and halting capture.
    pub fn flush_pending_chunk(&mut self) -> Result<(), DaemonError> {
        if self.pending_events.is_empty() {
            return Ok(());
        }

        let events = std::mem::take(&mut self.pending_events);
        self.pending_bytes = 0;

        let created_cont_ns = events
            .first()
            .and_then(capture_event_pc_cont_ns)
            .or(self.latest_pc_cont_ns)
            .unwrap_or(0);
        let end_cont_ns = events
            .iter()
            .filter_map(capture_event_pc_cont_ns)
            .max()
            .unwrap_or(created_cont_ns)
            .max(created_cont_ns);
        let duration_ns =
            u64::try_from(end_cont_ns.saturating_sub(created_cont_ns).max(0)).unwrap_or(0);
        let event_count = events.len();
        let content = ChunkContent::of(&events);
        let payload = encode_ring_chunk(events)?;
        let chunk_id = next_id("chunk");

        match self.ring.write_closed_chunk(
            chunk_id.clone(),
            RingChunkKind::Video,
            created_cont_ns,
            duration_ns,
            &payload,
        ) {
            Ok((_desc, evicted)) => {
                for id in evicted {
                    self.chunk_contents.remove(&id);
                }
                self.chunk_contents.insert(chunk_id, content);
                self.drain_ring_discontinuities();
                Ok(())
            }
            Err(StorageError::StoragePressureStop { .. }) => {
                self.storage_pressure_stopped = true;
                self.active = false;
                // The ring recorded one `StoragePressureStop` for this episode; drain it below.
                self.drain_ring_discontinuities();
                Ok(())
            }
            Err(StorageError::RingFullPinned {
                remaining_bytes,
                max_ring_bytes,
                ..
            }) => {
                self.storage_pressure_stopped = true;
                self.active = false;
                self.record_carried_discontinuity(
                    DiscontinuityEvent {
                        kind: DiscontinuityKind::StoragePressureStop,
                        stream: None,
                        boot_id: None,
                        start_boottime_ns: None,
                        end_boottime_ns: None,
                        pc_cont_start_ns: created_cont_ns,
                        pc_cont_end_ns: Some(created_cont_ns),
                        delta_ns: None,
                        detail: Some(format!(
                            "ring full of pinned chunks ({remaining_bytes}/{max_ring_bytes} bytes); capture halted to protect pins"
                        )),
                    },
                    CarriedEffect::HaltCapture,
                );
                Ok(())
            }
            Err(other) => {
                // R3 S5: never drop a batch silently. The events are gone, so record the gap
                // (it never reaches a chunk either, hence "carried") and keep capturing.
                self.ring_write_errors = self.ring_write_errors.saturating_add(1);
                self.record_carried_discontinuity(
                    DiscontinuityEvent {
                        kind: DiscontinuityKind::StreamGap,
                        stream: None,
                        boot_id: None,
                        start_boottime_ns: None,
                        end_boottime_ns: None,
                        pc_cont_start_ns: created_cont_ns,
                        pc_cont_end_ns: Some(end_cont_ns),
                        delta_ns: None,
                        detail: Some(format!(
                            "ring chunk write failed; {event_count} captured events lost: {other}"
                        )),
                    },
                    CarriedEffect::KeepCapturing,
                );
                Err(DaemonError::Storage(other))
            }
        }
    }

    /// Drains the ring store's discontinuity buffer (S18). `RingStore` accumulates one
    /// `StoragePressureStop` per pressure episode; draining each time keeps its memory bounded
    /// during long sessions, and the events are retained (bounded) here so they are shown live
    /// and merged into every overlapping bundle saved afterwards.
    fn drain_ring_discontinuities(&mut self) {
        for event in self.ring.take_discontinuities() {
            self.record_carried_discontinuity(event, CarriedEffect::HaltCapture);
        }
    }

    fn record_carried_discontinuity(
        &mut self,
        mut event: DiscontinuityEvent,
        effect: CarriedEffect,
    ) {
        if event.boot_id.is_none() {
            event.boot_id = self.timeline.current_boot_id().map(str::to_owned);
        }
        if event.start_boottime_ns.is_none() {
            event.start_boottime_ns = self.latest_heartbeat_boottime_ns;
            event.end_boottime_ns = self.latest_heartbeat_boottime_ns;
        }
        if effect == CarriedEffect::HaltCapture {
            self.storage_pressure_stopped = true;
            self.active = false;
        }
        self.discontinuities_seen += 1;
        self.push_live_discontinuity(event.clone());
        self.pressure_discontinuities.push(event);
        if self.pressure_discontinuities.len() > MAX_PRESSURE_DISCONTINUITIES {
            let excess = self.pressure_discontinuities.len() - MAX_PRESSURE_DISCONTINUITIES;
            self.pressure_discontinuities.drain(..excess);
        }
    }

    /// Phase 1, step 1 (R3 S1/M2/S3): fixes `T_trigger` **before** any drain wait, builds the
    /// save window ending at `T_trigger`, and durably pins only the ring chunks overlapping it
    /// (plus the nearest preceding keyframe / video-config / heartbeat chunk).
    ///
    /// The returned [`PendingSave`] releases its pins when dropped unless it is passed to
    /// [`Self::phase1_commit`].
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Conflict`] if no ring chunk overlaps the requested window, or
    /// storage errors if the pin journal cannot be written.
    pub fn phase1_pin(
        &mut self,
        duration_sec: u64,
        dest_parent_dir: &Path,
        policy: SelectionPolicy,
    ) -> Result<PendingSave, DaemonError> {
        // Flush buffered events into closed immutable chunks first. A failed flush is already
        // recorded as a discontinuity; the chunks written so far can still be saved.
        let _ = self.flush_pending_chunk();

        let trigger = self.trigger_point();
        let window = self.build_save_window(duration_sec, trigger);
        let chunk_ids = self.select_window_chunks(&window, &policy);
        if chunk_ids.is_empty() {
            return Err(DaemonError::Conflict(
                "no captured ring chunks overlap the requested save window".to_owned(),
            ));
        }

        let job_id = next_id("job");
        let bundle_id = next_id("bundle");
        let now_utc = utc_now_iso8601();
        let dest_bundle_dir = dest_parent_dir.join(&bundle_id);

        let mut guard = PinRollbackGuard {
            pin_journal: self.ring.pin_journal().clone(),
            data_dir: self.data_dir.clone(),
            job_id: job_id.clone(),
            armed: false,
        };
        let _pin_record =
            self.ring
                .pin_chunks(&job_id, &dest_bundle_dir, chunk_ids.clone(), &now_utc)?;
        guard.armed = true;

        let (must_drain, drain_decision) = match (self.is_active(), self.is_connected()) {
            (true, true) => (true, TailCompleteness::Complete),
            (true, false) => (false, TailCompleteness::MayBeIncomplete),
            _ => (false, TailCompleteness::Complete),
        };

        Ok(PendingSave {
            guard,
            bundle_id,
            dest_bundle_dir,
            duration_sec,
            trigger_utc: now_utc,
            window,
            policy,
            pinned_chunk_ids: chunk_ids,
            drain_trigger_boottime_ns: self
                .latest_heartbeat_boottime_ns
                .unwrap_or(0)
                .saturating_add(1),
            drain_decision,
            must_drain,
        })
    }

    /// Phase 1, step 2: after the boundary drain, extends the pins to tail chunks written
    /// during the drain, writes the materialization parameters atomically, creates the
    /// `Pinned` catalog job, and returns the [`Phase2Plan`] to run without the capture lock.
    ///
    /// # Errors
    ///
    /// Returns storage/catalog errors; the pins of `pending` are released on any error (S3).
    pub fn phase1_commit(
        &mut self,
        catalog: &Catalog,
        mut pending: PendingSave,
        tail: TailCompleteness,
    ) -> Result<(CatalogSaveJob, Phase2Plan), DaemonError> {
        let _ = self.flush_pending_chunk();
        let mut chunk_ids = pending.pinned_chunk_ids.clone();
        for id in self.select_window_chunks(&pending.window, &pending.policy) {
            if !chunk_ids.contains(&id) {
                chunk_ids.push(id);
            }
        }
        let job_id = pending.guard.job_id.clone();
        if chunk_ids.len() != pending.pinned_chunk_ids.len() {
            let _ = self.ring.pin_chunks(
                &job_id,
                &pending.dest_bundle_dir,
                chunk_ids.clone(),
                &pending.trigger_utc,
            )?;
        }

        let params = SaveJobMaterializeParams {
            bundle_id: pending.bundle_id.clone(),
            duration_sec: pending.duration_sec,
            trigger_utc: pending.trigger_utc.clone(),
            window: pending.window.clone(),
            policy: pending.policy,
            tail_may_be_incomplete: tail.may_be_incomplete(),
            device: self.device.clone(),
            video_width: self.video_width,
            video_height: self.video_height,
            fallback_video_config: self.latest_video_config.clone(),
            extra_discontinuities: self
                .carried_discontinuities_for(&pending.window, &pending.policy),
        };
        write_job_params(&self.data_dir, &job_id, &params)?;

        let created = catalog.create_pinned_job(
            &job_id,
            &pending.dest_bundle_dir,
            &chunk_ids,
            &pending.trigger_utc,
        )?;
        let plan = self.phase2_plan(&created.job.pinned_chunk_ids)?;
        pending.guard.armed = false;
        Ok((created, plan))
    }

    /// Convenience wrapper running Phase 1 without a drain wait (tests / idle capture).
    ///
    /// # Errors
    ///
    /// See [`Self::phase1_pin`] and [`Self::phase1_commit`].
    pub fn phase1_pin_save(
        &mut self,
        catalog: &Catalog,
        duration_sec: u64,
        dest_parent_dir: &Path,
        policy: SelectionPolicy,
        tail_completeness: TailCompleteness,
    ) -> Result<(CatalogSaveJob, Phase2Plan), DaemonError> {
        let pending = self.phase1_pin(duration_sec, dest_parent_dir, policy)?;
        self.phase1_commit(catalog, pending, tail_completeness)
    }

    /// Builds the immutable [`Phase2Plan`] for already-pinned `chunk_ids`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] (`NotFound`) if a pinned chunk is missing.
    pub fn phase2_plan(&self, chunk_ids: &[String]) -> Result<Phase2Plan, DaemonError> {
        let chunk_paths = chunk_ids
            .iter()
            .map(|id| {
                self.ring.chunk_path(id).ok_or_else(|| {
                    DaemonError::Storage(StorageError::NotFound {
                        entity: "chunk",
                        id: id.clone(),
                    })
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Phase2Plan {
            data_dir: self.data_dir.clone(),
            pin_journal: self.ring.pin_journal().clone(),
            chunk_paths,
            faults: Arc::clone(&self.faults),
        })
    }

    /// Validates a `FailedRetryable` job, builds its [`Phase2Plan`] and moves it to
    /// `Materializing` (`save.retry`). Run under the capture lock; Phase 2 runs without it.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the job is missing or not `FailedRetryable`.
    pub fn prepare_retry(
        &self,
        catalog: &Catalog,
        job_id: &str,
    ) -> Result<Phase2Plan, DaemonError> {
        let job = load_failed_retryable_job(catalog, job_id)?;
        let plan = self.phase2_plan(&job.job.pinned_chunk_ids)?;
        let _ = catalog.retry_job(job_id, &utc_now_iso8601())?;
        Ok(plan)
    }

    /// Validates a `FailedRetryable` job (R3 S4: **before** touching the pin journal),
    /// retargets it to `new_dest_dir`, and returns its [`Phase2Plan`] (`save.retarget`).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the job is missing, not `FailedRetryable`, or updates fail;
    /// a failed catalog update rolls the pin journal back to its previous destination.
    pub fn prepare_retarget(
        &self,
        catalog: &Catalog,
        job_id: &str,
        new_dest_dir: &Path,
    ) -> Result<Phase2Plan, DaemonError> {
        if new_dest_dir.as_os_str().is_empty() {
            return Err(DaemonError::InvalidParams(
                "dest_dir must not be empty".to_owned(),
            ));
        }
        let job = load_failed_retryable_job(catalog, job_id)?;
        let params = read_job_params(&self.data_dir, job_id)?;
        let new_bundle_dir = new_dest_dir.join(&params.bundle_id);
        let plan = self.phase2_plan(&job.job.pinned_chunk_ids)?;
        let journal = self.ring.pin_journal();
        let previous = journal.load(job_id).ok().map(|r| r.destination_path);
        let _ = journal.update_destination(job_id, &new_bundle_dir)?;
        if let Err(err) = catalog.retarget_job(job_id, &new_bundle_dir, &utc_now_iso8601()) {
            if let Some(prev) = previous {
                let _ = journal.update_destination(job_id, prev);
            }
            return Err(err.into());
        }
        Ok(plan)
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

    /// `T_trigger` (R3 S1): the latest ingest time (or, for a live session, the host clock now)
    /// and the device boottime extrapolated from the latest heartbeat to that instant.
    fn trigger_point(&self) -> TriggerPoint {
        let observed = self.latest_pc_cont_ns.unwrap_or(0);
        let pc_cont_ns = if self.clock_domain == ClockDomain::HostLive && self.active {
            sample_host_clock().map_or(observed, |s| s.continuous_mono_ns.max(observed))
        } else {
            observed
        };
        let boottime_ns = match (
            self.latest_heartbeat_boottime_ns,
            self.latest_heartbeat_pc_cont_ns,
        ) {
            (Some(bt), Some(hb_pc)) => bt.saturating_add(pc_cont_ns.saturating_sub(hb_pc).max(0)),
            (Some(bt), None) => bt,
            (None, _) => pc_cont_ns,
        };
        TriggerPoint {
            pc_cont_ns,
            boottime_ns,
        }
    }

    fn build_save_window(&self, duration_sec: u64, trigger: TriggerPoint) -> WindowSpec {
        let effective_sec = duration_sec.max(1);
        let duration_ns = i64::try_from(effective_sec)
            .unwrap_or(30)
            .saturating_mul(NANOS_PER_SEC);
        let boot_id = self
            .timeline
            .current_boot_id()
            .unwrap_or("unknown")
            .to_owned();
        let end_boottime_ns = trigger.boottime_ns;
        let start_boottime_ns = end_boottime_ns.saturating_sub(duration_ns);
        let ingest_end_cont_ns = trigger.pc_cont_ns;
        let ingest_start_cont_ns = ingest_end_cont_ns.saturating_sub(duration_ns);

        WindowSpec::RealElapsed {
            boot_id,
            start_boottime_ns,
            end_boottime_ns,
            ingest_start_cont_ns,
            ingest_end_cont_ns,
        }
    }

    /// Ring chunks whose `[created, created + duration]` overlaps the window's ingest range
    /// widened by the pin margin, plus the nearest preceding keyframe / video-config /
    /// heartbeat chunk (R3 M2). Never the whole ring.
    fn select_window_chunks(&self, window: &WindowSpec, policy: &SelectionPolicy) -> Vec<String> {
        let margin = i64::try_from(policy.unanchored_ingest_margin_ns)
            .unwrap_or(i64::MAX)
            .max(MIN_PIN_WINDOW_MARGIN_NS);
        let lo = window.ingest_start_cont_ns().saturating_sub(margin);
        let hi = window.ingest_end_cont_ns().saturating_add(margin);
        let chunks = self.ring.chunks();
        let overlaps = |c: &RingChunkDescriptor| {
            let end = c
                .created_cont_ns
                .saturating_add(i64::try_from(c.duration_ns).unwrap_or(i64::MAX));
            c.created_cont_ns <= hi && end >= lo
        };
        let Some(first_idx) = chunks.iter().position(overlaps) else {
            return Vec::new();
        };
        let mut selected: Vec<String> = chunks
            .iter()
            .filter(|c| overlaps(c))
            .map(|c| c.chunk_id.clone())
            .collect();
        let mut need_keyframe = true;
        let mut need_config = true;
        let mut need_heartbeat = true;
        for chunk in chunks[..first_idx].iter().rev() {
            if !(need_keyframe || need_config || need_heartbeat) {
                break;
            }
            let Some(content) = self.chunk_contents.get(&chunk.chunk_id) else {
                continue;
            };
            let wanted = (need_keyframe && content.keyframe)
                || (need_config && content.video_config)
                || (need_heartbeat && content.heartbeat);
            if wanted {
                need_keyframe &= !content.keyframe;
                need_config &= !content.video_config;
                need_heartbeat &= !content.heartbeat;
                selected.insert(0, chunk.chunk_id.clone());
            }
        }
        selected
    }

    /// Carried discontinuities whose `pc_cont` span overlaps the save window (R3 C5).
    fn carried_discontinuities_for(
        &self,
        window: &WindowSpec,
        policy: &SelectionPolicy,
    ) -> Vec<DiscontinuityEvent> {
        let margin = i64::try_from(policy.unanchored_ingest_margin_ns)
            .unwrap_or(i64::MAX)
            .max(MIN_PIN_WINDOW_MARGIN_NS);
        let lo = window.ingest_start_cont_ns().saturating_sub(margin);
        let hi = window.ingest_end_cont_ns().saturating_add(margin);
        self.pressure_discontinuities
            .iter()
            .filter(|d| {
                let end = d.pc_cont_end_ns.unwrap_or(d.pc_cont_start_ns);
                d.pc_cont_start_ns <= hi && end.max(d.pc_cont_start_ns) >= lo
            })
            .cloned()
            .collect()
    }
}

fn load_failed_retryable_job(
    catalog: &Catalog,
    job_id: &str,
) -> Result<CatalogSaveJob, DaemonError> {
    let job = catalog
        .save_job(job_id)?
        .ok_or_else(|| DaemonError::NotFound(format!("save job `{job_id}` not found")))?;
    if job.job.state != SaveJobState::FailedRetryable {
        return Err(DaemonError::Conflict(format!(
            "save job `{job_id}` is {:?}; only failed_retryable jobs can be retried or retargeted",
            job.job.state
        )));
    }
    Ok(job)
}

/// Result of a successful materialization (bundle published on disk).
struct Materialized {
    bundle_id: String,
    unanchored_summary: UnanchoredSummary,
    warning: Option<String>,
}

/// Executes Phase-2 materialization for `job_id` from its durably pinned Zone-1 chunks,
/// **without** the capture lock (R3 M2).
///
/// Every path ends with the job in `Completed` or `FailedRetryable` (R3 M1):
/// - write/commit failure (e.g. unwritable destination) → `FailedRetryable`, pins preserved;
/// - commit succeeded but the parent-directory `fsync` failed (`CommittedParentSyncFailed`)
///   → treated as **published** (warning surfaced), never discarded;
/// - catalog registration / completion failure after commit → `FailedRetryable`; the retry
///   re-registers idempotently ([`Catalog::register_bundle_idempotent`]).
///
/// # Errors
///
/// Returns [`DaemonError`] if the job does not exist, cannot enter `Materializing`, or the
/// `FailedRetryable` transition itself fails.
pub fn phase2_materialize(
    plan: &Phase2Plan,
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

    // From here on the job is `Materializing`: no `?` may leave it there.
    let dest_path = PathBuf::from(&job_record.job.destination_path);
    let params = match read_job_params(&plan.data_dir, job_id) {
        Ok(params) => params,
        Err(err) => return fail_job(catalog, job_id, &err.to_string(), None, None),
    };
    let materialized = match materialize_from_chunks(plan, job_id, &dest_path, &params) {
        Ok(m) => m,
        Err(err) => {
            return fail_job(catalog, job_id, &err.to_string(), Some(&params), None);
        }
    };

    let registered = catalog.register_bundle_idempotent(&NewBundleEntry {
        bundle_id: materialized.bundle_id.clone(),
        path: dest_path.clone(),
        format_version: CatalogFormatVersion::V2,
        parent_bundle_id: None,
        created_at_utc: params.trigger_utc.clone(),
    });
    if registered.is_ok() && FaultInjection::fire(&plan.faults.crash_after_register_once) {
        return Err(DaemonError::Internal(
            "injected crash between register_bundle and complete_job".to_owned(),
        ));
    }
    let completed = registered.map_err(DaemonError::from).and_then(|_| {
        if FaultInjection::fire(&plan.faults.fail_complete_job_once) {
            return Err(DaemonError::Internal(
                "injected complete_job failure".to_owned(),
            ));
        }
        Ok(catalog.complete_job(job_id, &utc_now_iso8601())?)
    });
    match completed {
        Ok(completed) => {
            let _ = plan.pin_journal.unpin(job_id);
            let _ = remove_job_params(&plan.data_dir, job_id);
            Ok(SaveOutcomeResponse {
                job: save_job_to_view(&completed),
                bundle_id: Some(materialized.bundle_id),
                bundle_path: Some(dest_path),
                tail_may_be_incomplete: params.tail_may_be_incomplete,
                unanchored_summary: Some(materialized.unanchored_summary),
                warning: materialized.warning,
            })
        }
        Err(err) => fail_job(
            catalog,
            job_id,
            &format!(
                "bundle `{}` was written to `{}` but cataloging failed (retry re-registers it): {err}",
                materialized.bundle_id,
                dest_path.display()
            ),
            Some(&params),
            materialized.warning,
        ),
    }
}

fn fail_job(
    catalog: &Catalog,
    job_id: &str,
    message: &str,
    params: Option<&SaveJobMaterializeParams>,
    warning: Option<String>,
) -> Result<SaveOutcomeResponse, DaemonError> {
    let failed = catalog.mark_failed_retryable(job_id, message, &utc_now_iso8601())?;
    Ok(SaveOutcomeResponse {
        job: save_job_to_view(&failed),
        bundle_id: None,
        bundle_path: None,
        tail_may_be_incomplete: params.is_some_and(|p| p.tail_may_be_incomplete),
        unanchored_summary: None,
        warning,
    })
}

fn materialize_from_chunks(
    plan: &Phase2Plan,
    job_id: &str,
    dest_bundle_dir: &Path,
    params: &SaveJobMaterializeParams,
) -> Result<Materialized, DaemonError> {
    let mut replayed = ReplayCollector::new(params);
    for path in &plan.chunk_paths {
        let bytes = fs::read(path)?;
        for event in decode_ring_chunk(&bytes)? {
            replayed.ingest_one(event);
        }
    }
    let draft = replayed.build_draft(job_id, params, &plan.data_dir)?;
    let unanchored_summary = draft.unanchored_summary;
    let bundle_id = draft.bundle_id.clone();

    // Write atomically via V2BundleWriter (AtomicStagingDir cleans up on error).
    let written = V2BundleWriter::write(dest_bundle_dir, job_id, &draft)
        .and_then(|_| plan.faults.maybe_fail_parent_sync(dest_bundle_dir));
    let warning = match written {
        Ok(()) => None,
        // The rename succeeded: the bundle is published. Never fail/discard it.
        Err(err) if err.committed_bundle_path_on_parent_sync_error().is_some() => Some(format!(
            "bundle committed but its parent directory could not be fsynced; it may need to be re-saved after a power loss: {err}"
        )),
        Err(err) => return Err(err.into()),
    };
    Ok(Materialized {
        bundle_id,
        unanchored_summary,
        warning,
    })
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

/// Video frames selected for the main timeline plus the uncertain frames left out (R3 S2).
struct SelectedVideo {
    main: Vec<AlignedVideoItem>,
    unanchored_excluded: usize,
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
            discontinuities: params.extra_discontinuities.clone(),
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

    /// Consumes one event (payloads are moved, never cloned: R3 M2 memory bound).
    fn ingest_one(&mut self, event: CaptureEvent) {
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
                self.record_epoch_boottime(&boot_id, payload.boottime_ns);
                let _ = self
                    .timeline
                    .ingest_device_triple(triple.clone(), pc_cont_ns);
                self.clock_anchors
                    .push(ClockAnchorRecord::DeviceTriple(triple));
            }
            CaptureEvent::SyncProbe { probe, .. } => {
                self.record_epoch_boottime(&probe.boot_id, probe.dev_boottime_ns);
                let _ = self.timeline.ingest_pc_sync_probe(probe.clone());
                self.clock_anchors
                    .push(ClockAnchorRecord::PcSyncProbe(probe));
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
                    self.video_config = Some(packet.data);
                } else {
                    let pts_mono_ns = packet.pts_monotonic_ns().unwrap_or(0);
                    let is_keyframe = packet.is_keyframe();
                    self.raw_video.push(RawCapturedVideo {
                        boot_id,
                        pts_mono_ns,
                        pc_ingest_cont_ns: pc_cont_ns,
                        is_keyframe,
                        annex_b: packet.data,
                    });
                }
            }
            CaptureEvent::AudioFrame { .. } => self.audio_frames_count += 1,
            CaptureEvent::AudioUnavailable { .. } => self.audio_unavailable = true,
            other => self.ingest_non_media_event(other),
        }
    }

    fn ingest_non_media_event(&mut self, event: CaptureEvent) {
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
                    boot_id,
                    header_epoch_ns: payload.header_epoch_ns(),
                    device_read_boottime_ns: Some(payload.device_read_boottime_ns),
                    pc_ingest_cont_ns: pc_cont_ns,
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
                let norm = normalized.or_else(|| self.touch_normalizer.ingest(&payload, viewport));
                if let Some(t) = norm {
                    self.raw_taps.push(RawCapturedTap {
                        boot_id,
                        event_mono_ns: t.device_mono_ns,
                        pc_ingest_cont_ns: pc_cont_ns,
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
                self.discontinuities.push(disc);
            }
            _ => {}
        }
    }

    fn select_video(
        timeline: &ClockTimeline,
        raw_video: Vec<RawCapturedVideo>,
        params: &SaveJobMaterializeParams,
    ) -> SelectedVideo {
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
        // R3 S2: frames with uncertain time cannot be muxed into the frame-indexed main track,
        // but they are counted (including those dropped by the quarantine caps), never lost
        // silently.
        let unanchored_excluded = part
            .unanchored
            .len()
            .saturating_add(part.unanchored_truncated_count);
        let main = if let Some(first_kf) = part.main.iter().position(|v| v.record.is_keyframe) {
            part.main.into_iter().skip(first_kf).collect()
        } else {
            part.main
        };
        SelectedVideo {
            main,
            unanchored_excluded,
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
        let mut discontinuities = self.discontinuities;
        if selected_video.unanchored_excluded > 0 {
            discontinuities.push(DiscontinuityEvent {
                kind: DiscontinuityKind::StreamGap,
                stream: Some(StreamKind::Video),
                boot_id: Some(params.window.boot_id().to_owned()),
                start_boottime_ns: None,
                end_boottime_ns: None,
                pc_cont_start_ns: params.window.ingest_start_cont_ns(),
                pc_cont_end_ns: Some(params.window.ingest_end_cont_ns()),
                delta_ns: None,
                detail: Some(format!(
                    "{} unanchored video frames (uncertain time or other boot_id) excluded from the main track",
                    selected_video.unanchored_excluded
                )),
            });
        }
        let (video_mp4, final_frames, video_status) = mux_selected_video(
            job_id,
            data_dir,
            self.video_width,
            self.video_height,
            self.video_config.as_deref(),
            selected_video.main,
        )?;
        let (final_logs, unanchored_logs, trunc_logs) =
            Self::select_logs(&self.timeline, self.raw_logs, params);
        let (final_taps, unanchored_taps, trunc_taps) =
            Self::select_taps(&self.timeline, self.raw_taps, params);

        let unanchored_summary = UnanchoredSummary {
            included_count: u64::try_from(unanchored_logs.saturating_add(unanchored_taps))
                .unwrap_or(u64::MAX),
            truncated_count: u64::try_from(
                trunc_logs
                    .saturating_add(trunc_taps)
                    .saturating_add(selected_video.unanchored_excluded),
            )
            .unwrap_or(u64::MAX),
        };

        let frame_count = u64::try_from(final_frames.len()).unwrap_or(0);
        let streams = build_streams_summary(&StreamSummaryCounts {
            frame_count,
            video_status,
            audio_frames_count: self.audio_frames_count,
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
            discontinuities,
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
            } else {
                // `arbd` does not mux AAC frames into `audio.m4a` yet (no audio muxer in
                // `arb-media`), and `V2BundleReader::verify_integrity` (R2 S15) requires
                // `audio.m4a` whenever audio is `Present`. Frames that were captured but not
                // persisted are therefore reported honestly as `Partial`, never `Present`.
                StreamStatus::Partial
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

/// Removes a temporary file on every exit path (R3 M3).
struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
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
    let _tmp_guard = TempFileGuard(tmp_mp4.clone());

    match mux_raw_h264_to_mp4(
        &tmp_mp4,
        width.max(16),
        height.max(16),
        MP4_TIMESCALE,
        MP4_DEFAULT_FRAME_DURATION_TICKS,
        &packets,
    ) {
        Ok(_report) => {
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
            Ok((
                Some(MediaPayload::Bytes(mp4_bytes)),
                frames,
                StreamStatus::Present,
            ))
        }
        // Only an elementary stream without parameter sets / slices (synthetic stub streams)
        // falls back to timeline-only `Partial` video. Every I/O or unexpected mux failure is
        // propagated so Phase 2 ends in `FailedRetryable` with the pins kept (R3 M3).
        Err(MediaError::InvalidContainer(_)) => {
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
        Err(other) => Err(other.into()),
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

/// Writes the job parameters atomically (tmp + `fsync` + rename + directory `fsync`, R3 S3),
/// so a crash can never leave a torn, un-materializable params file.
fn write_job_params(
    data_dir: &Path,
    job_id: &str,
    params: &SaveJobMaterializeParams,
) -> Result<(), DaemonError> {
    let path = job_params_path(data_dir, job_id);
    let parent = path
        .parent()
        .ok_or_else(|| DaemonError::Internal("params path has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(params)?;
    let tmp = parent.join(format!(".{job_id}.params.json.tmp"));
    let written = (|| -> Result<(), DaemonError> {
        let mut file = File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &path)?;
        fsync_dir(parent)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use arb_adb::mock::CaptureEvent;
    use arb_adb::scrcpy::{VideoPacket, VideoPacketKind};
    use arb_core::bundle::{FrameRecord, StreamStatus};
    use arb_core::state::RingBudgetPolicy;
    use arb_core::time::{DiscontinuityKind, TimeAlignment, UncertaintyReason};
    use arb_media::MediaError;
    use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
    use arb_os::OsError;
    use arb_os::fs::DiskSpaceInfo;
    use arb_storage::ring::DiskSpaceProvider;
    use tempfile::TempDir;

    use super::{
        AlignedVideoItem, CapturePipeline, SharedDiskSpaceProvider, decode_ring_chunk,
        encode_ring_chunk, mux_selected_video,
    };
    use crate::error::DaemonError;

    #[derive(Debug)]
    struct PlentyOfDisk;

    impl DiskSpaceProvider for PlentyOfDisk {
        fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
            Ok(DiskSpaceInfo {
                available_bytes: 1 << 40,
                total_bytes: 1 << 41,
            })
        }
    }

    fn video_event(pc_cont_ns: i64, data: Vec<u8>) -> CaptureEvent {
        CaptureEvent::VideoFrame {
            pc_cont_ns,
            boot_id: "boot-t".to_owned(),
            packet: VideoPacket {
                kind: VideoPacketKind::Keyframe,
                pts_us: 1_000,
                data,
            },
        }
    }

    fn aligned_items(packets: Vec<arb_media::mp4::RawH264Packet>) -> Vec<AlignedVideoItem> {
        packets
            .into_iter()
            .enumerate()
            .map(|(idx, pkt)| AlignedVideoItem {
                record: FrameRecord {
                    frame_index: u64::try_from(idx).expect("idx fits u64"),
                    boot_id: "boot-t".to_owned(),
                    pts_device_mono_ns: pkt.pts_ns,
                    pc_ingest_cont_ns: pkt.pts_ns,
                    alignment: TimeAlignment::Unanchored {
                        pc_ingest_cont_ns: pkt.pts_ns,
                        reason: UncertaintyReason::PcIngestOnly,
                    },
                    is_keyframe: pkt.is_keyframe,
                    gop_id: 0,
                    mp4_sample_offset: 0,
                    mp4_sample_size: 0,
                },
                annex_b: pkt.annex_b,
            })
            .collect()
    }

    fn real_h264_items() -> Vec<AlignedVideoItem> {
        let frames: Vec<DecodedFrame> = (0..4_u64)
            .map(|i| DecodedFrame {
                frame_index: i,
                width: 64,
                height: 64,
                format: PixelFormat::Yuv420p,
                data: vec![u8::try_from(i * 40).expect("fits u8"); 64 * 64 * 3 / 2],
            })
            .collect();
        let packets = OpenH264Backend::new()
            .encode_yuv420p_sequence(&frames, 2, 33_333_333, EncodeParams::default())
            .expect("OpenH264 encode must succeed");
        aligned_items(packets)
    }

    #[test]
    fn r3_m2_compact_ring_chunk_is_binary_and_round_trips() {
        let blob: Vec<u8> = (0..256_u32)
            .cycle()
            .take(512 * 1024)
            .map(|v| u8::try_from(v).expect("fits u8"))
            .collect();
        let encoded =
            encode_ring_chunk(vec![video_event(7, blob.clone())]).expect("encode must succeed");
        assert!(
            encoded.len() < blob.len() + 4096,
            "binary payload must be stored raw, not as a ~3.6x JSON number array ({} bytes)",
            encoded.len()
        );
        let decoded = decode_ring_chunk(&encoded).expect("decode must succeed");
        match decoded.as_slice() {
            [CaptureEvent::VideoFrame { packet, .. }] => {
                assert_eq!(packet.data, blob, "payload must round-trip byte-exact");
            }
            other => panic!("unexpected decoded events: {other:?}"),
        }
    }

    #[test]
    fn r3_m2_legacy_json_ring_chunks_still_decode() {
        let legacy = serde_json::to_vec(&super::RingChunkBatch {
            events: vec![video_event(9, vec![1, 2, 3])],
        })
        .expect("legacy encode");
        let decoded = decode_ring_chunk(&legacy).expect("legacy chunk must decode");
        assert_eq!(decoded.len(), 1, "one legacy event expected");
        let mut truncated = encode_ring_chunk(vec![video_event(9, vec![4; 32])]).expect("enc");
        truncated.truncate(truncated.len() - 1);
        assert!(
            decode_ring_chunk(&truncated).is_err(),
            "a truncated compact chunk must be rejected, never silently shortened"
        );
    }

    #[test]
    fn r3_m3_mux_io_error_propagates_instead_of_partial_without_video() {
        let tmp = TempDir::new().expect("tempdir");
        // Occupy the temporary MP4 path with a directory: muxing a *valid* stream must hit an
        // I/O error, which must fail the save (FailedRetryable), never become "no video".
        std::fs::create_dir_all(tmp.path().join("tmp").join("mux-job-m3.mp4"))
            .expect("create blocking dir");
        let err = mux_selected_video("job-m3", tmp.path(), 64, 64, None, real_h264_items())
            .expect_err("I/O mux failure must propagate");
        assert!(
            matches!(err, DaemonError::Media(MediaError::Io(_))),
            "expected MediaError::Io, got {err:?}"
        );
    }

    #[test]
    fn r3_m3_stub_stream_without_parameter_sets_falls_back_to_partial() {
        let tmp = TempDir::new().expect("tempdir");
        let stub = aligned_items(vec![arb_media::mp4::RawH264Packet {
            annex_b: vec![0, 0, 0, 1, 0x09, 0xF0],
            pts_ns: 1,
            is_keyframe: true,
        }]);
        let (mp4, frames, status) = mux_selected_video("job-stub", tmp.path(), 64, 64, None, stub)
            .expect("stub stream must fall back cleanly");
        assert!(mp4.is_none(), "stub stream must not produce an MP4");
        assert_eq!(frames.len(), 1, "timeline metadata must be kept");
        assert_eq!(status, StreamStatus::Partial, "stub video must be Partial");
        assert!(
            !tmp.path().join("tmp").join("mux-job-stub.mp4").exists(),
            "temporary MP4 must be cleaned up"
        );
    }

    #[test]
    fn r3_s5_ring_write_io_error_records_stream_gap_instead_of_silent_loss() {
        let tmp = TempDir::new().expect("tempdir");
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .expect("open pipeline");
        let _ = pipeline.begin_session("serial-s5");
        // Replace the ring directory with a regular file so the chunk write fails with I/O.
        let ring_dir = tmp.path().join("ring");
        std::fs::remove_dir_all(&ring_dir).expect("remove ring dir");
        std::fs::write(&ring_dir, b"not a directory").expect("block ring dir");

        pipeline
            .ingest_event(CaptureEvent::StaticHold {
                pc_cont_ns: 5,
                boot_id: "boot-t".to_owned(),
                silent_duration_ns: 1,
            })
            .expect("buffered ingest");
        assert!(
            pipeline.flush_pending_chunk().is_err(),
            "the failed write must still be reported to the caller"
        );
        assert_eq!(
            pipeline.ring_write_errors(),
            1,
            "write error must be counted"
        );
        assert!(
            pipeline.discontinuities().iter().any(|d| {
                d.kind == DiscontinuityKind::StreamGap
                    && d.detail
                        .as_deref()
                        .is_some_and(|s| s.contains("1 captured events lost"))
            }),
            "lost batch must be recorded as a StreamGap: {:?}",
            pipeline.discontinuities()
        );
        assert!(
            pipeline.is_active(),
            "a ring write error must not halt capture"
        );
    }

    #[test]
    fn r3_s3_pending_save_drop_rolls_back_pin_journal() {
        let tmp = TempDir::new().expect("tempdir");
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .expect("open pipeline");
        let _ = pipeline.begin_session("serial-s3");
        pipeline
            .ingest_event(video_event(1_000_000_000, vec![0, 0, 0, 1, 0x65]))
            .expect("ingest");
        pipeline.flush_pending_chunk().expect("flush");

        let pending = pipeline
            .phase1_pin(
                30,
                &tmp.path().join("out"),
                arb_core::selection::SelectionPolicy::default(),
            )
            .expect("phase1_pin must succeed");
        let job_id = pending.job_id().to_owned();
        assert!(
            !pipeline
                .ring_store()
                .pinned_chunk_ids()
                .expect("pins")
                .is_empty(),
            "chunks must be pinned while PendingSave is live"
        );
        drop(pending);
        assert!(
            pipeline
                .ring_store()
                .pinned_chunk_ids()
                .expect("pins")
                .is_empty(),
            "dropping PendingSave without commit must roll back pins"
        );
        assert!(
            pipeline.ring_store().pin_journal().load(&job_id).is_err(),
            "rolled-back pin journal file must be removed"
        );
    }

    #[test]
    fn r3_s4_retarget_non_retryable_job_leaves_pin_journal_untouched() {
        let tmp = TempDir::new().expect("tempdir");
        let catalog = arb_storage::catalog::Catalog::open(
            tmp.path().join("catalog.sqlite"),
            tmp.path().join("bundles"),
        )
        .expect("open catalog");
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .expect("open pipeline");
        let _ = pipeline.begin_session("serial-s4");
        pipeline
            .ingest_event(video_event(1_000_000_000, vec![0, 0, 0, 1, 0x65]))
            .expect("ingest");
        pipeline.flush_pending_chunk().expect("flush");

        let orig_dest = tmp.path().join("orig");
        let (job, _plan) = pipeline
            .phase1_pin_save(
                &catalog,
                30,
                &orig_dest,
                arb_core::selection::SelectionPolicy::default(),
                super::TailCompleteness::Complete,
            )
            .expect("phase1_pin_save must succeed");
        let job_id = &job.job.job_id;
        let before_dest = pipeline
            .ring_store()
            .pin_journal()
            .load(job_id)
            .expect("pin journal exists")
            .destination_path;

        // Job is still `Pinned` (not `FailedRetryable`): retarget must fail with Conflict
        // WITHOUT mutating the pin journal destination.
        let err = pipeline
            .prepare_retarget(&catalog, job_id, &tmp.path().join("new-dest"))
            .expect_err("retargeting a Pinned job must fail");
        assert!(
            matches!(err, DaemonError::Conflict(_)),
            "expected Conflict, got {err:?}"
        );
        let after_dest = pipeline
            .ring_store()
            .pin_journal()
            .load(job_id)
            .expect("pin journal still exists")
            .destination_path;
        assert_eq!(
            after_dest, before_dest,
            "pin journal destination must remain unchanged when catalog validation fails"
        );
    }
}
