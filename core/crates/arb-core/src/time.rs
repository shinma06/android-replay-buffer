//! Dual-layer time model and discontinuity detection engine.
//!
//! Raw observations from the Android device and host PC are preserved without
//! mutation, while derived alignments map observations onto a single
//! `DeviceBoottime` reference axis within a single `boot_id` epoch. Cross-boot
//! arithmetic and interpolation are strictly forbidden.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default threshold (50 ms) for detecting device sleep, wall jumps, and PC sleep.
pub const DEFAULT_DISCONTINUITY_THRESHOLD_NS: u64 = 50_000_000;

/// Default maximum RTT/2 half-width (50 ms) before a PC sync probe is treated as stale.
pub const DEFAULT_MAX_HALF_WIDTH_NS: u64 = 50_000_000;

/// Default maximum age (10 s) of a clock anchor before sync is flagged stale.
pub const DEFAULT_MAX_ANCHOR_AGE_NS: u64 = 10_000_000_000;

/// Identifies a logical device connection epoch.
///
/// `boot_id` comes from `/proc/sys/kernel/random/boot_id` and always changes
/// across device reboots, forming a hard time-axis boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceSessionId {
    /// ADB device serial identifier.
    pub serial: String,
    /// Kernel boot UUID that changes on every device reboot.
    pub boot_id: String,
    /// Per-connection session UUID assigned by the host daemon.
    pub session_id: String,
}

/// Physical or logical clock source of a raw timestamp observation.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockKind {
    /// Android `CLOCK_MONOTONIC` (pauses during deep sleep; used by video/audio PTS and input).
    DeviceMonotonic,
    /// Android `CLOCK_BOOTTIME` (advances during deep sleep; resets on reboot).
    DeviceBoottime,
    /// Android `CLOCK_REALTIME` wall clock (logcat header; subject to NTP/manual jumps).
    DeviceWallEpoch,
    /// Host PC active monotonic clock (pauses during host sleep).
    PcActiveMono,
    /// Host PC continuous monotonic clock (advances during host sleep).
    PcContinuousMono,
    /// Host PC wall clock for human-readable display only.
    PcWallEpoch,
}

/// Semantic meaning attached to a timestamp so derived alignments stay honest.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampSemantics {
    /// Video encoder input timestamp estimate (not physical display scanout time).
    VideoEncoderInputEstimate,
    /// Audio capture PTS with uncalibrated A/V pipeline offset.
    AudioCapturePtsUncalibrated,
    /// Input subsystem event timestamp (`getevent` or `InputDispatcher`).
    InputEventTime,
    /// Logcat entry header wall-clock timestamp (may jump or roll back).
    LogcatHeaderWall,
    /// Device `CLOCK_BOOTTIME` observed when reading a logcat entry (causal upper bound).
    LogcatDeviceReadBoottime,
    /// Host PC ingestion time only (diagnostic / unanchored proximity bound, never event time).
    PcIngestOnly,
}

/// Stream identifier associated with a stream-scoped discontinuity.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    /// Video capture stream.
    Video,
    /// Audio capture stream.
    Audio,
    /// Logcat capture stream.
    Logcat,
    /// Input / touch capture stream.
    Input,
}

/// Simultaneous 3-clock reading taken on the Android device within a single boot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceClockTriple {
    /// Device boot UUID when this triple was sampled.
    pub boot_id: String,
    /// `CLOCK_BOOTTIME` in nanoseconds.
    pub boottime_ns: i64,
    /// `CLOCK_MONOTONIC` in nanoseconds.
    pub monotonic_ns: i64,
    /// `CLOCK_REALTIME` in nanoseconds since Unix epoch.
    pub realtime_ns: i64,
}

impl DeviceClockTriple {
    /// Cumulative deep-sleep offset `boottime_ns - monotonic_ns` using widening arithmetic.
    pub fn sleep_offset_ns(&self) -> i128 {
        i128::from(self.boottime_ns) - i128::from(self.monotonic_ns)
    }

    /// Wall-clock offset `realtime_ns - boottime_ns` using widening arithmetic.
    pub fn wall_offset_ns(&self) -> i128 {
        i128::from(self.realtime_ns) - i128::from(self.boottime_ns)
    }
}

/// Round-trip synchronization probe exchanged between host PC and device agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PcSyncProbe {
    /// Device boot UUID reported in the probe response.
    pub boot_id: String,
    /// Host `PcContinuousMono` immediately before sending the probe.
    pub pc_send_cont_ns: i64,
    /// Host `PcActiveMono` immediately before sending the probe.
    pub pc_send_active_ns: i64,
    /// Device `CLOCK_BOOTTIME` recorded while handling the probe.
    pub dev_boottime_ns: i64,
    /// Device `CLOCK_MONOTONIC` recorded while handling the probe.
    pub dev_monotonic_ns: i64,
    /// Host `PcContinuousMono` immediately upon receiving the reply.
    pub pc_recv_cont_ns: i64,
    /// Host `PcActiveMono` immediately upon receiving the reply.
    pub pc_recv_active_ns: i64,
}

impl PcSyncProbe {
    /// Round-trip time `pc_recv_cont_ns - pc_send_cont_ns`, or `None` if negative.
    pub fn rtt_ns(&self) -> Option<u64> {
        let diff = i128::from(self.pc_recv_cont_ns) - i128::from(self.pc_send_cont_ns);
        u64::try_from(diff).ok()
    }

    /// Synchronization error half-width `rtt_ns / 2`, or `None` if RTT is negative.
    pub fn half_width_ns(&self) -> Option<u64> {
        self.rtt_ns().map(|rtt| rtt.div_ceil(2))
    }

    /// Midpoint of host continuous send and receive timestamps.
    pub fn pc_midpoint_cont_ns(&self) -> i64 {
        let sum = i128::from(self.pc_send_cont_ns) + i128::from(self.pc_recv_cont_ns);
        let mid = sum / 2;
        i64::try_from(mid).unwrap_or(self.pc_send_cont_ns)
    }

    /// Estimated offset `dev_boottime_ns - pc_midpoint_cont_ns`, or `None` on overflow.
    pub fn offset_ns(&self) -> Option<i64> {
        let diff = i128::from(self.dev_boottime_ns) - i128::from(self.pc_midpoint_cont_ns());
        i64::try_from(diff).ok()
    }

    /// Host cumulative sleep offset `pc_recv_cont_ns - pc_recv_active_ns`.
    pub fn pc_recv_sleep_offset_ns(&self) -> i128 {
        i128::from(self.pc_recv_cont_ns) - i128::from(self.pc_recv_active_ns)
    }

    /// Host sleep delta that occurred strictly during this single probe exchange.
    pub fn intra_probe_pc_sleep_ns(&self) -> i128 {
        let send_off = i128::from(self.pc_send_cont_ns) - i128::from(self.pc_send_active_ns);
        self.pc_recv_sleep_offset_ns() - send_off
    }
}

/// Classification of timeline discontinuities detected during capture.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscontinuityKind {
    /// Device `boot_id` changed; hard boundary forbidding cross-boot interpolation.
    DeviceReboot,
    /// Device entered deep sleep (`boottime - monotonic` advanced past threshold).
    DeviceSleep,
    /// Device wall clock jumped forward or rolled backward (`|Δ(realtime - boottime)|`).
    DeviceWallJump,
    /// Host PC entered sleep (`pc_cont - pc_active` advanced past threshold).
    PcSleep,
    /// ADB or transport connection to the device was lost.
    ConnectionLost,
    /// A capture stream reported a gap or dropped segment.
    StreamGap,
    /// Capture stopped because host free disk space dropped below the safety floor.
    StoragePressureStop,
    /// Clock synchronization probe RTT or anchor age exceeded freshness limits.
    ClockSyncStale,
}

