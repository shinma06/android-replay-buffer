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
use std::sync::mpsc::Sender;

use arb_adb::mock::{CaptureEvent, NormalizedTouchEvent, TouchAction};
use arb_adb::scrcpy::{
    AudioTunnelStatus, VideoPacketKind, audio_unavailable_reason_for_api,
    unsupported_android_api_warning,
};
use arb_adb::wire::{
    LogRecordPayload, StreamId, StreamLifecycleState, TouchEventPayload, UNKNOWN_U32,
};
use arb_core::analytics::{ParsedAmCrash, decode_binary_android_event, parse_am_crash_binary};
use arb_core::bundle::{
    AudioStreamMeta, AvOffsetStatus, BundleKind, DeviceInfo, EpochSpan, FrameRecord,
    InputStreamMeta, LogCompleteness, LogRecord, LogcatStreamMeta, Provenance, RecordBucket,
    RequestedWindow, StreamStatus, StreamsSummary, TapRecord, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::config::CaptureMode;
use arb_core::gesture::{GestureKind, TouchPoint, classify_stroke, touch_slop};
use arb_core::selection::{SelectionPolicy, WindowSpec, select_records};
use arb_core::state::{RingBudgetPolicy, SaveJobState, SaveTriggerKind};
use arb_core::time::{
    ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind,
    StreamKind, TimeAlignment, TimestampSemantics,
};
use arb_media::MediaError;
use arb_media::audio::{
    AacMuxInput, AacPacket, build_aac_lc_audio_specific_config, mux_aac_to_m4a,
    parse_audio_specific_config,
};
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
use crate::protocol::{
    AudioCaptureState, CaptureStatusView, SaveJobView, SaveMode, SaveOutcomeResponse,
};
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

/// Notification emitted by [`CapturePipeline::ingest_event`] when a live fatal `am_crash` arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashAutoSaveNotice {
    /// Capture generation that owns this crash.
    pub generation: u64,
    /// Device boot epoch of the triggering log.
    pub boot_id: String,
    /// Trigger timestamp in the device boottime domain.
    pub boottime_ns: i64,
    /// Host continuous monotonic timestamp associated with the crash event.
    pub pc_cont_ns: i64,
    /// Crashing Android process name.
    pub process_name: String,
    /// Exception class reported by `am_crash`.
    pub exception_class: String,
    /// Whether `STORAGE_PRESSURE_STOP` was active when the crash arrived.
    pub storage_pressure_stopped: bool,
}

/// Durable Phase-1 parameters saved alongside `<data_dir>/save_jobs/<job-id>.json`
/// so `save.retry` / `save.retarget` can rematerialize identically even across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SaveJobMaterializeParams {
    bundle_id: String,
    #[serde(default)]
    mode: SaveMode,
    duration_sec: u64,
    trigger_utc: String,
    window: WindowSpec,
    policy: SelectionPolicy,
    tail_may_be_incomplete: bool,
    device: DeviceInfo,
    video_width: u16,
    video_height: u16,
    /// Physical device display width in pixels (from `wm size`), if observed.
    #[serde(default)]
    display_width: Option<u32>,
    /// Physical device display height in pixels (from `wm size`), if observed.
    #[serde(default)]
    display_height: Option<u32>,
    fallback_video_config: Option<Vec<u8>>,
    /// Discontinuities that never reached a ring chunk (`StoragePressureStop`, failed ring
    /// writes) and overlap the save window. They are merged into the saved bundle.
    #[serde(default)]
    extra_discontinuities: Vec<DiscontinuityEvent>,
    /// Best-effort UID -> Android package name map captured from `cmd package list packages -U`.
    #[serde(default)]
    uid_packages: BTreeMap<u32, String>,
}

