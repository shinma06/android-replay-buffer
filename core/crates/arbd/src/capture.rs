//! Live capture ingestion pipeline, Zone-1 ring chunk persistence, and two-phase save engine.
//!
//! Bridges [`arb_adb::mock::DeviceEventSource`] (both live ADB sessions and deterministic
//! [`arb_adb::mock::MockDeviceSource`]) into [`ClockTimeline`], [`RingStore`],
//! [`arb_core::selection::select_records`], [`arb_media::mp4::mux_raw_h264_to_mp4_with_last_duration`],
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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use arb_adb::mock::{CaptureEvent, NormalizedTouchEvent, TouchAction};
use arb_adb::scrcpy::{AudioTunnelStatus, VideoPacketKind, unsupported_android_api_warning};
use arb_adb::wire::{
    LogRecordPayload, StreamId, StreamLifecycleState, TouchEventPayload, UNKNOWN_U32,
};
use arb_core::analytics::{ParsedAmCrash, decode_binary_android_event, parse_am_crash_binary};
use arb_core::audio_policy::AudioModeSelection;
use arb_core::bundle::{
    AudioStreamMeta, AvOffsetStatus, BundleKind, CarriedInFrame, CarryInEvidence, DeviceInfo,
    EpochSpan, FrameRecord, InputStreamMeta, LogCompleteness, LogRecord, LogcatStreamMeta,
    Provenance, RecordBucket, RequestedWindow, StreamStatus, StreamsSummary, TapRecord,
    UnanchoredSummary, VideoStreamMeta,
};
use arb_core::config::{AudioCaptureMode, CaptureMode};
use arb_core::coverage::{
    CarryCandidate, CoverageLedger, CoverageSnapshot, VideoCoverageKey, coverage_duration_for_key,
    coverage_duration_in_window, covering_end, select_carry_in,
};
use arb_core::gesture::{GestureKind, TouchPoint, classify_stroke, touch_slop};
use arb_core::input_capture::PhysicalStatus;
use arb_core::save_failure::{SaveFailureClass, SaveFailureStage};
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
use arb_media::mp4::{Mp4VideoTrack, RawH264Packet, mux_raw_h264_to_mp4_with_last_duration};
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

use crate::error::{DaemonError, NotFoundEntity};
use crate::protocol::{
    AudioCaptureState, AudioCause, BufferState, CaptureStatusView, SaveJobView, SaveMode,
    SaveOutcomeResponse,
};
use crate::screenshot::RecordingGop;
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
    /// Simulates an external destination losing write access after a successful probe.
    pub deny_output_after_pin_once: AtomicBool,
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
    #[serde(default)]
    input_channel: Option<PhysicalStatus>,
    bundle_id: String,
    #[serde(default)]
    note: Option<String>,
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
    #[serde(default)]
    fallback_video_config: Option<Vec<u8>>,
    #[serde(default)]
    fallback_audio_config: Option<Vec<u8>>,
    #[serde(default)]
    trigger_kind: SaveTriggerKind,
    /// Discontinuities that never reached a ring chunk (`StoragePressureStop`, failed ring
    /// writes) and overlap the save window. They are merged into the saved bundle.
    #[serde(default)]
    extra_discontinuities: Vec<DiscontinuityEvent>,
    /// Best-effort UID -> Android package name map captured from `cmd package list packages -U`.
    #[serde(default)]
    uid_packages: BTreeMap<u32, String>,
    /// Video / logcat coverage intervals intersecting the window (host ingest axis), captured
    /// with the pin and refreshed after the drain (#564 / #570). `None` on jobs pinned before
    /// the ledger existed: those keep the span-based measurements and never carry a frame in.
    #[serde(default)]
    coverage: Option<CoverageSnapshot>,
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
    initial_flush_failed: bool,
    pub(super) generation: u64,
    params: SaveJobMaterializeParams,
    dest_bundle_dir: PathBuf,
    trigger_kind: SaveTriggerKind,
    pinned_chunk_ids: Vec<String>,
    drain_trigger_boottime_ns: i64,
    drain_decision: TailCompleteness,
    must_drain: bool,
}