/// Concrete discontinuity record emitted by [`ClockTimeline`] or capture layers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DiscontinuityEvent {
    /// Discontinuity category.
    pub kind: DiscontinuityKind,
    /// Specific stream affected, or `None` when the event spans the whole session.
    pub stream: Option<StreamKind>,
    /// Device `boot_id` active when detected, if known.
    pub boot_id: Option<String>,
    /// Start of the affected interval on `DeviceBoottime`, if known.
    pub start_boottime_ns: Option<i64>,
    /// End of the affected interval on `DeviceBoottime`, if known.
    pub end_boottime_ns: Option<i64>,
    /// Host `PcContinuousMono` timestamp when the discontinuity started or was observed.
    pub pc_cont_start_ns: i64,
    /// Host `PcContinuousMono` timestamp when the discontinuity ended, if bounded.
    pub pc_cont_end_ns: Option<i64>,
    /// Signed magnitude of the jump or sleep duration in nanoseconds, when applicable.
    pub delta_ns: Option<i64>,
    /// Optional human-readable diagnostic detail.
    pub detail: Option<String>,
}

/// Reason why a record's derived time alignment is bounded or unanchored.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyReason {
    /// No valid clock anchor exists for the record's `boot_id`.
    NoClockAnchor,
    /// Record belongs to a different `boot_id` than the target window.
    CrossBootBoundary,
    /// Record lies across a device deep-sleep interval where monotonic time paused.
    DeviceSleepSpanned,
    /// Wall-clock jump or rollback made logcat header epoch ambiguous.
    DeviceWallJumpSpanned,
    /// Clock anchor RTT or age exceeded the freshness threshold.
    ClockSyncStale,
    /// Only host PC ingestion time was observed; device event time is unknown.
    PcIngestOnly,
    /// Bounded interval partially overlaps a selection window boundary.
    PartialWindowOverlap,
    /// Legacy bundle used an explicit unknown-time sentinel (`Long.MAX_VALUE` or `null`).
    LegacySentinelUnknown,
    /// Monotonic timestamp fell outside bracketing clock triples and cannot be
    /// safely extrapolated across potential undetected device sleep.
    UnbracketedMonotonicExtrapolation,
}

/// Derived alignment of a raw observation onto the `DeviceBoottime` reference axis.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "certainty", rename_all = "snake_case")]
pub enum TimeAlignment {
    /// High-confidence point estimate on `DeviceBoottime` with explicit error half-width.
    Anchored {
        /// Device boot epoch owning `boottime_ns`.
        boot_id: String,
        /// Estimated occurrence time on `DeviceBoottime` in nanoseconds.
        boottime_ns: i64,
        /// Symmetric error half-width (`±half_width_ns`).
        half_width_ns: u64,
        /// Semantic provenance of the underlying observation.
        semantics: TimestampSemantics,
    },
    /// Interval-only bound on `DeviceBoottime` when a point estimate cannot be proved.
    Bounded {
        /// Device boot epoch owning both bounds.
        boot_id: String,
        /// Inclusive lower bound on `DeviceBoottime` in nanoseconds.
        min_boottime_ns: i64,
        /// Inclusive upper bound on `DeviceBoottime` in nanoseconds.
        max_boottime_ns: i64,
        /// Explanation for why the time is interval-bounded rather than anchored.
        reason: UncertaintyReason,
    },
    /// Time cannot be aligned to the reference axis; only host ingestion time is retained.
    Unanchored {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_ingest_cont_ns: i64,
        /// Explanation for why alignment failed.
        reason: UncertaintyReason,
    },
}

/// Immutable raw timestamp observation captured before alignment.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RawTimestampObservation {
    /// Monotonic device timestamp (video PTS, audio PTS, input events).
    DeviceMonotonic {
        /// Device boot epoch in which `monotonic_ns` was observed.
        boot_id: String,
        /// Raw `CLOCK_MONOTONIC` timestamp in nanoseconds.
        monotonic_ns: i64,
        /// Semantic label for the observation.
        semantics: TimestampSemantics,
        /// Host `PcContinuousMono` receive timestamp in nanoseconds.
        pc_ingest_cont_ns: i64,
    },
    /// Logcat entry with wall-clock header and optional device-read `CLOCK_BOOTTIME` ceiling.
    LogcatWall {
        /// Device boot epoch in which the logcat entry was read.
        boot_id: String,
        /// Raw logcat header wall timestamp (`CLOCK_REALTIME`) in nanoseconds.
        header_epoch_ns: i64,
        /// Optional device `CLOCK_BOOTTIME` when the log line was read (causal upper bound).
        device_read_boottime_ns: Option<i64>,
        /// Host `PcContinuousMono` receive timestamp in nanoseconds.
        pc_ingest_cont_ns: i64,
    },
    /// Observation carrying only host PC ingestion time.
    PcIngestOnly {
        /// Host `PcContinuousMono` receive timestamp in nanoseconds.
        pc_ingest_cont_ns: i64,
    },
}

impl RawTimestampObservation {
    /// Returns the clock kind of the primary raw timestamp.
    pub const fn primary_clock_kind(&self) -> ClockKind {
        match self {
            Self::DeviceMonotonic { .. } => ClockKind::DeviceMonotonic,
            Self::LogcatWall { .. } => ClockKind::DeviceWallEpoch,
            Self::PcIngestOnly { .. } => ClockKind::PcContinuousMono,
        }
    }

    /// Returns the immutable host PC continuous ingestion timestamp in nanoseconds.
    pub const fn pc_ingest_cont_ns(&self) -> i64 {
        match self {
            Self::DeviceMonotonic {
                pc_ingest_cont_ns, ..
            }
            | Self::LogcatWall {
                pc_ingest_cont_ns, ..
            }
            | Self::PcIngestOnly { pc_ingest_cont_ns } => *pc_ingest_cont_ns,
        }
    }
}

/// Errors returned by explicit clock arithmetic helpers.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClockError {
    /// Arithmetic or interpolation was attempted across two different device boot epochs.
    #[error("cross-boot arithmetic forbidden between '{left_boot_id}' and '{right_boot_id}'")]
    CrossBootArithmetic {
        /// First boot epoch identifier.
        left_boot_id: String,
        /// Second boot epoch identifier.
        right_boot_id: String,
    },
    /// Difference exceeded signed 64-bit nanosecond range.
    #[error("timestamp delta overflowed i64 nanoseconds")]
    DeltaOverflow,
}

/// Threshold configuration for [`ClockTimeline`] discontinuity detection and mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClockTimelineConfig {
    /// Minimum `Δ(boottime - monotonic)` in nanoseconds that triggers [`DiscontinuityKind::DeviceSleep`].
    pub sleep_threshold_ns: u64,
    /// Minimum `|Δ(realtime - boottime)|` in nanoseconds that triggers [`DiscontinuityKind::DeviceWallJump`].
    pub wall_jump_threshold_ns: u64,
    /// Minimum `Δ(pc_cont - pc_active)` in nanoseconds that triggers [`DiscontinuityKind::PcSleep`].
    pub pc_sleep_threshold_ns: u64,
    /// Maximum acceptable sync probe error half-width (`rtt / 2`) in nanoseconds.
    pub max_half_width_ns: u64,
    /// Maximum anchor age in nanoseconds before sync is considered stale.
    pub max_anchor_age_ns: u64,
}

impl ClockTimelineConfig {
    /// Creates a configuration with the default SPEC-02 thresholds.
    pub const fn new() -> Self {
        Self {
            sleep_threshold_ns: DEFAULT_DISCONTINUITY_THRESHOLD_NS,
            wall_jump_threshold_ns: DEFAULT_DISCONTINUITY_THRESHOLD_NS,
            pc_sleep_threshold_ns: DEFAULT_DISCONTINUITY_THRESHOLD_NS,
            max_half_width_ns: DEFAULT_MAX_HALF_WIDTH_NS,
            max_anchor_age_ns: DEFAULT_MAX_ANCHOR_AGE_NS,
        }
    }
}

impl Default for ClockTimelineConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
struct BootEpochState {
    triples: Vec<DeviceClockTriple>,
    valid_probes: Vec<PcSyncProbe>,
    wall_jump_indices: Vec<usize>,
}

impl BootEpochState {
    const fn new() -> Self {
        Self {
            triples: Vec::new(),
            valid_probes: Vec::new(),
            wall_jump_indices: Vec::new(),
        }
    }
}