/// What a closed ring chunk contains, used to pick the nearest preceding anchor chunks.
#[derive(Debug, Default, Clone, Copy)]
struct ChunkContent {
    picture: Option<VideoPacketKind>,
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
                    content.picture = Some(VideoPacketKind::Keyframe);
                }
                CaptureEvent::VideoFrame { .. } => {
                    content.picture.get_or_insert(VideoPacketKind::DeltaFrame);
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
    generation: u64,
    params: SaveJobMaterializeParams,
    dest_bundle_dir: PathBuf,
    trigger_kind: SaveTriggerKind,
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
    ) -> Vec<NormalizedTouchEvent> {
        match (payload.ev_type, payload.ev_code) {
            (EV_ABS, ABS_MT_SLOT) => {
                self.current_slot = payload.ev_value;
                Vec::new()
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
                Vec::new()
            }
            (EV_ABS, ABS_MT_POSITION_X | ABS_X) => {
                let slot = self.slots.entry(self.current_slot).or_default();
                slot.x = Some(payload.ev_value);
                if slot.active && slot.pending_action.is_none() {
                    slot.pending_action = Some(TouchAction::Move);
                }
                Vec::new()
            }
            (EV_ABS, ABS_MT_POSITION_Y | ABS_Y) => {
                let slot = self.slots.entry(self.current_slot).or_default();
                slot.y = Some(payload.ev_value);
                if slot.active && slot.pending_action.is_none() {
                    slot.pending_action = Some(TouchAction::Move);
                }
                Vec::new()
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
                Vec::new()
            }
            (EV_SYN, SYN_REPORT) => {
                // Linux MT Protocol B reports updates for multiple slots in a single SYN_REPORT
                // packet; drain every slot with a pending transition in deterministic slot order.
                let mut emitted = Vec::new();
                for (&slot_id, slot) in &mut self.slots {
                    if let Some(action) = slot.pending_action.take() {
                        emitted.push(NormalizedTouchEvent {
                            device_mono_ns: payload.event_monotonic_ns(),
                            device_read_boottime_ns: payload.device_read_boottime_ns,
                            action,
                            pointer_id: slot_id,
                            x: slot.x.unwrap_or(0),
                            y: slot.y.unwrap_or(0),
                            viewport,
                        });
                    }
                }
                emitted
            }
            _ => Vec::new(),
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
    capture_mode: CaptureMode,
    last_video_error: Option<String>,
    audio_state: AudioCaptureState,
    audio_reason: Option<String>,
    device_api_warning: Option<String>,
    clock_domain: ClockDomain,
    session_generation: u64,
    serial: Option<String>,
    device: DeviceInfo,
    video_width: u16,
    video_height: u16,
    display_width: Option<u32>,
    display_height: Option<u32>,
    uid_packages: BTreeMap<u32, String>,
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
    session_first_event_cont_ns: Option<i64>,
    latest_heartbeat_boottime_ns: Option<i64>,
    latest_heartbeat_pc_cont_ns: Option<i64>,
    latest_pc_cont_ns: Option<i64>,
    last_chunk_created_cont_ns: Option<i64>,
    last_discontinuities: Vec<DiscontinuityEvent>,
    pressure_discontinuities: Vec<DiscontinuityEvent>,
    chunk_contents: BTreeMap<String, ChunkContent>,
    crash_notifier: Option<Sender<CrashAutoSaveNotice>>,
    pub(super) crash_daemon: std::sync::Weak<super::DaemonState>,
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
        let last_chunk_created_cont_ns = ring.chunks().last().map(|c| c.created_cont_ns);
        Ok(Self {
            data_dir,
            ring,
            faults: Arc::new(FaultInjection::default()),
            active: false,
            connected: false,
            storage_pressure_stopped: false,
            capture_mode: CaptureMode::VideoAndLogs,
            last_video_error: None,
            audio_state: AudioCaptureState::Unknown,
            audio_reason: None,
            device_api_warning: None,
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
            display_width: None,
            display_height: None,
            uid_packages: BTreeMap::new(),
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
            session_first_event_cont_ns: None,
            latest_heartbeat_boottime_ns: None,
            latest_heartbeat_pc_cont_ns: None,
            latest_pc_cont_ns: None,
            last_chunk_created_cont_ns,
            last_discontinuities: Vec::new(),
            pressure_discontinuities: Vec::new(),
            chunk_contents: BTreeMap::new(),
            crash_notifier: None,
            crash_daemon: std::sync::Weak::new(),
        })
    }

    /// Installs the shared [`FaultInjection`] switches (tests only; defaults to all-off).
    pub fn set_fault_injection(&mut self, faults: Arc<FaultInjection>) {
        self.faults = faults;
    }

    /// Installs or clears the channel receiving live fatal `am_crash` notifications.
    pub fn set_crash_notifier(&mut self, notifier: Option<Sender<CrashAutoSaveNotice>>) {
        self.crash_notifier = notifier;
    }

    /// Updates the Zone-1 ring budget and free-disk safety floor policy live.
    pub const fn set_ring_policy(&mut self, policy: RingBudgetPolicy) {
        self.ring.set_policy(policy);
    }

    /// Returns the live free-disk safety floor (`min_free_disk_bytes`) of the ring policy.
    #[must_use]
    pub const fn min_free_disk_bytes(&self) -> u64 {
        self.ring.policy().min_free_disk_bytes
    }

    /// Returns the serial of the currently active capture session, if any.
    #[must_use]
    pub fn active_serial(&self) -> Option<String> {
        self.serial.clone()
    }

    /// Returns `true` while capture is halted due to `STORAGE_PRESSURE_STOP`.
    #[must_use]
    pub const fn is_storage_pressure_stopped(&self) -> bool {
        self.storage_pressure_stopped
    }

    /// Clears a latched `STORAGE_PRESSURE_STOP` when the user explicitly re-arms capture.
    pub const fn clear_storage_pressure_stop(&mut self) {
        self.storage_pressure_stopped = false;
    }

    /// Marks the start of a new (replayed / mock) capture session for `serial` whose event
    /// timestamps are in their own `pc_cont_ns` domain, and returns its session generation.
    pub fn begin_session(&mut self, serial: impl Into<String>) -> u64 {
        self.begin_session_inner(
            serial.into(),
            ClockDomain::Replayed,
            CaptureMode::VideoAndLogs,
        )
    }

    /// Marks the start of a new (replayed / mock) capture session with an explicit
    /// [`CaptureMode`], and returns its session generation.
    pub fn begin_session_with_mode(
        &mut self,
        serial: impl Into<String>,
        capture_mode: CaptureMode,
    ) -> u64 {
        self.begin_session_inner(serial.into(), ClockDomain::Replayed, capture_mode)
    }

    /// Marks the start of a live capture session for `serial` whose `pc_cont_ns` stamps come
    /// from the host continuous clock, and returns its session generation.
    ///
    /// For live sessions `T_trigger` is taken from the host clock at the save request, so a
    /// save made long after a disconnect ends at the request time, not at the last heartbeat.
    pub fn begin_live_session(&mut self, serial: impl Into<String>) -> u64 {
        self.begin_session_inner(
            serial.into(),
            ClockDomain::HostLive,
            CaptureMode::VideoAndLogs,
        )
    }

    /// Marks the start of a live capture session with an explicit [`CaptureMode`].
    pub fn begin_live_session_with_mode(
        &mut self,
        serial: impl Into<String>,
        capture_mode: CaptureMode,
    ) -> u64 {
        self.begin_session_inner(serial.into(), ClockDomain::HostLive, capture_mode)
    }

    /// Returns the [`CaptureMode`] of the current (or most recent) capture session.
    #[must_use]
    pub const fn capture_mode(&self) -> CaptureMode {
        self.capture_mode
    }

    /// Records the physical device display resolution (e.g., from `adb shell wm size`) for the
    /// current capture session so downstream viewers and contact sheets can map touch coordinates.
    pub const fn set_display_size(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.display_width = Some(width);
            self.display_height = Some(height);
        }
    }

    /// Generation-scoped variant of [`Self::set_display_size`] for live ADB supervisor threads.
    pub const fn set_display_size_for_session(&mut self, generation: u64, width: u32, height: u32) {
        if generation == self.session_generation {
            self.set_display_size(width, height);
        }
    }

    /// Stores the UID -> Android package name map for foreground per-app log extraction.
    pub fn set_package_uids(&mut self, uid_packages: BTreeMap<u32, String>) {
        self.uid_packages = uid_packages;
    }

    /// Generation-scoped variant of [`Self::set_package_uids`] for live ADB supervisor threads.
    pub fn set_package_uids_for_session(
        &mut self,
        generation: u64,
        uid_packages: BTreeMap<u32, String>,
    ) {
        if generation == self.session_generation {
            self.uid_packages = uid_packages;
        }
    }

    /// Records or clears the latest video pipeline diagnostic message for `system.status`.
    pub fn set_last_video_error_for_session(&mut self, generation: u64, error: Option<String>) {
        if generation == self.session_generation {
            self.last_video_error = error;
        }
    }

    /// Records the live audio capture availability state and optional diagnostic reason
    /// for `system.status`.
    pub fn set_audio_status_for_session(
        &mut self,
        generation: u64,
        state: AudioCaptureState,
        reason: Option<String>,
    ) {
        if generation == self.session_generation {
            self.audio_state = state;
            self.audio_reason = reason;
        }
    }

    /// Records or clears a non-blocking device Android API support-floor warning.
    pub fn set_device_api_warning_for_session(&mut self, generation: u64, warning: Option<String>) {
        if generation == self.session_generation {
            self.device_api_warning = warning;
        }
    }

    fn begin_session_inner(
        &mut self,
        serial: String,
        clock_domain: ClockDomain,
        capture_mode: CaptureMode,
    ) -> u64 {
        if self.serial.as_deref() != Some(&serial) {
            let _ = self.flush_pending_chunk();
            self.chunk_contents.clear();
            self.timeline = ClockTimeline::new(ClockTimelineConfig::new());
            self.latest_heartbeat_boottime_ns = None;
            self.latest_heartbeat_pc_cont_ns = None;
            self.latest_pc_cont_ns = None;
            self.last_discontinuities.clear();
            self.pressure_discontinuities.clear();
            self.video_width = 1920;
            self.video_height = 1080;
            self.device.model = "unknown".into();
            self.device.api_level = 0;
            self.device.is_emulator = false;
        }
        self.session_generation = self.session_generation.wrapping_add(1);
        self.active = true;
        self.connected = false;
        self.storage_pressure_stopped = false;
        self.capture_mode = capture_mode;
        self.last_video_error = None;
        self.audio_state = if capture_mode == CaptureMode::LogsOnly {
            AudioCaptureState::Disabled
        } else {
            AudioCaptureState::Unknown
        };
        self.audio_reason = None;
        self.device_api_warning = None;
        self.clock_domain = clock_domain;
        self.serial = Some(serial.clone());
        self.device.serial = serial;
        self.display_width = None;
        self.display_height = None;
        self.uid_packages.clear();
        self.session_first_event_cont_ns = None;
        self.session_generation
    }

    pub(super) fn adb_resource_dir(&self) -> PathBuf {
        self.data_dir.join("adb-sessions")
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
            capture_mode: self.capture_mode,
            last_video_error: self.last_video_error.clone(),
            audio_state: self.audio_state,
            audio_reason: self.audio_reason.clone(),
            device_api_warning: self.device_api_warning.clone(),
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

    /// Computes the honest elapsed duration currently retained in the Zone-1 ring buffer (in
    /// whole seconds), scoped to the active capture session when one is running and bounded by
    /// the configured ring duration cap.
    #[must_use]
    pub fn buffered_secs(&self) -> u64 {
        let chunks = self.ring.chunks();
        let session_floor = self.session_first_event_cont_ns;
        let mut oldest: Option<i64> = None;
        let mut newest: Option<i64> = None;

        for chunk in &chunks {
            if let Some(floor) = session_floor
                && chunk.created_cont_ns < floor
            {
                continue;
            }
            let start = chunk.created_cont_ns;
            let end = start.saturating_add(i64::try_from(chunk.duration_ns).unwrap_or(i64::MAX));
            oldest = Some(oldest.map_or(start, |prev| prev.min(start)));
            newest = Some(newest.map_or(end, |prev| prev.max(end)));
        }

        for event in &self.pending_events {
            if let Some(ts) = capture_event_pc_cont_ns(event) {
                if let Some(floor) = session_floor
                    && ts < floor
                {
                    continue;
                }
                oldest = Some(oldest.map_or(ts, |prev| prev.min(ts)));
                newest = Some(newest.map_or(ts, |prev| prev.max(ts)));
            }
        }

        let (Some(lo), Some(hi)) = (oldest, newest) else {
            return 0;
        };
        let span_ns = u64::try_from(hi.saturating_sub(lo).max(0)).unwrap_or(0);
        let max_duration_ns = self.ring.policy().max_ring_duration_ns;
        let bounded_ns = if max_duration_ns > 0 {
            span_ns.min(max_duration_ns)
        } else {
            span_ns
        };
        bounded_ns / 1_000_000_000
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
        let detected_crash = detect_crash_from_event(&event);
        if self.storage_pressure_stopped {
            self.notify_detected_crash(detected_crash);
            return Ok(());
        }

        let event_pc_ns = capture_event_pc_cont_ns(&event);
        if let Some(ts) = event_pc_ns {
            if self.session_first_event_cont_ns.is_none() {
                self.session_first_event_cont_ns = Some(ts);
            }
            self.latest_pc_cont_ns = Some(self.latest_pc_cont_ns.map_or(ts, |prev| prev.max(ts)));
        }

        let mut should_flush = detected_crash.is_some();
        let approx_bytes = estimate_event_bytes(&event);
        if self.apply_live_event_state(&event) {
            should_flush = true;
        }

        self.pending_bytes = self.pending_bytes.saturating_add(approx_bytes);
        self.pending_events.push(event);

        let flushed = if should_flush
            || self.pending_events.len() >= MAX_BATCH_EVENTS
            || self.pending_bytes >= MAX_BATCH_BYTES
        {
            self.flush_pending_chunk()
        } else {
            Ok(())
        };

        // Notify even when the flush failed: a crash must still be counted and may auto-save.
        self.notify_detected_crash(detected_crash);
        flushed
    }

    fn apply_live_event_state(&mut self, event: &CaptureEvent) -> bool {
        let mut should_flush = false;
        match event {
            CaptureEvent::Hello { payload, .. } => {
                self.connected = true;
                self.device.model.clone_from(&payload.model);
                self.device.api_level = payload.api_level;
                self.device.is_emulator = payload.is_emulator;
                if self.device_api_warning.is_none() {
                    self.device_api_warning = unsupported_android_api_warning(payload.api_level);
                }
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
                    should_flush = true;
                } else {
                    self.video_frames_seen += 1;
                }
            }
            CaptureEvent::AudioFrame { .. } => {
                self.audio_frames_seen += 1;
                self.audio_state = AudioCaptureState::Available;
                self.audio_reason = None;
            }
            CaptureEvent::AudioUnavailable { status, .. } => {
                self.audio_state = AudioCaptureState::Unavailable;
                if self.audio_reason.is_none() {
                    self.audio_reason = Some(default_audio_unavailable_reason(*status));
                }
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
        should_flush
    }

    fn notify_detected_crash(&mut self, detected: Option<(i64, String, i64, ParsedAmCrash)>) {
        let Some((pc_cont_ns, boot_id, boottime_ns, crash)) = detected else {
            return;
        };
        let notice = CrashAutoSaveNotice {
            generation: self.session_generation,
            boot_id,
            boottime_ns,
            pc_cont_ns,
            process_name: crash.process_name,
            exception_class: crash.exception_class,
            storage_pressure_stopped: self.storage_pressure_stopped,
        };
        if let Some(tx) = &self.crash_notifier {
            let _ = tx.send(notice.clone());
        }
        if let Some(state) = self.crash_daemon.upgrade() {
            super::pin_crash_notice(&state, self, &notice);
        }
    }

    pub(super) fn pin_crash(
        &mut self,
        notice: &CrashAutoSaveNotice,
        secs: u64,
        dest: &Path,
    ) -> Result<PendingSave, DaemonError> {
        if notice.generation != self.session_generation {
            return Err(DaemonError::Conflict(
                "crash belongs to an earlier capture session".into(),
            ));
        }
        let mut window = self.build_save_window(
            secs,
            TriggerPoint {
                pc_cont_ns: notice.pc_cont_ns,
                boottime_ns: notice.boottime_ns,
            },
        );
        if let WindowSpec::RealElapsed { boot_id, .. } = &mut window {
            boot_id.clone_from(&notice.boot_id);
        }
        let mode = if self.capture_mode == CaptureMode::LogsOnly {
            SaveMode::LogsOnly
        } else {
            SaveMode::Full
        };
        self.pin_window(
            secs,
            dest,
            SelectionPolicy::default(),
            mode,
            SaveTriggerKind::CrashAutoSave,
            window,
        )
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

        let raw_created_cont_ns = events
            .first()
            .and_then(capture_event_pc_cont_ns)
            .or(self.latest_pc_cont_ns)
            .unwrap_or(0);
        // Keep chunk `created_cont_ns` strictly increasing across flushes: concurrent
        // companion/video/audio reader threads sample `pc_cont_ns` before taking the
        // pipeline lock, so an earlier timestamp from another stream could otherwise
        // land first in the next chunk and cause `RingStore::chunks()` to replay chunks
        // — and H.264 frames — out of order.
        let created_cont_ns = match self.last_chunk_created_cont_ns {
            Some(prev) => raw_created_cont_ns.max(prev.saturating_add(1)),
            None => raw_created_cont_ns,
        };
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
                self.last_chunk_created_cont_ns = Some(created_cont_ns);
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
        self.phase1_pin_with_options(
            duration_sec,
            dest_parent_dir,
            policy,
            SaveMode::Full,
            SaveTriggerKind::Manual,
        )
    }

    /// Variant of [`Self::phase1_pin`] accepting an explicit [`SaveMode`] (`Full` or `LogsOnly`)
    /// and [`SaveTriggerKind`] (`Manual` or `CrashAutoSave`).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Conflict`] if no ring chunk overlaps the requested window, or
    /// storage errors if the pin journal cannot be written.
    pub fn phase1_pin_with_options(
        &mut self,
        duration_sec: u64,
        dest_parent_dir: &Path,
        policy: SelectionPolicy,
        mode: SaveMode,
        trigger_kind: SaveTriggerKind,
    ) -> Result<PendingSave, DaemonError> {
        let window = self.build_save_window(duration_sec, self.trigger_point());
        self.pin_window(
            duration_sec,
            dest_parent_dir,
            policy,
            mode,
            trigger_kind,
            window,
        )
    }

    fn pin_window(
        &mut self,
        duration_sec: u64,
        dest_parent_dir: &Path,
        policy: SelectionPolicy,
        mode: SaveMode,
        trigger_kind: SaveTriggerKind,
        window: WindowSpec,
    ) -> Result<PendingSave, DaemonError> {
        let _ = self.flush_pending_chunk();
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

        let extra_discontinuities = self.carried_discontinuities_for(&window, &policy);
        let params = SaveJobMaterializeParams {
            bundle_id,
            mode,
            duration_sec,
            trigger_utc: now_utc,
            window,
            policy,
            tail_may_be_incomplete: false,
            device: self.device.clone(),
            video_width: self.video_width,
            video_height: self.video_height,
            display_width: self.display_width,
            display_height: self.display_height,
            fallback_video_config: None,
            extra_discontinuities,
            uid_packages: self.uid_packages.clone(),
        };
        Ok(PendingSave {
            guard,
            generation: self.session_generation,
            params,
            dest_bundle_dir,
            trigger_kind,
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
        let mut chunk_ids = pending.pinned_chunk_ids.clone();
        if pending.generation == self.session_generation {
            let _ = self.flush_pending_chunk();
            for id in self.select_window_chunks(&pending.params.window, &pending.params.policy) {
                if !chunk_ids.contains(&id) {
                    chunk_ids.push(id);
                }
            }
        }
        let job_id = pending.guard.job_id.clone();
        if chunk_ids.len() != pending.pinned_chunk_ids.len() {
            let _ = self.ring.pin_chunks(
                &job_id,
                &pending.dest_bundle_dir,
                chunk_ids.clone(),
                &pending.params.trigger_utc,
            )?;
        }

        pending.params.tail_may_be_incomplete =
            tail.may_be_incomplete() || self.session_generation != pending.generation;
        write_job_params(&self.data_dir, &job_id, &pending.params)?;

        let created = catalog.create_pinned_job_with_trigger(
            &job_id,
            &pending.dest_bundle_dir,
            &chunk_ids,
            &pending.params.trigger_utc,
            pending.trigger_kind,
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
            self.chunk_contents.contains_key(&c.chunk_id) && c.created_cont_ns <= hi && end >= lo
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
            let wanted = (need_keyframe && content.picture.is_some())
                || (need_config && content.video_config)
                || (need_heartbeat && content.heartbeat);
            if wanted {
                need_keyframe &= content.picture != Some(VideoPacketKind::Keyframe);
                if !need_keyframe {
                    need_config &= !content.video_config;
                }
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
    audio_config: Option<Vec<u8>>,
    raw_audio: Vec<RawCapturedAudio>,
    audio_unavailable: bool,
    audio_gap: bool,
    raw_logs: Vec<RawCapturedLog>,
    raw_taps: Vec<RawCapturedTap>,
    discontinuities: Vec<DiscontinuityEvent>,
    log_completeness: LogCompleteness,
    log_overflow_events: u64,
    touch_normalizer: TouchNormalizer,
    /// In-progress touch strokes keyed by pointer id (T-6H item 2 gesture classifier), flushed
    /// into `raw_taps` on `UP`/`CANCEL` or, for any still-active stroke, in [`Self::build_draft`]
    /// (end-of-stream / disconnect / stop) so no gesture is ever silently lost.
    active_strokes: BTreeMap<i32, ActiveStroke>,
    /// Resolution-aware `Tap` vs `Swipe` touch slop, resolved once from `params.display_width`/
    /// `display_height` at construction (`arb_core::gesture::touch_slop`).
    touch_slop_px: f64,
    epochs_map: BTreeMap<String, (i64, i64)>,
    uid_packages: BTreeMap<u32, String>,
}

struct RawCapturedVideo {
    dimensions: (u16, u16),
    boot_id: String,
    pts_mono_ns: i64,
    pc_ingest_cont_ns: i64,
    is_keyframe: bool,
    annex_b: Vec<u8>,
}

struct RawCapturedAudio {
    boot_id: String,
    pts_us: u64,
    pc_ingest_cont_ns: i64,
    data: Vec<u8>,
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
    uid: Option<u32>,
    package_name: Option<String>,
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
    gesture_kind: GestureKind,
    end_x: Option<f64>,
    end_y: Option<f64>,
    end_t_offset_ns: Option<i64>,
    path: Vec<TouchPoint>,
}

/// Upper bound on raw samples retained in one in-progress [`ActiveStroke`] before decimation,
/// keeping memory bounded on long high-rate drags while always retaining start, peak excursion,
/// and latest endpoint.
const MAX_ACTIVE_STROKE_SAMPLES: usize = 2_048;

/// One in-progress `DOWN -> MOVE* -> [UP]` touch stroke, accumulated per pointer so a completed
/// gesture can be classified ONCE (T-6H item 2) instead of emitting one raw tap per sample.
struct ActiveStroke {
    pointer_id: i32,
    boot_id: String,
    /// Host `PcContinuousMono` ingestion time captured at the stroke's `DOWN` sample.
    pc_ingest_cont_ns: i64,
    /// Device `CLOCK_MONOTONIC` timestamp of the `DOWN` sample (kept for alignment, as before).
    down_event_mono_ns: Option<i64>,
    /// Device `CLOCK_BOOTTIME` of the `DOWN` sample; every later sample's `t_offset_ns` is
    /// relative to this (always present, unlike `device_mono_ns`).
    down_read_boottime_ns: i64,
    samples: Vec<TouchPoint>,
    /// Maximum squared excursion from `samples[0]` seen so far (for bounded-memory peak tracking).
    max_excursion_sq: f64,
}

impl ActiveStroke {
    fn push_sample(&mut self, t: &NormalizedTouchEvent) {
        let offset_ns = t
            .device_read_boottime_ns
            .saturating_sub(self.down_read_boottime_ns)
            .max(0);
        let pt = TouchPoint {
            x: f64::from(t.x),
            y: f64::from(t.y),
            t_offset_ns: offset_ns,
        };
        if let Some(start) = self.samples.first() {
            let dx = pt.x - start.x;
            let dy = pt.y - start.y;
            let dist_sq = dx * dx + dy * dy;
            if self.samples.len() < MAX_ACTIVE_STROKE_SAMPLES {
                if dist_sq > self.max_excursion_sq {
                    self.max_excursion_sq = dist_sq;
                }
                self.samples.push(pt);
            } else {
                // At capacity: keep `samples[0]` (start), reserve `len - 2` for the highest
                // excursion point seen (so swipe-and-return classification stays exact), and
                // keep `len - 1` as the latest endpoint.
                let last_idx = self.samples.len() - 1;
                let peak_idx = self.samples.len() - 2;
                if dist_sq > self.max_excursion_sq {
                    self.max_excursion_sq = dist_sq;
                    self.samples[peak_idx] = pt;
                }
                self.samples[last_idx] = pt;
            }
        } else {
            self.samples.push(pt);
        }
    }

    /// Classifies the accumulated samples into exactly one [`RawCapturedTap`] gesture row.
    fn finalize(self, slop: f64) -> Option<RawCapturedTap> {
        if self.samples.is_empty() {
            return None;
        }
        let classified = classify_stroke(&self.samples, slop);
        let start = self.samples[0];
        let (end_x, end_y) = match classified.kind {
            GestureKind::Swipe => (Some(classified.end_x), Some(classified.end_y)),
            _ => (None, None),
        };
        Some(RawCapturedTap {
            boot_id: self.boot_id,
            event_mono_ns: self.down_event_mono_ns,
            pc_ingest_cont_ns: self.pc_ingest_cont_ns,
            action: gesture_kind_action_label(classified.kind).to_owned(),
            x: Some(start.x),
            y: Some(start.y),
            pointer_id: Some(self.pointer_id),
            gesture_kind: classified.kind,
            end_x,
            end_y,
            end_t_offset_ns: Some(classified.end_t_offset_ns),
            path: classified.path,
        })
    }
}

const fn gesture_kind_action_label(kind: GestureKind) -> &'static str {
    match kind {
        GestureKind::Swipe => "SWIPE",
        _ => "TAP",
    }
}

/// Video frames selected for the main timeline plus the uncertain frames left out (R3 S2).
struct SelectedVideo {
    preroll: Vec<RawH264Packet>,
    main: Vec<AlignedVideoItem>,
    unanchored_excluded: usize,
}

/// Audio packets selected for the main timeline plus the uncertain packets left out (R3 S2).
struct SelectedAudio {
    main: Vec<AacPacket>,
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
            audio_config: None,
            raw_audio: Vec::new(),
            audio_unavailable: false,
            audio_gap: false,
            raw_logs: Vec::new(),
            raw_taps: Vec::new(),
            discontinuities: params.extra_discontinuities.clone(),
            log_completeness: LogCompleteness::Unverified,
            log_overflow_events: 0,
            touch_normalizer: TouchNormalizer::default(),
            active_strokes: BTreeMap::new(),
            touch_slop_px: touch_slop(
                params.display_width.map(f64::from),
                params.display_height.map(f64::from),
            ),
            epochs_map: BTreeMap::new(),
            uid_packages: params.uid_packages.clone(),
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
                    let mut annex_b = packet.data;
                    if is_keyframe {
                        // Parameter sets embedded in this IDR take effect immediately. Cache
                        // them for later IDRs on devices that omit periodic configuration.
                        let mut embedded = Vec::new();
                        for nal in arb_media::mp4::split_annexb_nals(&annex_b) {
                            if nal.first().is_some_and(|b| matches!(b & 31, 7 | 8)) {
                                embedded.extend_from_slice(&[0, 0, 0, 1]);
                                embedded.extend_from_slice(nal);
                            }
                        }
                        if !embedded.is_empty() {
                            self.video_config = Some(embedded);
                        } else if let Some(config) = self.video_config.as_ref() {
                            let mut combined = config.clone();
                            combined.append(&mut annex_b);
                            annex_b = combined;
                        }
                    }
                    self.raw_video.push(RawCapturedVideo {
                        dimensions: (self.video_width, self.video_height),
                        boot_id,
                        pts_mono_ns,
                        pc_ingest_cont_ns: pc_cont_ns,
                        is_keyframe,
                        annex_b,
                    });
                }
            }
            CaptureEvent::AudioConfig { packet, .. } => {
                self.audio_config = Some(packet.data);
            }
            CaptureEvent::AudioFrame {
                pc_cont_ns,
                boot_id,
                packet,
            } => {
                self.raw_audio.push(RawCapturedAudio {
                    boot_id,
                    pts_us: packet.pts_us,
                    pc_ingest_cont_ns: pc_cont_ns,
                    data: packet.data,
                });
            }
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
                let (tag, message) = decode_log_tag_and_message(&payload);
                let level = Some(log_priority_label(payload.priority).to_owned());
                let uid = (payload.uid != UNKNOWN_U32).then_some(payload.uid);
                let package_name = uid.and_then(|u| {
                    self.uid_packages
                        .get(&u)
                        .or_else(|| self.uid_packages.get(&(u % 100_000)))
                        .cloned()
                });
                self.raw_logs.push(RawCapturedLog {
                    boot_id,
                    header_epoch_ns: payload.header_epoch_ns(),
                    device_read_boottime_ns: Some(payload.device_read_boottime_ns),
                    pc_ingest_cont_ns: pc_cont_ns,
                    level,
                    tag: (!tag.is_empty()).then_some(tag),
                    pid: Some(payload.pid),
                    tid: Some(payload.tid),
                    uid,
                    package_name,
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
                if let Some(t) = normalized {
                    self.ingest_touch_sample(boot_id, pc_cont_ns, &t);
                } else {
                    for t in self.touch_normalizer.ingest(&payload, viewport) {
                        self.ingest_touch_sample(boot_id.clone(), pc_cont_ns, &t);
                    }
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

    /// Feeds one normalized touch sample into the per-pointer [`ActiveStroke`] accumulator
    /// (T-6H item 2). `DOWN` starts a stroke, `MOVE` appends to it, and `UP`/`CANCEL` classifies
    /// the completed stroke into exactly one [`RawCapturedTap`] gesture row.
    fn ingest_touch_sample(&mut self, boot_id: String, pc_cont_ns: i64, t: &NormalizedTouchEvent) {
        // Never merge touch samples across a device reboot (`boot_id` change): flush any stale
        // stroke for this pointer from the previous boot first.
        if self
            .active_strokes
            .get(&t.pointer_id)
            .is_some_and(|existing| existing.boot_id != boot_id)
            && let Some(stale) = self.active_strokes.remove(&t.pointer_id)
            && let Some(tap) = stale.finalize(self.touch_slop_px)
        {
            self.raw_taps.push(tap);
        }

        match t.action {
            TouchAction::Down => {
                // Defensive: a stray `DOWN` without a prior `UP` (e.g. a dropped event) flushes
                // the stale stroke first so no sample is ever silently discarded.
                if let Some(stale) = self.active_strokes.remove(&t.pointer_id)
                    && let Some(tap) = stale.finalize(self.touch_slop_px)
                {
                    self.raw_taps.push(tap);
                }
                let mut stroke = ActiveStroke {
                    pointer_id: t.pointer_id,
                    boot_id,
                    pc_ingest_cont_ns: pc_cont_ns,
                    down_event_mono_ns: t.device_mono_ns,
                    down_read_boottime_ns: t.device_read_boottime_ns,
                    samples: Vec::new(),
                    max_excursion_sq: 0.0,
                };
                stroke.push_sample(t);
                self.active_strokes.insert(t.pointer_id, stroke);
            }
            TouchAction::Move => {
                let stroke =
                    self.active_strokes
                        .entry(t.pointer_id)
                        .or_insert_with(|| ActiveStroke {
                            pointer_id: t.pointer_id,
                            boot_id,
                            pc_ingest_cont_ns: pc_cont_ns,
                            down_event_mono_ns: t.device_mono_ns,
                            down_read_boottime_ns: t.device_read_boottime_ns,
                            samples: Vec::new(),
                            max_excursion_sq: 0.0,
                        });
                stroke.push_sample(t);
            }
            TouchAction::Up | TouchAction::Cancel => {
                let mut stroke = self
                    .active_strokes
                    .remove(&t.pointer_id)
                    .unwrap_or_else(|| ActiveStroke {
                        pointer_id: t.pointer_id,
                        boot_id,
                        pc_ingest_cont_ns: pc_cont_ns,
                        down_event_mono_ns: t.device_mono_ns,
                        down_read_boottime_ns: t.device_read_boottime_ns,
                        samples: Vec::new(),
                        max_excursion_sq: 0.0,
                    });
                stroke.push_sample(t);
                if let Some(tap) = stroke.finalize(self.touch_slop_px) {
                    self.raw_taps.push(tap);
                }
            }
            _ => {}
        }
    }

    /// Flushes any touch strokes still open at end-of-stream (disconnect / stop) so an in-progress
    /// swipe is never silently lost. Pointer-id ordering keeps output deterministic.
    fn flush_active_strokes(&mut self) {
        let slop = self.touch_slop_px;
        for (_, stroke) in std::mem::take(&mut self.active_strokes) {
            if let Some(tap) = stroke.finalize(slop) {
                self.raw_taps.push(tap);
            }
        }
    }

    fn select_video(
        timeline: &ClockTimeline,
        raw_video: Vec<RawCapturedVideo>,
        params: &SaveJobMaterializeParams,
    ) -> SelectedVideo {
        let mut aligned: Vec<AlignedVideoItem> = raw_video
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
                    dimensions: raw.dimensions,
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
        let mut preroll = Vec::new();
        if let Some(first) = aligned.iter().position(|v| {
            arb_core::selection::classify_record(v, &params.window)
                == arb_core::selection::Membership::InRange
        }) && !aligned[first].record.is_keyframe
            && let Some(start) = aligned[..first].iter().rposition(|v| v.record.is_keyframe)
            && aligned[start..first].iter().all(|v| {
                v.record.boot_id == aligned[first].record.boot_id
                    && v.dimensions == aligned[first].dimensions
            })
        {
            preroll.extend(aligned[start..first].iter_mut().map(|v| RawH264Packet {
                pts_ns: v.record.pts_device_mono_ns,
                is_keyframe: v.record.is_keyframe,
                annex_b: std::mem::take(&mut v.annex_b),
            }));
        }
        let part = select_records(aligned, &params.window, &params.policy);
        // R3 S2: frames with uncertain time cannot be muxed into the frame-indexed main track,
        // but they are counted (including those dropped by the quarantine caps), never lost
        // silently.
        let unanchored_excluded = part
            .unanchored
            .len()
            .saturating_add(part.unanchored_truncated_count);
        SelectedVideo {
            main: part.main,
            preroll,
            unanchored_excluded,
        }
    }

    fn select_audio(
        timeline: &ClockTimeline,
        raw_audio: Vec<RawCapturedAudio>,
        params: &SaveJobMaterializeParams,
    ) -> SelectedAudio {
        let aligned: Vec<AlignedAudioItem> = raw_audio
            .into_iter()
            .map(|raw| {
                let pts_mono_ns = raw
                    .pts_us
                    .checked_mul(1_000)
                    .and_then(|ns| i64::try_from(ns).ok())
                    .unwrap_or(0);
                let alignment = timeline.map_device_monotonic(
                    &raw.boot_id,
                    pts_mono_ns,
                    TimestampSemantics::AudioCapturePtsUncalibrated,
                    raw.pc_ingest_cont_ns,
                );
                AlignedAudioItem {
                    packet: AacPacket {
                        pts_us: i64::try_from(raw.pts_us).unwrap_or(i64::MAX),
                        data: raw.data,
                    },
                    alignment,
                    pc_ingest_cont_ns: raw.pc_ingest_cont_ns,
                }
            })
            .collect();
        let part = select_records(aligned, &params.window, &params.policy);
        // R3 S2 applied to audio: packets with uncertain time cannot be muxed into the
        // timeline-indexed `audio.m4a`, but they are counted, never lost silently.
        let unanchored_excluded = part
            .unanchored
            .len()
            .saturating_add(part.unanchored_truncated_count);
        SelectedAudio {
            main: part.main.into_iter().map(|a| a.packet).collect(),
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
                    uid: raw.uid,
                    package_name: raw.package_name,
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
                    gesture_kind: raw.gesture_kind,
                    end_x: raw.end_x,
                    end_y: raw.end_y,
                    end_t_offset_ns: raw.end_t_offset_ns,
                    path: raw.path,
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
        mut self,
        job_id: &str,
        params: &SaveJobMaterializeParams,
        data_dir: &Path,
    ) -> Result<V2BundleDraft, DaemonError> {
        // End-of-stream: classify any touch stroke still open (disconnect / stop mid-gesture) so
        // an in-progress swipe is never silently dropped (T-6H item 2).
        self.flush_active_strokes();
        let audio_continuity = self.audio_continuity();
        let mut discontinuities = self.discontinuities;
        let (final_logs, unanchored_logs, trunc_logs) =
            Self::select_logs(&self.timeline, self.raw_logs, params);

        let media = if params.mode == SaveMode::LogsOnly {
            SelectedMediaTracks::logs_only()
        } else {
            build_full_media_tracks(
                &self.timeline,
                job_id,
                params,
                data_dir,
                self.video_width,
                self.video_height,
                self.audio_config.as_deref(),
                audio_continuity,
                self.raw_video,
                self.raw_audio,
                self.raw_taps,
                &mut discontinuities,
            )?
        };

        let unanchored_summary = UnanchoredSummary {
            included_count: u64::try_from(unanchored_logs.saturating_add(media.unanchored_taps))
                .unwrap_or(u64::MAX),
            truncated_count: u64::try_from(
                trunc_logs
                    .saturating_add(media.trunc_taps)
                    .saturating_add(media.video_unanchored_excluded),
            )
            .unwrap_or(u64::MAX),
        };

        let frame_count = u64::try_from(media.frames.len()).unwrap_or(0);
        let (display_width, display_height) = if params.mode == SaveMode::LogsOnly {
            (None, None)
        } else {
            (params.display_width, params.display_height)
        };
        let streams = build_streams_summary(&StreamSummaryCounts {
            frame_count,
            video_status: media.video_status,
            display_width,
            display_height,
            audio_status: media.audio_status,
            logs_count: final_logs.len(),
            log_completeness: self.log_completeness,
            log_overflow_events: self.log_overflow_events,
            taps_count: media.taps.len(),
            tail_may_be_incomplete: params.tail_may_be_incomplete,
            av_lead_in_offset_ns: media.av_lead_in_offset_ns,
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
            bundle_kind: media.bundle_kind,
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
            video_mp4: media.video_mp4,
            audio_m4a: media.audio_m4a,
            share_mp4: None,
            frames: media.frames,
            discontinuities,
            logs: final_logs,
            taps: media.taps,
            clock_anchors: self.clock_anchors,
        })
    }
}

struct SelectedMediaTracks {
    bundle_kind: BundleKind,
    video_mp4: Option<MediaPayload>,
    audio_m4a: Option<MediaPayload>,
    frames: Vec<FrameRecord>,
    taps: Vec<TapRecord>,
    video_status: StreamStatus,
    audio_status: StreamStatus,
    av_lead_in_offset_ns: i64,
    unanchored_taps: usize,
    trunc_taps: usize,
    video_unanchored_excluded: usize,
}

impl SelectedMediaTracks {
    const fn logs_only() -> Self {
        Self {
            bundle_kind: BundleKind::LogsOnly,
            video_mp4: None,
            audio_m4a: None,
            frames: Vec::new(),
            taps: Vec::new(),
            video_status: StreamStatus::Absent,
            audio_status: StreamStatus::Absent,
            av_lead_in_offset_ns: 0,
            unanchored_taps: 0,
            trunc_taps: 0,
            video_unanchored_excluded: 0,
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "assembles independently captured video, audio, and tap streams into a full draft"
)]
fn build_full_media_tracks(
    timeline: &ClockTimeline,
    job_id: &str,
    params: &SaveJobMaterializeParams,
    data_dir: &Path,
    video_width: u16,
    video_height: u16,
    audio_config: Option<&[u8]>,
    audio_continuity: AudioContinuity,
    raw_video: Vec<RawCapturedVideo>,
    raw_audio: Vec<RawCapturedAudio>,
    raw_taps: Vec<RawCapturedTap>,
    discontinuities: &mut Vec<DiscontinuityEvent>,
) -> Result<SelectedMediaTracks, DaemonError> {
    let selected_video = ReplayCollector::select_video(timeline, raw_video, params);
    let selected_audio = ReplayCollector::select_audio(timeline, raw_audio, params);
    let av_lead_in_offset_ns = av_lead_in_offset_ns(&selected_video, &selected_audio);
    for (kind, label, excluded) in [
        (
            StreamKind::Video,
            "video frames",
            selected_video.unanchored_excluded,
        ),
        (
            StreamKind::Audio,
            "audio packets",
            selected_audio.unanchored_excluded,
        ),
    ] {
        if excluded > 0 {
            push_unanchored_exclusion_gap(discontinuities, kind, params, excluded, label);
        }
    }
    let (video_mp4, frames, video_status) = mux_selected_video(
        job_id,
        data_dir,
        video_width,
        video_height,
        selected_video.preroll,
        selected_video.main,
    )?;
    let (audio_m4a, audio_status) = mux_selected_audio(
        job_id,
        data_dir,
        audio_config,
        audio_continuity,
        selected_audio.main,
    )?;
    let (taps, unanchored_taps, trunc_taps) =
        ReplayCollector::select_taps(timeline, raw_taps, params);
    Ok(SelectedMediaTracks {
        bundle_kind: BundleKind::Full,
        video_mp4,
        audio_m4a,
        frames,
        taps,
        video_status,
        audio_status,
        av_lead_in_offset_ns,
        unanchored_taps,
        trunc_taps,
        video_unanchored_excluded: selected_video.unanchored_excluded,
    })
}

/// R4 M4: exact lead-in skew `D` between the two rebased file timelines —
/// `first_selected_audio_pts_device_mono_ns - first_selected_video_pts_device_mono_ns`.
/// `video.mp4` pts0 = first selected frame's device-mono pts; `audio.m4a` pts0 = first
/// selected packet's device-mono pts (`AacPacket::pts_us` is microseconds on the same
/// clock — `select_audio` maps `pts_us * 1000` as device-mono ns). `0` when either stream
/// selected nothing (no shared origin exists).
fn av_lead_in_offset_ns(video: &SelectedVideo, audio: &SelectedAudio) -> i64 {
    match (audio.main.first(), video.main.first()) {
        (Some(first_audio), Some(first_video)) => first_audio
            .pts_us
            .saturating_mul(1_000)
            .saturating_sub(first_video.record.pts_device_mono_ns),
        _ => 0,
    }
}

struct StreamSummaryCounts {
    frame_count: u64,
    video_status: StreamStatus,
    display_width: Option<u32>,
    display_height: Option<u32>,
    audio_status: StreamStatus,
    logs_count: usize,
    log_completeness: LogCompleteness,
    log_overflow_events: u64,
    taps_count: usize,
    tail_may_be_incomplete: bool,
    av_lead_in_offset_ns: i64,
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
            display_width: counts.display_width,
            display_height: counts.display_height,
        }),
        audio: Some(AudioStreamMeta {
            // Computed by `mux_selected_audio`: `Present` only when `audio.m4a` was
            // actually written (required by `verify_integrity`, R2 S15), `Partial` when
            // the file exists but the stream had gaps/outages or when captured packets
            // could not be persisted, `Absent` when no packets were selected.
            status: counts.audio_status,
            // R4 M4: `av_offset_ms` now carries the measured lead-in skew
            // (`audio_lead_in_offset_ns` keeps the exact nanosecond value for trim).
            // The status stays `Uncalibrated`: the skew is measured at save time but
            // pipeline A/V lag is never empirically calibrated.
            av_offset_status: AvOffsetStatus::Uncalibrated,
            av_offset_ms: counts.av_lead_in_offset_ns / 1_000_000,
            audio_lead_in_offset_ns: counts.av_lead_in_offset_ns,
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
    dimensions: (u16, u16),
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

struct AlignedAudioItem {
    packet: AacPacket,
    alignment: TimeAlignment,
    pc_ingest_cont_ns: i64,
}

impl arb_core::selection::AlignedRecord for AlignedAudioItem {
    fn time_alignment(&self) -> &TimeAlignment {
        &self.alignment
    }

    fn pc_ingest_cont_ns(&self) -> i64 {
        self.pc_ingest_cont_ns
    }

    fn estimated_bytes(&self) -> usize {
        self.packet.data.len()
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
    mut packets: Vec<RawH264Packet>,
    mut selected: Vec<AlignedVideoItem>,
) -> Result<(Option<MediaPayload>, Vec<FrameRecord>, StreamStatus), DaemonError> {
    if selected.is_empty() {
        return Ok((None, Vec::new(), StreamStatus::Absent));
    }

    let trim_start = u64::try_from(packets.len()).unwrap_or(u64::MAX);
    let (width, height) = selected
        .iter()
        .map(|v| v.dimensions)
        .max_by_key(|&(w, h)| u32::from(w) * u32::from(h))
        .unwrap_or((width, height));
    packets.extend(selected.iter_mut().map(|item| RawH264Packet {
        annex_b: std::mem::take(&mut item.annex_b),
        pts_ns: item.record.pts_device_mono_ns,
        is_keyframe: item.record.is_keyframe,
    }));

    let tmp_dir = data_dir.join("tmp");
    fs::create_dir_all(&tmp_dir)?;
    let tmp_mp4 = tmp_dir.join(format!("mux-{job_id}.mp4"));
    let _tmp_guard = TempFileGuard(tmp_mp4.clone());

    let trimmed = tmp_dir.join(format!("head-{job_id}.mp4"));
    let _trim_guard = TempFileGuard(trimmed.clone());
    let decodable_head = packets.first().is_some_and(|p| {
        arb_media::mp4::split_annexb_nals(&p.annex_b)
            .iter()
            .any(|n| n.first().is_some_and(|b| b & 31 == 5))
    });
    let muxed = if decodable_head {
        mux_raw_h264_to_mp4(
            &tmp_mp4,
            width.max(16),
            height.max(16),
            MP4_TIMESCALE,
            MP4_DEFAULT_FRAME_DURATION_TICKS,
            &packets,
        )
        .and_then(|report| {
            if trim_start == 0 {
                return Ok(&tmp_mp4);
            }
            arb_media::trim::trim_video(
                &tmp_mp4,
                &trimmed,
                trim_start,
                report.frame_count - 1,
                &arb_media::codec::OpenH264Backend::new(),
            )?;
            Ok(&trimmed)
        })
    } else {
        Err(MediaError::InvalidContainer("missing initial IDR".into()))
    };
    match muxed {
        Ok(path) => {
            let track = Mp4VideoTrack::open(path)?;
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
            let mp4_bytes = fs::read(path)?;
            Ok((
                Some(MediaPayload::Bytes(mp4_bytes)),
                frames,
                StreamStatus::Present,
            ))
        }
        // Only an elementary stream without parameter sets / slices (synthetic stub streams)
        // falls back to timeline-only `Partial` video. Every I/O or unexpected mux failure is
        // propagated so Phase 2 ends in `FailedRetryable` with the pins kept (R3 M3).
        Err(MediaError::InvalidContainer(_) | MediaError::Codec(_)) => {
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

/// Default `AudioSpecificConfig` used when the `scrcpy` audio tunnel delivered AAC-LC frames
/// without a preceding `AudioConfig` packet (48 kHz stereo, per scrcpy's encoder default).
fn default_audio_specific_config() -> Vec<u8> {
    build_aac_lc_audio_specific_config(48_000, 2)
        .map_or_else(|_| vec![0x11, 0x90], |asc| asc.to_vec())
}

/// Whether the audio stream stayed continuous through the saved window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioContinuity {
    /// No gap, error, stop, or `AudioUnavailable` was observed during the window.
    Continuous,
    /// A stream gap, error, stop or `AudioUnavailable` was observed: a muxed file is still
    /// persisted but the status must degrade to `Partial` (never `Present`).
    Degraded,
}

impl ReplayCollector {
    /// `Degraded` when either an audio stream gap or an explicit `AudioUnavailable` was seen.
    fn audio_continuity(&self) -> AudioContinuity {
        if self.audio_gap || self.audio_unavailable {
            AudioContinuity::Degraded
        } else {
            AudioContinuity::Continuous
        }
    }
}

/// Records a `StreamGap` discontinuity for records quarantined out of the main track
/// (uncertain time or other `boot_id`), so exclusions are never silent (R3 S2).
fn push_unanchored_exclusion_gap(
    discontinuities: &mut Vec<DiscontinuityEvent>,
    stream: StreamKind,
    params: &SaveJobMaterializeParams,
    excluded: usize,
    item_label: &str,
) {
    discontinuities.push(DiscontinuityEvent {
        kind: DiscontinuityKind::StreamGap,
        stream: Some(stream),
        boot_id: Some(params.window.boot_id().to_owned()),
        start_boottime_ns: None,
        end_boottime_ns: None,
        pc_cont_start_ns: params.window.ingest_start_cont_ns(),
        pc_cont_end_ns: Some(params.window.ingest_end_cont_ns()),
        delta_ns: None,
        detail: Some(format!(
            "{excluded} unanchored {item_label} (uncertain time or other boot_id) excluded from the main track"
        )),
    });
}

/// Muxes the selected AAC-LC packets into a temporary `audio.m4a` and returns its payload
/// plus the honest stream status.
///
/// `continuity == Degraded` means the audio stream reported a gap, error, stop or an
/// explicit `AudioUnavailable` during the window: the file is still persisted, but the
/// status is downgraded to `Partial` (never `Present`). Malformed or non-AAC-LC payloads
/// fall back to file-less `Partial` (mirroring the video `InvalidContainer` fallback);
/// every I/O or unexpected mux failure is propagated so Phase 2 ends in `FailedRetryable`
/// with the pins kept (R3 M3).
fn mux_selected_audio(
    job_id: &str,
    data_dir: &Path,
    audio_config: Option<&[u8]>,
    continuity: AudioContinuity,
    selected: Vec<AacPacket>,
) -> Result<(Option<MediaPayload>, StreamStatus), DaemonError> {
    if selected.is_empty() {
        return Ok((None, StreamStatus::Absent));
    }

    // Prefer the captured `AudioSpecificConfig` when it parses; otherwise fall back to the
    // scrcpy default so raw AAC-LC frames are still persisted (never silently dropped).
    let asc_bytes = match audio_config {
        Some(bytes) if parse_audio_specific_config(bytes).is_ok() => bytes.to_vec(),
        _ => default_audio_specific_config(),
    };
    let asc = parse_audio_specific_config(&asc_bytes).map_err(|err| {
        DaemonError::Internal(format!("chosen AudioSpecificConfig rejected: {err}"))
    })?;

    let tmp_dir = data_dir.join("tmp");
    fs::create_dir_all(&tmp_dir)?;
    let tmp_m4a = tmp_dir.join(format!("mux-{job_id}.m4a"));
    let _tmp_guard = TempFileGuard(tmp_m4a.clone());

    let input = AacMuxInput {
        audio_specific_config: asc_bytes,
        sample_rate_hz: asc.sample_rate_hz,
        channels: asc.channel_count,
        packets: selected,
    };
    match mux_aac_to_m4a(&tmp_m4a, &input) {
        Ok(_report) => {
            let status = match continuity {
                AudioContinuity::Degraded => StreamStatus::Partial,
                AudioContinuity::Continuous => StreamStatus::Present,
            };
            Ok((Some(MediaPayload::Bytes(fs::read(&tmp_m4a)?)), status))
        }
        Err(MediaError::InvalidContainer(_) | MediaError::UnsupportedFeature(_)) => {
            Ok((None, StreamStatus::Partial))
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
        trigger: record.trigger_kind,
        destination_path: PathBuf::from(&record.job.destination_path),
        pinned_chunk_ids: record.job.pinned_chunk_ids.clone(),
        last_error: record.job.last_error.clone(),
        created_at_utc: record.created_at_utc.clone(),
        updated_at_utc: record.updated_at_utc.clone(),
    }
}

fn default_audio_unavailable_reason(status: AudioTunnelStatus) -> String {
    match status {
        AudioTunnelStatus::UnavailableApiLevel { api_level, .. } => {
            audio_unavailable_reason_for_api(api_level).unwrap_or_else(|| {
                format!(
                    "Audio capture unavailable on Android API {api_level}. Continuing with video + logs + taps."
                )
            })
        }
        AudioTunnelStatus::DisabledByDevice => {
            "Device disabled or refused audio capture (codec_id = 0). Continuing with video + logs + taps."
                .to_owned()
        }
        AudioTunnelStatus::DeviceError => {
            "Device audio capture failed to initialize. Continuing with video + logs + taps."
                .to_owned()
        }
        _ => "Audio capture unavailable. Continuing with video + logs + taps.".to_owned(),
    }
}

fn decode_log_tag_and_message(payload: &LogRecordPayload) -> (String, String) {
    let (tag, message) = decode_log_tag_and_message_raw(payload);
    // System-server event tags (`am_crash`, `am_focused_activity`, `am_resume_activity`,
    // `wm_resumed_activity`, `ActivityManager`, `ActivityTaskManager`, ...) are only authentic
    // from system_server (uid 1000). An app that writes its own text or `EventLog` record under
    // one of them must not be stored under that tag, or `timeline.crashes` / `timeline.screens`
    // would show forged entries.
    let trimmed = tag.trim();
    let is_system_only_tag = matches!(
        trimmed,
        "am_crash"
            | "30039"
            | "am_focused_activity"
            | "am_resume_activity"
            | "wm_resume_activity"
            | "30007"
            | "wm_resumed_activity"
            | "wm_set_resumed_activity"
            | "30043"
            | "ActivityTaskManager"
            | "ActivityManager"
    );
    // `am_on_resume_called` / `wm_on_resume_called` (binary events tag 30022) is emitted by
    // ActivityThread inside the app's own process (app UID), whereas plain text logs under that
    // tag from a non-system UID are rejected to prevent text-log spoofing.
    let is_spoofed_text_resume = matches!(
        trimmed,
        "wm_on_resume_called" | "am_on_resume_called" | "30022"
    ) && !payload.is_binary_event();
    if (is_system_only_tag || is_spoofed_text_resume) && payload.uid != 1000 {
        return (format!("app:{tag}"), message);
    }
    (tag, message)
}

fn decode_log_tag_and_message_raw(payload: &LogRecordPayload) -> (String, String) {
    if let Some(parsed) = payload.parse_text_tag_and_message() {
        return parsed;
    }
    if payload.is_binary_event()
        && let Some(decoded) = decode_binary_android_event(&payload.payload)
    {
        return (decoded.tag_name, decoded.message);
    }
    (
        String::new(),
        format!("<binary {} bytes>", payload.payload.len()),
    )
}

fn detect_crash_from_event(event: &CaptureEvent) -> Option<(i64, String, i64, ParsedAmCrash)> {
    let CaptureEvent::LogRecord {
        pc_cont_ns,
        payload,
        boot_id,
        ..
    } = event
    else {
        return None;
    };
    detect_live_fatal_am_crash(payload).map(|parsed| {
        (
            *pc_cont_ns,
            boot_id.clone(),
            payload.device_read_boottime_ns,
            parsed,
        )
    })
}

fn detect_live_fatal_am_crash(payload: &LogRecordPayload) -> Option<ParsedAmCrash> {
    // Real `am_crash` events are binary records of the events buffer written by system_server
    // (uid 1000). An app's text `Log.e("am_crash", ...)` line must never trigger a save.
    if !payload.is_binary_event() || payload.uid != 1000 {
        return None;
    }
    let parsed = parse_am_crash_binary(&payload.payload)?;
    parsed.is_fatal().then_some(parsed)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;

    use arb_adb::mock::{CaptureEvent, NormalizedTouchEvent, TouchAction};
    use arb_adb::scrcpy::{VideoPacket, VideoPacketKind};
    use arb_core::bundle::{DeviceInfo, FrameRecord, StreamStatus};
    use arb_core::gesture::GestureKind;
    use arb_core::selection::{SelectionPolicy, WindowSpec};
    use arb_core::state::RingBudgetPolicy;
    use arb_core::time::{DiscontinuityKind, TimeAlignment, UncertaintyReason};
    use arb_media::MediaError;
    use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
    use arb_os::OsError;
    use arb_os::fs::DiskSpaceInfo;
    use arb_storage::ring::DiskSpaceProvider;
    use tempfile::TempDir;

    use super::{
        AlignedVideoItem, CapturePipeline, RawCapturedTap, ReplayCollector,
        SaveJobMaterializeParams, SaveMode, SharedDiskSpaceProvider, decode_log_tag_and_message,
        decode_ring_chunk, detect_live_fatal_am_crash, encode_ring_chunk, mux_selected_video,
    };
    use crate::error::DaemonError;

    fn text_payload(uid: u32, tag: &str, msg: &str) -> arb_adb::wire::LogRecordPayload {
        let mut bytes = vec![4_u8];
        bytes.extend_from_slice(tag.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(msg.as_bytes());
        bytes.push(0);
        arb_adb::wire::LogRecordPayload {
            device_read_boottime_ns: 1,
            log_sec: 1,
            log_nsec: 0,
            pid: 1,
            tid: 1,
            lid: 0,
            uid,
            hdr_size: 28,
            priority: 4,
            record_flags: 0,
            payload: bytes,
        }
    }

    #[test]
    fn app_written_am_crash_records_are_not_stored_or_detected_as_system_crashes() {
        let body = "[0,1,com.evil,0,java.lang.Error,fake,F.kt,1]";
        let app = text_payload(10_123, "am_crash", body);
        assert_eq!(decode_log_tag_and_message(&app).0, "app:am_crash");
        assert!(
            detect_live_fatal_am_crash(&app).is_none(),
            "text never triggers"
        );

        let system = text_payload(1000, "am_crash", body);
        assert_eq!(decode_log_tag_and_message(&system).0, "am_crash");
        let padded = text_payload(10_123, "am_crash ", body);
        assert_eq!(
            decode_log_tag_and_message(&padded).0,
            "app:am_crash ",
            "readers trim tags, so a padded tag must not bypass the guard"
        );
        let other = text_payload(10_123, "AppTag", "hello");
        assert_eq!(decode_log_tag_and_message(&other).0, "AppTag");
    }

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
                dimensions: (64, 64),
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

    fn video_frames_with_luma_marker(
        width: u32,
        height: u32,
        initial: u8,
        count: usize,
    ) -> Vec<DecodedFrame> {
        (0..count)
            .map(|i| DecodedFrame {
                width,
                height,
                frame_index: u64::try_from(i).unwrap(),
                format: PixelFormat::Yuv420p,
                data: {
                    let mut data = vec![initial; usize::try_from(width * height * 3 / 2).unwrap()];
                    for row in 0..8 {
                        let start = row * usize::try_from(width).unwrap();
                        data[start..start + 8].fill(initial + u8::try_from(i * 10).unwrap());
                    }
                    data
                },
            })
            .collect::<Vec<_>>()
    }

    fn draft_for_video_window(
        runs: &[(u32, u32, u8, usize)],
        lo: i64,
        hi: i64,
        missing_frame: Option<i64>,
    ) -> (TempDir, super::V2BundleDraft) {
        let tmp = TempDir::new().unwrap();
        let mut params = t6h_gesture_params(None, None);
        params.window = WindowSpec::RealElapsed {
            boot_id: "boot-t".into(),
            start_boottime_ns: lo,
            end_boottime_ns: hi,
            ingest_start_cont_ns: lo,
            ingest_end_cont_ns: hi,
        };
        let mut collector = ReplayCollector::new(&params);
        for t in [0, 1_000_000_000] {
            collector.timeline.ingest_device_triple(
                super::DeviceClockTriple {
                    boot_id: "boot-t".into(),
                    boottime_ns: t,
                    monotonic_ns: t,
                    realtime_ns: t,
                },
                t,
            );
        }
        let mut t = 0;
        for &(width, height, initial, count) in runs {
            collector.video_width = u16::try_from(width).unwrap();
            collector.video_height = u16::try_from(height).unwrap();
            let frames = video_frames_with_luma_marker(width, height, initial, count);
            let packets = OpenH264Backend::new()
                .encode_yuv420p_sequence(&frames, 5, 40_000_000, EncodeParams::default())
                .unwrap();
            if count > 2 {
                assert!(!packets[2].is_keyframe && !packets[3].is_keyframe);
            }
            for pkt in packets {
                if missing_frame == Some(t) {
                    collector.ingest_one(CaptureEvent::Discontinuity(super::DiscontinuityEvent {
                        kind: super::DiscontinuityKind::StreamGap,
                        stream: Some(super::StreamKind::Video),
                        boot_id: Some("boot-t".into()),
                        pc_cont_start_ns: t,
                        pc_cont_end_ns: Some(t),
                        start_boottime_ns: Some(t),
                        end_boottime_ns: Some(t),
                        delta_ns: None,
                        detail: None,
                    }));
                    t += 40_000_000;
                    continue;
                }
                let mut config = Vec::new();
                let mut picture = Vec::new();
                for nal in arb_media::mp4::split_annexb_nals(&pkt.annex_b) {
                    let out = if matches!(nal[0] & 31, 7 | 8) {
                        &mut config
                    } else {
                        &mut picture
                    };
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(nal);
                }
                let mut push = |kind, data| {
                    collector.ingest_one(CaptureEvent::VideoFrame {
                        pc_cont_ns: t,
                        boot_id: "boot-t".into(),
                        packet: VideoPacket {
                            kind,
                            data,
                            pts_us: u64::try_from(t / 1000).unwrap(),
                        },
                    });
                };
                if !config.is_empty() {
                    push(VideoPacketKind::Config, config);
                }
                push(
                    if pkt.is_keyframe {
                        VideoPacketKind::Keyframe
                    } else {
                        VideoPacketKind::DeltaFrame
                    },
                    picture,
                );
                t += 40_000_000;
            }
        }
        let draft = collector.build_draft("test", &params, tmp.path()).unwrap();
        (tmp, draft)
    }

    fn saved_video_for_window(
        runs: &[(u32, u32, u8, usize)],
        lo: i64,
        hi: i64,
    ) -> (TempDir, arb_media::mp4::Mp4VideoTrack) {
        let (tmp, draft) = draft_for_video_window(runs, lo, hi, None);
        let Some(super::MediaPayload::Bytes(bytes)) = draft.video_mp4 else {
            panic!("expected a saved track")
        };
        let path = tmp.path().join("saved.mp4");
        std::fs::write(&path, bytes).unwrap();
        (tmp, arb_media::mp4::Mp4VideoTrack::open(path).unwrap())
    }

    #[test]
    fn save_between_idrs_decodes_window_endpoints() {
        use arb_media::codec::VideoCodecBackend as _;
        let (_dir, track) = saved_video_for_window(&[(64, 64, 30, 7)], 79_000_000, 121_000_000);
        assert_eq!(track.frame_count(), 2);
        for (index, y) in [(0, 50u8), (1, 60)] {
            let frame = OpenH264Backend::new()
                .decode_frame(&track, index, PixelFormat::Yuv420p)
                .unwrap();
            assert!(frame.data[0].abs_diff(y) <= 4, "luma {}", frame.data[0]);
        }
    }

    #[test]
    fn missing_reference_before_window_reports_partial_video() {
        let (_, draft) = draft_for_video_window(
            &[(64, 64, 30, 7)],
            79_000_000,
            121_000_000,
            Some(40_000_000),
        );
        assert_eq!(draft.streams.video.unwrap().status, StreamStatus::Partial);
        assert!(draft.video_mp4.is_none());
    }

    #[test]
    fn save_separate_configs_decodes_both_dimensions() {
        use arb_media::codec::VideoCodecBackend as _;
        let (_dir, track) =
            saved_video_for_window(&[(64, 96, 40, 1), (96, 64, 180, 1)], 0, 100_000_000);
        for (idx, w, h, y) in [(0, 64, 96, 40u8), (1, 96, 64, 180)] {
            let frame = OpenH264Backend::new()
                .decode_frame(&track, idx, PixelFormat::Yuv420p)
                .unwrap();
            assert_eq!((frame.width, frame.height), (w, h));
            assert!(frame.data[0].abs_diff(y) <= 2);
        }
    }

    #[test]
    fn save_mid_gop_with_orientation_roundtrip_decodes_all_segments() {
        use arb_media::codec::VideoCodecBackend as _;
        let (dir, track) = saved_video_for_window(
            &[(64, 96, 40, 4), (96, 64, 180, 2), (64, 96, 120, 2)],
            79_000_000,
            320_000_000,
        );
        let atoms = arb_media::mp4::inspect_mp4_atoms(dir.path().join("saved.mp4")).unwrap();
        assert_eq!((atoms.edts_count, atoms.elst_count), (0, 0));
        assert_eq!(track.frame_count(), 6);
        for (idx, w, h, y) in [(0, 64, 96, 60u8), (2, 96, 64, 180), (4, 64, 96, 120)] {
            let frame = OpenH264Backend::new()
                .decode_frame(&track, idx, PixelFormat::Yuv420p)
                .unwrap();
            assert_eq!((frame.width, frame.height), (w, h), "frame {idx}");
            assert!(
                frame.data[0].abs_diff(y) <= 4,
                "frame {idx} luma {} expected ~{y}",
                frame.data[0]
            );
        }
    }

    #[test]
    fn r3_m3_mux_io_error_propagates_instead_of_partial_without_video() {
        let tmp = TempDir::new().expect("tempdir");
        // Occupy the temporary MP4 path with a directory: muxing a *valid* stream must hit an
        // I/O error, which must fail the save (FailedRetryable), never become "no video".
        std::fs::create_dir_all(tmp.path().join("tmp").join("mux-job-m3.mp4"))
            .expect("create blocking dir");
        let err = mux_selected_video("job-m3", tmp.path(), 64, 64, Vec::new(), real_h264_items())
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
        let (mp4, frames, status) =
            mux_selected_video("job-stub", tmp.path(), 64, 64, Vec::new(), stub)
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
    fn pending_save_keeps_original_device_across_switch() {
        let tmp = TempDir::new().unwrap();
        let mut cap = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .unwrap();
        let out = tmp.path().join("out");
        let catalog = arb_storage::catalog::Catalog::open(tmp.path().join("cat"), &out).unwrap();
        cap.begin_session("A");
        cap.set_display_size(123, 456);
        cap.ingest_event(video_event(1000, vec![1])).unwrap();
        let pending = cap
            .phase1_pin(30, &out, SelectionPolicy::default())
            .unwrap();
        cap.begin_session("B");
        assert!(
            cap.phase1_pin(30, &out, SelectionPolicy::default())
                .is_err()
        );
        cap.ingest_event(video_event(2000, vec![2])).unwrap();
        let (job, _) = cap
            .phase1_commit(&catalog, pending, super::TailCompleteness::MayBeIncomplete)
            .unwrap();
        let params = super::read_job_params(tmp.path(), &job.job.job_id).unwrap();
        assert_eq!(params.device.serial, "A");
        assert_eq!(params.display_width, Some(123));
        assert_eq!(job.job.pinned_chunk_ids.len(), 1);
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

    #[test]
    fn ring_chunks_preserve_fifo_order_when_concurrent_stream_timestamps_interleave() {
        let tmp = TempDir::new().expect("tempdir");
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .expect("open pipeline");
        let _ = pipeline.begin_session("serial-fifo");

        // Chunk 1 starts with a slightly later pre-lock pc_cont_ns and carries VideoFrame A.
        pipeline
            .ingest_event(video_event(2_000_000_000, vec![0, 0, 0, 1, 0x65, 0xAA]))
            .expect("ingest frame A");
        pipeline.flush_pending_chunk().expect("flush chunk 1");

        // Chunk 2 starts with an earlier pre-lock pc_cont_ns (as can happen when an audio/log
        // reader thread sampled pc_cont_ns before waiting on the pipeline lock) and carries VideoFrame B.
        pipeline
            .ingest_event(video_event(1_950_000_000, vec![0, 0, 0, 1, 0x41, 0xBB]))
            .expect("ingest frame B");
        pipeline.flush_pending_chunk().expect("flush chunk 2");

        let chunks = pipeline.ring_store().chunks();
        assert_eq!(chunks.len(), 2, "expected two ring chunks");
        assert!(
            chunks[0].created_cont_ns < chunks[1].created_cont_ns,
            "chunk created_cont_ns must stay strictly monotonic: {chunks:?}"
        );

        let mut replayed_markers = Vec::new();
        for desc in &chunks {
            let bytes = pipeline
                .ring_store()
                .read_chunk(&desc.chunk_id)
                .expect("read chunk");
            for ev in decode_ring_chunk(&bytes).expect("decode chunk") {
                if let CaptureEvent::VideoFrame { packet, .. } = ev {
                    replayed_markers.push(*packet.data.last().expect("marker byte"));
                }
            }
        }
        assert_eq!(
            replayed_markers,
            vec![0xAA, 0xBB],
            "video frames must replay in exact FIFO capture order"
        );
    }

    #[test]
    fn buffered_secs_reports_honest_ring_span_and_resets_for_new_session() {
        let tmp = TempDir::new().expect("tempdir");
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .expect("open pipeline");
        assert_eq!(pipeline.buffered_secs(), 0, "fresh empty ring must be 0s");

        let _ = pipeline.begin_session("serial-buf");
        pipeline.set_display_size(1080, 2400);
        // Simulate static screen: only 2 video frames spanning 12 real seconds.
        pipeline
            .ingest_event(video_event(10_000_000_000, vec![0, 0, 0, 1, 0x65]))
            .expect("ingest start");
        pipeline.flush_pending_chunk().expect("flush start");
        pipeline
            .ingest_event(video_event(22_000_000_000, vec![0, 0, 0, 1, 0x41]))
            .expect("ingest end");
        pipeline.flush_pending_chunk().expect("flush end");
        assert_eq!(
            pipeline.buffered_secs(),
            12,
            "buffered_secs must reflect real ring span (12s), not video_frames/fps (0s)"
        );

        // Starting a fresh session resets the honest buffered span to 0s until new events arrive.
        let _ = pipeline.begin_session("serial-buf");
        pipeline
            .ingest_event(video_event(100_000_000_000, vec![0, 0, 0, 1, 0x65]))
            .expect("ingest s2 start");
        pipeline
            .ingest_event(video_event(103_000_000_000, vec![0, 0, 0, 1, 0x41]))
            .expect("ingest s2 end");
        assert_eq!(
            pipeline.buffered_secs(),
            3,
            "new session must report only its own buffered span (3s)"
        );
    }

    #[test]
    fn t6e_decode_log_tag_preserves_binary_app_30022_and_blocks_spoofed_aos9_am_tags() {
        use arb_adb::wire::{LOG_RECORD_FLAG_BINARY_EVENT, LogRecordPayload};
        use arb_core::analytics::{
            AM_RESUME_ACTIVITY_EVENT_TAG, WM_ON_RESUME_CALLED_EVENT_TAG,
            WM_RESUMED_ACTIVITY_EVENT_TAG,
        };

        let make_text = |uid: u32, tag: &str, msg: &str| {
            let mut bytes = vec![4u8];
            bytes.extend_from_slice(tag.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(msg.as_bytes());
            bytes.push(0);
            LogRecordPayload {
                device_read_boottime_ns: 1_000,
                log_sec: 1,
                log_nsec: 0,
                pid: 4200,
                tid: 4200,
                lid: 0,
                uid,
                hdr_size: 28,
                priority: 4,
                record_flags: 0,
                payload: bytes,
            }
        };

        let make_bin = |uid: u32, event_tag: i32, component: &str| {
            let mut raw = Vec::new();
            raw.extend_from_slice(&event_tag.to_le_bytes());
            raw.push(3); // LIST
            raw.push(3);
            raw.push(0);
            raw.extend_from_slice(&0_i32.to_le_bytes());
            raw.push(2);
            raw.extend_from_slice(&u32::try_from(component.len()).expect("len").to_le_bytes());
            raw.extend_from_slice(component.as_bytes());
            raw.push(2);
            raw.extend_from_slice(&6_u32.to_le_bytes());
            raw.extend_from_slice(b"RESUME");
            LogRecordPayload {
                device_read_boottime_ns: 1_000,
                log_sec: 1,
                log_nsec: 0,
                pid: 4200,
                tid: 4200,
                lid: 2,
                uid,
                hdr_size: 28,
                priority: 0,
                record_flags: LOG_RECORD_FLAG_BINARY_EVENT,
                payload: raw,
            }
        };

        // Authentic binary 30022 (am_on_resume_called / wm_on_resume_called) from app UID 10200 is preserved!
        let app_bin_30022 = make_bin(10_200, WM_ON_RESUME_CALLED_EVENT_TAG, "com.work.app.Main");
        assert_eq!(
            super::decode_log_tag_and_message(&app_bin_30022).0,
            "wm_on_resume_called"
        );

        // Spoofed text log using tag `am_on_resume_called` from app UID 10200 is quarantined with `app:`.
        let spoof_text_resume = make_text(
            10_200,
            "am_on_resume_called",
            "[0,com.victim.app.Main,RESUME]",
        );
        assert_eq!(
            super::decode_log_tag_and_message(&spoof_text_resume).0,
            "app:am_on_resume_called"
        );

        // Spoofed system_server binary/text tags from app UID 10200 are quarantined with `app:`.
        let spoof_bin_30043 = make_bin(10_200, WM_RESUMED_ACTIVITY_EVENT_TAG, "com.evil/.Main");
        assert_eq!(
            super::decode_log_tag_and_message(&spoof_bin_30043).0,
            "app:wm_resumed_activity"
        );
        let spoof_bin_30007 = make_bin(10_200, AM_RESUME_ACTIVITY_EVENT_TAG, "com.evil/.Main");
        assert_eq!(
            super::decode_log_tag_and_message(&spoof_bin_30007).0,
            "app:am_resume_activity"
        );
        let sys_bin_30007 = make_bin(1000, AM_RESUME_ACTIVITY_EVENT_TAG, "com.work/.Main");
        assert_eq!(
            super::decode_log_tag_and_message(&sys_bin_30007).0,
            "am_resume_activity"
        );
    }

    #[test]
    fn t6e_capture_pipeline_surfaces_aos9_audio_unavailable_status() {
        let tmp = TempDir::new().expect("tempdir");
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .expect("open pipeline");
        let _ = pipeline.begin_live_session("aos9-dev");
        pipeline
            .ingest_event(CaptureEvent::AudioUnavailable {
                pc_cont_ns: 1_000,
                boot_id: "boot-1".to_owned(),
                status: arb_adb::scrcpy::AudioTunnelStatus::UnavailableApiLevel {
                    api_level: 28,
                    min_required: 33,
                },
            })
            .expect("ingest AudioUnavailable");
        let view = pipeline.status_view();
        assert_eq!(
            view.audio_state,
            crate::protocol::AudioCaptureState::Unavailable
        );
        assert_eq!(
            view.audio_reason.as_deref(),
            Some(
                "Audio capture requires Android 13+ (API 33+, non-muting playback capture); detected API 28. Continuing with video + logs + taps."
            )
        );
        assert_eq!(view.device_api_warning, None);

        // API < 28 (e.g. API 27) sets a non-blocking warning without blocking capture.
        let gen2 = pipeline.begin_live_session("aos8-legacy");
        pipeline
            .ingest_event_for_session(
                gen2,
                CaptureEvent::Hello {
                    pc_cont_ns: 2_000,
                    payload: arb_adb::wire::HelloPayload {
                        protocol_version: 1,
                        api_level: 27,
                        boot_count: 1,
                        boot_id_source: arb_adb::wire::BootIdSource::KernelProcBootId,
                        is_emulator: false,
                        heartbeat_interval_ms: 250,
                        boot_id: "boot-27".to_owned(),
                        model: "Legacy".to_owned(),
                        manufacturer: "OEM".to_owned(),
                        agent_version: "0.1.0".to_owned(),
                        boot_id_detail: String::new(),
                    },
                    classification: arb_adb::adb::ReconnectClassification::InitialConnection {
                        boot_id: "boot-27".to_owned(),
                        boot_id_source: arb_adb::wire::BootIdSource::KernelProcBootId,
                    },
                },
            )
            .expect("API 27 Hello must not be hard-blocked");
        let view27 = pipeline.status_view();
        assert!(view27.active && view27.connected);
        assert!(
            view27
                .device_api_warning
                .as_deref()
                .is_some_and(|w| w.contains("API 27")),
            "API 27 must surface non-blocking warning: {:?}",
            view27.device_api_warning
        );
    }

    // -- T-6H item 2: per-pointer stroke accumulation (`ActiveStroke`/`ingest_touch_sample`) --

    fn t6h_gesture_params(
        display_width: Option<u32>,
        display_height: Option<u32>,
    ) -> SaveJobMaterializeParams {
        SaveJobMaterializeParams {
            bundle_id: "01JT6HGESTURE00000000000001".to_owned(),
            mode: SaveMode::Full,
            duration_sec: 10,
            trigger_utc: "2026-11-24T00:00:00Z".to_owned(),
            window: WindowSpec::RealElapsed {
                boot_id: "boot-gesture".to_owned(),
                start_boottime_ns: 0,
                end_boottime_ns: 1_000_000_000,
                ingest_start_cont_ns: 0,
                ingest_end_cont_ns: 1_000_000_000,
            },
            policy: SelectionPolicy::new(),
            tail_may_be_incomplete: false,
            device: DeviceInfo {
                serial: "EMU-GESTURE".to_owned(),
                model: "Test".to_owned(),
                api_level: 33,
                is_emulator: true,
            },
            video_width: 1080,
            video_height: 2400,
            display_width,
            display_height,
            fallback_video_config: None,
            extra_discontinuities: Vec::new(),
            uid_packages: BTreeMap::new(),
        }
    }

    fn t6h_touch_sample(
        pointer_id: i32,
        action: TouchAction,
        x: i32,
        y: i32,
        device_read_boottime_ns: i64,
    ) -> NormalizedTouchEvent {
        NormalizedTouchEvent {
            device_mono_ns: Some(device_read_boottime_ns),
            device_read_boottime_ns,
            action,
            pointer_id,
            x,
            y,
            viewport: Some((1080, 2400)),
        }
    }

    #[test]
    fn t6h_short_stroke_emits_exactly_one_tap_regardless_of_move_sample_count() {
        let params = t6h_gesture_params(Some(1080), Some(2400));
        let mut collector = ReplayCollector::new(&params);
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            0,
            &t6h_touch_sample(0, TouchAction::Down, 500, 1_000, 0),
        );
        for i in 1..5 {
            collector.ingest_touch_sample(
                "boot-gesture".to_owned(),
                i * 1_000_000,
                &t6h_touch_sample(0, TouchAction::Move, 501, 1_001, i * 1_000_000),
            );
        }
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            10_000_000,
            &t6h_touch_sample(0, TouchAction::Up, 502, 1_002, 10_000_000),
        );

        assert!(
            collector.active_strokes.is_empty(),
            "UP must close the stroke"
        );
        assert_eq!(
            collector.raw_taps.len(),
            1,
            "one DOWN->MOVE*->UP stroke must become exactly one gesture row, not one per sample"
        );
        let tap = &collector.raw_taps[0];
        assert_eq!(tap.gesture_kind, GestureKind::Tap);
        assert_eq!(tap.action, "TAP");
        assert_eq!(tap.x, Some(500.0));
        assert_eq!(tap.y, Some(1_000.0));
        assert_eq!(tap.end_x, None, "Tap carries no end point");
        assert!(tap.path.is_empty(), "Tap carries no path");
    }

    #[test]
    fn t6h_long_drag_emits_one_swipe_with_endpoint_and_path() {
        let params = t6h_gesture_params(Some(1080), Some(2400));
        let mut collector = ReplayCollector::new(&params);
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            0,
            &t6h_touch_sample(0, TouchAction::Down, 500, 2_000, 0),
        );
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            50_000_000,
            &t6h_touch_sample(0, TouchAction::Move, 500, 1_000, 50_000_000),
        );
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            100_000_000,
            &t6h_touch_sample(0, TouchAction::Up, 500, 200, 100_000_000),
        );

        assert_eq!(collector.raw_taps.len(), 1, "one Swipe gesture row");
        let tap = &collector.raw_taps[0];
        assert_eq!(tap.gesture_kind, GestureKind::Swipe);
        assert_eq!(tap.action, "SWIPE");
        assert_eq!(tap.x, Some(500.0), "start point is the DOWN sample");
        assert_eq!(tap.y, Some(2_000.0));
        assert_eq!(tap.end_x, Some(500.0));
        assert_eq!(tap.end_y, Some(200.0));
        assert_eq!(tap.end_t_offset_ns, Some(100_000_000));
        assert!(
            tap.path.len() >= 2,
            "Swipe carries a decimated multi-point path"
        );
    }

    #[test]
    fn t6h_stroke_still_open_at_end_of_stream_is_flushed_not_lost() {
        let params = t6h_gesture_params(Some(1080), Some(2400));
        let mut collector = ReplayCollector::new(&params);
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            0,
            &t6h_touch_sample(0, TouchAction::Down, 10, 20, 0),
        );
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            20_000_000,
            &t6h_touch_sample(0, TouchAction::Move, 400, 900, 20_000_000),
        );
        // Disconnect / stop mid-gesture: no UP/CANCEL ever arrives.
        assert!(
            collector.raw_taps.is_empty(),
            "stroke must still be open before flush"
        );
        assert_eq!(collector.active_strokes.len(), 1);

        collector.flush_active_strokes();

        assert!(
            collector.active_strokes.is_empty(),
            "flush must close every still-open stroke"
        );
        assert_eq!(
            collector.raw_taps.len(),
            1,
            "the in-progress swipe must not be silently dropped"
        );
        assert_eq!(collector.raw_taps[0].gesture_kind, GestureKind::Swipe);
    }

    #[test]
    fn t6h_concurrent_pointers_classify_independently() {
        let params = t6h_gesture_params(Some(1080), Some(2400));
        let mut collector = ReplayCollector::new(&params);
        // Pointer 0: short tap.
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            0,
            &t6h_touch_sample(0, TouchAction::Down, 100, 100, 0),
        );
        // Pointer 1 starts while pointer 0 is still down (multi-touch).
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            5_000_000,
            &t6h_touch_sample(1, TouchAction::Down, 900, 2_000, 5_000_000),
        );
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            10_000_000,
            &t6h_touch_sample(0, TouchAction::Up, 101, 101, 10_000_000),
        );
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            60_000_000,
            &t6h_touch_sample(1, TouchAction::Up, 900, 200, 60_000_000),
        );

        assert!(collector.active_strokes.is_empty());
        assert_eq!(collector.raw_taps.len(), 2, "two independent gesture rows");
        let by_pointer: BTreeMap<i32, &RawCapturedTap> = collector
            .raw_taps
            .iter()
            .map(|t| (t.pointer_id.expect("pointer_id set"), t))
            .collect();
        assert_eq!(by_pointer[&0].gesture_kind, GestureKind::Tap);
        assert_eq!(by_pointer[&1].gesture_kind, GestureKind::Swipe);
    }

    #[test]
    fn t6h_stray_down_without_prior_up_flushes_the_stale_stroke_first() {
        let params = t6h_gesture_params(Some(1080), Some(2400));
        let mut collector = ReplayCollector::new(&params);
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            0,
            &t6h_touch_sample(0, TouchAction::Down, 10, 10, 0),
        );
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            5_000_000,
            &t6h_touch_sample(0, TouchAction::Move, 600, 1_200, 5_000_000),
        );
        // A dropped UP followed by a new DOWN for the same pointer must not merge the two
        // gestures together or lose the first one.
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            6_000_000,
            &t6h_touch_sample(0, TouchAction::Down, 700, 1_800, 6_000_000),
        );
        collector.ingest_touch_sample(
            "boot-gesture".to_owned(),
            7_000_000,
            &t6h_touch_sample(0, TouchAction::Up, 701, 1_801, 7_000_000),
        );

        assert_eq!(
            collector.raw_taps.len(),
            2,
            "the stale pre-DOWN stroke and the new stroke must both be recorded"
        );
        assert_eq!(collector.raw_taps[0].gesture_kind, GestureKind::Swipe);
        assert_eq!(collector.raw_taps[1].gesture_kind, GestureKind::Tap);
    }

    fn raw_evdev(
        ev_type: u16,
        ev_code: u16,
        ev_value: i32,
        read_bt_ns: i64,
    ) -> arb_adb::wire::TouchEventPayload {
        arb_adb::wire::TouchEventPayload {
            device_read_boottime_ns: read_bt_ns,
            event_sec: read_bt_ns / 1_000_000_000,
            event_usec: i32::try_from((read_bt_ns % 1_000_000_000) / 1_000).expect("usec fits i32"),
            ev_type,
            ev_code,
            ev_value,
            device_path: "/dev/input/event2".to_owned(),
            raw_line: String::new(),
        }
    }

    #[test]
    fn t6h_touch_normalizer_protocol_b_multi_slot_same_syn_report_emits_all_slots() {
        let mut norm = super::TouchNormalizer::default();
        let vp = Some((1080, 2400));

        // Slot 0 and Slot 1 both go DOWN in one SYN_REPORT packet.
        for (ev_code, ev_value) in [
            (super::ABS_MT_SLOT, 0),
            (super::ABS_MT_TRACKING_ID, 10),
            (super::ABS_MT_POSITION_X, 100),
            (super::ABS_MT_POSITION_Y, 200),
            (super::ABS_MT_SLOT, 1),
            (super::ABS_MT_TRACKING_ID, 11),
            (super::ABS_MT_POSITION_X, 500),
            (super::ABS_MT_POSITION_Y, 900),
        ] {
            assert_eq!(
                norm.ingest(&raw_evdev(super::EV_ABS, ev_code, ev_value, 10_000_000), vp)
                    .len(),
                0
            );
        }
        let downs = norm.ingest(
            &raw_evdev(super::EV_SYN, super::SYN_REPORT, 0, 10_000_000),
            vp,
        );
        assert_eq!(downs.len(), 2, "both slots must emit DOWN on SYN_REPORT");
        assert_eq!(downs[0].pointer_id, 0);
        assert_eq!(downs[0].action, TouchAction::Down);
        assert_eq!(downs[1].pointer_id, 1);
        assert_eq!(downs[1].action, TouchAction::Down);

        // Slot 0 lifts (0xffffffff == -1) while Slot 1 moves in the SAME SYN_REPORT packet.
        for (ev_code, ev_value) in [
            (super::ABS_MT_SLOT, 0),
            (super::ABS_MT_TRACKING_ID, -1),
            (super::ABS_MT_SLOT, 1),
            (super::ABS_MT_POSITION_Y, 300),
        ] {
            norm.ingest(&raw_evdev(super::EV_ABS, ev_code, ev_value, 50_000_000), vp);
        }
        let mixed = norm.ingest(
            &raw_evdev(super::EV_SYN, super::SYN_REPORT, 0, 50_000_000),
            vp,
        );
        assert_eq!(
            mixed.len(),
            2,
            "slot 0 UP must not be dropped when slot 1 is current_slot at SYN_REPORT"
        );
        assert_eq!(mixed[0].pointer_id, 0);
        assert_eq!(mixed[0].action, TouchAction::Up);
        assert_eq!(mixed[1].pointer_id, 1);
        assert_eq!(mixed[1].action, TouchAction::Move);
        assert_eq!(mixed[1].y, 300);
    }

    #[test]
    fn t6h_touch_normalizer_protocol_a_single_touch_btn_touch_swipe() {
        let params = t6h_gesture_params(Some(1080), Some(2400));
        let mut collector = ReplayCollector::new(&params);

        let raw_events = [
            raw_evdev(super::EV_ABS, super::ABS_X, 100, 10_000_000),
            raw_evdev(super::EV_ABS, super::ABS_Y, 1_800, 10_000_000),
            raw_evdev(super::EV_KEY, super::BTN_TOUCH, 1, 10_000_000),
            raw_evdev(super::EV_SYN, super::SYN_REPORT, 0, 10_000_000),
            raw_evdev(super::EV_ABS, super::ABS_X, 100, 40_000_000),
            raw_evdev(super::EV_ABS, super::ABS_Y, 600, 40_000_000),
            raw_evdev(super::EV_SYN, super::SYN_REPORT, 0, 40_000_000),
            raw_evdev(super::EV_KEY, super::BTN_TOUCH, 0, 80_000_000),
            raw_evdev(super::EV_SYN, super::SYN_REPORT, 0, 80_000_000),
        ];
        for (idx, payload) in raw_events.into_iter().enumerate() {
            let idx_i64 = i64::try_from(idx).expect("idx fits i64");
            collector.ingest_one(CaptureEvent::TouchEvent {
                pc_cont_ns: idx_i64 * 10_000_000,
                boot_id: "boot-single".to_owned(),
                flags: arb_adb::wire::FrameFlags::default(),
                payload,
                normalized: None,
            });
        }

        assert_eq!(collector.active_strokes.len(), 0);
        assert_eq!(collector.raw_taps.len(), 1);
        assert_eq!(collector.raw_taps[0].gesture_kind, GestureKind::Swipe);
        assert_eq!(collector.raw_taps[0].x, Some(100.0));
        assert_eq!(collector.raw_taps[0].y, Some(1_800.0));
        assert_eq!(collector.raw_taps[0].end_x, Some(100.0));
        assert_eq!(collector.raw_taps[0].end_y, Some(600.0));
        assert_eq!(collector.raw_taps[0].end_t_offset_ns, Some(70_000_000));
    }

    #[test]
    fn t6h_active_stroke_clamps_negative_offset_flushes_on_boot_change_and_caps_memory() {
        let params = t6h_gesture_params(Some(1080), Some(2400));
        let mut collector = ReplayCollector::new(&params);

        // 1. Backwards boottime timestamp must clamp t_offset_ns to >= 0.
        collector.ingest_touch_sample(
            "boot-1".to_owned(),
            100,
            &t6h_touch_sample(0, TouchAction::Down, 100, 100, 50_000_000),
        );
        collector.ingest_touch_sample(
            "boot-1".to_owned(),
            200,
            &t6h_touch_sample(0, TouchAction::Up, 102, 101, 40_000_000),
        );
        assert_eq!(collector.raw_taps.len(), 1);
        assert_eq!(
            collector.raw_taps[0].end_t_offset_ns,
            Some(0),
            "backwards timestamp must clamp t_offset_ns to 0"
        );

        // 2. High-rate swipe-and-return exceeding MAX_ACTIVE_STROKE_SAMPLES must bound memory
        //    AND still classify as Swipe via peak-excursion preservation.
        collector.ingest_touch_sample(
            "boot-1".to_owned(),
            300,
            &t6h_touch_sample(0, TouchAction::Down, 100, 100, 100_000_000),
        );
        for i in 1..=(super::MAX_ACTIVE_STROKE_SAMPLES + 500) {
            let y = if i == super::MAX_ACTIVE_STROKE_SAMPLES + 100 {
                1_200 // peak excursion happens AFTER cap is reached
            } else {
                100
            };
            let i_i64 = i64::try_from(i).expect("i fits i64");
            collector.ingest_touch_sample(
                "boot-1".to_owned(),
                300 + i_i64,
                &t6h_touch_sample(0, TouchAction::Move, 100, y, 100_000_000 + i_i64 * 1_000),
            );
        }
        assert!(
            collector.active_strokes[&0].samples.len() <= super::MAX_ACTIVE_STROKE_SAMPLES,
            "ActiveStroke::samples must stay bounded"
        );
        collector.ingest_touch_sample(
            "boot-1".to_owned(),
            9_000,
            &t6h_touch_sample(0, TouchAction::Up, 101, 101, 200_000_000),
        );
        assert_eq!(collector.raw_taps.len(), 2);
        assert_eq!(
            collector.raw_taps[1].gesture_kind,
            GestureKind::Swipe,
            "peak excursion past cap must still classify swipe-and-return as Swipe"
        );

        // 3. Boot change without UP flushes old boot's stroke before starting new boot's stroke.
        collector.ingest_touch_sample(
            "boot-1".to_owned(),
            10_000,
            &t6h_touch_sample(0, TouchAction::Down, 50, 50, 300_000_000),
        );
        collector.ingest_touch_sample(
            "boot-2".to_owned(),
            20_000,
            &t6h_touch_sample(0, TouchAction::Move, 500, 900, 10_000_000),
        );
        collector.ingest_touch_sample(
            "boot-2".to_owned(),
            21_000,
            &t6h_touch_sample(0, TouchAction::Up, 500, 900, 20_000_000),
        );
        assert_eq!(collector.raw_taps.len(), 4);
        assert_eq!(collector.raw_taps[2].boot_id, "boot-1");
        assert_eq!(collector.raw_taps[3].boot_id, "boot-2");
    }
}