struct SaveIntent {
    mode: SaveMode,
    trigger_kind: SaveTriggerKind,
    note: Option<String>,
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
    rescue_after_secs: u64,
    data_dir: PathBuf,
    pin_journal: PinJournal,
    chunks: Vec<(String, Option<RingChunkDescriptor>)>,
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
    max_pinned_fraction: f64,
    rescue_after_secs: u64,
    data_dir: PathBuf,
    ring: RingStore<SharedDiskSpaceProvider>,
    faults: Arc<FaultInjection>,
    active: bool,
    connected: bool,
    storage_pressure_stopped: bool,
    capture_mode: CaptureMode,
    last_video_error: Option<String>,
    audio_state: AudioCaptureState,
    requested_audio: AudioModeSelection,
    active_audio_mode: AudioCaptureMode,
    audio_cause: Option<AudioCause>,
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
    tap_records_seen: u64,
    live_normalizer: TouchNormalizer,
    gesture_count: u64,
    command_counts: arb_core::input_capture::CommandCounts,
    physical_channel: PhysicalStatus,
    session_input_start: u64,
    session_gesture_start: u64,
    discontinuities_seen: usize,
    static_hold_seen: usize,
    ring_write_errors: usize,
    session_first_event_cont_ns: Option<i64>,
    drain_watermark_ns: Option<i64>,
    latest_heartbeat_boottime_ns: Option<i64>,
    latest_heartbeat_pc_cont_ns: Option<i64>,
    latest_pc_cont_ns: Option<i64>,
    last_chunk_created_cont_ns: Option<i64>,
    host_boot_ns: i64,
    resume_from_ns: Option<i64>,
    recovered_boot_id: Option<String>,
    latest_video_config: Option<Vec<u8>>,
    latest_audio_config: Option<Vec<u8>>,
    last_discontinuities: Vec<DiscontinuityEvent>,
    pressure_discontinuities: Vec<DiscontinuityEvent>,
    chunk_contents: BTreeMap<String, ChunkContent>,
    /// Continuous video / logcat coverage evidence of the current device (#564 / #570).
    coverage: CoverageLedger,
    screenshot: RecordingGop,
    screenshot_beacon: Option<i64>,
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
        Self::open_at(
            data_dir.as_ref(),
            policy,
            disk_provider,
            sample_host_clock()?,
        )
    }

    fn open_at(
        data_dir: &Path,
        policy: RingBudgetPolicy,
        disk_provider: SharedDiskSpaceProvider,
        now: arb_os::clock::HostClockSample,
    ) -> Result<Self, DaemonError> {
        let data_dir = data_dir.to_path_buf();
        let host_boot_ns = now.wall_epoch_ns.saturating_sub(now.continuous_mono_ns);
        let ring = RingStore::open_with_disk_provider(&data_dir, policy, disk_provider)?;
        let mut pipeline = Self {
            data_dir,
            ring,
            faults: Arc::new(FaultInjection::default()),
            max_pinned_fraction: arb_core::config::ArbConfig::new().max_pinned_ring_fraction,
            rescue_after_secs: arb_core::config::ArbConfig::new().failed_job_rescue_after_secs,
            active: false,
            connected: false,
            storage_pressure_stopped: false,
            capture_mode: CaptureMode::VideoAndLogs,
            last_video_error: None,
            audio_state: AudioCaptureState::Unknown,
            requested_audio: AudioModeSelection::default(),
            active_audio_mode: AudioCaptureMode::default(),
            audio_cause: None,
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
            live_normalizer: TouchNormalizer::default(),
            gesture_count: 0,
            command_counts: arb_core::input_capture::CommandCounts::default(),
            physical_channel: PhysicalStatus::Unavailable,
            session_input_start: 0,
            session_gesture_start: 0,
            discontinuities_seen: 0,
            static_hold_seen: 0,
            ring_write_errors: 0,
            session_first_event_cont_ns: None,
            drain_watermark_ns: None,
            latest_heartbeat_boottime_ns: None,
            latest_heartbeat_pc_cont_ns: None,
            latest_pc_cont_ns: None,
            last_chunk_created_cont_ns: None,
            host_boot_ns,
            resume_from_ns: None,
            recovered_boot_id: None,
            latest_video_config: None,
            latest_audio_config: None,
            last_discontinuities: Vec::new(),
            pressure_discontinuities: Vec::new(),
            chunk_contents: BTreeMap::new(),
            screenshot: RecordingGop::default(),
            screenshot_beacon: None,
            coverage: CoverageLedger::new(arb_adb::adb::DEFAULT_BEACON_TIMEOUT_NS),
            crash_notifier: None,
            crash_daemon: std::sync::Weak::new(),
        };
        pipeline.restore_capture(now.continuous_mono_ns);
        Ok(pipeline)
    }

    fn restore_capture(&mut self, now: i64) {
        let chunks = self.ring.chunks();
        let mut excluded_ids = Vec::new();
        for chunk in chunks {
            let Some(info) = chunk.capture.filter(|info| {
                info.host_boot_ns.abs_diff(self.host_boot_ns) < 1_000_000_000
                    && chunk.created_cont_ns <= now
            }) else {
                excluded_ids.push(chunk.chunk_id);
                continue;
            };
            let boot_id = info
                .device_boot_id
                .or_else(|| info.heartbeat.as_ref().map(|h| h.0.clone()));
            if self.serial.as_ref() != Some(&info.device_serial)
                || self
                    .recovered_boot_id
                    .as_ref()
                    .zip(boot_id.as_ref())
                    .is_some_and(|(old, new)| old != new)
            {
                self.session_first_event_cont_ns = Some(chunk.created_cont_ns);
                self.ring.set_span_floor(self.session_first_event_cont_ns);
                self.chunk_contents.clear();
            }
            self.serial = Some(info.device_serial.clone());
            self.device.serial = info.device_serial;
            self.latest_pc_cont_ns = Some(info.ingest_range.1);
            self.last_chunk_created_cont_ns = Some(chunk.created_cont_ns);
            self.clock_domain = if info.host_live {
                ClockDomain::HostLive
            } else {
                ClockDomain::Replayed
            };
            self.latest_video_config = info.video_config;
            self.latest_audio_config = info.audio_config;
            (self.video_width, self.video_height) = info.video_size;
            self.recovered_boot_id = boot_id;
            self.latest_heartbeat_boottime_ns = info.heartbeat.as_ref().map(|h| h.1);
            self.latest_heartbeat_pc_cont_ns = info.heartbeat.map(|h| h.2);
            self.chunk_contents.insert(
                chunk.chunk_id,
                ChunkContent {
                    picture: if info.has_keyframe {
                        Some(VideoPacketKind::Keyframe)
                    } else {
                        info.has_video.then_some(VideoPacketKind::DeltaFrame)
                    },
                    video_config: info.has_video_config,
                    heartbeat: info.has_anchor,
                },
            );
        }
        if !excluded_ids.is_empty() {
            // Purge unpinned stale chunks from earlier host boots so their larger pre-reboot
            // `created_cont_ns` values cannot cause new-boot chunks to be evicted first.
            let _ = self.ring.purge_unpinned_chunks(&excluded_ids);
            self.record_carried_discontinuity(
                stream_gap(
                    now,
                    now,
                    "以前のホスト起動または時刻不明の録画を除外しました".into(),
                ),
                CarriedEffect::KeepCapturing,
            );
        }
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

    pub(crate) fn select_audio_mode(&mut self, mode: AudioCaptureMode) {
        self.requested_audio.select(mode);
    }

    pub(crate) const fn requested_audio(&self) -> AudioModeSelection {
        self.requested_audio
    }

    pub(crate) fn begin_audio_attempt(&mut self) -> AudioModeSelection {
        if self.active_audio_mode != self.requested_audio.mode() {
            self.active_audio_mode = self.requested_audio.mode();
            if self.capture_mode == CaptureMode::VideoAndLogs {
                self.audio_state = AudioCaptureState::Unknown;
                self.audio_reason = None;
                self.audio_cause = None;
            }
        }
        self.requested_audio
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

    /// Records the live audio capture availability state, its machine-readable cause
    /// (decided by `arb_core::audio_policy`) and optional diagnostic reason for `system.status`.
    pub fn set_audio_status_for_session(
        &mut self,
        generation: u64,
        state: AudioCaptureState,
        cause: AudioCause,
        reason: Option<String>,
    ) {
        if generation == self.session_generation {
            self.audio_state = state;
            self.audio_cause = Some(cause);
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
        self.resume_from_ns = self.latest_pc_cont_ns;
        if self.serial.as_deref().is_some_and(|old| old != serial) {
            self.resume_from_ns = None;
            self.session_first_event_cont_ns = None;
            self.ring.set_span_floor(Some(i64::MAX));
            let _ = self.flush_pending_chunk();
            self.recovered_boot_id = None;
            self.latest_video_config = None;
            self.latest_audio_config = None;
            self.chunk_contents.clear();
            self.coverage.clear();
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
        self.drain_watermark_ns = None;
        self.screenshot = RecordingGop::default();
        self.screenshot_beacon = None;
        // A new generation never extends the coverage of an older one.
        self.coverage.close_all();
        self.session_generation = self.session_generation.wrapping_add(1);
        self.session_input_start = self.tap_records_seen;
        self.live_normalizer = TouchNormalizer::default();
        self.session_gesture_start = self.gesture_count;
        self.command_counts = arb_core::input_capture::CommandCounts::default();
        self.physical_channel = if capture_mode == CaptureMode::VideoAndLogs {
            PhysicalStatus::Absent
        } else {
            PhysicalStatus::Disabled
        };
        self.active = true;
        self.connected = false;
        self.storage_pressure_stopped = false;
        self.capture_mode = capture_mode;
        self.active_audio_mode = self.requested_audio.mode();
        self.last_video_error = None;
        (self.audio_state, self.audio_cause) = if capture_mode == CaptureMode::LogsOnly {
            (AudioCaptureState::Disabled, Some(AudioCause::LogsOnly))
        } else {
            (AudioCaptureState::Unknown, None)
        };
        self.audio_reason = None;
        self.device_api_warning = None;
        self.clock_domain = clock_domain;
        self.serial = Some(serial.clone());
        self.device.serial = serial;
        self.display_width = None;
        self.display_height = None;
        self.uid_packages.clear();
        self.session_generation
    }

    pub(super) fn adb_resource_dir(&self) -> PathBuf {
        self.data_dir.join("adb-sessions")
    }

    pub(crate) fn screenshot_gop(
        &self,
        serial: Option<&str>,
        now: i64,
    ) -> Result<RecordingGop, &'static str> {
        if !self.is_active() || !self.connected {
            return Err("capture_inactive");
        }
        if self.serial.as_deref() != serial {
            return Err("serial_mismatch");
        }
        if !self.screenshot_beacon.is_some_and(|t| {
            (0..=arb_adb::adb::DEFAULT_BEACON_TIMEOUT_NS).contains(&now.saturating_sub(t))
        }) {
            return Err("beacon_stale");
        }
        let interval = self
            .coverage
            .video
            .current_at(now)
            .ok_or("video_coverage_closed")?;
        if self.timeline.current_boot_id() != Some(interval.key.boot_id.as_str()) {
            return Err("video_boot_mismatch");
        }
        self.screenshot.snapshot(interval, now)
    }

    /// Returns the generation of the most recently started capture session.
    #[must_use]
    pub const fn session_generation(&self) -> u64 {
        self.session_generation
    }

    /// Marks the active capture session as stopped and flushes buffered events to Zone-1 ring.
    pub fn mark_stopped(&mut self) {
        self.screenshot = RecordingGop::default();
        self.screenshot_beacon = None;
        self.active = false;
        self.connected = false;
        self.coverage.close_all();
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
        let newest = self.buffer_span().map(|(_, hi)| hi);
        let buffer_state = if self.is_active() {
            BufferState::Live
        } else if newest.is_some() {
            BufferState::RetainedAfterStop
        } else {
            BufferState::Empty
        };
        let newest_age_secs = newest.and_then(|ts| {
            let now = sample_host_clock().ok()?.continuous_mono_ns;
            (self.clock_domain == ClockDomain::HostLive && now >= ts)
                .then(|| std::time::Duration::from_nanos(now.abs_diff(ts)).as_secs_f64())
        });
        CaptureStatusView {
            buffer_state,
            newest_age_secs,
            active: self.active,
            connected: self.connected,
            serial: self.serial.clone(),
            boot_id: self.timeline.current_boot_id().map(str::to_owned),
            storage_pressure_stopped: self.storage_pressure_stopped,
            capture_mode: self.capture_mode,
            last_video_error: self.last_video_error.clone(),
            audio_state: self.audio_state,
            audio_capture_mode: self.active_audio_mode,
            audio_cause: self.audio_cause,
            audio_reason: self.audio_reason.clone(),
            device_api_level: (self.device.api_level > 0).then_some(self.device.api_level),
            device_api_warning: self.device_api_warning.clone(),
            video_frames: self.video_frames_seen,
            audio_frames: self.audio_frames_seen,
            log_records: self.log_records_seen,
            tap_records: self.tap_records_seen,
            input_events: self.tap_records_seen,
            gesture_count: self.gesture_count,
            input_capture: arb_core::input_capture::InputCapture::new(
                self.physical_status(),
                Some(self.tap_records_seen - self.session_input_start),
                self.gesture_count - self.session_gesture_start,
                self.command_counts,
            ),
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
        Ok(self.ring.usage_counts()?)
    }

    /// Computes the honest elapsed duration currently retained in the Zone-1 ring buffer (in
    /// whole seconds), scoped to the current serial and device boot and bounded by
    /// the configured ring duration cap.
    #[must_use]
    pub fn buffered_secs(&self) -> u64 {
        let Some((lo, hi)) = self.buffer_span() else {
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

    fn buffer_span(&self) -> Option<(i64, i64)> {
        let session_floor = self.session_first_event_cont_ns;
        let (mut oldest, mut newest) = self.ring.retained_span().unzip();

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

        oldest.zip(newest)
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

        let announced_boot = match &event {
            CaptureEvent::Hello { payload, .. } => Some(payload.boot_id.as_str()),
            CaptureEvent::Heartbeat { boot_id, .. } => Some(boot_id.as_str()),
            _ => None,
        };
        if let Some(boot) = announced_boot {
            if self
                .timeline
                .current_boot_id()
                .or(self.recovered_boot_id.as_deref())
                .is_some_and(|old| old != boot)
            {
                self.flush_pending_chunk()?;
                self.chunk_contents.clear();
                self.coverage.close_all();
                self.screenshot = RecordingGop::default();
                self.screenshot_beacon = None;
                self.timeline = ClockTimeline::new(ClockTimelineConfig::new());
                self.live_normalizer = TouchNormalizer::default();
                self.latest_heartbeat_boottime_ns = None;
                self.latest_heartbeat_pc_cont_ns = None;
                self.drain_watermark_ns = None;
                self.latest_video_config = None;
                self.latest_audio_config = None;
                self.session_first_event_cont_ns = None;
                self.resume_from_ns = None;
                self.pressure_discontinuities.clear();
            }
            self.recovered_boot_id = Some(boot.to_owned());
        }
        let event_pc_ns = capture_event_pc_cont_ns(&event);
        if let Some(end) = event_pc_ns
            && let Some(start) = self.resume_from_ns.take()
        {
            let gap = stream_gap(
                start,
                end.max(start),
                "記録を停止後、同じ端末で再開しました".into(),
            );
            self.pending_events
                .push(CaptureEvent::Discontinuity(gap.clone()));
            self.record_live_discontinuity(&gap);
        }
        if let Some(ts) = event_pc_ns {
            if self.session_first_event_cont_ns.is_none() {
                self.session_first_event_cont_ns = Some(ts);
                self.ring.set_span_floor(Some(ts));
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

    fn accept_hello(&mut self, payload: &arb_adb::wire::HelloPayload) {
        self.connected = true;
        self.device.model.clone_from(&payload.model);
        self.device.api_level = payload.api_level;
        self.device.is_emulator = payload.is_emulator;
        if self.device_api_warning.is_none() {
            self.device_api_warning = unsupported_android_api_warning(payload.api_level);
        }
    }

    /// Coverage evidence of one event (#564 / #570). Discontinuities are handled by
    /// [`Self::close_coverage_for`] where they are recorded.
    fn apply_coverage_event(&mut self, event: &CaptureEvent) {
        match event {
            // A new transport session (or its end): coverage restarts from fresh evidence.
            CaptureEvent::Hello { .. } | CaptureEvent::Shutdown { .. } => {
                self.screenshot = RecordingGop::default();
                self.screenshot_beacon = None;
                self.coverage.close_all();
            }
            CaptureEvent::Heartbeat {
                pc_cont_ns,
                boot_id,
                payload,
            } => self.observe_heartbeat_coverage(*pc_cont_ns, boot_id, payload),
            // A regenerated video session (new encoder, resolution or configuration) never
            // continues the coverage of the previous one.
            CaptureEvent::VideoSession { .. } => self.coverage.video.close(),
            CaptureEvent::VideoFrame {
                packet,
                pc_cont_ns,
                boot_id,
            } if !packet.is_config() => {
                // Keyed by the dimensions of the session the frame belongs to.
                self.coverage.video.observe_data(
                    VideoCoverageKey {
                        boot_id: boot_id.clone(),
                        width: self.video_width,
                        height: self.video_height,
                    },
                    *pc_cont_ns,
                );
            }
            CaptureEvent::LogRecord {
                pc_cont_ns,
                boot_id,
                ..
            } => self
                .coverage
                .logcat
                .observe_data(boot_id.clone(), *pc_cont_ns),
            CaptureEvent::StreamState { payload, .. }
                if !matches!(
                    payload.state,
                    StreamLifecycleState::Running | StreamLifecycleState::Starting
                ) =>
            {
                match payload.target_stream_id {
                    StreamId::Logcat => self.coverage.logcat.close(),
                    StreamId::Video => self.coverage.video.close(),
                    StreamId::Control => self.coverage.close_all(),
                    _ => {}
                }
            }
            _ => {}
        }
        if !self.coverage.video.is_open() {
            self.screenshot.clear_frames();
        }
    }

    pub(super) fn disable_input(&mut self) {
        self.physical_channel = PhysicalStatus::Disabled;
    }

    fn physical_status(&self) -> PhysicalStatus {
        if self.physical_channel == PhysicalStatus::Disabled {
            PhysicalStatus::Disabled
        } else if self.tap_records_seen > self.session_input_start {
            PhysicalStatus::Recorded
        } else if !self.active || !self.connected {
            PhysicalStatus::Unavailable
        } else {
            self.physical_channel
        }
    }

    fn observe_input(&mut self, event: &CaptureEvent) {
        match event {
            CaptureEvent::LogRecord { payload, .. } => {
                self.log_records_seen += 1;
                if matches!(payload.uid, 0 | 2000) {
                    let (tag, message) = decode_log_tag_and_message(payload);
                    let bad = u64::try_from(arb_core::injected_input::visit_commands(
                        Some(&tag),
                        Some(payload.uid),
                        &message,
                        |_, command| {
                            self.command_counts.commands += 1;
                            self.command_counts.gestures += u64::from(command.is_gesture());
                        },
                    ))
                    .unwrap_or(u64::MAX);
                    self.command_counts.commands += bad;
                    self.command_counts.unparsed_commands += bad;
                }
            }
            CaptureEvent::TouchEvent {
                payload,
                normalized,
                ..
            } => {
                self.tap_records_seen += 1;
                let completed = |sample: &NormalizedTouchEvent| {
                    matches!(sample.action, TouchAction::Up | TouchAction::Cancel)
                };
                if let Some(sample) = normalized {
                    self.gesture_count += u64::from(completed(sample));
                } else {
                    self.gesture_count += u64::try_from(
                        self.live_normalizer
                            .ingest(payload, None)
                            .iter()
                            .filter(|t| completed(t))
                            .count(),
                    )
                    .unwrap_or(u64::MAX);
                }
            }
            CaptureEvent::StreamState { payload, .. }
                if payload.target_stream_id == StreamId::Input
                    && self.physical_channel != PhysicalStatus::Disabled =>
            {
                self.physical_channel = match payload.state {
                    StreamLifecycleState::Error
                    | StreamLifecycleState::Stopped
                    | StreamLifecycleState::Restarting => PhysicalStatus::Unavailable,
                    _ => PhysicalStatus::Absent,
                };
            }
            _ => {}
        }
    }

    fn apply_live_event_state(&mut self, event: &CaptureEvent) -> bool {
        // VideoSession updates the dimensions after closing the old coverage; frames are keyed
        // by the dimensions in effect when they arrive.
        self.apply_coverage_event(event);
        self.observe_input(event);
        let mut should_flush = false;
        match event {
            CaptureEvent::Hello { payload, .. } => {
                self.accept_hello(payload);
                should_flush = true;
            }
            CaptureEvent::Heartbeat {
                pc_cont_ns,
                boot_id,
                payload,
            } => {
                self.screenshot_beacon = Some(*pc_cont_ns);
                self.connected = true;
                // Companion streams (Control, Logcat, Input) share a single ordered FIFO stream;
                // `StreamHealthEntry::last_pts_ns` records the last emitted event (0 when idle),
                // not a heartbeat watermark, so the heartbeat's `boottime_ns` is the watermark.
                self.drain_watermark_ns = Some(payload.boottime_ns);
                self.latest_heartbeat_boottime_ns = Some(payload.boottime_ns);
                self.latest_heartbeat_pc_cont_ns = Some(*pc_cont_ns);
                let triple = DeviceClockTriple {
                    boot_id: boot_id.clone(),
                    boottime_ns: payload.boottime_ns,
                    monotonic_ns: payload.monotonic_ns,
                    realtime_ns: payload.realtime_ns,
                };
                let _ = self.timeline.ingest_device_triple(triple, *pc_cont_ns);
                self.timeline.bound_live_history();
                should_flush = true;
            }
            CaptureEvent::SyncProbe { probe, .. } => {
                let _ = self.timeline.ingest_pc_sync_probe(probe.clone());
                self.timeline.bound_live_history();
            }
            CaptureEvent::VideoSession { meta, boot_id, .. } => {
                self.screenshot.begin(boot_id, meta);
                self.video_width = u16::try_from(meta.width).unwrap_or(1920);
                self.video_height = u16::try_from(meta.height).unwrap_or(1080);
                should_flush = true;
            }
            CaptureEvent::VideoFrame {
                packet,
                pc_cont_ns,
                boot_id,
            } => {
                self.screenshot.push(boot_id, *pc_cont_ns, packet);
                if packet.is_config() {
                    self.latest_video_config = Some(packet.data.clone());
                    should_flush = true;
                } else {
                    if packet.is_keyframe()
                        && let Some(config) = embedded_config(&packet.data)
                    {
                        self.latest_video_config = Some(config);
                    }
                    self.video_frames_seen += 1;
                }
            }
            CaptureEvent::AudioConfig { packet, .. } => {
                self.latest_audio_config = Some(packet.data.clone());
            }
            CaptureEvent::AudioFrame { .. } => {
                self.audio_frames_seen += 1;
                self.audio_state = AudioCaptureState::Available;
                self.audio_cause = Some(AudioCause::None);
                self.audio_reason = None;
            }
            CaptureEvent::AudioUnavailable { status, .. } => {
                self.audio_state = AudioCaptureState::Unavailable;
                if matches!(self.audio_cause, None | Some(AudioCause::None)) {
                    self.audio_cause = Some(default_audio_unavailable_cause(
                        *status,
                        self.active_audio_mode,
                    ));
                }
                if self.audio_reason.is_none() {
                    self.audio_reason = Some(default_audio_unavailable_reason(
                        *status,
                        self.active_audio_mode,
                    ));
                }
            }
            CaptureEvent::StaticHold { .. } => {
                self.static_hold_seen += 1;
            }
            CaptureEvent::Discontinuity(disc) => {
                self.record_live_discontinuity(disc);
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

    fn record_live_discontinuity(&mut self, disc: &DiscontinuityEvent) {
        if disc.kind == DiscontinuityKind::StreamGap
            && disc.stream == Some(StreamKind::Audio)
            && self.audio_state == AudioCaptureState::Available
        {
            self.audio_state = AudioCaptureState::Unavailable;
            self.audio_cause = Some(AudioCause::Unknown);
            self.audio_reason.clone_from(&disc.detail);
        }
        self.close_coverage_for(disc);
        self.discontinuities_seen += 1;
        self.push_live_discontinuity(disc.clone());
        if disc.kind == DiscontinuityKind::ConnectionLost {
            self.connected = false;
        }
    }

    /// Gaps, disconnects, storage stops and reboots end the coverage of the affected streams.
    fn close_coverage_for(&mut self, disc: &DiscontinuityEvent) {
        if !matches!(
            disc.kind,
            DiscontinuityKind::StreamGap
                | DiscontinuityKind::ConnectionLost
                | DiscontinuityKind::StoragePressureStop
                | DiscontinuityKind::DeviceReboot
        ) {
            return;
        }
        if matches!(disc.stream, None | Some(StreamKind::Video)) {
            self.screenshot.clear_frames();
        }
        match disc.stream {
            None => self.coverage.close_all(),
            Some(StreamKind::Video) => self.coverage.video.close(),
            Some(StreamKind::Logcat) => self.coverage.logcat.close(),
            Some(_) => {}
        }
    }

    /// A companion beacon keeps a still-alive stream covered (static screen, quiet logcat);
    /// a beacon reporting a stopped stream ends that stream's coverage (#564 / #570).
    fn observe_heartbeat_coverage(
        &mut self,
        pc_cont_ns: i64,
        boot_id: &str,
        payload: &arb_adb::wire::HeartbeatPayload,
    ) {
        let state_of = |id: StreamId| {
            payload
                .streams
                .iter()
                .find(|e| e.stream_id == id)
                .map(|e| e.state)
        };
        match state_of(StreamId::Logcat) {
            Some(StreamLifecycleState::Running) => {
                self.coverage
                    .logcat
                    .observe_data(boot_id.to_owned(), pc_cont_ns);
            }
            Some(_) => self.coverage.logcat.close(),
            None => self.coverage.logcat.observe_alive(pc_cont_ns),
        }
        match state_of(StreamId::Video) {
            Some(StreamLifecycleState::Running) | None => {
                self.coverage.video.observe_alive(pc_cont_ns);
            }
            Some(_) => self.coverage.video.close(),
        }
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
            SaveIntent {
                mode,
                trigger_kind: SaveTriggerKind::CrashAutoSave,
                note: None,
            },
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

    fn capture_info(
        &self,
        events: &[CaptureEvent],
        content: ChunkContent,
        raw_created_cont_ns: i64,
    ) -> arb_storage::ring::CaptureChunkInfo {
        arb_storage::ring::CaptureChunkInfo {
            host_live: self.clock_domain == ClockDomain::HostLive,
            host_boot_ns: self.host_boot_ns,
            device_serial: self.device.serial.clone(),
            device_boot_id: self
                .timeline
                .current_boot_id()
                .or(self.recovered_boot_id.as_deref())
                .map(str::to_owned),
            ingest_range: (
                events
                    .iter()
                    .filter_map(capture_event_pc_cont_ns)
                    .min()
                    .unwrap_or(raw_created_cont_ns),
                events
                    .iter()
                    .filter_map(capture_event_pc_cont_ns)
                    .max()
                    .unwrap_or(raw_created_cont_ns),
            ),
            has_video: content.picture.is_some(),
            has_keyframe: content.picture == Some(VideoPacketKind::Keyframe),
            has_video_config: content.video_config,
            has_audio: events
                .iter()
                .any(|e| matches!(e, CaptureEvent::AudioFrame { .. })),
            has_audio_config: events
                .iter()
                .any(|e| matches!(e, CaptureEvent::AudioConfig { .. })),
            has_anchor: content.heartbeat,
            video_size: (self.video_width, self.video_height),
            video_config: self.latest_video_config.clone(),
            audio_config: self.latest_audio_config.clone(),
            heartbeat: self
                .timeline
                .current_boot_id()
                .or(self.recovered_boot_id.as_deref())
                .zip(self.latest_heartbeat_boottime_ns)
                .zip(self.latest_heartbeat_pc_cont_ns)
                .map(|((b, t), p)| (b.to_owned(), t, p)),
        }
    }

    /// Conservative heartbeat progress across all active streams of this session.
    pub(super) const fn drain_watermark_ns(&self) -> Option<i64> {
        self.drain_watermark_ns
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
        let info = self.capture_info(&events, content, raw_created_cont_ns);
        let payload = encode_ring_chunk(events)?;
        let chunk_id = next_id("chunk");

        match self.ring.write_capture_chunk(
            chunk_id.clone(),
            RingChunkKind::Video,
            created_cont_ns,
            duration_ns,
            &payload,
            Some(info),
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
        self.close_coverage_for(&event);
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
        self.pin_request(
            duration_sec,
            dest_parent_dir,
            policy,
            mode,
            trigger_kind,
            None,
        )
    }

    pub(super) fn pin_request(
        &mut self,
        duration_sec: u64,
        dest_parent_dir: &Path,
        policy: SelectionPolicy,
        mode: SaveMode,
        trigger_kind: SaveTriggerKind,
        note: Option<String>,
    ) -> Result<PendingSave, DaemonError> {
        let window = self.build_save_window(duration_sec, self.trigger_point());
        self.pin_window(
            duration_sec,
            dest_parent_dir,
            policy,
            SaveIntent {
                mode,
                trigger_kind,
                note,
            },
            window,
        )
    }

    fn pin_window(
        &mut self,
        duration_sec: u64,
        dest_parent_dir: &Path,
        policy: SelectionPolicy,
        intent: SaveIntent,
        window: WindowSpec,
    ) -> Result<PendingSave, DaemonError> {
        let SaveIntent {
            mode,
            trigger_kind,
            note,
        } = intent;
        let initial_flush_failed =
            self.flush_pending_chunk().is_err() || self.storage_pressure_stopped;
        let chunk_ids = self.select_window_chunks(&window, &policy, mode);
        if chunk_ids.is_empty() {
            return Err(DaemonError::Conflict(
                "no captured ring chunks overlap the requested save window".to_owned(),
            ));
        }

        self.check_pin_budget(None, &chunk_ids)?;
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
        let (must_drain, drain_decision) = match (self.is_active(), self.is_connected()) {
            (true, true) => (true, TailCompleteness::Complete),
            (true, false) => (false, TailCompleteness::MayBeIncomplete),
            _ => (false, TailCompleteness::Complete),
        };

        let extra_discontinuities = self.carried_discontinuities_for(&window, &policy);
        let drain_trigger_boottime_ns = window.end_boottime_ns();
        let params = SaveJobMaterializeParams {
            input_channel: Some(self.physical_channel),
            note,
            bundle_id,
            mode,
            duration_sec,
            trigger_utc: now_utc,
            window,
            policy,
            tail_may_be_incomplete: true,
            device: self.device.clone(),
            video_width: self.video_width,
            video_height: self.video_height,
            display_width: self.display_width,
            display_height: self.display_height,
            fallback_video_config: self.latest_video_config.clone(),
            fallback_audio_config: self.latest_audio_config.clone(),
            trigger_kind,
            extra_discontinuities,
            uid_packages: self.uid_packages.clone(),
            coverage: None,
        };
        let mut params = params;
        params.coverage = Some(self.coverage_snapshot_for(&params.window));
        write_job_params(&self.data_dir, &job_id, &params)?;
        guard.armed = true;
        self.ring.pin_chunks(
            &job_id,
            &dest_bundle_dir,
            chunk_ids.clone(),
            &params.trigger_utc,
        )?;
        Ok(PendingSave {
            guard,
            initial_flush_failed,
            generation: self.session_generation,
            params,
            dest_bundle_dir,
            trigger_kind,
            pinned_chunk_ids: chunk_ids,
            drain_trigger_boottime_ns,
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
        let mut flush_failed = false;
        if pending.generation == self.session_generation {
            flush_failed = self.flush_pending_chunk().is_err() || self.storage_pressure_stopped;
            for gap in
                self.carried_discontinuities_for(&pending.params.window, &pending.params.policy)
            {
                if !pending.params.extra_discontinuities.contains(&gap) {
                    pending.params.extra_discontinuities.push(gap);
                }
            }
            for id in self.select_window_chunks(
                &pending.params.window,
                &pending.params.policy,
                pending.params.mode,
            ) {
                if !chunk_ids.contains(&id) {
                    chunk_ids.push(id);
                }
            }
            // The drain may have delivered the beacon that confirms coverage up to `T`.
            pending.params.coverage = Some(self.coverage_snapshot_for(&pending.params.window));
        }
        let job_id = pending.guard.job_id.clone();
        if chunk_ids.len() != pending.pinned_chunk_ids.len() {
            self.check_pin_budget(Some(&job_id), &chunk_ids)?;
            let _ = self.ring.pin_chunks(
                &job_id,
                &pending.dest_bundle_dir,
                chunk_ids.clone(),
                &pending.params.trigger_utc,
            )?;
        }

        pending.params.tail_may_be_incomplete = pending.initial_flush_failed
            || flush_failed
            || tail.may_be_incomplete()
            || self.session_generation != pending.generation;
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

    pub(super) fn set_save_policy(&mut self, config: &arb_core::config::ArbConfig) {
        self.max_pinned_fraction = config.max_pinned_ring_fraction;
        self.rescue_after_secs = config.failed_job_rescue_after_secs;
    }

    /// Invoked under the capture lock before both the initial pin and its drain extension.
    /// Only pins held by other jobs count against a save, so a save is never refused by
    /// its own selection.
    fn check_pin_budget(
        &self,
        own_job: Option<&str>,
        selected: &[String],
    ) -> Result<(), DaemonError> {
        if self.ring.pinned_chunk_ids()?.is_empty() {
            return Ok(());
        }
        let mut protected: BTreeSet<String> = self
            .ring
            .pin_journal()
            .list_all()?
            .into_iter()
            .filter(|record| Some(record.job_id.as_str()) != own_job)
            .flat_map(|record| record.pinned_chunk_ids)
            .collect();
        if protected.is_empty() {
            return Ok(());
        }
        protected.extend(selected.iter().cloned());
        let bytes = protected
            .iter()
            .filter_map(|id| self.ring.descriptor(id))
            .fold(0u64, |sum, c| sum.saturating_add(c.bytes));
        #[expect(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "configured ring capacity is at most 64 GiB and exactly representable in f64"
        )]
        let fraction = bytes as f64 / self.ring.policy().max_ring_bytes.max(1) as f64;
        if fraction > self.max_pinned_fraction {
            return Err(DaemonError::PinBudget {
                pinned_fraction: fraction,
                max_fraction: self.max_pinned_fraction,
                failed_save_jobs: 0,
            });
        }
        Ok(())
    }

    /// Builds the immutable [`Phase2Plan`] for already-pinned `chunk_ids`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Storage`] (`NotFound`) if a pinned chunk is missing.
    pub fn phase2_plan(&self, chunk_ids: &[String]) -> Result<Phase2Plan, DaemonError> {
        let chunks = chunk_ids
            .iter()
            .map(|id| (id.clone(), self.ring.descriptor(id).cloned()))
            .collect();
        Ok(Phase2Plan {
            rescue_after_secs: self.rescue_after_secs,
            data_dir: self.data_dir.clone(),
            pin_journal: self.ring.pin_journal().clone(),
            chunks,
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
        let old_destination = Path::new(&job.job.destination_path);
        let current_id = old_destination
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&params.bundle_id);
        let new_bundle_dir = if job.rescue_attempted
            || (old_destination.parent() != Some(new_dest_dir)
                && catalog.bundle(current_id)?.is_some())
        {
            // Keep the original publication, catalog path and any active leases intact.
            new_dest_dir.join(next_id("bundle"))
        } else {
            new_dest_dir.join(current_id)
        };
        let plan = self.phase2_plan(&job.job.pinned_chunk_ids)?;
        let journal = self.ring.pin_journal();
        let previous = journal.load(job_id).ok().map(|r| r.destination_path);
        catalog.track_staging(job_id, old_destination)?;
        catalog.clean_tracked_staging(job_id, old_destination);
        let _ = journal.update_destination(job_id, &new_bundle_dir)?;
        if let Err(err) = catalog.retarget_job(job_id, &new_bundle_dir, &utc_now_iso8601()) {
            if let Some(prev) = previous {
                let _ = journal.update_destination(job_id, prev);
            }
            return Err(err.into());
        }
        Ok(plan)
    }

    pub(super) fn prepare_rescue(
        &self,
        catalog: &Catalog,
        job: &CatalogSaveJob,
    ) -> Result<Phase2Plan, DaemonError> {
        let id = &job.job.job_id;
        let mut params = read_job_params(&self.data_dir, id)?;
        let original = Path::new(&job.job.destination_path);
        let message = format!(
            "自動救出: 元の保存先 {}",
            original.parent().unwrap_or(original).display()
        );
        params.note = Some(
            params
                .note
                .map_or_else(|| message.clone(), |n| format!("{n}\n{message}")),
        );
        write_job_params(&self.data_dir, id, &params)?;
        self.prepare_retarget(catalog, id, catalog.managed_bundles_root())
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
        catalog.clean_tracked_staging(job_id, Path::new(&discarded.job.destination_path));
        let _ = self.ring.unpin_job(job_id);
        let _ = remove_job_params(&self.data_dir, job_id);
        Ok(save_job_to_view(&discarded, self.rescue_after_secs))
    }

    /// `T_trigger` (R3 S1): the latest ingest time (or, for a live session, the host clock now)
    /// and the device boottime extrapolated from the latest heartbeat to that instant.
    fn trigger_point(&self) -> TriggerPoint {
        let observed = self.latest_pc_cont_ns.unwrap_or(0);
        let pc_cont_ns = if self.clock_domain == ClockDomain::HostLive {
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
            .or(self.recovered_boot_id.as_deref())
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
    fn select_window_chunks(
        &self,
        window: &WindowSpec,
        policy: &SelectionPolicy,
        mode: SaveMode,
    ) -> Vec<String> {
        let margin = i64::try_from(policy.unanchored_ingest_margin_ns)
            .unwrap_or(i64::MAX)
            .max(MIN_PIN_WINDOW_MARGIN_NS);
        let lo = window.ingest_start_cont_ns().saturating_sub(margin);
        let hi = window.ingest_end_cont_ns().saturating_add(margin);
        let chunks = self.ring.chunks();
        let overlaps = |c: &RingChunkDescriptor| {
            c.capture.as_ref().is_some_and(|info| {
                self.chunk_contents.contains_key(&c.chunk_id)
                    && info.ingest_range.0 <= hi
                    && info.ingest_range.1 >= lo
            })
        };
        let Some(first_idx) = chunks.iter().position(overlaps) else {
            return Vec::new();
        };
        let mut selected: Vec<String> = chunks
            .iter()
            .filter(|c| overlaps(c))
            .map(|c| c.chunk_id.clone())
            .collect();
        let mut need_keyframe = mode == SaveMode::Full;
        let mut need_config = mode == SaveMode::Full;
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

    /// Coverage intervals that intersect the window's ingest range, persisted with the job.
    fn coverage_snapshot_for(&self, window: &WindowSpec) -> CoverageSnapshot {
        self.coverage
            .snapshot(window.ingest_start_cont_ns(), window.ingest_end_cont_ns())
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

pub(crate) fn embedded_config(annex_b: &[u8]) -> Option<Vec<u8>> {
    let mut config = Vec::new();
    for nal in arb_media::mp4::split_annexb_nals(annex_b) {
        if nal.first().is_some_and(|b| matches!(b & 31, 7 | 8)) {
            config.extend_from_slice(&[0, 0, 0, 1]);
            config.extend_from_slice(nal);
        }
    }
    (!config.is_empty()).then_some(config)
}

fn stream_gap(start: i64, end: i64, detail: String) -> DiscontinuityEvent {
    DiscontinuityEvent {
        kind: DiscontinuityKind::StreamGap,
        stream: None,
        boot_id: None,
        start_boottime_ns: None,
        end_boottime_ns: None,
        pc_cont_start_ns: start,
        pc_cont_end_ns: Some(end),
        delta_ns: None,
        detail: Some(detail),
    }
}

pub(super) fn recover_provisional_jobs(
    data_dir: &Path,
    catalog: &Catalog,
    pins: &PinJournal,
) -> Result<(), DaemonError> {
    for record in pins.list_all()? {
        if catalog.save_job(&record.job_id)?.is_none()
            && job_params_path(data_dir, &record.job_id).try_exists()?
        {
            let params = read_job_params(data_dir, &record.job_id)?;
            catalog.create_pinned_job_with_trigger(
                &record.job_id,
                &record.destination_path,
                &record.pinned_chunk_ids,
                &record.created_at_utc,
                params.trigger_kind,
            )?;
        }
    }
    Ok(())
}

fn load_failed_retryable_job(
    catalog: &Catalog,
    job_id: &str,
) -> Result<CatalogSaveJob, DaemonError> {
    let job = catalog
        .save_job(job_id)?
        .ok_or_else(|| DaemonError::NotFound {
            entity: NotFoundEntity::Job,
            detail: format!("save job `{job_id}` not found"),
        })?;
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
    summary: Option<crate::protocol::CatalogSummary>,
    evidence: crate::protocol::SaveEvidence,
    unanchored_summary: UnanchoredSummary,
    warning: Option<String>,
}

/// Per-stream shortfall flags (`actual_ms + 1000 < requested_secs * 1000`).
///
/// Logs are judged by `log_coverage_ms` (window time covered by a running logcat stream) when
/// the coverage ledger measured it; the span of log rows is only the fallback for jobs pinned
/// before the ledger existed (#570). A healthy stream that merely had no row near an edge is
/// therefore not short, while a stream that started late or broke is.
fn stream_shortfalls(
    mode: SaveMode,
    requested_secs: u64,
    video_ms: Option<u64>,
    logs_ms: u64,
    log_coverage_ms: Option<u64>,
) -> (bool, bool) {
    let short =
        |actual_ms: u64| actual_ms.saturating_add(1000) < requested_secs.saturating_mul(1000);
    (
        mode == SaveMode::Full && short(video_ms.unwrap_or(0)),
        short(log_coverage_ms.unwrap_or(logs_ms)),
    )
}

/// Conditions that each produce exactly one warning (code + message).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent flag per warning code; the list is the single source of warnings"
)]
struct WarningFlags {
    video_shortfall: bool,
    log_shortfall: bool,
    logcat_overflow: bool,
    unanchored_truncated: bool,
    audio_absent: bool,
    stream_discontinuity: bool,
    tail_unconfirmed: bool,
}

/// The single source of save warnings: `warning_codes` and `warnings` are both projected from
/// this list, so they always have the same length and order (#570).
fn evidence_warnings(flags: WarningFlags) -> Vec<crate::protocol::WarningDetail> {
    [
        (
            flags.video_shortfall,
            "video_shortfall",
            "映像が要求した保存時間に達していません",
        ),
        (
            flags.log_shortfall,
            "log_shortfall",
            "ログが要求した保存時間に達していません",
        ),
        (
            flags.video_shortfall || flags.log_shortfall,
            "window_shortfall",
            "要求した保存時間に達していない記録があります",
        ),
        (
            flags.logcat_overflow,
            "logcat_overflow",
            "logcat のオーバーフローを検出しました",
        ),
        (
            flags.unanchored_truncated,
            "unanchored_truncated",
            "時刻未確定の記録を件数・容量制限で切り捨てました",
        ),
        (flags.audio_absent, "audio_absent", "音声がありません"),
        (
            flags.stream_discontinuity,
            "stream_discontinuity",
            "記録に不連続箇所があります",
        ),
        (
            flags.tail_unconfirmed,
            "tail_unconfirmed",
            "保存範囲の末尾は未確認です",
        ),
    ]
    .into_iter()
    .filter(|(flag, _, _)| *flag)
    .map(|(_, code, message)| crate::protocol::WarningDetail {
        code: code.to_owned(),
        message: message.to_owned(),
        details: None,
    })
    .collect()
}

/// Static holds shorter than this are ordinary frame intervals, not an informational hold.
const STATIC_HOLD_INFO_MS: u64 = 2_000;

/// Window coverage of the video and logcat streams from the persisted ledger snapshot.
///
/// `video_from` is the earliest ingest time the saved `video.mp4` shows (`i64::MIN` for a
/// carried-in head displayed from `W0`), or `None` when no video was written. Coverage before
/// the first written frame (for example orphan head frames dropped under K15) is not counted.
fn window_coverage_ms(
    params: &SaveJobMaterializeParams,
    video_from: Option<i64>,
) -> (Option<u64>, Option<u64>) {
    let Some(coverage) = params.coverage.as_ref() else {
        return (None, None);
    };
    let window = &params.window;
    let (lo, hi) = (window.ingest_start_cont_ns(), window.ingest_end_cont_ns());
    let boot = window.boot_id().to_owned();
    let video: Vec<_> = coverage
        .video
        .iter()
        .filter(|c| c.key.boot_id == boot)
        .cloned()
        .collect();
    let video_ms = video_from.map_or(0, |from| {
        coverage_duration_in_window(&video, lo.max(from), hi) / 1_000_000
    });
    (
        Some(video_ms),
        Some(coverage_duration_for_key(&coverage.logcat, &boot, lo, hi) / 1_000_000),
    )
}

/// Held display time at the window edges: the carried-in first sample (when later frames
/// follow) plus the last sample's display length.
fn saved_static_hold_ms(
    reader: &dyn arb_storage::bundle_io::BundleReader,
    carried_in: Option<&CarriedInFrame>,
) -> u64 {
    let Some(track) = reader
        .video_path()
        .and_then(|path| Mp4VideoTrack::open(path).ok())
    else {
        return 0;
    };
    let frames = track.frames();
    let head = match frames {
        [first, second, ..] if carried_in.is_some() => {
            u64::try_from(second.pts_ns - first.pts_ns).unwrap_or(0)
        }
        _ => 0,
    };
    let tail = track
        .last_presentation_duration_ns()
        .ok()
        .flatten()
        .unwrap_or(0);
    head.saturating_add(tail) / 1_000_000
}

fn measure_saved_evidence(
    path: &Path,
    params: &SaveJobMaterializeParams,
) -> Result<
    (
        crate::protocol::SaveEvidence,
        crate::protocol::CatalogSummary,
    ),
    DaemonError,
> {
    let reader = arb_storage::bundle_io::open_bundle(path)?;
    let logs = reader.log_statistics()?;
    let summary = crate::inspect_export::catalog_summary_with_logs(reader.as_ref(), &logs)?;
    let manifest = reader.manifest_summary();
    let video = summary
        .duration_ms
        .filter(|_| summary.has_video == Some(true));
    let carried_in = manifest
        .streams
        .video
        .as_ref()
        .and_then(|v| v.carried_in.as_ref());
    let video_from = if reader.video_path().is_none() {
        None
    } else if carried_in.is_some() {
        Some(i64::MIN)
    } else {
        reader.frames()?.iter().map(|f| f.pc_ingest_cont_ns).min()
    };
    let (video_coverage_ms, log_coverage_ms) = window_coverage_ms(params, video_from);
    let (video_shortfall, log_shortfall) = stream_shortfalls(
        params.mode,
        params.duration_sec,
        video,
        logs.duration_ms,
        log_coverage_ms,
    );
    let window_shortfall = video_shortfall || log_shortfall;
    let completeness = crate::protocol::LogcatEvidence::from(manifest);
    let discontinuity_count = reader.discontinuities()?.len();
    let full = params.mode == SaveMode::Full;
    let carried_in_frame = carried_in.is_some();
    let static_hold_ms = if full {
        saved_static_hold_ms(reader.as_ref(), carried_in)
    } else {
        0
    };
    let info_codes = if full && (carried_in_frame || static_hold_ms >= STATIC_HOLD_INFO_MS) {
        vec!["video_static_hold".to_owned()]
    } else {
        Vec::new()
    };
    let mut evidence = crate::protocol::SaveEvidence {
        video_shortfall: Some(video_shortfall),
        log_shortfall: Some(log_shortfall),
        logcat_completeness: Some(completeness.clone()),
        note: params.note.clone(),
        actual_video_duration_ms: Some(video.unwrap_or(0)),
        actual_log_duration_ms: Some(logs.duration_ms),
        video_frame_count: summary.frame_count,
        log_count: Some(logs.count),
        unanchored_log_count: Some(logs.unanchored),
        truncated_unanchored_count: Some(manifest.unanchored_summary.truncated_count),
        discontinuity_count: Some(discontinuity_count),
        window_shortfall: Some(window_shortfall),
        log_coverage_ms,
        video_coverage_ms: video_coverage_ms.filter(|_| full),
        carried_in_frame: full.then_some(carried_in_frame),
        static_hold_ms: full.then_some(static_hold_ms),
        info_codes: Some(info_codes),
        ..Default::default()
    };
    evidence.set_warnings(evidence_warnings(WarningFlags {
        video_shortfall,
        log_shortfall,
        logcat_overflow: completeness.overflow_events > 0
            || completeness.status == Some(LogCompleteness::OverflowDetected),
        unanchored_truncated: completeness.unanchored_truncated_count > 0,
        audio_absent: full && summary.has_audio != Some(true),
        stream_discontinuity: discontinuity_count > 0,
        tail_unconfirmed: params.tail_may_be_incomplete,
    }));
    Ok((evidence, summary))
}

fn saved_evidence(
    path: &Path,
    params: &SaveJobMaterializeParams,
) -> (
    crate::protocol::SaveEvidence,
    Option<crate::protocol::CatalogSummary>,
) {
    match measure_saved_evidence(path, params) {
        Ok((evidence, summary)) => (evidence, Some(summary)),
        Err(e) => {
            let mut evidence = crate::protocol::SaveEvidence {
                note: params.note.clone(),
                ..Default::default()
            };
            evidence.set_warnings(vec![crate::protocol::WarningDetail {
                code: "evidence_unavailable".into(),
                message: format!("保存済み記録の集計に失敗しました: {e}"),
                details: None,
            }]);
            (evidence, None)
        }
    }
}

fn begin_materialization(catalog: &Catalog, job_id: &str) -> Result<CatalogSaveJob, DaemonError> {
    let now_utc = utc_now_iso8601();
    let job_record = catalog
        .save_job(job_id)?
        .ok_or_else(|| DaemonError::NotFound {
            entity: NotFoundEntity::Job,
            detail: format!("save job `{job_id}` not found"),
        })?;

    Ok(match job_record.job.state {
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
    })
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
    let job_record = begin_materialization(catalog, job_id)?;
    // From here on the job is `Materializing`: no `?` may leave it there.
    let dest_path = PathBuf::from(&job_record.job.destination_path);
    let mut params = match read_job_params(&plan.data_dir, job_id) {
        Ok(params) => params,
        Err(err) => {
            return fail_job(
                catalog,
                job_id,
                &err.to_string(),
                crate::save_policy::failure_class(SaveFailureStage::Pin, &err),
                plan.rescue_after_secs,
                None,
                None,
            );
        }
    };
    // The catalog's K5 destination is durable across a crash during retarget.
    // A published WORM and its separately rescued copy have distinct bundle IDs.
    if let Some(id) = dest_path.file_name().and_then(|name| name.to_str()) {
        id.clone_into(&mut params.bundle_id);
    }
    let materialized = match materialize_from_chunks(plan, job_id, &dest_path, &params) {
        Ok(m) => m,
        Err((stage, err)) => {
            return fail_job(
                catalog,
                job_id,
                &err.to_string(),
                crate::save_policy::failure_class(stage, &err),
                plan.rescue_after_secs,
                Some(&params),
                None,
            );
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
        if let Some(summary) = &materialized.summary {
            catalog.record_bundle_summary(&materialized.bundle_id, summary)?;
        }
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
                evidence: materialized.evidence,
                requested_duration_secs: Some(params.duration_sec),
                job: save_job_to_view(&completed, plan.rescue_after_secs),
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
            crate::save_policy::failure_class(SaveFailureStage::CatalogRegister, &err),
            plan.rescue_after_secs,
            Some(&params),
            materialized.warning,
        ).map(|mut outcome| { outcome.evidence = materialized.evidence; outcome }),
    }
}

fn fail_job(
    catalog: &Catalog,
    job_id: &str,
    message: &str,
    class: SaveFailureClass,
    rescue_after_secs: u64,
    params: Option<&SaveJobMaterializeParams>,
    warning: Option<String>,
) -> Result<SaveOutcomeResponse, DaemonError> {
    let failed = catalog.mark_failed_classified(job_id, message, class, &utc_now_iso8601())?;
    Ok(SaveOutcomeResponse {
        evidence: crate::protocol::SaveEvidence {
            note: params.and_then(|p| p.note.clone()),
            ..Default::default()
        },
        requested_duration_secs: params.map(|p| p.duration_sec),
        job: save_job_to_view(&failed, rescue_after_secs),
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
) -> Result<Materialized, (SaveFailureStage, DaemonError)> {
    if let Some(manifest) =
        arb_storage::bundle_io::existing_committed_manifest(dest_bundle_dir, &params.bundle_id)
    {
        let (evidence, summary) = saved_evidence(dest_bundle_dir, params);
        return Ok(Materialized {
            evidence,
            summary,
            bundle_id: manifest.bundle_id,
            unanchored_summary: manifest.unanchored_summary,
            warning: None,
        });
    }
    if FaultInjection::fire(&plan.faults.deny_output_after_pin_once) {
        return Err((
            SaveFailureStage::Materialize,
            std::io::Error::from(std::io::ErrorKind::PermissionDenied).into(),
        ));
    }
    let mut replayed = ReplayCollector::new(params);
    let ring_dir = plan.data_dir.join("ring");
    let mut readable = false;
    for (id, desc) in &plan.chunks {
        let events = match desc.as_ref().map(|chunk| chunk.read_verified(&ring_dir)) {
            Some(Ok(bytes)) => decode_ring_chunk(&bytes).ok(),
            Some(Err(StorageError::Io { source, .. }))
                if source.kind() != std::io::ErrorKind::NotFound =>
            {
                return Err((SaveFailureStage::Pin, source.into()));
            }
            _ => None,
        };
        if let Some(events) = events {
            readable = true;
            for event in events {
                if let CaptureEvent::LogOverflowStats { payload, .. } = &event
                    && !(params.window.start_boottime_ns()..=params.window.end_boottime_ns())
                        .contains(&payload.boottime_ns)
                {
                    continue;
                }
                if event_boot_id(&event).is_some_and(|boot| boot != params.window.boot_id()) {
                    continue;
                }
                replayed.ingest_one(event);
            }
        } else {
            let (start, end) = desc.as_ref().and_then(|c| c.capture.as_ref()).map_or(
                (
                    params.window.ingest_start_cont_ns(),
                    params.window.ingest_end_cont_ns(),
                ),
                |info| info.ingest_range,
            );
            replayed.discontinuities.push(stream_gap(
                start,
                end,
                format!("チャンク {id} の欠損または整合性エラー"),
            ));
        }
    }
    if !readable {
        return Err((
            SaveFailureStage::Pin,
            DaemonError::SaveFailed("保存に必要な全チャンクが欠損または破損しています。".into()),
        ));
    }
    let draft = replayed
        .build_draft(job_id, params, &plan.data_dir)
        .map_err(|err| (SaveFailureStage::Pin, err))?;
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
        Err(err) => return Err((SaveFailureStage::Materialize, err.into())),
    };
    let (evidence, summary) = saved_evidence(dest_bundle_dir, params);
    Ok(Materialized {
        bundle_id,
        summary,
        evidence,
        unanchored_summary,
        warning,
    })
}

/// Replays pinned [`CaptureEvent`] records through [`ClockTimeline`] and the
/// 3-valued [`arb_core::selection`] engine to build a [`V2BundleDraft`].
struct ReplayCollector {
    raw_input_window: std::ops::RangeInclusive<i64>,
    raw_input_events: u64,
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
    dropped_count: Option<u64>,
    touch_normalizer: TouchNormalizer,
    /// In-progress touch strokes keyed by pointer id (T-6H item 2 gesture classifier), flushed
    /// into `raw_taps` on `UP`/`CANCEL` or, for any still-active stroke, in [`Self::build_draft`]
    /// (end-of-stream / disconnect / stop) so no gesture is ever silently lost.
    active_strokes: BTreeMap<i32, ActiveStroke>,
    /// Resolution-aware `Tap` vs `Swipe` touch slop, resolved once from `params.display_width`/
    /// `display_height` at construction (`arb_core::gesture::touch_slop`).
    touch_slop_px: f64,
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
    /// `main[0]` was captured before the window and is displayed from `W0` (#564).
    carried_in: Option<CarriedInFrame>,
}

/// Explicit MP4 timing for the first and last saved samples (#564).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct VideoMuxTiming {
    /// Device-mono PTS at which the carried-in first sample starts being displayed (`W0`).
    head_display_pts_ns: Option<i64>,
    /// Display length of the last sample in `MP4_TIMESCALE` ticks, held until coverage ends.
    last_sample_ticks: Option<u32>,
}

/// Display start of an aligned frame on `DeviceBoottime` (`Anchored` center / `Bounded` low).
fn aligned_boottime_ns(alignment: &TimeAlignment) -> Option<i64> {
    match alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns, ..
        } => Some(*min_boottime_ns),
        _ => None,
    }
}

fn video_coverage_key(item: &AlignedVideoItem) -> VideoCoverageKey {
    VideoCoverageKey {
        boot_id: item.record.boot_id.clone(),
        width: item.dimensions.0,
        height: item.dimensions.1,
    }
}

/// Picks the frame on screen at `W0` (#564, K14) among the frames before the first in-window
/// frame, or `None` without continuous video coverage from that frame to `W0`.
///
/// Returns the carried frame and the IDR that starts its GOP. A frame is eligible only when it
/// is confidently before the window on the same boot; the decode chain from its IDR must stay on
/// one boot and one resolution, and with in-window frames it must be their immediate
/// predecessor so the saved GOP has no holes.
fn carry_in_frame(
    aligned: &[AlignedVideoItem],
    first_in: Option<usize>,
    params: &SaveJobMaterializeParams,
) -> Option<(usize, usize)> {
    let coverage = params.coverage.as_ref()?;
    let window = &params.window;
    let prefix_end = first_in.unwrap_or(aligned.len());
    let w0_bt = window.start_boottime_ns();
    let candidates: Vec<CarryCandidate<Option<VideoCoverageKey>>> = aligned[..prefix_end]
        .iter()
        .map(|v| {
            let eligible = v.record.boot_id == window.boot_id()
                && arb_core::selection::classify_record(v, window)
                    == arb_core::selection::Membership::OutOfRange
                && aligned_boottime_ns(&v.record.alignment).is_some_and(|bt| bt < w0_bt);
            CarryCandidate {
                ts_ns: v.record.pc_ingest_cont_ns,
                key: eligible.then(|| video_coverage_key(v)),
            }
        })
        .collect();
    let intervals: Vec<_> = coverage
        .video
        .iter()
        .map(|c| arb_core::coverage::CoverageInterval {
            key: Some(c.key.clone()),
            start_ns: c.start_ns,
            end_ns: c.end_ns,
        })
        .collect();
    let idx = select_carry_in(&candidates, window.ingest_start_cont_ns(), &intervals)?;
    if first_in.is_some_and(|first| idx + 1 != first) {
        return None;
    }
    // `f_prev` is on screen at `W0` only if every later frame of this boot (ingest order is not
    // capture order) is displayed at least one MP4 tick after `W0`.
    let one_tick_ns = i64::from(1_000_000_000_u32 / MP4_TIMESCALE + 1);
    let min_display_ns = w0_bt.saturating_add(one_tick_ns);
    if !aligned[idx + 1..]
        .iter()
        .filter(|v| v.record.boot_id == window.boot_id())
        .all(|v| aligned_boottime_ns(&v.record.alignment).is_some_and(|bt| bt >= min_display_ns))
    {
        return None;
    }
    let gop_start = aligned[..=idx].iter().rposition(|v| v.record.is_keyframe)?;
    aligned[gop_start..=idx]
        .iter()
        .all(|v| {
            v.record.boot_id == aligned[idx].record.boot_id
                && v.dimensions == aligned[idx].dimensions
        })
        .then_some((idx, gop_start))
}

/// Display length of the last saved frame: held until `min(T, video coverage end)` (#564).
///
/// `None` (fixed default sample length) without coverage evidence for that frame.
fn tail_hold_ticks(
    main: &[AlignedVideoItem],
    carried_in: Option<&CarriedInFrame>,
    params: &SaveJobMaterializeParams,
) -> Option<u32> {
    let coverage = params.coverage.as_ref()?;
    let last = main.last()?;
    let window = &params.window;
    let end_pc = covering_end(
        &coverage.video,
        &video_coverage_key(last),
        last.record.pc_ingest_cont_ns,
    )?
    .min(window.ingest_end_cont_ns());
    // The save window fixes one ingest <-> boottime correspondence at `T`.
    let end_bt = end_pc
        .saturating_add(
            window
                .end_boottime_ns()
                .saturating_sub(window.ingest_end_cont_ns()),
        )
        .min(window.end_boottime_ns());
    let start_bt = match carried_in {
        Some(c) if main.len() == 1 => c.display_start_boottime_ns,
        _ => aligned_boottime_ns(&last.record.alignment)?,
    };
    let hold_ns = end_bt.checked_sub(start_bt).filter(|d| *d > 0)?;
    let ticks = (i128::from(hold_ns) * i128::from(MP4_TIMESCALE) + i128::from(NANOS_PER_SEC / 2))
        / i128::from(NANOS_PER_SEC);
    u32::try_from(ticks.max(1)).ok()
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
            audio_config: params.fallback_audio_config.clone(),
            raw_audio: Vec::new(),
            audio_unavailable: false,
            audio_gap: false,
            raw_logs: Vec::new(),
            raw_taps: Vec::new(),
            discontinuities: params.extra_discontinuities.clone(),
            log_completeness: LogCompleteness::Unverified,
            log_overflow_events: 0,
            dropped_count: Some(0),
            raw_input_window: params.window.start_boottime_ns()..=params.window.end_boottime_ns(),
            raw_input_events: 0,
            touch_normalizer: TouchNormalizer::default(),
            active_strokes: BTreeMap::new(),
            touch_slop_px: touch_slop(
                params.display_width.map(f64::from),
                params.display_height.map(f64::from),
            ),
            uid_packages: params.uid_packages.clone(),
        }
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
                    boot_id,
                    boottime_ns: payload.boottime_ns,
                    monotonic_ns: payload.monotonic_ns,
                    realtime_ns: payload.realtime_ns,
                };
                let _ = self
                    .timeline
                    .ingest_device_triple(triple.clone(), pc_cont_ns);
                self.clock_anchors
                    .push(ClockAnchorRecord::DeviceTriple(triple));
            }
            CaptureEvent::SyncProbe { probe, .. } => {
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
                        if let Some(embedded) = embedded_config(&annex_b) {
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
                self.dropped_count = self
                    .dropped_count
                    .zip(payload.dropped_records())
                    .and_then(|(total, delta)| total.checked_add(delta));
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
                mut payload,
                display_value,
                normalized,
                ..
            } => {
                if self
                    .raw_input_window
                    .contains(&payload.device_read_boottime_ns)
                {
                    self.raw_input_events += 1;
                }
                let viewport = Some((u32::from(self.video_width), u32::from(self.video_height)));
                if let Some(t) = normalized {
                    self.ingest_touch_sample(boot_id, pc_cont_ns, &t);
                } else {
                    if let Some(pixel) = display_value {
                        payload.ev_value = pixel;
                    }
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
        let first_in = aligned.iter().position(|v| {
            arb_core::selection::classify_record(v, &params.window)
                == arb_core::selection::Membership::InRange
        });
        let mut preroll = Vec::new();
        let mut carried: Option<(AlignedVideoItem, CarriedInFrame)> = None;
        if let Some((idx, gop_start)) = carry_in_frame(&aligned, first_in, params) {
            preroll.extend(aligned[gop_start..idx].iter_mut().map(|v| RawH264Packet {
                pts_ns: v.record.pts_device_mono_ns,
                is_keyframe: v.record.is_keyframe,
                annex_b: std::mem::take(&mut v.annex_b),
            }));
            let item = &mut aligned[idx];
            let captured_boottime_ns = aligned_boottime_ns(&item.record.alignment).unwrap_or(0);
            carried = Some((
                AlignedVideoItem {
                    dimensions: item.dimensions,
                    // WORM: the capture-time record is kept as captured.
                    record: item.record.clone(),
                    annex_b: std::mem::take(&mut item.annex_b),
                },
                CarriedInFrame {
                    frame_index: 0,
                    captured_boottime_ns,
                    display_start_boottime_ns: params.window.start_boottime_ns(),
                    evidence: CarryInEvidence::Coverage,
                },
            ));
        } else if let Some(first) = first_in
            && !aligned[first].record.is_keyframe
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
        let mut main = part.main;
        let carried_in = carried.map(|(item, meta)| {
            main.insert(0, item);
            meta
        });
        SelectedVideo {
            preroll,
            main,
            unanchored_excluded,
            carried_in,
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
        policy: &SelectionPolicy,
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
        let part = select_records(aligned, &params.window, policy);
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
        let mut discontinuities = crate::media_engine::filter_discontinuities_for_window(
            self.discontinuities,
            &params.window,
        );
        let (final_logs, unanchored_logs, trunc_logs) =
            Self::select_logs(&self.timeline, self.raw_logs, params);

        let tap_budget = params.policy.after_logs(&final_logs);
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
                &tap_budget,
                &mut discontinuities,
            )?
        };

        let unanchored_summary = UnanchoredSummary {
            included_count: u64::try_from(unanchored_logs.saturating_add(media.unanchored_taps))
                .unwrap_or(u64::MAX),
            truncated_count: u64::try_from(
                trunc_logs
                    .saturating_add(media.trunc_taps)
                    .saturating_add(media.media_unanchored_excluded),
            )
            .unwrap_or(u64::MAX),
        };

        let frame_count = u64::try_from(media.frames.len()).unwrap_or(0);
        let (display_width, display_height) = if params.mode == SaveMode::LogsOnly {
            (None, None)
        } else {
            (params.display_width, params.display_height)
        };
        let (log_completeness, dropped_count) =
            saved_log_completeness(self.log_completeness, self.dropped_count, &discontinuities);
        let streams = build_streams_summary(&StreamSummaryCounts {
            carried_in: media.carried_in.clone(),
            frame_count,
            video_status: media.video_status,
            display_width,
            display_height,
            audio_status: media.audio_status,
            logs_count: final_logs.len(),
            log_completeness,
            log_overflow_events: self.log_overflow_events,
            dropped_count,
            input_channel: params.input_channel,
            raw_input_events: (params.mode != SaveMode::LogsOnly
                && params.input_channel != Some(PhysicalStatus::Disabled))
            .then_some(self.raw_input_events),
            taps_count: media.taps.len(),
            tail_may_be_incomplete: params.tail_may_be_incomplete,
            av_lead_in_offset_ns: media.av_lead_in_offset_ns,
        });

        let epochs = vec![EpochSpan {
            boot_id: params.window.boot_id().to_owned(),
            start_boottime_ns: params.window.start_boottime_ns(),
            end_boottime_ns: params.window.end_boottime_ns(),
        }];

        Ok(V2BundleDraft {
            memo: params.note.clone(),
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
    carried_in: Option<CarriedInFrame>,
    video_mp4: Option<MediaPayload>,
    audio_m4a: Option<MediaPayload>,
    frames: Vec<FrameRecord>,
    taps: Vec<TapRecord>,
    video_status: StreamStatus,
    audio_status: StreamStatus,
    av_lead_in_offset_ns: i64,
    unanchored_taps: usize,
    trunc_taps: usize,
    media_unanchored_excluded: usize,
}

impl SelectedMediaTracks {
    const fn logs_only() -> Self {
        Self {
            bundle_kind: BundleKind::LogsOnly,
            carried_in: None,
            video_mp4: None,
            audio_m4a: None,
            frames: Vec::new(),
            taps: Vec::new(),
            video_status: StreamStatus::Absent,
            audio_status: StreamStatus::Absent,
            av_lead_in_offset_ns: 0,
            unanchored_taps: 0,
            trunc_taps: 0,
            media_unanchored_excluded: 0,
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
    tap_budget: &SelectionPolicy,
    discontinuities: &mut Vec<DiscontinuityEvent>,
) -> Result<SelectedMediaTracks, DaemonError> {
    let selected_video = ReplayCollector::select_video(timeline, raw_video, params);
    let selected_audio = ReplayCollector::select_audio(timeline, raw_audio, params);
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
    let carried_in = selected_video.carried_in.clone();
    let timing = VideoMuxTiming {
        head_display_pts_ns: carried_in.as_ref().and_then(|c| {
            selected_video.main.first().map(|f| {
                f.record
                    .pts_device_mono_ns
                    .saturating_add(c.display_start_boottime_ns - c.captured_boottime_ns)
            })
        }),
        last_sample_ticks: tail_hold_ticks(&selected_video.main, carried_in.as_ref(), params),
    };
    let head_span = selected_video
        .main
        .iter()
        .map(|v| v.record.pc_ingest_cont_ns)
        .collect::<Vec<_>>();
    let muxed = mux_selected_video(
        job_id,
        data_dir,
        video_width,
        video_height,
        selected_video.preroll,
        selected_video.main,
        timing,
    )?;
    if muxed.dropped_head > 0 {
        push_orphan_head_gap(discontinuities, params, &head_span, muxed.dropped_head);
    }
    // A carried-in frame exists only when the track was actually written with it.
    let carried_in = carried_in.filter(|_| muxed.mp4.is_some() && muxed.dropped_head == 0);
    // R4 M4: the A/V origin is the first frame actually written to `video.mp4`.
    let av_lead_in_offset_ns =
        av_lead_in_offset_ns(muxed.frames.first(), carried_in.as_ref(), &selected_audio);
    let (video_mp4, frames, video_status) = (muxed.mp4, muxed.frames, muxed.status);
    let (audio_m4a, audio_status) = mux_selected_audio(
        job_id,
        data_dir,
        audio_config,
        audio_continuity,
        selected_audio.main,
    )?;
    let (taps, unanchored_taps, trunc_taps) =
        ReplayCollector::select_taps(timeline, raw_taps, params, tap_budget);
    Ok(SelectedMediaTracks {
        bundle_kind: BundleKind::Full,
        carried_in,
        video_mp4,
        audio_m4a,
        frames,
        taps,
        video_status,
        audio_status,
        av_lead_in_offset_ns,
        unanchored_taps,
        trunc_taps,
        media_unanchored_excluded: selected_video
            .unanchored_excluded
            .saturating_add(selected_audio.unanchored_excluded),
    })
}

/// R4 M4: exact lead-in skew `D` between the two rebased file timelines —
/// `first_selected_audio_pts_device_mono_ns - first_written_video_pts_device_mono_ns`.
/// `video.mp4` pts0 = first written frame's device-mono pts (after any orphan head was
/// dropped, K15); `audio.m4a` pts0 = first selected packet's device-mono pts
/// (`AacPacket::pts_us` is microseconds on the same clock — `select_audio` maps
/// `pts_us * 1000` as device-mono ns). `0` when either stream has nothing (no shared origin).
fn av_lead_in_offset_ns(
    first_video: Option<&FrameRecord>,
    carried_in: Option<&CarriedInFrame>,
    audio: &SelectedAudio,
) -> i64 {
    match (audio.main.first(), first_video) {
        (Some(first_audio), Some(first_video)) => {
            // A carried-in frame starts its display (media time 0) at `W0`.
            let video_origin = first_video.pts_device_mono_ns.saturating_add(
                carried_in.map_or(0, |c| c.display_start_boottime_ns - c.captured_boottime_ns),
            );
            first_audio
                .pts_us
                .saturating_mul(1_000)
                .saturating_sub(video_origin)
        }
        _ => 0,
    }
}

/// Records the orphan head frames dropped from the saved video (K15) as a video stream gap,
/// from the first dropped frame to the first written one (or the last dropped one).
fn push_orphan_head_gap(
    discontinuities: &mut Vec<DiscontinuityEvent>,
    params: &SaveJobMaterializeParams,
    ingest_ns: &[i64],
    dropped: usize,
) {
    let start = ingest_ns
        .first()
        .copied()
        .unwrap_or_else(|| params.window.ingest_start_cont_ns());
    let end = ingest_ns
        .get(dropped)
        .or_else(|| ingest_ns.last())
        .copied()
        .unwrap_or(start)
        .max(start);
    discontinuities.push(DiscontinuityEvent {
        kind: DiscontinuityKind::StreamGap,
        stream: Some(StreamKind::Video),
        boot_id: Some(params.window.boot_id().to_owned()),
        start_boottime_ns: None,
        end_boottime_ns: None,
        pc_cont_start_ns: start,
        pc_cont_end_ns: Some(end),
        delta_ns: None,
        detail: Some(format!(
            "保存範囲の先頭の映像フレーム {dropped} 枚は参照する IDR がリングから失われ復号できないため破棄しました"
        )),
    });
}

struct StreamSummaryCounts {
    carried_in: Option<CarriedInFrame>,
    frame_count: u64,
    video_status: StreamStatus,
    display_width: Option<u32>,
    display_height: Option<u32>,
    audio_status: StreamStatus,
    logs_count: usize,
    log_completeness: LogCompleteness,
    log_overflow_events: u64,
    dropped_count: Option<u64>,
    raw_input_events: Option<u64>,
    input_channel: Option<PhysicalStatus>,
    taps_count: usize,
    tail_may_be_incomplete: bool,
    av_lead_in_offset_ns: i64,
}

fn saved_log_completeness(
    device_completeness: LogCompleteness,
    device_dropped_count: Option<u64>,
    discontinuities: &[DiscontinuityEvent],
) -> (LogCompleteness, Option<u64>) {
    // A device probe cannot account for data lost between the device and this bundle.
    let log_gap = discontinuities.iter().any(|d| {
        matches!(d.stream, None | Some(StreamKind::Logcat))
            && matches!(
                d.kind,
                DiscontinuityKind::StreamGap
                    | DiscontinuityKind::ConnectionLost
                    | DiscontinuityKind::StoragePressureStop
            )
    });
    let dropped_count = device_dropped_count
        .filter(|_| !log_gap && device_completeness != LogCompleteness::Unverified);
    let log_completeness = match (device_completeness, dropped_count) {
        (LogCompleteness::VerifiedNoOverflow, None) => LogCompleteness::Unverified,
        (completeness, _) => completeness,
    };
    (log_completeness, dropped_count)
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
            carried_in: counts.carried_in.clone(),
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
            dropped_count: counts.dropped_count,
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
            input_events: counts.raw_input_events,
            status: if counts.raw_input_events.is_none() && counts.taps_count == 0 {
                StreamStatus::Excluded
            } else if counts.taps_count == 0 && counts.raw_input_events == Some(0) {
                if counts.input_channel == Some(PhysicalStatus::Unavailable) {
                    StreamStatus::Partial
                } else {
                    StreamStatus::Absent
                }
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
pub(crate) struct TempFileGuard(pub(crate) PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Saved video track: the written `video.mp4`, its frame rows, and how many orphan head frames
/// were dropped because no decoder could show them (K15).
#[derive(Debug)]
struct MuxedVideo {
    mp4: Option<MediaPayload>,
    frames: Vec<FrameRecord>,
    status: StreamStatus,
    dropped_head: usize,
}

impl MuxedVideo {
    /// No decodable frame: nothing is declared, so no frame rows are kept (K15).
    const fn absent(dropped_head: usize) -> Self {
        Self {
            mp4: None,
            frames: Vec::new(),
            status: StreamStatus::Absent,
            dropped_head,
        }
    }
}

/// Muxes `packets` (preroll, then the selected frames from `trim_start`) into an MP4 under
/// `tmp_dir` and returns the path of the trimmed track.
fn mux_video_attempt(
    tmp_mp4: &Path,
    trimmed: &Path,
    width: u16,
    height: u16,
    packets: &[RawH264Packet],
    trim_start: u64,
    last_sample_ticks: Option<u32>,
) -> Result<PathBuf, MediaError> {
    let decodable_head = packets.first().is_some_and(|p| {
        arb_media::mp4::split_annexb_nals(&p.annex_b)
            .iter()
            .any(|n| n.first().is_some_and(|b| b & 31 == 5))
    });
    if !decodable_head {
        return Err(MediaError::InvalidContainer("missing initial IDR".into()));
    }
    let report = mux_raw_h264_to_mp4_with_last_duration(
        tmp_mp4,
        width.max(16),
        height.max(16),
        MP4_TIMESCALE,
        MP4_DEFAULT_FRAME_DURATION_TICKS,
        last_sample_ticks,
        packets,
    )?;
    if trim_start == 0 {
        return Ok(tmp_mp4.to_path_buf());
    }
    arb_media::trim::trim_video(
        tmp_mp4,
        trimmed,
        trim_start,
        report.frame_count - 1,
        &arb_media::codec::OpenH264Backend::new(),
    )?;
    Ok(trimmed.to_path_buf())
}

/// Writes the selected frames as `video.mp4` (K15).
///
/// The head is muxed with its preroll first. When no decoder can start there (the IDR was
/// evicted from the ring, or the reference chain is broken), the orphan head frames up to the
/// first selected keyframe are dropped and the decodable suffix is written as `Partial`. When
/// no selected frame is decodable, the track is `Absent` with no frame rows, so a bundle never
/// declares video without `video.mp4`. I/O and unexpected mux failures propagate so Phase 2
/// ends in `FailedRetryable` with the pins kept (R3 M3).
fn mux_selected_video(
    job_id: &str,
    data_dir: &Path,
    width: u16,
    height: u16,
    mut packets: Vec<RawH264Packet>,
    mut selected: Vec<AlignedVideoItem>,
    timing: VideoMuxTiming,
) -> Result<MuxedVideo, DaemonError> {
    if selected.is_empty() {
        return Ok(MuxedVideo::absent(0));
    }

    let mut trim_start = packets.len();
    let (width, height) = selected
        .iter()
        .map(|v| v.dimensions)
        .max_by_key(|&(w, h)| u32::from(w) * u32::from(h))
        .unwrap_or((width, height));
    packets.extend(selected.iter_mut().enumerate().map(|(idx, item)| {
        RawH264Packet {
            annex_b: std::mem::take(&mut item.annex_b),
            // Only the muxed display time of a carried-in frame moves to `W0`; its record keeps
            // the capture PTS.
            pts_ns: timing
                .head_display_pts_ns
                .filter(|_| idx == 0)
                .unwrap_or(item.record.pts_device_mono_ns),
            is_keyframe: item.record.is_keyframe,
        }
    }));

    let tmp_dir = data_dir.join("tmp");
    fs::create_dir_all(&tmp_dir)?;
    let tmp_mp4 = tmp_dir.join(format!("mux-{job_id}.mp4"));
    let _tmp_guard = TempFileGuard(tmp_mp4.clone());
    let trimmed = tmp_dir.join(format!("head-{job_id}.mp4"));
    let _trim_guard = TempFileGuard(trimmed.clone());

    let attempt = |packets: &[RawH264Packet], trim_start: usize| {
        mux_video_attempt(
            &tmp_mp4,
            &trimmed,
            width,
            height,
            packets,
            u64::try_from(trim_start).unwrap_or(u64::MAX),
            timing.last_sample_ticks,
        )
    };
    let mut dropped_head = 0;
    let mut muxed = attempt(&packets, trim_start);
    if matches!(
        muxed,
        Err(MediaError::InvalidContainer(_) | MediaError::Codec(_))
    ) {
        // The head cannot be decoded: restart from the first selected keyframe, without the
        // preroll, and drop the orphan frames before it.
        let keys: Vec<bool> = selected.iter().map(|v| v.record.is_keyframe).collect();
        let retry = arb_core::video_commit::decodable_start(&keys, false)
            .filter(|&first| first > 0 || trim_start > 0);
        let Some(first) = retry else {
            return Ok(MuxedVideo::absent(selected.len()));
        };
        packets.drain(..trim_start + first);
        selected.drain(..first);
        trim_start = 0;
        dropped_head = first;
        muxed = attempt(&packets, trim_start);
    }
    match muxed {
        Ok(path) => {
            let track = Mp4VideoTrack::open(&path)?;
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
            if frames.len() != indexed.len() {
                return Err(DaemonError::Internal(format!(
                    "video mux wrote {} samples for {} frames",
                    indexed.len(),
                    frames.len()
                )));
            }
            let mp4_bytes = fs::read(&path)?;
            let (status, _) = arb_core::video_commit::saved_video_track(
                u64::try_from(frames.len() + dropped_head).unwrap_or(u64::MAX),
                u64::try_from(dropped_head).unwrap_or(u64::MAX),
                true,
            );
            Ok(MuxedVideo {
                mp4: Some(MediaPayload::Bytes(mp4_bytes)),
                frames,
                status,
                dropped_head,
            })
        }
        // Only an elementary stream without decodable slices (synthetic stub streams, or a
        // suffix whose IDR is unusable) ends with no video at all.
        Err(MediaError::InvalidContainer(_) | MediaError::Codec(_)) => {
            Ok(MuxedVideo::absent(dropped_head + selected.len()))
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
pub fn save_job_to_view(record: &CatalogSaveJob, rescue_after_secs: u64) -> SaveJobView {
    SaveJobView {
        job_id: record.job.job_id.clone(),
        state: record.job.state,
        trigger: record.trigger_kind,
        destination_path: PathBuf::from(&record.job.destination_path),
        pinned_chunk_ids: record.job.pinned_chunk_ids.clone(),
        pinned_chunk_count: record.job.pinned_chunk_ids.len(),
        failure_class: record.failure_class,
        recommended_action: record
            .failure_class
            .map(SaveFailureClass::recommended_action),
        rescue_attempted: record.rescue_attempted,
        rescue_eligible_at_utc: crate::save_policy::eligible_ms(record, rescue_after_secs)
            .map(arb_core::time::format_unix_ms_iso8601),
        last_error: record.job.last_error.clone(),
        created_at_utc: record.created_at_utc.clone(),
        updated_at_utc: record.updated_at_utc.clone(),
    }
}

/// Cause for an `AudioUnavailable` event whose sender did not already record one.
fn default_audio_unavailable_cause(
    status: AudioTunnelStatus,
    mode: AudioCaptureMode,
) -> AudioCause {
    match status {
        // Only a known level below the floor is an OS limitation (PDD G8-1 / G8-2).
        AudioTunnelStatus::UnavailableApiLevel { api_level, .. }
            if mode.unsupported_on(api_level) =>
        {
            AudioCause::ApiBelowMin
        }
        AudioTunnelStatus::DisabledByDevice => AudioCause::DeviceCaptureFailed,
        _ => AudioCause::Unknown,
    }
}

fn default_audio_unavailable_reason(status: AudioTunnelStatus, mode: AudioCaptureMode) -> String {
    match status {
        AudioTunnelStatus::UnavailableApiLevel { api_level, .. } => {
            arb_core::audio_policy::below_min_reason(mode, api_level).unwrap_or_else(|| {
                format!(
                    "Audio capture unavailable on Android API {api_level}. Continuing with video + logs + taps."
                )
            })
        }
        AudioTunnelStatus::DisabledByDevice => {
            arb_core::audio_policy::runtime_failure_reason(AudioCause::DeviceCaptureFailed, None, None)
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
        arb_os::fs::replace_file(&tmp, &path)?;
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
    let bytes = fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => DaemonError::SaveFailed(format!(
            "materialization parameters missing for save job `{job_id}`"
        )),
        _ => err.into(),
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

fn event_boot_id(event: &CaptureEvent) -> Option<&str> {
    match event {
        CaptureEvent::Hello { payload, .. } => Some(&payload.boot_id),
        CaptureEvent::SyncProbe { probe, .. } => Some(&probe.boot_id),
        CaptureEvent::Heartbeat { boot_id, .. }
        | CaptureEvent::VideoSession { boot_id, .. }
        | CaptureEvent::VideoFrame { boot_id, .. }
        | CaptureEvent::AudioConfig { boot_id, .. }
        | CaptureEvent::AudioFrame { boot_id, .. }
        | CaptureEvent::AudioUnavailable { boot_id, .. }
        | CaptureEvent::LogRecord { boot_id, .. }
        | CaptureEvent::LogOverflowStats { boot_id, .. }
        | CaptureEvent::TouchEvent { boot_id, .. }
        | CaptureEvent::StreamState { boot_id, .. }
        | CaptureEvent::StaticHold { boot_id, .. }
        | CaptureEvent::ConnectionRestored { boot_id, .. } => Some(boot_id),
        CaptureEvent::Discontinuity(d) => d.boot_id.as_deref(),
        _ => None,
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
#[path = "capture_pdd_g5_tests.rs"]
mod pdd_g5_tests;

#[cfg(test)]
#[path = "capture_coverage_tests.rs"]
mod coverage_tests;

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
    use arb_storage::ring::{DiskSpaceProvider, RingChunkKind};
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

    /// K15: a broken reference chain with no keyframe in the window leaves nothing decodable,
    /// so the track is `Absent` with no frame rows instead of a file-less `Partial`.
    #[test]
    fn missing_reference_before_window_without_keyframe_reports_absent_video() {
        let (_, draft) = draft_for_video_window(
            &[(64, 64, 30, 7)],
            79_000_000,
            121_000_000,
            Some(40_000_000),
        );
        let video = draft.streams.video.unwrap();
        assert_eq!(video.status, StreamStatus::Absent);
        assert_eq!(video.frame_count, 0);
        assert_eq!(draft.frames.len(), 0);
        assert!(draft.video_mp4.is_none());
    }

    /// K15: with a keyframe later in the window, the frames with a broken reference chain are
    /// dropped and the decodable suffix is written as `Partial`.
    #[test]
    fn missing_reference_before_window_keeps_the_decodable_suffix() {
        let (_, draft) = draft_for_video_window(
            &[(64, 64, 30, 7)],
            79_000_000,
            241_000_000,
            Some(40_000_000),
        );
        let video = draft.streams.video.clone().unwrap();
        assert_eq!(video.status, StreamStatus::Partial);
        // Frames at 80 / 120 / 160 ms are dropped; the IDR at 200 ms and 240 ms are written.
        assert_eq!(video.frame_count, 2);
        assert_eq!(draft.frames.len(), 2);
        assert!(draft.frames[0].is_keyframe);
        assert!(draft.video_mp4.is_some());
        assert!(draft.discontinuities.iter().any(|d| {
            d.stream == Some(super::StreamKind::Video)
                && d.detail.as_deref().is_some_and(|m| m.contains("3 枚"))
        }));
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
        let err = mux_selected_video(
            "job-m3",
            tmp.path(),
            64,
            64,
            Vec::new(),
            real_h264_items(),
            super::VideoMuxTiming::default(),
        )
        .expect_err("I/O mux failure must propagate");
        assert!(
            matches!(err, DaemonError::Media(MediaError::Io(_))),
            "expected MediaError::Io, got {err:?}"
        );
    }

    #[test]
    fn r3_m3_stub_stream_without_parameter_sets_falls_back_to_absent() {
        let tmp = TempDir::new().expect("tempdir");
        let stub = aligned_items(vec![arb_media::mp4::RawH264Packet {
            annex_b: vec![0, 0, 0, 1, 0x09, 0xF0],
            pts_ns: 1,
            is_keyframe: true,
        }]);
        let muxed = mux_selected_video(
            "job-stub",
            tmp.path(),
            64,
            64,
            Vec::new(),
            stub,
            super::VideoMuxTiming::default(),
        )
        .expect("stub stream must fall back cleanly");
        assert!(muxed.mp4.is_none(), "stub stream must not produce an MP4");
        // K15: no frame rows may be declared without `video.mp4`.
        assert!(muxed.frames.is_empty(), "no frame rows without a file");
        assert_eq!(
            muxed.status,
            StreamStatus::Absent,
            "stub video must be Absent"
        );
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

    fn open_test_pipeline(tmp: &TempDir) -> CapturePipeline {
        CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .unwrap()
    }

    #[test]
    fn reported_evidence_shortfall_matches_shared_contract() {
        let table: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/specs/ui-feedback-contract-table.json"
        ))
        .expect("table");
        for row in table["capture_evidence_rows"].as_array().expect("rows") {
            let mode = if row["logs_only"] == true {
                SaveMode::LogsOnly
            } else {
                SaveMode::Full
            };
            let (v, l) = super::stream_shortfalls(
                mode,
                row["requested_duration_secs"].as_u64().unwrap(),
                row["actual_video_duration_ms"].as_u64(),
                row["actual_log_duration_ms"].as_u64().unwrap(),
                row["log_coverage_ms"].as_u64(),
            );
            assert_eq!(row["video_shortfall"], v);
            assert_eq!(row["log_shortfall"], l);
            assert_eq!(
                v || l,
                row["window_shortfall"].as_bool().expect("shortfall"),
                "row {row}"
            );
            // #570: the shared rows' details are exactly what the single warning source emits,
            // and the flat arrays are its one-to-one projection.
            let has = |code: &str| {
                row["warning_codes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c == code)
            };
            let details = super::evidence_warnings(super::WarningFlags {
                video_shortfall: v,
                log_shortfall: l,
                logcat_overflow: has("logcat_overflow"),
                unanchored_truncated: has("unanchored_truncated"),
                audio_absent: has("audio_absent"),
                stream_discontinuity: has("stream_discontinuity"),
                tail_unconfirmed: has("tail_unconfirmed"),
            });
            assert_eq!(
                serde_json::to_value(&details).unwrap(),
                row["warning_details"],
                "row {}",
                row["id"]
            );
            let mut evidence = crate::protocol::SaveEvidence::default();
            evidence.set_warnings(details);
            assert_eq!(
                serde_json::to_value(&evidence.warning_codes).unwrap(),
                row["warning_codes"]
            );
            assert_eq!(
                serde_json::to_value(&evidence.warnings).unwrap(),
                row["warnings"]
            );
        }
    }

    #[test]
    fn p1_new_boot_can_save_at_sixty_seconds() {
        let tmp = TempDir::new().unwrap();
        let open = |uptime, boot| {
            CapturePipeline::open_at(
                tmp.path(),
                RingBudgetPolicy::default(),
                SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
                arb_os::clock::HostClockSample {
                    active_mono_ns: uptime,
                    continuous_mono_ns: uptime,
                    wall_epoch_ns: boot + uptime,
                },
            )
            .unwrap()
        };
        let mut cap = open(5 * 86_400_000_000_000, 0);
        cap.begin_session("A");
        cap.ingest_event(heartbeat_at(5 * 86_400_000_000_000))
            .unwrap();
        cap.mark_stopped();
        drop(cap);
        let mut cap = open(60_000_000_000, 5 * 86_400_000_000_000);
        cap.begin_session("A");
        cap.ingest_event(heartbeat_at(60_000_000_000)).unwrap();
        let out = tmp.path().join("bundles");
        let catalog = arb_storage::catalog::Catalog::open(tmp.path().join("cat"), &out).unwrap();
        let pending = cap
            .phase1_pin_with_options(
                10,
                &out,
                SelectionPolicy::default(),
                SaveMode::LogsOnly,
                super::SaveTriggerKind::Manual,
            )
            .unwrap();
        assert_eq!(pending.pinned_chunk_ids.len(), 1);
        let (job, plan) = cap
            .phase1_commit(&catalog, pending, super::TailCompleteness::Complete)
            .unwrap();
        let path = super::phase2_materialize(&plan, &catalog, &job.job.job_id)
            .unwrap()
            .bundle_path
            .unwrap();
        let anchors = arb_storage::bundle_io::V2BundleReader::open(path)
            .unwrap()
            .clock_anchors()
            .unwrap();
        assert_eq!(anchors.len(), 1);
        assert!(
            matches!(&anchors[0], arb_storage::bundle_io::ClockAnchorRecord::DeviceTriple(triple) if triple.boottime_ns == 60_000_000_000)
        );
    }

    #[test]
    fn p1_same_boot_reopen_can_pin_old_recording() {
        let tmp = TempDir::new().unwrap();
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        cap.ingest_event(video_event(100, vec![1])).unwrap();
        cap.mark_stopped();
        drop(cap);
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        assert!(
            cap.phase1_pin(10, tmp.path(), SelectionPolicy::default())
                .is_ok()
        );
    }

    #[test]
    fn p1_pin_persists_replay_params_with_latest_codec_config() {
        let tmp = TempDir::new().unwrap();
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        let config = vec![0, 0, 0, 1, 0x67, 42, 0, 0, 0, 1, 0x68, 1];
        let mut event = video_event(100, config.clone());
        if let CaptureEvent::VideoFrame { packet, .. } = &mut event {
            packet.kind = arb_adb::scrcpy::VideoPacketKind::Config;
        }
        cap.ingest_event(event).unwrap();
        cap.ingest_event(video_event(120, vec![1])).unwrap();
        let pending = cap
            .phase1_pin(10, tmp.path(), SelectionPolicy::default())
            .unwrap();
        assert_eq!(pending.params.fallback_video_config, Some(config));
        let params = super::read_job_params(tmp.path(), pending.job_id()).unwrap();
        assert!(params.tail_may_be_incomplete);
    }

    fn heartbeat_at(time: i64) -> CaptureEvent {
        CaptureEvent::Heartbeat {
            pc_cont_ns: time,
            boot_id: "boot-t".into(),
            payload: arb_adb::wire::HeartbeatPayload {
                boottime_ns: time,
                monotonic_ns: time,
                realtime_ns: time,
                streams: vec![],
            },
        }
    }

    #[test]
    fn late_heartbeat_before_the_save_trigger_does_not_complete_the_tail() {
        use arb_core::selection::{DrainOutcome, evaluate_stream_drain};
        let tmp = TempDir::new().unwrap();
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        cap.ingest_event(heartbeat_at(100_000_000_000)).unwrap();
        cap.ingest_event(video_event(100_800_000_000, vec![1]))
            .unwrap();
        let policy = SelectionPolicy::default();
        let pending = cap.phase1_pin(10, tmp.path(), policy).unwrap();
        let trigger = pending.drain_trigger_boottime_ns().unwrap();
        assert_eq!(trigger, 100_800_000_000);
        cap.ingest_event(heartbeat_at(100_300_000_000)).unwrap();
        assert_eq!(
            evaluate_stream_drain(trigger, cap.drain_watermark_ns(), 300, &policy),
            DrainOutcome::Waiting
        );
        cap.ingest_event(heartbeat_at(trigger)).unwrap();
        assert_eq!(
            evaluate_stream_drain(trigger, cap.drain_watermark_ns(), 800, &policy),
            DrainOutcome::DrainedComplete
        );
    }

    #[test]
    fn p1_audit_drain_watermark_completes_when_idle_input_stream_has_zero_last_pts() {
        use arb_adb::wire::{HeartbeatPayload, StreamHealthEntry, StreamId, StreamLifecycleState};
        use arb_core::selection::{DrainOutcome, evaluate_stream_drain};
        let tmp = TempDir::new().unwrap();
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        cap.ingest_event(heartbeat_at(100_000_000_000)).unwrap();
        cap.ingest_event(video_event(100_500_000_000, vec![1]))
            .unwrap();
        let policy = SelectionPolicy::default();
        let pending = cap.phase1_pin(10, tmp.path(), policy).unwrap();
        let trigger = pending.drain_trigger_boottime_ns().unwrap();
        assert_eq!(trigger, 100_500_000_000);
        // Realistic companion Heartbeat: Control reports previous heartbeat's last_pts_ns,
        // Logcat was quiet for 2s, and Input has emitted no touch events yet (last_pts_ns == 0).
        cap.ingest_event(CaptureEvent::Heartbeat {
            pc_cont_ns: trigger,
            boot_id: "boot-t".into(),
            payload: HeartbeatPayload {
                boottime_ns: trigger,
                monotonic_ns: trigger,
                realtime_ns: trigger,
                streams: vec![
                    StreamHealthEntry {
                        stream_id: StreamId::Control,
                        state: StreamLifecycleState::Running,
                        restart_count: 0,
                        emitted_seq: 2,
                        last_pts_ns: 100_000_000_000,
                    },
                    StreamHealthEntry {
                        stream_id: StreamId::Logcat,
                        state: StreamLifecycleState::Running,
                        restart_count: 0,
                        emitted_seq: 1,
                        last_pts_ns: 98_500_000_000,
                    },
                    StreamHealthEntry {
                        stream_id: StreamId::Input,
                        state: StreamLifecycleState::Running,
                        restart_count: 0,
                        emitted_seq: 0,
                        last_pts_ns: 0,
                    },
                ],
            },
        })
        .unwrap();
        assert_eq!(
            evaluate_stream_drain(trigger, cap.drain_watermark_ns(), 50, &policy),
            DrainOutcome::DrainedComplete,
            "a post-trigger companion heartbeat must complete drain even when Input/Logcat are idle"
        );
    }

    #[test]
    fn p1_audit_post_reboot_evicts_old_boot_chunks_before_new_chunks() {
        let tmp = TempDir::new().unwrap();
        let policy = RingBudgetPolicy {
            max_ring_bytes: 2_500,
            max_ring_duration_ns: u64::MAX,
            min_free_disk_bytes: 0,
        };
        let open = |uptime, boot| {
            CapturePipeline::open_at(
                tmp.path(),
                policy,
                SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
                arb_os::clock::HostClockSample {
                    active_mono_ns: uptime,
                    continuous_mono_ns: uptime,
                    wall_epoch_ns: boot + uptime,
                },
            )
            .unwrap()
        };
        let day5 = 5 * 86_400_000_000_000_i64;
        let mut cap = open(day5, 0);
        cap.begin_session("A");
        cap.ingest_event(video_event(day5, vec![0xAA; 600]))
            .unwrap();
        cap.flush_pending_chunk().unwrap();
        cap.ingest_event(video_event(day5 + 1_000_000_000, vec![0xBB; 600]))
            .unwrap();
        cap.flush_pending_chunk().unwrap();
        // Pin only the first old-boot chunk to verify pinned old chunks remain protected.
        let old_chunks = cap.ring.chunks();
        assert_eq!(old_chunks.len(), 2);
        let pinned_old_id = old_chunks[0].chunk_id.clone();
        cap.ring
            .pin_chunks(
                "old-job",
                tmp.path().join("bundles/old"),
                vec![pinned_old_id.clone()],
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
        drop(cap);

        // PC reboots: uptime drops from 5 days back to 60s.
        let mut cap = open(60_000_000_000, day5);
        assert!(
            cap.ring.descriptor(&pinned_old_id).is_some(),
            "pinned pre-reboot chunk must remain intact for retryable jobs"
        );
        assert_eq!(
            cap.ring.chunks().len(),
            1,
            "unpinned pre-reboot chunk must be purged on reboot so it cannot starve new-boot chunks"
        );
        cap.begin_session("A");
        cap.ingest_event(video_event(60_000_000_000, vec![0xCC; 600]))
            .unwrap();
        cap.flush_pending_chunk().unwrap();
        cap.ingest_event(video_event(65_000_000_000, vec![0xDD; 600]))
            .unwrap();
        cap.flush_pending_chunk().unwrap();
        let new_chunks: Vec<_> = cap
            .ring
            .chunks()
            .into_iter()
            .filter(|c| c.chunk_id != pinned_old_id)
            .collect();
        assert_eq!(
            new_chunks.len(),
            2,
            "both new-boot chunks must survive without being evicted in favor of dead old-boot chunks"
        );
    }

    #[test]
    fn save_after_codec_chunk_eviction_replays_video_and_correct_audio() {
        use arb_adb::scrcpy::{AudioPacket, AudioPacketKind};
        use arb_media::codec::VideoCodecBackend as _;
        let tmp = TempDir::new().unwrap();
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        let source = real_h264_items().remove(0).annex_b;
        // The first IDR supplies embedded parameter sets. Later IDRs omit them.
        cap.ingest_event(video_event(1, source.clone())).unwrap();
        cap.ingest_event(CaptureEvent::AudioConfig {
            pc_cont_ns: 2,
            boot_id: "boot-t".into(),
            packet: AudioPacket {
                kind: AudioPacketKind::Config,
                pts_us: 0,
                data: super::build_aac_lc_audio_specific_config(44_100, 1)
                    .unwrap()
                    .to_vec(),
            },
        })
        .unwrap();
        cap.flush_pending_chunk().unwrap();
        let limit = RingBudgetPolicy::default().max_ring_bytes;
        let removed = cap.ring.enforce_budget(limit, 3).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(cap.ring.chunks().len(), 0);
        cap.video_width = 64;
        cap.video_height = 64;
        cap.ingest_event(heartbeat_at(0)).unwrap();
        let mut picture = vec![];
        for nal in arb_media::mp4::split_annexb_nals(&source) {
            if !matches!(nal[0] & 31, 7 | 8) {
                picture.extend_from_slice(&[0, 0, 0, 1]);
                picture.extend_from_slice(nal);
            }
        }
        cap.ingest_event(video_event(1_000_000, picture)).unwrap();
        cap.ingest_event(CaptureEvent::AudioFrame {
            pc_cont_ns: 1_000_000,
            boot_id: "boot-t".into(),
            packet: AudioPacket {
                kind: AudioPacketKind::Frame,
                pts_us: 1_000,
                data: vec![0x21, 0x10, 0x05, 0x44],
            },
        })
        .unwrap();
        cap.ingest_event(heartbeat_at(2_000_000)).unwrap();
        let out = tmp.path().join("bundles");
        let cat = arb_storage::catalog::Catalog::open(tmp.path().join("cat"), &out).unwrap();
        let (job, plan) = cap
            .phase1_pin_save(
                &cat,
                10,
                &out,
                SelectionPolicy::default(),
                super::TailCompleteness::Complete,
            )
            .unwrap();
        let saved = super::phase2_materialize(&plan, &cat, &job.job.job_id)
            .unwrap()
            .bundle_path
            .unwrap();
        let video = arb_media::mp4::Mp4VideoTrack::open(saved.join("video.mp4")).unwrap();
        let frame = OpenH264Backend::new()
            .decode_frame(&video, 0, PixelFormat::Yuv420p)
            .unwrap();
        assert_eq!((frame.width, frame.height), (64, 64));
        let audio = arb_media::audio::Mp4AudioTrack::open(saved.join("audio.m4a")).unwrap();
        assert_eq!((audio.sample_rate_hz(), audio.channels()), (44_100, 1));
    }

    #[test]
    fn missing_materialization_inputs_report_permanent_failure_and_preserve_pins() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("bundles");
        let catalog = arb_storage::catalog::Catalog::open(tmp.path().join("cat"), &out).unwrap();
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        cap.ingest_event(heartbeat_at(100)).unwrap();
        let (job, plan) = cap
            .phase1_pin_save(
                &catalog,
                10,
                &out,
                SelectionPolicy::default(),
                super::TailCompleteness::Complete,
            )
            .unwrap();
        let id = &job.job.job_id;
        super::remove_job_params(tmp.path(), id).unwrap();
        let failed = super::phase2_materialize(&plan, &catalog, id).unwrap();
        assert_eq!(
            failed.job.failure_class,
            Some(arb_core::save_failure::SaveFailureClass::Integrity)
        );
        assert!(failed.job.rescue_eligible_at_utc.is_none());
        let errors = [
            cap.prepare_retarget(&catalog, id, &out).unwrap_err(),
            cap.ring.read_chunk("missing-chunk").unwrap_err().into(),
            plan.pin_journal.load("missing-journal").unwrap_err().into(),
        ];
        for error in errors {
            let data = error.into_response(1.into()).error.unwrap().data.unwrap();
            assert_eq!(data["error_code"], "SAVE_FAILED");
            assert_eq!(data["retryable"], false);
        }
        assert!(plan.pin_journal.load(id).is_ok());
        assert_eq!(
            catalog.save_job(id).unwrap().unwrap().job.state,
            arb_core::state::SaveJobState::FailedRetryable
        );
    }

    #[test]
    fn provisional_pin_survives_restart_and_one_damaged_chunk_becomes_a_gap() {
        use arb_storage::bundle_io::BundleReader as _;
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("bundles");
        let catalog = arb_storage::catalog::Catalog::open(tmp.path().join("cat"), &out).unwrap();
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        cap.ingest_event(heartbeat_at(100)).unwrap();
        cap.ingest_event(heartbeat_at(200)).unwrap();
        cap.ingest_event(CaptureEvent::LogOverflowStats {
            pc_cont_ns: 200,
            boot_id: "boot-t".into(),
            payload: arb_adb::wire::LogOverflowStatsPayload {
                boottime_ns: 200,
                completeness: arb_adb::wire::WireLogCompleteness::Verified,
                buffers_count: 4,
                pruned_delta: 0,
                pruned_cumulative: 0,
                detail: "zero_pruned_delta".into(),
            },
        })
        .unwrap();
        let mut pending = cap
            .pin_request(
                10,
                &out,
                SelectionPolicy::default(),
                SaveMode::LogsOnly,
                super::SaveTriggerKind::Manual,
                Some("保存メモ".to_owned()),
            )
            .unwrap();
        let job_id = pending.job_id().to_owned();
        let first = cap.ring.descriptor(&pending.pinned_chunk_ids[0]).unwrap();
        std::fs::write(tmp.path().join("ring").join(&first.file_name), b"damaged").unwrap();
        pending.guard.armed = false; // simulate process exit before the SQL transaction
        drop(pending);
        drop(cap);
        let cap = open_test_pipeline(&tmp);
        let pins = cap.ring.pin_journal();
        super::recover_provisional_jobs(tmp.path(), &catalog, pins).unwrap();
        let (recovered, _) = catalog
            .recover_on_startup_with_pins("2026-01-01T00:00:00Z", pins)
            .unwrap();
        assert_eq!(recovered.recovered_job_ids, std::slice::from_ref(&job_id));
        let plan = cap.prepare_retry(&catalog, &job_id).unwrap();
        let path = super::phase2_materialize(&plan, &catalog, &job_id)
            .unwrap()
            .bundle_path
            .unwrap();
        let manifest = arb_storage::bundle_io::read_bundle_manifest(&path).unwrap();
        assert_eq!(manifest["memo"], "保存メモ");
        assert_eq!(manifest["streams"]["logcat"]["completeness"], "unverified");
        assert!(manifest["streams"]["logcat"]["dropped_count"].is_null());
        let saved = arb_storage::bundle_io::V2BundleReader::open(path).unwrap();
        assert_eq!(saved.clock_anchors().unwrap().len(), 1);
        assert!(
            saved
                .discontinuities()
                .unwrap()
                .iter()
                .any(|d| d.kind == DiscontinuityKind::StreamGap && d.pc_cont_start_ns == 100)
        );
        assert_eq!(pins.list_all().unwrap().len(), 0);
    }

    fn assert_rejected_pin_budget(
        state: &Arc<crate::DaemonState>,
        record: &arb_storage::catalog::CatalogSaveJob,
    ) {
        use serde_json::json;
        let id = &record.job.job_id;
        let mut cap = state.lock_capture().unwrap();
        let bytes = cap.ring_stats().unwrap().1;
        let mut policy = cap.ring.policy();
        policy.max_ring_bytes = bytes * 2;
        cap.set_ring_policy(policy);
        assert!(
            cap.check_pin_budget(None, &record.job.pinned_chunk_ids)
                .is_ok()
        ); // shared pins counted once
        policy.max_ring_bytes -= 1;
        cap.set_ring_policy(policy);
        drop(cap);
        let error = state
            .dispatch_guarded("save.logs", json!({"drain_timeout_ms":0}))
            .unwrap_err()
            .into_response(1.into())
            .error
            .unwrap()
            .data
            .unwrap();
        assert_eq!(error["error_code"], "STORAGE_PRESSURE");
        assert_eq!(error["details"]["failed_save_jobs"], 1);
        assert_eq!(error["details"]["max_fraction"], 0.5);
        assert_eq!(state.catalog.list_save_jobs().unwrap().len(), 1);
        state
            .dispatch("save.discard", json!({"job_id":id}))
            .unwrap();
        state
            .lock_capture()
            .unwrap()
            .set_ring_policy(RingBudgetPolicy::default());
    }

    fn assert_rescue_deadline(
        state: &crate::DaemonState,
        job: &serde_json::Value,
        id: &str,
    ) -> i64 {
        let eligible = arb_storage::catalog::parse_utc_timestamp_ns(
            job["rescue_eligible_at_utc"].as_str().unwrap(),
        )
        .unwrap()
            / 1_000_000;
        let failed =
            arb_storage::catalog::parse_utc_timestamp_ns(job["updated_at_utc"].as_str().unwrap())
                .unwrap()
                / 1_000_000;
        assert_eq!(eligible - failed, 900_000);
        let at = i64::try_from(eligible).unwrap();
        state.rescue_failed_jobs(at - 1).unwrap();
        assert!(
            !state
                .catalog
                .save_job(id)
                .unwrap()
                .unwrap()
                .rescue_attempted
        );
        at
    }

    #[test]
    fn pin_budget_never_blocks_a_save_when_no_other_job_holds_pins() {
        use serde_json::json;
        let tmp = TempDir::new().unwrap();
        let paths = arb_os::paths::ArbPaths {
            data_dir: tmp.path().join("data"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: tmp.path().join("run"),
        };
        let server = crate::DaemonServer::start(crate::DaemonConfig::new(paths)).unwrap();
        let state = &server.state;
        {
            let mut cap = state.lock_capture().unwrap();
            cap.begin_session("A");
            cap.ingest_event(heartbeat_at(100)).unwrap();
            let bytes = cap.ring_stats().unwrap().1;
            let mut policy = cap.ring.policy();
            policy.max_ring_bytes = bytes;
            cap.set_ring_policy(policy);
            // The selection alone is 100% of the ring budget.
        }
        let saved = state
            .dispatch_guarded("save.logs", json!({"drain_timeout_ms":0}))
            .unwrap();
        assert_eq!(saved["job"]["state"], "completed");
    }

    #[test]
    fn failed_jobs_rescue_at_900_seconds_once_and_respect_pin_capacity() {
        use serde_json::json;
        use std::sync::atomic::Ordering;
        let tmp = TempDir::new().unwrap();
        let paths = arb_os::paths::ArbPaths {
            data_dir: tmp.path().join("data"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: tmp.path().join("run"),
        };
        let server = crate::DaemonServer::start(crate::DaemonConfig::new(paths)).unwrap();
        let state = &server.state;
        {
            let mut cap = state.lock_capture().unwrap();
            cap.begin_session("A");
            cap.ingest_event(heartbeat_at(100)).unwrap();
        }
        let external = tmp.path().join("external");
        let trigger = || {
            state
                .dispatch(
                    "save.logs",
                    json!({"dest_dir": external,"note":"元メモ","drain_timeout_ms":0}),
                )
                .unwrap()
        };
        for (fail_rescue, published, class, action) in [
            (true, false, "permanent_destination", "retarget"),
            (false, false, "permanent_destination", "retarget"),
            (false, true, "transient", "retry"),
        ] {
            if published {
                state
                    .faults
                    .fail_complete_job_once
                    .store(true, Ordering::Release);
            } else {
                state
                    .faults
                    .deny_output_after_pin_once
                    .store(true, Ordering::Release);
            }
            let job = trigger()["job"].clone();
            let id = job["job_id"].as_str().unwrap();
            assert_eq!(job["failure_class"], class);
            assert_eq!(job["recommended_action"], action);
            let original = Path::new(job["destination_path"].as_str().unwrap());
            let old_manifest = std::fs::read(original.join("manifest.json")).ok();
            let at = assert_rescue_deadline(state, &job, id);
            if fail_rescue {
                state
                    .dispatch("config.update", json!({"failed_job_rescue_after_secs":0}))
                    .unwrap();
                state.rescue_failed_jobs(at).unwrap();
                assert!(
                    !state
                        .catalog
                        .save_job(id)
                        .unwrap()
                        .unwrap()
                        .rescue_attempted
                );
                state
                    .dispatch("config.update", json!({"failed_job_rescue_after_secs":900}))
                    .unwrap();
                state
                    .faults
                    .deny_output_after_pin_once
                    .store(true, Ordering::Release);
            }
            state.rescue_failed_jobs(at).unwrap();
            let record = state.catalog.save_job(id).unwrap().unwrap();
            assert!(record.rescue_attempted);
            assert!(
                Path::new(&record.job.destination_path)
                    .starts_with(state.catalog.managed_bundles_root())
            );
            if fail_rescue {
                assert_ne!(record.job.pinned_chunk_ids, Vec::<String>::new());
                state.rescue_failed_jobs(at + 900_000).unwrap();
                assert_eq!(state.catalog.save_job(id).unwrap().unwrap(), record);
                assert_rejected_pin_budget(state, &record);
            } else {
                assert_eq!(record.job.pinned_chunk_ids, Vec::<String>::new());
                assert_eq!(state.lock_capture().unwrap().ring_stats().unwrap().2, 0);
                let manifest = arb_storage::bundle_io::read_bundle_manifest(Path::new(
                    &record.job.destination_path,
                ))
                .unwrap();
                assert!(manifest.to_string().contains("元メモ"));
                assert!(manifest.to_string().contains("自動救出"));
                if published {
                    assert_eq!(
                        std::fs::read(original.join("manifest.json")).unwrap(),
                        old_manifest.unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn interrupted_crash_autosave_is_requeued_once_on_startup() {
        let tmp = TempDir::new().unwrap();
        let paths = arb_os::paths::ArbPaths {
            data_dir: tmp.path().into(),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: tmp.path().join("run"),
        };
        let mut cap = open_test_pipeline(&tmp);
        cap.begin_session("A");
        cap.ingest_event(heartbeat_at(100)).unwrap();
        let mut pending = cap
            .phase1_pin_with_options(
                10,
                &tmp.path().join("bundles"),
                SelectionPolicy::default(),
                SaveMode::LogsOnly,
                super::SaveTriggerKind::CrashAutoSave,
            )
            .unwrap();
        let id = pending.job_id().to_owned();
        pending.guard.armed = false;
        drop(pending);
        drop(cap);
        let config = crate::DaemonConfig::new(paths);
        let server = crate::DaemonServer::start(config.clone()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let status = server.state.crash_autosave_status.lock().unwrap();
            if status.last_job_id.as_ref() == Some(&id) {
                assert!(status.last_bundle_id.is_some());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "crash job failed: {:?}",
                status.last_error
            );
            drop(status);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        server.shutdown();
        let server = crate::DaemonServer::start(config).unwrap();
        assert!(!server.state.recovery.recovered_job_ids.contains(&id));
        assert_eq!(server.state.catalog.list_save_jobs().unwrap().len(), 1);
        assert_eq!(
            server
                .state
                .crash_jobs
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        server.shutdown();
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
    fn status_reads_incremental_usage_and_session_span_at_scale() {
        let tmp = TempDir::new().unwrap();
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .unwrap();
        for i in 0..1000 {
            pipeline
                .ring
                .write_closed_chunk(
                    format!("c{i}"),
                    RingChunkKind::Video,
                    i * 1_000_000,
                    if i == 0 { 5_000_000_000 } else { 1 },
                    &[1],
                )
                .unwrap();
        }
        let before = pipeline.ring.work_counters();
        for _ in 0..1000 {
            assert_eq!(
                pipeline.ring_stats().unwrap(),
                (1000, 1000, 0),
                "all chunks remain present"
            );
            assert_eq!(
                pipeline.buffered_secs(),
                5,
                "the oldest long chunk ends last"
            );
        }
        assert_eq!(
            pipeline.ring.work_counters(),
            before,
            "status must not request full snapshots or replay journals"
        );
    }

    #[test]
    fn buffered_secs_preserves_same_device_ring_across_stop_start() {
        use crate::protocol::BufferState;
        let tmp = TempDir::new().expect("tempdir");
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .expect("open pipeline");
        assert_eq!(pipeline.buffered_secs(), 0, "fresh empty ring must be 0s");
        assert_eq!(pipeline.status_view().buffer_state, BufferState::Empty);
        assert_eq!(pipeline.status_view().newest_age_secs, None);

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

        assert_eq!(pipeline.status_view().buffer_state, BufferState::Live);
        pipeline.mark_stopped();
        assert_eq!(
            pipeline.status_view().buffer_state,
            BufferState::RetainedAfterStop
        );
        let _ = pipeline.begin_session("serial-buf");
        pipeline
            .ingest_event(video_event(100_000_000_000, vec![0, 0, 0, 1, 0x65]))
            .expect("ingest s2 start");
        pipeline
            .ingest_event(video_event(103_000_000_000, vec![0, 0, 0, 1, 0x41]))
            .expect("ingest s2 end");
        assert_eq!(
            pipeline.buffered_secs(),
            93,
            "same-device restart retains the previous events and the intervening gap"
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
        assert_eq!(
            view.audio_cause,
            Some(crate::protocol::AudioCause::ApiBelowMin),
            "API 28 is the only OS-limitation cause"
        );

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

    /// G8-1 / G8-2 (status view): an `UnavailableApiLevel` event for API >= 33 or an unknown
    /// API never becomes `api_below_min`; the device API level and `logs_only` cause are reported.
    #[test]
    fn g8_status_view_never_reports_api_below_min_for_api_33_plus_or_unknown() {
        use crate::protocol::AudioCause;

        let tmp = TempDir::new().expect("tempdir");
        let mut pipeline = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
        )
        .expect("open pipeline");
        for api_level in [0_u32, 33, 34, u32::MAX] {
            let _ = pipeline.begin_live_session("aos13-dev");
            assert_eq!(
                pipeline.status_view().audio_cause,
                None,
                "fresh session has no cause"
            );
            pipeline
                .ingest_event(CaptureEvent::AudioUnavailable {
                    pc_cont_ns: 1_000,
                    boot_id: "boot-1".to_owned(),
                    status: arb_adb::scrcpy::AudioTunnelStatus::UnavailableApiLevel {
                        api_level,
                        min_required: 33,
                    },
                })
                .expect("ingest AudioUnavailable");
            let view = pipeline.status_view();
            assert_ne!(
                view.audio_cause,
                Some(AudioCause::ApiBelowMin),
                "API {api_level} must not be reported as unsupported"
            );
        }
        let generation = pipeline
            .begin_live_session_with_mode("aos13-dev", arb_core::config::CaptureMode::LogsOnly);
        let view = pipeline.status_view();
        assert_eq!(
            view.audio_cause,
            Some(AudioCause::LogsOnly),
            "logs_only cause"
        );
        pipeline.set_audio_status_for_session(
            generation,
            crate::protocol::AudioCaptureState::Unavailable,
            AudioCause::DeviceCaptureFailed,
            Some("codec_id = 0".to_owned()),
        );
        assert_eq!(
            pipeline.status_view().audio_cause,
            Some(AudioCause::DeviceCaptureFailed),
            "the recorded cause is reported verbatim"
        );
        let json = serde_json::to_value(pipeline.status_view()).expect("serialize");
        assert_eq!(json["audio_cause"], "device_capture_failed", "wire value");
    }

    // -- T-6H item 2: per-pointer stroke accumulation (`ActiveStroke`/`ingest_touch_sample`) --

    fn t6h_gesture_params(
        display_width: Option<u32>,
        display_height: Option<u32>,
    ) -> SaveJobMaterializeParams {
        SaveJobMaterializeParams {
            input_channel: None,
            note: None,
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
            fallback_audio_config: None,
            trigger_kind: arb_core::state::SaveTriggerKind::Manual,
            extra_discontinuities: Vec::new(),
            uid_packages: BTreeMap::new(),
            coverage: None,
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
    fn live_axis_ranges_scale_gesture_slop_and_saved_swipe_path() {
        use super::{
            ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_TRACKING_ID, EV_ABS, EV_SYN, SYN_REPORT,
        };
        use crate::touch_calibration::TouchCalibration;

        let axes = "add device 1: /dev/input/event2\nABS_MT_POSITION_X : value 0, min 0, max 32767\nABS_MT_POSITION_Y : value 0, min 0, max 32767";
        let calibration = TouchCalibration::from_getevent(axes, 1080, 2400);
        let params = t6h_gesture_params(Some(1080), Some(2400));
        let mut collector = ReplayCollector::new(&params);
        for end_y in [16600, 32767] {
            for (idx, (ty, code, value)) in [
                (EV_ABS, ABS_MT_TRACKING_ID, 1),
                (EV_ABS, ABS_MT_POSITION_X, 16384),
                (EV_ABS, ABS_MT_POSITION_Y, 16384),
                (EV_SYN, SYN_REPORT, 0),
                (EV_ABS, ABS_MT_POSITION_Y, end_y),
                (EV_SYN, SYN_REPORT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_SYN, SYN_REPORT, 0),
            ]
            .into_iter()
            .enumerate()
            {
                let t = i64::try_from(idx).unwrap() * 10_000_000;
                let payload = raw_evdev(ty, code, value, t);
                collector.ingest_one(CaptureEvent::TouchEvent {
                    pc_cont_ns: t,
                    boot_id: "boot".into(),
                    flags: arb_adb::wire::FrameFlags::default(),
                    display_value: calibration.display_value(&payload),
                    normalized: None,
                    payload,
                });
            }
        }
        let [tap, swipe] = collector.raw_taps.as_slice() else {
            panic!("expected two gestures")
        };
        assert_eq!(tap.gesture_kind, GestureKind::Tap);
        assert_eq!((tap.x, tap.y), (Some(540.0), Some(1200.0)));
        assert_eq!(swipe.gesture_kind, GestureKind::Swipe);
        assert_eq!((swipe.end_x, swipe.end_y), (Some(540.0), Some(2399.0)));
        assert_eq!(
            swipe.path.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
            [(540.0, 1200.0), (540.0, 2399.0), (540.0, 2399.0)]
        );
    }

    #[test]
    fn axis_calibration_is_per_device_and_preserves_uncalibrated_pixels() {
        use crate::touch_calibration::TouchCalibration;

        let axes = "add device 1: /dev/input/event1\nABS (0003): ABS_X : value 8, min -10, max 4085\nABS_Y : value 0, min 0, max 4095\nadd device 2: /dev/input/event2\nABS (0003): ABS_X : value 0, min 0, max 1080";
        let calibration = TouchCalibration::from_getevent(axes, 1080, 2400);
        let mut e = raw_evdev(super::EV_ABS, super::ABS_X, 800, 0);
        assert_eq!(calibration.display_value(&e), Some(800));
        assert_eq!(TouchCalibration::default().display_value(&e), None);
        e.device_path = "/dev/input/event1".into();
        assert_eq!(calibration.display_value(&e), Some(213));
        e.ev_code = super::ABS_Y;
        e.ev_value = 4095;
        assert_eq!(calibration.display_value(&e), Some(2399));

        let axes = "add device 2: /dev/input/event2\nABS_MT_POSITION_X : value 0, min 0, max 32767\nABS_MT_POSITION_Y : value 0, min 0, max 32767";
        let emulator = TouchCalibration::from_getevent(axes, 1080, 2400);
        for (code, raw, pixel) in [
            (super::ABS_MT_POSITION_X, 364_077, 1079),
            (super::ABS_MT_POSITION_Y, 204_793, 2399),
            (super::ABS_MT_POSITION_X, -1, 0),
            (super::ABS_MT_POSITION_X, 12_000, 395),
        ] {
            let event = raw_evdev(super::EV_ABS, code, raw, 0);
            assert_eq!(emulator.display_value(&event), Some(pixel));
        }
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
                display_value: None,
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