/// Pure, deterministic clock-anchor processor and discontinuity detector.
///
/// Ingests streams of [`DeviceClockTriple`] and [`PcSyncProbe`] readings,
/// records discontinuities without mutating raw observations, and maps raw
/// timestamps onto [`TimeAlignment`] strictly within their own `boot_id`.
#[derive(Debug, Clone)]
pub struct ClockTimeline {
    config: ClockTimelineConfig,
    current_boot_id: Option<String>,
    epochs: BTreeMap<String, BootEpochState>,
    last_probe: Option<PcSyncProbe>,
    last_valid_probe: Option<PcSyncProbe>,
    discontinuities: Vec<DiscontinuityEvent>,
}

impl ClockTimeline {
    /// Creates an empty timeline with the given threshold configuration.
    pub fn new(config: ClockTimelineConfig) -> Self {
        Self {
            config,
            current_boot_id: None,
            epochs: BTreeMap::new(),
            last_probe: None,
            last_valid_probe: None,
            discontinuities: Vec::new(),
        }
    }

    /// Keeps recent live anchors while immutable ring chunks retain full replay history.
    pub fn bound_live_history(&mut self) {
        const CAP: usize = 2048;
        self.epochs
            .retain(|id, _| Some(id) == self.current_boot_id.as_ref());
        for epoch in self.epochs.values_mut() {
            if epoch.triples.len() > CAP {
                epoch.triples.drain(..CAP / 2);
                epoch.wall_jump_indices.retain_mut(|idx| {
                    if let Some(new) = idx.checked_sub(CAP / 2) {
                        *idx = new;
                        true
                    } else {
                        false
                    }
                });
            }
            if epoch.valid_probes.len() > CAP {
                epoch.valid_probes.drain(..CAP / 2);
            }
        }
        if self.discontinuities.len() > CAP {
            self.discontinuities.drain(..CAP / 2);
        }
    }

    /// Returns the active threshold configuration.
    pub const fn config(&self) -> ClockTimelineConfig {
        self.config
    }

    /// Returns the most recently observed device `boot_id`, if any.
    pub fn current_boot_id(&self) -> Option<&str> {
        self.current_boot_id.as_deref()
    }

    /// Returns all discontinuities detected so far in chronological detection order.
    pub fn discontinuities(&self) -> &[DiscontinuityEvent] {
        &self.discontinuities
    }

    /// Computes `end_boottime_ns - start_boottime_ns` strictly within one `boot_id`.
    ///
    /// # Errors
    ///
    /// Returns [`ClockError::CrossBootArithmetic`] if `start_boot_id != end_boot_id`,
    /// or [`ClockError::DeltaOverflow`] if the difference does not fit in `i64`.
    pub fn elapsed_same_boot(
        start_boot_id: &str,
        start_boottime_ns: i64,
        end_boot_id: &str,
        end_boottime_ns: i64,
    ) -> Result<i64, ClockError> {
        if start_boot_id != end_boot_id {
            return Err(ClockError::CrossBootArithmetic {
                left_boot_id: start_boot_id.to_owned(),
                right_boot_id: end_boot_id.to_owned(),
            });
        }
        let diff = i128::from(end_boottime_ns) - i128::from(start_boottime_ns);
        i64::try_from(diff).map_err(|_| ClockError::DeltaOverflow)
    }

    /// Ingests a device 3-clock triple and returns any newly detected discontinuities.
    pub fn ingest_device_triple(
        &mut self,
        triple: DeviceClockTriple,
        pc_cont_ns: i64,
    ) -> Vec<DiscontinuityEvent> {
        let mut emitted = Vec::new();

        if let Some(prev_boot) = &self.current_boot_id
            && prev_boot != &triple.boot_id
        {
            emitted.push(DiscontinuityEvent {
                kind: DiscontinuityKind::DeviceReboot,
                stream: None,
                boot_id: Some(triple.boot_id.clone()),
                start_boottime_ns: None,
                end_boottime_ns: Some(triple.boottime_ns),
                pc_cont_start_ns: pc_cont_ns,
                pc_cont_end_ns: Some(pc_cont_ns),
                delta_ns: None,
                detail: Some(format!(
                    "boot_id changed from '{prev_boot}' to '{}'",
                    triple.boot_id
                )),
            });
        }

        let epoch = self
            .epochs
            .entry(triple.boot_id.clone())
            .or_insert_with(BootEpochState::new);

        if let Some(prev) = epoch.triples.last() {
            if triple.monotonic_ns < prev.monotonic_ns || triple.boottime_ns < prev.boottime_ns {
                return Vec::new();
            }
            let sleep_delta = triple.sleep_offset_ns() - prev.sleep_offset_ns();
            if sleep_delta > i128::from(self.config.sleep_threshold_ns) {
                emitted.push(DiscontinuityEvent {
                    kind: DiscontinuityKind::DeviceSleep,
                    stream: None,
                    boot_id: Some(triple.boot_id.clone()),
                    start_boottime_ns: Some(prev.boottime_ns),
                    end_boottime_ns: Some(triple.boottime_ns),
                    pc_cont_start_ns: pc_cont_ns,
                    pc_cont_end_ns: Some(pc_cont_ns),
                    delta_ns: i64::try_from(sleep_delta).ok(),
                    detail: Some("device deep sleep detected from boottime-monotonic delta".into()),
                });
            }

            let wall_delta = triple.wall_offset_ns() - prev.wall_offset_ns();
            if wall_delta.unsigned_abs() > u128::from(self.config.wall_jump_threshold_ns) {
                let jump_idx = epoch.triples.len() - 1;
                epoch.wall_jump_indices.push(jump_idx);
                emitted.push(DiscontinuityEvent {
                    kind: DiscontinuityKind::DeviceWallJump,
                    stream: None,
                    boot_id: Some(triple.boot_id.clone()),
                    start_boottime_ns: Some(prev.boottime_ns),
                    end_boottime_ns: Some(triple.boottime_ns),
                    pc_cont_start_ns: pc_cont_ns,
                    pc_cont_end_ns: Some(pc_cont_ns),
                    delta_ns: i64::try_from(wall_delta).ok(),
                    detail: Some(
                        "device wall clock jump detected from realtime-boottime delta".into(),
                    ),
                });
            }
        }

        self.current_boot_id = Some(triple.boot_id.clone());
        epoch.triples.push(triple);
        self.discontinuities.extend(emitted.iter().cloned());
        emitted
    }

    /// Ingests a host-to-device synchronization probe and returns any newly detected discontinuities.
    pub fn ingest_pc_sync_probe(&mut self, probe: PcSyncProbe) -> Vec<DiscontinuityEvent> {
        let mut emitted = Vec::new();

        if let Some(prev_boot) = &self.current_boot_id
            && prev_boot != &probe.boot_id
        {
            emitted.push(DiscontinuityEvent {
                kind: DiscontinuityKind::DeviceReboot,
                stream: None,
                boot_id: Some(probe.boot_id.clone()),
                start_boottime_ns: None,
                end_boottime_ns: Some(probe.dev_boottime_ns),
                pc_cont_start_ns: probe.pc_send_cont_ns,
                pc_cont_end_ns: Some(probe.pc_recv_cont_ns),
                delta_ns: None,
                detail: Some(format!(
                    "boot_id changed from '{prev_boot}' to '{}' during sync probe",
                    probe.boot_id
                )),
            });
        }

        let intra_sleep = probe.intra_probe_pc_sleep_ns();
        let inter_sleep = self.last_probe.as_ref().map_or(0, |prev| {
            probe.pc_recv_sleep_offset_ns() - prev.pc_recv_sleep_offset_ns()
        });
        let pc_sleep_delta = intra_sleep.max(inter_sleep);
        if pc_sleep_delta > i128::from(self.config.pc_sleep_threshold_ns) {
            let start_pc = self
                .last_probe
                .as_ref()
                .map_or(probe.pc_send_cont_ns, |prev| prev.pc_recv_cont_ns);
            let start_bt = self
                .last_probe
                .as_ref()
                .and_then(|prev| (prev.boot_id == probe.boot_id).then_some(prev.dev_boottime_ns));
            emitted.push(DiscontinuityEvent {
                kind: DiscontinuityKind::PcSleep,
                stream: None,
                boot_id: Some(probe.boot_id.clone()),
                start_boottime_ns: start_bt,
                end_boottime_ns: Some(probe.dev_boottime_ns),
                pc_cont_start_ns: start_pc,
                pc_cont_end_ns: Some(probe.pc_recv_cont_ns),
                delta_ns: i64::try_from(pc_sleep_delta).ok(),
                detail: Some("host PC sleep detected from pc_cont-pc_active delta".into()),
            });
        }

        let half_width = probe.half_width_ns();
        let high_quality = half_width.is_some_and(|hw| hw <= self.config.max_half_width_ns);

        if high_quality {
            if let Some(prev_valid) = &self.last_valid_probe
                && prev_valid.boot_id == probe.boot_id
            {
                let anchor_age =
                    i128::from(probe.pc_send_cont_ns) - i128::from(prev_valid.pc_recv_cont_ns);
                if anchor_age > i128::from(self.config.max_anchor_age_ns) {
                    emitted.push(DiscontinuityEvent {
                        kind: DiscontinuityKind::ClockSyncStale,
                        stream: None,
                        boot_id: Some(probe.boot_id.clone()),
                        start_boottime_ns: Some(prev_valid.dev_boottime_ns),
                        end_boottime_ns: Some(probe.dev_boottime_ns),
                        pc_cont_start_ns: prev_valid.pc_recv_cont_ns,
                        pc_cont_end_ns: Some(probe.pc_recv_cont_ns),
                        delta_ns: i64::try_from(anchor_age).ok(),
                        detail: Some(
                            "elapsed time since last valid sync probe exceeded max anchor age"
                                .into(),
                        ),
                    });
                }
            }

            let epoch = self
                .epochs
                .entry(probe.boot_id.clone())
                .or_insert_with(BootEpochState::new);
            epoch.valid_probes.push(probe.clone());
            self.last_valid_probe = Some(probe.clone());
        } else {
            emitted.push(DiscontinuityEvent {
                kind: DiscontinuityKind::ClockSyncStale,
                stream: None,
                boot_id: Some(probe.boot_id.clone()),
                start_boottime_ns: Some(probe.dev_boottime_ns),
                end_boottime_ns: Some(probe.dev_boottime_ns),
                pc_cont_start_ns: probe.pc_send_cont_ns,
                pc_cont_end_ns: Some(probe.pc_recv_cont_ns),
                delta_ns: half_width.and_then(|hw| i64::try_from(hw).ok()),
                detail: Some("sync probe RTT half-width exceeded threshold or was invalid".into()),
            });
        }

        self.current_boot_id = Some(probe.boot_id.clone());
        self.last_probe = Some(probe);
        self.discontinuities.extend(emitted.iter().cloned());
        emitted
    }

    /// Maps any immutable [`RawTimestampObservation`] onto [`TimeAlignment`].
    pub fn map_observation(&self, observation: &RawTimestampObservation) -> TimeAlignment {
        match observation {
            RawTimestampObservation::DeviceMonotonic {
                boot_id,
                monotonic_ns,
                semantics,
                pc_ingest_cont_ns,
            } => self.map_device_monotonic(boot_id, *monotonic_ns, *semantics, *pc_ingest_cont_ns),
            RawTimestampObservation::LogcatWall {
                boot_id,
                header_epoch_ns,
                device_read_boottime_ns,
                pc_ingest_cont_ns,
            } => self.map_logcat_wall(
                boot_id,
                *header_epoch_ns,
                *device_read_boottime_ns,
                *pc_ingest_cont_ns,
            ),
            RawTimestampObservation::PcIngestOnly { pc_ingest_cont_ns } => {
                Self::map_pc_ingest_only(*pc_ingest_cont_ns)
            }
        }
    }

    /// Maps a `CLOCK_MONOTONIC` timestamp strictly within its own `boot_id`.
    ///
    /// If a device sleep occurred between the bracketing clock triples, this
    /// degrades to [`TimeAlignment::Bounded`] rather than fabricating an exact
    /// point timestamp.
    pub fn map_device_monotonic(
        &self,
        boot_id: &str,
        monotonic_ns: i64,
        semantics: TimestampSemantics,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        let Some(epoch) = self.epochs.get(boot_id) else {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            };
        };
        Self::map_monotonic_in_epoch(
            &self.config,
            &epoch.triples,
            boot_id,
            monotonic_ns,
            semantics,
            pc_ingest_cont_ns,
        )
    }

    /// [`Self::map_device_monotonic`] after the `boot_id` lookup: maps `monotonic_ns` against
    /// the triples ingested for that one boot epoch.
    pub(crate) fn map_monotonic_in_epoch(
        config: &ClockTimelineConfig,
        triples: &[DeviceClockTriple],
        boot_id: &str,
        monotonic_ns: i64,
        semantics: TimestampSemantics,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        if triples.is_empty() {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            };
        }

        // Look for an adjacent pair bracketing `monotonic_ns` within the same boot epoch.
        let idx = triples
            .partition_point(|t| t.monotonic_ns < monotonic_ns)
            .max(1);
        if let Some(after) = triples.get(idx) {
            let before = &triples[idx - 1];
            if before.monotonic_ns <= monotonic_ns && monotonic_ns <= after.monotonic_ns {
                let sleep_delta = after.sleep_offset_ns() - before.sleep_offset_ns();
                if sleep_delta > i128::from(config.sleep_threshold_ns) {
                    let min_bt = i128::from(before.boottime_ns)
                        + (i128::from(monotonic_ns) - i128::from(before.monotonic_ns));
                    let max_bt = i128::from(after.boottime_ns)
                        - (i128::from(after.monotonic_ns) - i128::from(monotonic_ns));
                    let (lo, hi) = if min_bt <= max_bt {
                        (min_bt, max_bt)
                    } else {
                        (max_bt, min_bt)
                    };
                    if let (Ok(min_boottime_ns), Ok(max_boottime_ns)) =
                        (i64::try_from(lo), i64::try_from(hi))
                    {
                        return TimeAlignment::Bounded {
                            boot_id: boot_id.to_owned(),
                            min_boottime_ns,
                            max_boottime_ns,
                            reason: UncertaintyReason::DeviceSleepSpanned,
                        };
                    }
                    return TimeAlignment::Unanchored {
                        pc_ingest_cont_ns,
                        reason: UncertaintyReason::DeviceSleepSpanned,
                    };
                }

                let mapped = i128::from(monotonic_ns) + before.sleep_offset_ns();
                let drift = u64::try_from(sleep_delta.unsigned_abs()).unwrap_or(u64::MAX);
                if let Ok(boottime_ns) = i64::try_from(mapped) {
                    return TimeAlignment::Anchored {
                        boot_id: boot_id.to_owned(),
                        boottime_ns,
                        half_width_ns: drift,
                        semantics,
                    };
                }
                return TimeAlignment::Unanchored {
                    pc_ingest_cont_ns,
                    reason: UncertaintyReason::NoClockAnchor,
                };
            }
        }

        // At an exact single-triple sample (no adjacent bracket exists),
        // `boottime_ns` and `monotonic_ns` were sampled simultaneously.
        if triples.len() == 1 && triples[0].monotonic_ns == monotonic_ns {
            return TimeAlignment::Anchored {
                boot_id: boot_id.to_owned(),
                boottime_ns: triples[0].boottime_ns,
                half_width_ns: 0,
                semantics,
            };
        }

        // Outside the bracketing triples (before the first triple or after the
        // last triple), NEVER extrapolate with `half_width_ns: 0`: an
        // undetected device deep sleep could have shifted `boottime - monotonic`
        // by an unknown amount. Degrade honestly to `Bounded` using only what is
        // provable: `boottime - monotonic` is the accumulated suspend time, so it
        // is `>= 0` and never decreases within a boot.
        let nearest = triples.iter().min_by_key(|triple| {
            (i128::from(triple.monotonic_ns) - i128::from(monotonic_ns)).unsigned_abs()
        });

        let Some(anchor) = nearest else {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            };
        };

        let distance = (i128::from(anchor.monotonic_ns) - i128::from(monotonic_ns)).unsigned_abs();
        if distance > u128::from(config.max_anchor_age_ns) {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::ClockSyncStale,
            };
        }

        Self::bound_unbracketed_monotonic(triples, boot_id, monotonic_ns, pc_ingest_cont_ns)
    }

    /// Provable `Bounded` placement for a monotonic timestamp outside every
    /// bracketing triple pair of one boot epoch's `triples`.
    fn bound_unbracketed_monotonic(
        triples: &[DeviceClockTriple],
        boot_id: &str,
        monotonic_ns: i64,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        let mono = i128::from(monotonic_ns);
        let (lo, hi) = match (triples.first(), triples.last()) {
            // After the last triple: at least the last observed suspend offset,
            // with no provable upper bound until the next triple arrives.
            (_, Some(last)) if monotonic_ns >= last.monotonic_ns => {
                (mono + last.sleep_offset_ns(), i128::from(i64::MAX))
            }
            // Before the first triple: suspend offset was between 0 and the
            // first observed offset.
            (Some(first), _) => (mono, mono + first.sleep_offset_ns()),
            _ => {
                return TimeAlignment::Unanchored {
                    pc_ingest_cont_ns,
                    reason: UncertaintyReason::NoClockAnchor,
                };
            }
        };
        match (i64::try_from(lo), i64::try_from(hi)) {
            (Ok(min_boottime_ns), Ok(max_boottime_ns)) if min_boottime_ns <= max_boottime_ns => {
                TimeAlignment::Bounded {
                    boot_id: boot_id.to_owned(),
                    min_boottime_ns,
                    max_boottime_ns,
                    reason: UncertaintyReason::UnbracketedMonotonicExtrapolation,
                }
            }
            _ => TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::UnbracketedMonotonicExtrapolation,
            },
        }
    }

    /// Maps a logcat wall-clock header timestamp using device clock triples and
    /// an optional `device_read_boottime_ns` causal ceiling.
    ///
    /// When a wall-clock jump or rollback makes `header_epoch_ns` ambiguous,
    /// this degrades cleanly to [`TimeAlignment::Bounded`] (capped by
    /// `device_read_boottime_ns` when available) or [`TimeAlignment::Unanchored`]
    /// instead of fabricating an exact point timestamp.
    pub fn map_logcat_wall(
        &self,
        boot_id: &str,
        header_epoch_ns: i64,
        device_read_boottime_ns: Option<i64>,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        let Some(epoch) = self.epochs.get(boot_id) else {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            };
        };
        Self::map_logcat_wall_in_epoch(
            &self.config,
            &epoch.triples,
            &epoch.wall_jump_indices,
            boot_id,
            header_epoch_ns,
            device_read_boottime_ns,
            pc_ingest_cont_ns,
        )
    }

    /// [`Self::map_logcat_wall`] after the `boot_id` lookup: maps `header_epoch_ns` against the
    /// triples ingested for that one boot epoch and its wall-jump indices (`idx` marks a jump
    /// between `triples[idx]` and `triples[idx + 1]`).
    pub(crate) fn map_logcat_wall_in_epoch(
        config: &ClockTimelineConfig,
        triples: &[DeviceClockTriple],
        wall_jump_indices: &[usize],
        boot_id: &str,
        header_epoch_ns: i64,
        device_read_boottime_ns: Option<i64>,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        if triples.is_empty() {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            };
        }

        let tolerance = i128::from(config.wall_jump_threshold_ns);

        // Fast path: no wall-clock jumps occurred in this boot epoch.
        if wall_jump_indices.is_empty() {
            return Self::map_logcat_stable_epoch(
                config,
                boot_id,
                triples,
                header_epoch_ns,
                device_read_boottime_ns,
                pc_ingest_cont_ns,
                tolerance,
            );
        }

        // Check if `device_read_boottime_ns` lies inside an interval where a wall jump happened.
        // Do NOT invent `before.boottime_ns` as a lower bound: a delayed logcat line
        // could have been written arbitrarily earlier (e.g., logd buffering or replay).
        if let Some(read_bt) = device_read_boottime_ns {
            for &jump_idx in wall_jump_indices {
                let before = &triples[jump_idx];
                let after = &triples[jump_idx + 1];
                if read_bt >= before.boottime_ns && read_bt <= after.boottime_ns {
                    return TimeAlignment::Bounded {
                        boot_id: boot_id.to_owned(),
                        min_boottime_ns: i64::MIN,
                        max_boottime_ns: read_bt,
                        reason: UncertaintyReason::DeviceWallJumpSpanned,
                    };
                }
            }
        }

        let candidates = Self::collect_wall_segment_candidates(
            config,
            triples,
            wall_jump_indices,
            header_epoch_ns,
            device_read_boottime_ns,
            tolerance,
        );

        if candidates.len() == 1 {
            let (cand_bt, hw) = candidates[0];
            let spans_jump = device_read_boottime_ns.is_some_and(|read_bt| {
                wall_jump_indices.iter().any(|&idx| {
                    let jump_start = triples[idx].boottime_ns;
                    let jump_end = triples[idx + 1].boottime_ns;
                    cand_bt <= jump_end && read_bt >= jump_start && cand_bt <= jump_start
                })
            });

            if !spans_jump {
                return TimeAlignment::Anchored {
                    boot_id: boot_id.to_owned(),
                    boottime_ns: cand_bt,
                    half_width_ns: hw,
                    semantics: TimestampSemantics::LogcatHeaderWall,
                };
            }
        }

        Self::degrade_ambiguous_logcat(
            boot_id,
            &candidates,
            device_read_boottime_ns,
            pc_ingest_cont_ns,
        )
    }

    /// Maps `header_epoch_ns` within a wall-stable slice of triples using the
    /// bracketing pair (when bracketed) or the nearest triple in wall time,
    /// returning `(mapped_boottime_ns, local_half_width_ns, min_distance_ns)`.
    fn map_wall_in_stable_segment(
        segment: &[DeviceClockTriple],
        header_epoch_ns: i64,
    ) -> Option<(i64, u64, u128)> {
        if segment.is_empty() {
            return None;
        }

        let header = i128::from(header_epoch_ns);

        // Prefer an adjacent triple pair that brackets `header_epoch_ns`.
        for pair in segment.windows(2) {
            let before = &pair[0];
            let after = &pair[1];
            let wall_lo = before.realtime_ns.min(after.realtime_ns);
            let wall_hi = before.realtime_ns.max(after.realtime_ns);
            if wall_lo <= header_epoch_ns && header_epoch_ns <= wall_hi {
                let bt_a = header - before.wall_offset_ns();
                let bt_b = header - after.wall_offset_ns();
                let boot_min = bt_a.min(bt_b);
                let boot_max = bt_a.max(bt_b);
                let span = boot_max - boot_min;
                let mid = boot_min + span / 2;
                let hw_i128 = (span + 1) / 2;
                let mapped_bt = i64::try_from(mid).ok()?;
                let half_width_ns = u64::try_from(hw_i128).unwrap_or(u64::MAX);
                let dist_before = (i128::from(before.boottime_ns) - mid).unsigned_abs();
                let dist_after = (i128::from(after.boottime_ns) - mid).unsigned_abs();
                return Some((mapped_bt, half_width_ns, dist_before.min(dist_after)));
            }
        }

        // Outside the segment's bracket: use the nearest triple in wall time
        // and its local neighbor drift (never the entire boot's cumulative slew).
        let (nearest_idx, nearest) = segment
            .iter()
            .enumerate()
            .min_by_key(|(_, triple)| (i128::from(triple.realtime_ns) - header).unsigned_abs())?;
        let nearest_offset = nearest.wall_offset_ns();
        let mapped_i128 = header - nearest_offset;
        let mapped_bt = i64::try_from(mapped_i128).ok()?;

        let mut local_drift: u128 = 0;
        if let Some(prev) = nearest_idx.checked_sub(1).and_then(|i| segment.get(i)) {
            local_drift = local_drift.max((prev.wall_offset_ns() - nearest_offset).unsigned_abs());
        }
        if let Some(next) = segment.get(nearest_idx + 1) {
            local_drift = local_drift.max((next.wall_offset_ns() - nearest_offset).unsigned_abs());
        }
        let half_width_ns = u64::try_from(local_drift).unwrap_or(u64::MAX);
        let min_distance = (i128::from(nearest.boottime_ns) - mapped_i128).unsigned_abs();
        Some((mapped_bt, half_width_ns, min_distance))
    }

    fn collect_wall_segment_candidates(
        config: &ClockTimelineConfig,
        triples: &[DeviceClockTriple],
        wall_jump_indices: &[usize],
        header_epoch_ns: i64,
        device_read_boottime_ns: Option<i64>,
        tolerance: i128,
    ) -> Vec<(i64, u64)> {
        let mut segment_starts = Vec::with_capacity(wall_jump_indices.len() + 1);
        segment_starts.push(0);
        for &jump_idx in wall_jump_indices {
            segment_starts.push(jump_idx + 1);
        }

        let mut candidates = Vec::new();
        for seg_idx in 0..segment_starts.len() {
            let start = segment_starts[seg_idx];
            let end = segment_starts
                .get(seg_idx + 1)
                .copied()
                .unwrap_or(triples.len());
            let segment = &triples[start..end];
            let (Some(first), Some(last)) = (segment.first(), segment.last()) else {
                continue;
            };

            let Some((cand_bt, hw, _)) = Self::map_wall_in_stable_segment(segment, header_epoch_ns)
            else {
                continue;
            };
            let cand_i128 = i128::from(cand_bt);

            let seg_min_bt = i128::from(first.boottime_ns) - i128::from(config.max_anchor_age_ns);
            let seg_max_bt = i128::from(last.boottime_ns) + i128::from(config.max_anchor_age_ns);
            let within_segment = cand_i128 >= seg_min_bt && cand_i128 <= seg_max_bt;
            let respects_read_ceiling = device_read_boottime_ns
                .is_none_or(|read_bt| cand_i128 <= i128::from(read_bt) + tolerance);

            if within_segment && respects_read_ceiling {
                candidates.push((cand_bt, hw));
            }
        }
        candidates
    }

    fn degrade_ambiguous_logcat(
        boot_id: &str,
        candidates: &[(i64, u64)],
        device_read_boottime_ns: Option<i64>,
        pc_ingest_cont_ns: i64,
    ) -> TimeAlignment {
        if let Some(read_bt) = device_read_boottime_ns {
            // Only use a finite lower bound when the matching wall segments
            // themselves produced multiple valid candidates; never invent
            // `min_from_triples` or `read_bt` as a lower bound because delayed
            // logcat entries can predate the earliest observed triple.
            let min_bt = if candidates.len() >= 2 {
                candidates
                    .iter()
                    .map(|(bt, _)| *bt)
                    .filter(|&bt| bt <= read_bt)
                    .min()
                    .unwrap_or(i64::MIN)
            } else {
                i64::MIN
            };

            return TimeAlignment::Bounded {
                boot_id: boot_id.to_owned(),
                min_boottime_ns: min_bt,
                max_boottime_ns: read_bt,
                reason: UncertaintyReason::DeviceWallJumpSpanned,
            };
        }

        if candidates.len() >= 2
            && let (Some(min_bt), Some(max_bt)) = (
                candidates.iter().map(|(bt, _)| *bt).min(),
                candidates.iter().map(|(bt, _)| *bt).max(),
            )
        {
            return TimeAlignment::Bounded {
                boot_id: boot_id.to_owned(),
                min_boottime_ns: min_bt,
                max_boottime_ns: max_bt,
                reason: UncertaintyReason::DeviceWallJumpSpanned,
            };
        }

        TimeAlignment::Unanchored {
            pc_ingest_cont_ns,
            reason: UncertaintyReason::DeviceWallJumpSpanned,
        }
    }

    fn map_logcat_stable_epoch(
        config: &ClockTimelineConfig,
        boot_id: &str,
        triples: &[DeviceClockTriple],
        header_epoch_ns: i64,
        device_read_boottime_ns: Option<i64>,
        pc_ingest_cont_ns: i64,
        tolerance: i128,
    ) -> TimeAlignment {
        let Some((mapped_bt, half_width_ns, min_distance)) =
            Self::map_wall_in_stable_segment(triples, header_epoch_ns)
        else {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            };
        };
        let mapped_i128 = i128::from(mapped_bt);

        // Enforce anchor freshness relative to the nearest/bracketing triple.
        if min_distance > u128::from(config.max_anchor_age_ns) {
            if let Some(read_bt) = device_read_boottime_ns {
                // Unknown lower bound: delayed logs can predate the earliest triple.
                return TimeAlignment::Bounded {
                    boot_id: boot_id.to_owned(),
                    min_boottime_ns: i64::MIN,
                    max_boottime_ns: read_bt,
                    reason: UncertaintyReason::ClockSyncStale,
                };
            }
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns,
                reason: UncertaintyReason::ClockSyncStale,
            };
        }

        // If the mapped time violates the causal read ceiling, an undetected wall jump occurred.
        if let Some(read_bt) = device_read_boottime_ns
            && mapped_i128 > i128::from(read_bt) + tolerance
        {
            // Unknown lower bound: we only know the log happened at or before `read_bt`.
            return TimeAlignment::Bounded {
                boot_id: boot_id.to_owned(),
                min_boottime_ns: i64::MIN,
                max_boottime_ns: read_bt,
                reason: UncertaintyReason::DeviceWallJumpSpanned,
            };
        }

        TimeAlignment::Anchored {
            boot_id: boot_id.to_owned(),
            boottime_ns: mapped_bt,
            half_width_ns,
            semantics: TimestampSemantics::LogcatHeaderWall,
        }
    }

    /// Maps a PC-ingest-only observation. Never fabricates a device event time.
    pub const fn map_pc_ingest_only(pc_ingest_cont_ns: i64) -> TimeAlignment {
        TimeAlignment::Unanchored {
            pc_ingest_cont_ns,
            reason: UncertaintyReason::PcIngestOnly,
        }
    }

    /// Maps a host PC continuous timestamp onto `target_boot_id`'s `DeviceBoottime`
    /// using the nearest fresh [`PcSyncProbe`] strictly inside `target_boot_id`.
    pub fn map_pc_cont_to_boottime(&self, pc_cont_ns: i64, target_boot_id: &str) -> TimeAlignment {
        let Some(epoch) = self.epochs.get(target_boot_id) else {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns: pc_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            };
        };

        let nearest = epoch.valid_probes.iter().min_by_key(|probe| {
            (i128::from(probe.pc_midpoint_cont_ns()) - i128::from(pc_cont_ns)).unsigned_abs()
        });

        let Some(probe) = nearest else {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns: pc_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            };
        };

        let age = (i128::from(probe.pc_midpoint_cont_ns()) - i128::from(pc_cont_ns)).unsigned_abs();
        if age > u128::from(self.config.max_anchor_age_ns) {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns: pc_cont_ns,
                reason: UncertaintyReason::ClockSyncStale,
            };
        }

        let (Some(offset_ns), Some(half_width_ns)) = (probe.offset_ns(), probe.half_width_ns())
        else {
            return TimeAlignment::Unanchored {
                pc_ingest_cont_ns: pc_cont_ns,
                reason: UncertaintyReason::ClockSyncStale,
            };
        };

        let mapped = i128::from(pc_cont_ns) + i128::from(offset_ns);
        match i64::try_from(mapped) {
            Ok(boottime_ns) => TimeAlignment::Anchored {
                boot_id: target_boot_id.to_owned(),
                boottime_ns,
                half_width_ns,
                semantics: TimestampSemantics::PcIngestOnly,
            },
            Err(_) => TimeAlignment::Unanchored {
                pc_ingest_cont_ns: pc_cont_ns,
                reason: UncertaintyReason::NoClockAnchor,
            },
        }
    }
}

impl Default for ClockTimeline {
    fn default() -> Self {
        Self::new(ClockTimelineConfig::default())
    }
}

const SECS_PER_DAY: i64 = 86_400;
const SECS_PER_HOUR: i64 = 3_600;
const SECS_PER_MINUTE: i64 = 60;
/// Formats Unix epoch seconds into an ISO-8601 UTC string using civil-from-days arithmetic.
#[must_use]
pub fn format_unix_secs_iso8601(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(SECS_PER_DAY);
    let secs_of_day = unix_secs.rem_euclid(SECS_PER_DAY);
    let hour = secs_of_day / SECS_PER_HOUR;
    let minute = (secs_of_day % SECS_PER_HOUR) / SECS_PER_MINUTE;
    let second = secs_of_day % SECS_PER_MINUTE;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Converts days since `1970-01-01` into proleptic Gregorian `(year, month, day)`.
///
/// Algorithm due to Howard Hinnant (`civil_from_days`).
const fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::{
        ClockError, ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityKind,
        PcSyncProbe, RawTimestampObservation, TimeAlignment, TimestampSemantics, UncertaintyReason,
    };

    #[test]
    fn detects_reboot_and_forbids_cross_boot_interpolation() {
        let mut timeline = ClockTimeline::default();
        let e1 = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-1".into(),
                boottime_ns: 10_000_000_000,
                monotonic_ns: 10_000_000_000,
                realtime_ns: 1_700_000_000_000_000_000,
            },
            1_000_000_000,
        );
        assert!(
            e1.is_empty(),
            "first triple should not emit discontinuities"
        );

        let e2 = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-2".into(),
                boottime_ns: 2_000_000_000,
                monotonic_ns: 2_000_000_000,
                realtime_ns: 1_700_000_060_000_000_000,
            },
            61_000_000_000,
        );
        assert_eq!(
            e2.len(),
            1,
            "boot_id change must emit exactly one reboot event"
        );
        assert_eq!(
            e2[0].kind,
            DiscontinuityKind::DeviceReboot,
            "event kind must be DeviceReboot"
        );

        let err = ClockTimeline::elapsed_same_boot("boot-1", 10, "boot-2", 20);
        assert!(
            matches!(err, Err(ClockError::CrossBootArithmetic { .. })),
            "cross-boot elapsed calculation must fail"
        );

        let unanchored = timeline.map_device_monotonic(
            "boot-3",
            5_000_000_000,
            TimestampSemantics::VideoEncoderInputEstimate,
            65_000_000_000,
        );
        assert!(
            matches!(
                unanchored,
                TimeAlignment::Unanchored {
                    reason: UncertaintyReason::NoClockAnchor,
                    ..
                }
            ),
            "unknown boot_id must never borrow anchors from another boot"
        );
    }

    #[test]
    fn detects_device_sleep_and_bounds_spanned_monotonic_timestamps() {
        let mut timeline = ClockTimeline::default();
        let _ = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-a".into(),
                boottime_ns: 10_000_000_000,
                monotonic_ns: 10_000_000_000,
                realtime_ns: 1_700_000_010_000_000_000,
            },
            10_000_000_000,
        );
        // Device slept for 5 seconds while monotonic advanced only 1 second.
        let events = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-a".into(),
                boottime_ns: 16_000_000_000,
                monotonic_ns: 11_000_000_000,
                realtime_ns: 1_700_000_016_000_000_000,
            },
            16_000_000_000,
        );
        assert_eq!(
            events.len(),
            1,
            "sleep gap should emit one DeviceSleep event"
        );
        assert_eq!(
            events[0].kind,
            DiscontinuityKind::DeviceSleep,
            "kind must be DeviceSleep"
        );
        assert_eq!(
            events[0].delta_ns,
            Some(5_000_000_000),
            "sleep duration delta must be 5s"
        );

        let aligned = timeline.map_device_monotonic(
            "boot-a",
            10_500_000_000,
            TimestampSemantics::VideoEncoderInputEstimate,
            15_000_000_000,
        );
        assert_eq!(
            aligned,
            TimeAlignment::Bounded {
                boot_id: "boot-a".into(),
                min_boottime_ns: 10_500_000_000,
                max_boottime_ns: 15_500_000_000,
                reason: UncertaintyReason::DeviceSleepSpanned,
            },
            "timestamp inside sleep bracket must degrade to Bounded"
        );
    }

    #[test]
    fn wall_clock_rollback_degrades_ambiguous_logcat_to_bounded() {
        let mut timeline = ClockTimeline::default();
        let wall_base = 1_700_000_000_000_000_000_i64;
        // Stable segment 1: bt 10s..15s -> wall base+10s..base+15s
        let _ = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-w".into(),
                boottime_ns: 10_000_000_000,
                monotonic_ns: 10_000_000_000,
                realtime_ns: wall_base + 10_000_000_000,
            },
            10_000_000_000,
        );
        let _ = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-w".into(),
                boottime_ns: 15_000_000_000,
                monotonic_ns: 15_000_000_000,
                realtime_ns: wall_base + 15_000_000_000,
            },
            15_000_000_000,
        );
        // Wall clock rolls back by 5 seconds at bt=16s -> wall base+11s..base+16s
        let events = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-w".into(),
                boottime_ns: 16_000_000_000,
                monotonic_ns: 16_000_000_000,
                realtime_ns: wall_base + 11_000_000_000,
            },
            16_000_000_000,
        );
        assert_eq!(events.len(), 1, "rollback must emit DeviceWallJump");
        assert_eq!(
            events[0].kind,
            DiscontinuityKind::DeviceWallJump,
            "kind must be DeviceWallJump"
        );
        let _ = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-w".into(),
                boottime_ns: 21_000_000_000,
                monotonic_ns: 21_000_000_000,
                realtime_ns: wall_base + 16_000_000_000,
            },
            21_000_000_000,
        );

        // Header wall = base + 12s matches BOTH bt=12s (pre-rollback) and bt=17s (post-rollback).
        let obs = RawTimestampObservation::LogcatWall {
            boot_id: "boot-w".into(),
            header_epoch_ns: wall_base + 12_000_000_000,
            device_read_boottime_ns: Some(18_000_000_000),
            pc_ingest_cont_ns: 18_010_000_000,
        };
        let aligned = timeline.map_observation(&obs);
        assert_eq!(
            aligned,
            TimeAlignment::Bounded {
                boot_id: "boot-w".into(),
                min_boottime_ns: 12_000_000_000,
                max_boottime_ns: 18_000_000_000,
                reason: UncertaintyReason::DeviceWallJumpSpanned,
            },
            "ambiguous rolled-back logcat header must degrade to Bounded capped by device_read_boottime"
        );
    }

    #[test]
    fn detects_pc_sleep_and_clock_sync_stale() {
        let mut timeline = ClockTimeline::new(ClockTimelineConfig::default());
        let e1 = timeline.ingest_pc_sync_probe(PcSyncProbe {
            boot_id: "boot-p".into(),
            pc_send_cont_ns: 100_000_000_000,
            pc_send_active_ns: 100_000_000_000,
            dev_boottime_ns: 50_000_000_000,
            dev_monotonic_ns: 50_000_000_000,
            pc_recv_cont_ns: 100_004_000_000,
            pc_recv_active_ns: 100_004_000_000,
        });
        assert!(
            e1.is_empty(),
            "healthy low-RTT probe must emit no discontinuity"
        );

        // High RTT probe (200 ms -> half_width = 100 ms > 50 ms)
        let e2 = timeline.ingest_pc_sync_probe(PcSyncProbe {
            boot_id: "boot-p".into(),
            pc_send_cont_ns: 101_000_000_000,
            pc_send_active_ns: 101_000_000_000,
            dev_boottime_ns: 51_100_000_000,
            dev_monotonic_ns: 51_100_000_000,
            pc_recv_cont_ns: 101_200_000_000,
            pc_recv_active_ns: 101_200_000_000,
        });
        assert!(
            e2.iter()
                .any(|ev| ev.kind == DiscontinuityKind::ClockSyncStale),
            "RTT/2 > 50ms must emit ClockSyncStale"
        );

        // PC slept for 30 seconds (pc_cont jumped by 30s, pc_active stayed put), also > 10s anchor age.
        let e3 = timeline.ingest_pc_sync_probe(PcSyncProbe {
            boot_id: "boot-p".into(),
            pc_send_cont_ns: 132_000_000_000,
            pc_send_active_ns: 102_000_000_000,
            dev_boottime_ns: 82_000_000_000,
            dev_monotonic_ns: 82_000_000_000,
            pc_recv_cont_ns: 132_002_000_000,
            pc_recv_active_ns: 102_002_000_000,
        });
        assert!(
            e3.iter().any(|ev| ev.kind == DiscontinuityKind::PcSleep),
            "PC continuous vs active divergence must emit PcSleep"
        );
        assert!(
            e3.iter()
                .any(|ev| ev.kind == DiscontinuityKind::ClockSyncStale),
            "anchor age > 10s must emit ClockSyncStale"
        );
    }

    #[test]
    fn regression_unbracketed_monotonic_never_claims_anchored_half_width_zero() {
        let mut timeline = ClockTimeline::default();
        let _ = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-extrap".into(),
                boottime_ns: 10_000_000_000,
                monotonic_ns: 10_000_000_000,
                realtime_ns: 1_700_000_010_000_000_000,
            },
            10_000_000_000,
        );
        let _ = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-extrap".into(),
                boottime_ns: 12_000_000_000,
                monotonic_ns: 12_000_000_000,
                realtime_ns: 1_700_000_012_000_000_000,
            },
            12_000_000_000,
        );

        // Bracketed event stays Anchored.
        assert_eq!(
            timeline.map_device_monotonic(
                "boot-extrap",
                11_000_000_000,
                TimestampSemantics::VideoEncoderInputEstimate,
                11_000_000_000,
            ),
            TimeAlignment::Anchored {
                boot_id: "boot-extrap".into(),
                boottime_ns: 11_000_000_000,
                half_width_ns: 0,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            "bracketed monotonic timestamp must remain Anchored"
        );

        // Post-last-triple event (e.g. after undetected device sleep!) MUST NOT claim Anchored/half_width_ns: 0.
        let after_last = timeline.map_device_monotonic(
            "boot-extrap",
            13_000_000_000,
            TimestampSemantics::VideoEncoderInputEstimate,
            25_000_000_000,
        );
        assert_eq!(
            after_last,
            TimeAlignment::Bounded {
                boot_id: "boot-extrap".into(),
                min_boottime_ns: 13_000_000_000,
                max_boottime_ns: i64::MAX,
                reason: UncertaintyReason::UnbracketedMonotonicExtrapolation,
            },
            "post-last-triple monotonic event must degrade to Bounded [mono + last offset, unknown], never Anchored with half_width_ns: 0"
        );
        // A post-wake frame (device slept 30 s after the last triple) must not be
        // excluded from a window that contains its true boottime.
        let post_wake_window = crate::selection::WindowSpec::FrameRange {
            boot_id: "boot-extrap".into(),
            in_frame_index: 0,
            out_frame_index: 0,
            start_boottime_ns: 40_000_000_000,
            end_boottime_ns: 50_000_000_000,
            ingest_start_cont_ns: 40_000_000_000,
            ingest_end_cont_ns: 50_000_000_000,
        };
        assert_ne!(
            crate::selection::classify_alignment(&after_last, &post_wake_window),
            crate::selection::Membership::OutOfRange,
            "post-last-triple event must stay a candidate for windows after an undetected sleep"
        );

        // Pre-first-triple event also degrades honestly.
        let before_first = timeline.map_device_monotonic(
            "boot-extrap",
            9_500_000_000,
            TimestampSemantics::InputEventTime,
            9_500_000_000,
        );
        assert_eq!(
            before_first,
            TimeAlignment::Bounded {
                boot_id: "boot-extrap".into(),
                min_boottime_ns: 9_500_000_000,
                max_boottime_ns: 9_500_000_000,
                reason: UncertaintyReason::UnbracketedMonotonicExtrapolation,
            },
            "pre-first-triple monotonic event (zero observed suspend) is bounded by [mono, mono + first offset]"
        );
        assert!(
            !matches!(
                timeline.map_device_monotonic(
                    "boot-extrap",
                    13_000_000_000,
                    TimestampSemantics::InputEventTime,
                    0
                ),
                TimeAlignment::Anchored { .. }
            ),
            "no post-last-triple event may claim Anchored"
        );
    }

    #[test]
    fn regression_exact_single_triple_monotonic_stays_anchored() {
        let mut single = ClockTimeline::default();
        let _ = single.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-one".into(),
                boottime_ns: 50_000_000_000,
                monotonic_ns: 40_000_000_000,
                realtime_ns: 1_700_000_050_000_000_000,
            },
            50_000_000_000,
        );
        assert_eq!(
            single.map_device_monotonic(
                "boot-one",
                40_000_000_000,
                TimestampSemantics::InputEventTime,
                50_000_000_000,
            ),
            TimeAlignment::Anchored {
                boot_id: "boot-one".into(),
                boottime_ns: 50_000_000_000,
                half_width_ns: 0,
                semantics: TimestampSemantics::InputEventTime,
            },
            "exact triple monotonic match must be Anchored"
        );
    }

    #[test]
    fn regression_logcat_wall_jump_and_stale_never_invent_false_lower_bound() {
        let mut timeline = ClockTimeline::default();
        let wall_base = 1_700_000_000_000_000_000_i64;
        let _ = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-lb".into(),
                boottime_ns: 100_000_000_000,
                monotonic_ns: 100_000_000_000,
                realtime_ns: wall_base + 100_000_000_000,
            },
            100_000_000_000,
        );
        let _ = timeline.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-lb".into(),
                boottime_ns: 105_000_000_000,
                monotonic_ns: 105_000_000_000,
                realtime_ns: wall_base + 115_000_000_000, // +10s forward wall jump
            },
            105_000_000_000,
        );

        // Log read inside the jump interval could have been written long before 100s.
        let during_jump = timeline.map_logcat_wall(
            "boot-lb",
            wall_base + 50_000_000_000,
            Some(102_000_000_000),
            102_000_000_000,
        );
        assert_eq!(
            during_jump,
            TimeAlignment::Bounded {
                boot_id: "boot-lb".into(),
                min_boottime_ns: i64::MIN,
                max_boottime_ns: 102_000_000_000,
                reason: UncertaintyReason::DeviceWallJumpSpanned,
            },
            "log read inside wall jump must not claim before.boottime_ns as lower bound"
        );

        // Stale logcat in a stable epoch must also use unknown lower bound (i64::MIN).
        let mut stable = ClockTimeline::default();
        let _ = stable.ingest_device_triple(
            DeviceClockTriple {
                boot_id: "boot-stale".into(),
                boottime_ns: 100_000_000_000,
                monotonic_ns: 100_000_000_000,
                realtime_ns: wall_base + 100_000_000_000,
            },
            100_000_000_000,
        );
        let stale_log = stable.map_logcat_wall(
            "boot-stale",
            wall_base + 20_000_000_000, // 80s before triple (> 10s max_anchor_age_ns)
            Some(101_000_000_000),
            101_000_000_000,
        );
        assert_eq!(
            stale_log,
            TimeAlignment::Bounded {
                boot_id: "boot-stale".into(),
                min_boottime_ns: i64::MIN,
                max_boottime_ns: 101_000_000_000,
                reason: UncertaintyReason::ClockSyncStale,
            },
            "stale logcat must not invent first triple as lower bound"
        );
    }

    #[test]
    fn regression_stable_epoch_logcat_uses_local_bracketing_offset_not_first_triple() {
        let mut timeline = ClockTimeline::default();
        let wall_base = 1_700_000_000_000_000_000_i64;
        // Simulate gradual NTP slew: +20 ms per 5s interval (below 50 ms jump threshold),
        // accumulating +100 ms over 5 intervals.
        for step in 0_i64..=5 {
            let bt = (10 + step * 5) * 1_000_000_000;
            let slew = step * 20_000_000;
            let events = timeline.ingest_device_triple(
                DeviceClockTriple {
                    boot_id: "boot-slew".into(),
                    boottime_ns: bt,
                    monotonic_ns: bt,
                    realtime_ns: wall_base + bt + slew,
                },
                bt,
            );
            assert!(
                events.is_empty(),
                "gradual 20ms NTP slew must not emit wall jump"
            );
        }

        // At step 4 (bt=30s, slew=+80ms) and step 5 (bt=35s, slew=+100ms),
        // a log written at true bt=32.5s has wall = wall_base + 32.5s + 90ms.
        let aligned = timeline.map_logcat_wall(
            "boot-slew",
            wall_base + 32_500_000_000 + 90_000_000,
            Some(33_000_000_000),
            33_000_000_000,
        );
        assert_eq!(
            aligned,
            TimeAlignment::Anchored {
                boot_id: "boot-slew".into(),
                boottime_ns: 32_500_000_000,
                half_width_ns: 10_000_000,
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            "stable logcat must use local bracketing triples (+10ms local half-width, exact center) rather than first triple (+100ms drift)"
        );
    }
}
