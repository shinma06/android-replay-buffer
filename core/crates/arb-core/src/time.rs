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

/// Formats Unix milliseconds as UTC, retaining three fractional digits before and after 1970.
#[must_use]
pub fn format_unix_ms_iso8601(unix_ms: i64) -> String {
    let mut stamp = format_unix_secs_iso8601(unix_ms.div_euclid(1000));
    stamp.pop();
    format!("{stamp}.{:03}Z", unix_ms.rem_euclid(1000))
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fraction]Z` (the UTC text this crate writes) into Unix
/// milliseconds and the text's resolution in milliseconds (1,000 without a fraction, 100 for one
/// digit, 10 for two, 1 otherwise). Other forms return `None`.
#[must_use]
pub fn parse_utc_iso8601_ms(text: &str) -> Option<(i64, u64)> {
    let b = text.as_bytes();
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = b.get(range)?;
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| digits.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b.last() != Some(&b'Z')
    {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let fraction = &b[19..b.len() - 1];
    let (millis, precision_ms) = match fraction {
        [] => (0, 1_000),
        [b'.', digits @ ..] if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) => {
            let take = digits.len().min(3);
            let mut ms = digits[..take]
                .iter()
                .fold(0, |n, d| n * 10 + i64::from(d - b'0'));
            for _ in take..3 {
                ms *= 10;
            }
            (ms, [100, 10, 1][take - 1])
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    if civil_from_days(days) != (year, month, day) {
        return None;
    }
    let secs = days * SECS_PER_DAY + hour * SECS_PER_HOUR + minute * SECS_PER_MINUTE + second;
    Some((secs * 1000 + millis, precision_ms))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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

/// Relative drift allowed between the device `CLOCK_BOOTTIME` and the host continuous clock when a
/// clock-offset estimate extrapolates between samples: 200 ppm, i.e. 1 ns per 5,000 ns.
pub const CLOCK_OFFSET_DRIFT_DIVISOR: u64 = 5_000;

/// Allowance for each clock pair (device triple, host sample) not being read at one instant.
pub const CLOCK_OFFSET_SAMPLE_SKEW_NS: u64 = 1_000_000;

/// Longest extrapolation (1 hour) between a probe, a triple and a host sample that still
/// yields an estimate. Beyond it the drift allowance dominates and no offset is reported.
pub const MAX_CLOCK_OFFSET_EXTRAPOLATION_NS: u64 = 3_600_000_000_000;

/// Extra uncertainty of a trigger estimate: the host trigger wall clock was stamped around the
/// pin, not with the device boottime at the window end.
pub const TRIGGER_ESTIMATE_BASE_UNCERTAINTY_NS: u64 = 1_000_000_000;

/// One coincident reading of the host continuous clock and the host wall clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostClockSample {
    /// Host `PcContinuousMono` nanoseconds.
    pub continuous_mono_ns: i64,
    /// Host wall clock, Unix epoch nanoseconds.
    pub wall_epoch_ns: i64,
}

/// How a device clock offset was estimated.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockOffsetSource {
    /// A host↔device round-trip probe, a device clock triple and a host clock sample.
    SyncProbe,
    /// An existing bundle: the window end boottime, a device triple and the trigger wall time.
    TriggerEstimate,
}

/// Why no device clock offset is reported.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockOffsetUnavailable {
    /// No device is connected to the live capture.
    NotConnected,
    /// The live clock domain is replayed, not the host clock.
    ReplayedClock,
    /// No round-trip probe within the error budget was received.
    NoSyncProbe,
    /// No device clock triple was received.
    NoClockTriple,
    /// The probe and the triple belong to different device boots.
    BootMismatch,
    /// The device wall clock jumped and no later triple has confirmed the new offset.
    WallClockJump,
    /// The samples are too far apart, or a value is outside the representable range.
    OutOfRange,
    /// A crash at the window end can precede the old host save stamp by an unknown delay.
    TriggerDelayUnknown,
    /// No paired host clock sample was available for this save.
    NoHostClock,
    /// The bundle records no host trigger wall time (trimmed or legacy bundle).
    NoTriggerWallClock,
}

/// A device clock offset: `offset_ms = device CLOCK_REALTIME − host wall clock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceClockOffsetEstimate {
    /// Device wall clock minus host wall clock, rounded half away from zero to milliseconds.
    pub offset_ms: i64,
    /// Half-width in milliseconds; the true offset lies in `offset_ms ± uncertainty_ms`
    /// whenever the drift and skew allowances hold.
    pub uncertainty_ms: u64,
    /// How the estimate was formed.
    pub source: ClockOffsetSource,
    /// Host wall clock (Unix milliseconds) of the instant the offset describes.
    pub sampled_at_unix_ms: i64,
}

/// Published device clock offset. Either `offset_ms` and its companions are set, or `reason`.
///
/// Times are never corrected with it: callers subtract `offset_ms` themselves to compare device
/// timestamps with host-side logs (SPEC-02).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceClockOffset {
    /// Device wall clock minus host wall clock in milliseconds.
    pub offset_ms: Option<i64>,
    /// Non-negative half-width of `offset_ms` in milliseconds.
    pub uncertainty_ms: Option<u64>,
    /// How the estimate was formed.
    pub source: Option<ClockOffsetSource>,
    /// Host wall clock (RFC 3339, millisecond UTC) of the instant the offset describes.
    pub sampled_at_utc: Option<String>,
    /// For legacy estimates this delay is additional to the numeric clock uncertainty.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trigger_delay_unbounded: bool,
    /// Why no offset is reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ClockOffsetUnavailable>,
}

impl DeviceClockOffset {
    /// An unavailable offset with its reason.
    pub const fn unavailable(reason: ClockOffsetUnavailable) -> Self {
        Self {
            offset_ms: None,
            uncertainty_ms: None,
            source: None,
            sampled_at_utc: None,
            trigger_delay_unbounded: false,
            reason: Some(reason),
        }
    }

    /// The published form of an estimate, or `reason` when there is none.
    pub fn from_estimate(
        estimate: Option<DeviceClockOffsetEstimate>,
        reason: ClockOffsetUnavailable,
    ) -> Self {
        estimate.map_or_else(
            || Self::unavailable(reason),
            |e| Self {
                offset_ms: Some(e.offset_ms),
                uncertainty_ms: Some(e.uncertainty_ms),
                trigger_delay_unbounded: e.source == ClockOffsetSource::TriggerEstimate,
                source: Some(e.source),
                sampled_at_utc: Some(format_unix_ms_iso8601(e.sampled_at_unix_ms)),
                reason: None,
            },
        )
    }
}

/// Estimates `device CLOCK_REALTIME − host wall clock` at the instant of the device triple.
///
/// The probe ties device boottime to the host continuous clock (± its RTT half-width), the
/// triple ties device boottime to the device wall clock, and the host sample ties the host
/// continuous clock to the host wall clock. Every intermediate uses `i128`; the uncertainty adds
/// the probe half-width, both sampling skews and 200 ppm of the extrapolated spans. Returns
/// `None` for a different boot, a negative RTT, spans over
/// [`MAX_CLOCK_OFFSET_EXTRAPOLATION_NS`], or values outside `i64` milliseconds.
pub fn compute_device_clock_offset(
    triple: &DeviceClockTriple,
    probe: &PcSyncProbe,
    host: &HostClockSample,
) -> Option<DeviceClockOffsetEstimate> {
    if triple.boot_id != probe.boot_id {
        return None;
    }
    sync_probe_offset(
        SyncProbeClocks {
            pc_send_cont_ns: probe.pc_send_cont_ns,
            pc_recv_cont_ns: probe.pc_recv_cont_ns,
            dev_boottime_ns: probe.dev_boottime_ns,
        },
        triple.boottime_ns,
        triple.realtime_ns,
        *host,
    )
}

/// The numeric part of a probe used by [`sync_probe_offset`] (K22).
#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_field_names,
    reason = "the `_ns` unit suffix matches `PcSyncProbe`"
)]
pub(crate) struct SyncProbeClocks {
    pub(crate) pc_send_cont_ns: i64,
    pub(crate) pc_recv_cont_ns: i64,
    pub(crate) dev_boottime_ns: i64,
}

/// The `i128` stage of an estimate, before rounding (K22 verifies both stages separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(clippy::struct_field_names, reason = "every field is in nanoseconds")]
pub(crate) struct OffsetParts {
    /// Device wall clock minus host wall clock, nanoseconds.
    pub(crate) offset_ns: i128,
    /// Total extrapolated span the drift allowance applies to, nanoseconds.
    pub(crate) span_ns: i128,
    /// Uncertainty before drift and rounding, nanoseconds.
    pub(crate) base_uncertainty_ns: u64,
    /// Host wall clock of the described instant, nanoseconds.
    pub(crate) host_wall_ns: i128,
}

/// [`compute_device_clock_offset`] after the boot check.
pub(crate) fn sync_probe_offset(
    probe: SyncProbeClocks,
    triple_boottime_ns: i64,
    triple_realtime_ns: i64,
    host: HostClockSample,
) -> Option<DeviceClockOffsetEstimate> {
    finish_estimate(
        sync_probe_parts(probe, triple_boottime_ns, triple_realtime_ns, host)?,
        ClockOffsetSource::SyncProbe,
    )
}

/// The `i128` stage of [`sync_probe_offset`]; `None` only for a negative round trip.
pub(crate) fn sync_probe_parts(
    probe: SyncProbeClocks,
    triple_boottime_ns: i64,
    triple_realtime_ns: i64,
    host: HostClockSample,
) -> Option<OffsetParts> {
    let send = i128::from(probe.pc_send_cont_ns);
    let rtt = i128::from(probe.pc_recv_cont_ns) - send;
    if rtt < 0 {
        return None;
    }
    // `rtt` is non-negative, so the shifts are the half and the rounded-up half.
    let midpoint = send + (rtt >> 1);
    let half_width = u64::try_from((rtt + 1) >> 1).ok()?;
    // Only the continuous clocks are extrapolated. A device wall jump before the triple
    // cannot move its reported sample time back to the pre-jump probe.
    let triple_cont = midpoint + i128::from(triple_boottime_ns) - i128::from(probe.dev_boottime_ns);
    let host_wall_ns =
        triple_cont + i128::from(host.wall_epoch_ns) - i128::from(host.continuous_mono_ns);
    let device_wall = i128::from(triple_realtime_ns);
    Some(OffsetParts {
        offset_ns: device_wall - host_wall_ns,
        span_ns: (i128::from(probe.dev_boottime_ns) - i128::from(triple_boottime_ns)).abs()
            + (midpoint - i128::from(host.continuous_mono_ns)).abs(),
        base_uncertainty_ns: half_width.checked_add(2 * CLOCK_OFFSET_SAMPLE_SKEW_NS)?,
        host_wall_ns,
    })
}

/// Approximates the offset of an existing bundle at its trigger: the device wall clock at the
/// window end boottime (from `triple`) minus the host trigger wall time.
///
/// `wall_precision_ms` is the resolution of the stored trigger text (1,000 for whole seconds).
/// The uncertainty adds [`TRIGGER_ESTIMATE_BASE_UNCERTAINTY_NS`] and the triple's drift span.
pub fn estimate_device_clock_offset_at_trigger(
    triple: &DeviceClockTriple,
    trigger_boottime_ns: i64,
    trigger_wall_unix_ms: i64,
    wall_precision_ms: u64,
) -> Option<DeviceClockOffsetEstimate> {
    finish_estimate(
        trigger_parts(
            triple.boottime_ns,
            triple.realtime_ns,
            trigger_boottime_ns,
            trigger_wall_unix_ms,
            wall_precision_ms,
        )?,
        ClockOffsetSource::TriggerEstimate,
    )
}

/// The `i128` stage of [`estimate_device_clock_offset_at_trigger`].
pub(crate) fn trigger_parts(
    triple_boottime_ns: i64,
    triple_realtime_ns: i64,
    trigger_boottime_ns: i64,
    trigger_wall_unix_ms: i64,
    wall_precision_ms: u64,
) -> Option<OffsetParts> {
    let device_wall = i128::from(trigger_boottime_ns) + i128::from(triple_realtime_ns)
        - i128::from(triple_boottime_ns);
    let host_wall_ns = i128::from(trigger_wall_unix_ms) * 1_000_000;
    Some(OffsetParts {
        offset_ns: device_wall - host_wall_ns,
        span_ns: (i128::from(trigger_boottime_ns) - i128::from(triple_boottime_ns)).abs(),
        base_uncertainty_ns: TRIGGER_ESTIMATE_BASE_UNCERTAINTY_NS
            .checked_add(wall_precision_ms.checked_mul(1_000_000)?)?
            .checked_add(CLOCK_OFFSET_SAMPLE_SKEW_NS)?,
        host_wall_ns,
    })
}

/// Rounds the nanosecond offset half away from zero to milliseconds and widens the uncertainty
/// so the millisecond interval contains the nanosecond one. `None` for spans over
/// [`MAX_CLOCK_OFFSET_EXTRAPOLATION_NS`] or values outside `i64`.
pub(crate) fn finish_estimate(
    parts: OffsetParts,
    source: ClockOffsetSource,
) -> Option<DeviceClockOffsetEstimate> {
    let span = u64::try_from(parts.span_ns).ok()?;
    if span > MAX_CLOCK_OFFSET_EXTRAPOLATION_NS {
        return None;
    }
    let (millis, rounding) = round_ns_to_ms(i64::try_from(parts.offset_ns).ok()?);
    let uncertainty_ns = parts
        .base_uncertainty_ns
        .checked_add(drift_allowance_ns(span))?
        .checked_add(rounding)?;
    Some(DeviceClockOffsetEstimate {
        offset_ms: millis,
        uncertainty_ms: ceil_ns_to_ms(uncertainty_ns),
        source,
        sampled_at_unix_ms: floor_ns_to_ms(i64::try_from(parts.host_wall_ns).ok()?),
    })
}

/// Nanoseconds rounded half away from zero to milliseconds, with the rounding error
/// `|millis × 10^6 − nanos|` (at most 500,000).
pub(crate) const fn round_ns_to_ms(nanos: i64) -> (i64, u64) {
    let (quotient, remainder) = (nanos / 1_000_000, nanos % 1_000_000);
    if remainder >= 500_000 {
        (quotient + 1, (1_000_000 - remainder).unsigned_abs())
    } else if remainder <= -500_000 {
        (quotient - 1, (1_000_000 + remainder).unsigned_abs())
    } else {
        (quotient, remainder.unsigned_abs())
    }
}

/// The 200 ppm drift allowance of an extrapolated span, rounded up.
pub(crate) const fn drift_allowance_ns(span_ns: u64) -> u64 {
    span_ns.div_ceil(CLOCK_OFFSET_DRIFT_DIVISOR)
}

/// The smallest whole number of milliseconds covering `nanos`.
pub(crate) const fn ceil_ns_to_ms(nanos: u64) -> u64 {
    nanos.div_ceil(1_000_000)
}

/// The Unix millisecond containing `nanos` (floor, also before 1970).
pub(crate) const fn floor_ns_to_ms(nanos: i64) -> i64 {
    nanos.div_euclid(1_000_000)
}

/// The tightest sync-probe estimate from saved or live anchors: each probe within
/// `max_half_width_ns` is paired with the same boot's triple nearest in boottime, and the
/// estimate with the smallest uncertainty wins (ties: the latest probe).
///
/// # Errors
///
/// Returns why no estimate exists: no triple, no probe within the budget, only probes of
/// another boot, or samples out of range.
pub fn best_sync_probe_offset(
    triples: &[DeviceClockTriple],
    probes: &[PcSyncProbe],
    host: &HostClockSample,
    max_half_width_ns: u64,
) -> Result<DeviceClockOffsetEstimate, ClockOffsetUnavailable> {
    let mut reason = if triples.is_empty() {
        ClockOffsetUnavailable::NoClockTriple
    } else {
        ClockOffsetUnavailable::NoSyncProbe
    };
    let mut by_boot = BTreeMap::<&str, Vec<&DeviceClockTriple>>::new();
    for triple in triples {
        by_boot.entry(&triple.boot_id).or_default().push(triple);
    }
    // Keep the current wall-clock segment. A probe between the triples bracketing a jump
    // cannot establish which wall-clock reading applied when it was taken.
    let mut segments = BTreeMap::new();
    for (boot, times) in &mut by_boot {
        times.sort_unstable_by_key(|t| t.boottime_ns);
        let start = times
            .windows(2)
            .rposition(|pair| {
                pair[0].wall_offset_ns().abs_diff(pair[1].wall_offset_ns())
                    > u128::from(DEFAULT_DISCONTINUITY_THRESHOLD_NS)
            })
            .map_or(0, |i| i + 1);
        segments.insert(*boot, (&times[start..], start != 0));
    }
    let mut best: Option<DeviceClockOffsetEstimate> = None;
    for probe in probes
        .iter()
        .filter(|p| p.half_width_ns().is_some_and(|hw| hw <= max_half_width_ns))
    {
        let Some(&(times, after_jump)) = segments.get(probe.boot_id.as_str()) else {
            if !triples.is_empty() {
                reason = ClockOffsetUnavailable::BootMismatch;
            }
            continue;
        };
        if after_jump && probe.dev_boottime_ns < times[0].boottime_ns {
            reason = ClockOffsetUnavailable::WallClockJump;
            continue;
        }
        let after = times.partition_point(|t| t.boottime_ns < probe.dev_boottime_ns);
        let before = times[after.saturating_sub(1)];
        let next = times.get(after).copied().unwrap_or(before);
        let triple = if before.boottime_ns.abs_diff(probe.dev_boottime_ns)
            <= next.boottime_ns.abs_diff(probe.dev_boottime_ns)
        {
            before
        } else {
            next
        };
        match compute_device_clock_offset(triple, probe, host) {
            Some(e) if best.is_none_or(|b| e.uncertainty_ms <= b.uncertainty_ms) => {
                best = Some(e);
            }
            Some(_) => {}
            None => reason = ClockOffsetUnavailable::OutOfRange,
        }
    }
    best.ok_or(reason)
}

#[cfg(test)]
mod clock_offset_tests {
    use super::{
        ClockOffsetSource, ClockOffsetUnavailable, DeviceClockOffset, DeviceClockTriple,
        HostClockSample, PcSyncProbe, best_sync_probe_offset, compute_device_clock_offset,
        estimate_device_clock_offset_at_trigger, format_unix_ms_iso8601, parse_utc_iso8601_ms,
    };

    const HOST_WALL_AT_CONT_0: i64 = 1_795_829_000_000_000_000;

    fn probe(boot: &str, send: i64, recv: i64, dev_bt: i64) -> PcSyncProbe {
        PcSyncProbe {
            boot_id: boot.into(),
            pc_send_cont_ns: send,
            pc_send_active_ns: send,
            dev_boottime_ns: dev_bt,
            dev_monotonic_ns: dev_bt,
            pc_recv_cont_ns: recv,
            pc_recv_active_ns: recv,
        }
    }

    /// A device whose boottime equals host continuous + 7 s, and whose wall clock runs
    /// `ahead_ns` ahead of the host wall clock.
    fn scene(ahead_ns: i64) -> (DeviceClockTriple, PcSyncProbe, HostClockSample) {
        let host = HostClockSample {
            continuous_mono_ns: 20_000_000_000,
            wall_epoch_ns: HOST_WALL_AT_CONT_0 + 20_000_000_000,
        };
        let probe = probe("b", 10_000_000_000, 10_004_000_000, 17_002_000_000);
        let triple = DeviceClockTriple {
            boot_id: "b".into(),
            boottime_ns: 17_300_000_000,
            monotonic_ns: 17_300_000_000,
            // Host wall at device boottime B is HOST_WALL_AT_CONT_0 + (B − 7 s).
            realtime_ns: HOST_WALL_AT_CONT_0 + 10_300_000_000 + ahead_ns,
        };
        (triple, probe, host)
    }

    #[test]
    fn issue_591_device_ahead_by_1_7_seconds() {
        let (triple, probe, host) = scene(1_700_000_000);
        let e = compute_device_clock_offset(&triple, &probe, &host).expect("estimate");
        assert_eq!(e.offset_ms, 1_700);
        assert_eq!(e.source, ClockOffsetSource::SyncProbe);
        // Half-width 2 ms + 2 ms skew + 200 ppm of (0.3 s + 10 s) spans ≈ 6.06 ms.
        assert_eq!(e.uncertainty_ms, 7);
        assert_eq!(
            format_unix_ms_iso8601(e.sampled_at_unix_ms),
            format_unix_ms_iso8601((HOST_WALL_AT_CONT_0 + 10_300_000_000) / 1_000_000)
        );
        let (triple, probe, host) = scene(-250_400_000);
        let behind = compute_device_clock_offset(&triple, &probe, &host).expect("estimate");
        assert_eq!(behind.offset_ms, -250);
    }

    #[test]
    fn boot_mismatch_negative_rtt_and_far_samples_yield_nothing() {
        let (mut triple, probe, host) = scene(0);
        triple.boot_id = "other".into();
        assert_eq!(compute_device_clock_offset(&triple, &probe, &host), None);
        let (triple, mut probe, host) = scene(0);
        probe.pc_recv_cont_ns = probe.pc_send_cont_ns - 1;
        assert_eq!(compute_device_clock_offset(&triple, &probe, &host), None);
        let (triple, probe, mut host) = scene(0);
        host.continuous_mono_ns += 4_000_000_000_000;
        host.wall_epoch_ns += 4_000_000_000_000;
        assert_eq!(compute_device_clock_offset(&triple, &probe, &host), None);
        let extremes = HostClockSample {
            continuous_mono_ns: i64::MIN,
            wall_epoch_ns: i64::MAX,
        };
        assert_eq!(
            compute_device_clock_offset(&triple, &probe, &extremes),
            None
        );
    }

    #[test]
    fn the_best_probe_wins_and_reasons_are_specific() {
        let (triple, good, host) = scene(1_000_000_000);
        let wide = probe("b", 10_000_000_000, 10_120_000_000, 17_060_000_000);
        let best = best_sync_probe_offset(
            std::slice::from_ref(&triple),
            &[wide.clone(), good],
            &host,
            50_000_000,
        )
        .expect("estimate");
        assert_eq!(best.offset_ms, 1_000);
        assert_eq!(
            best_sync_probe_offset(std::slice::from_ref(&triple), &[wide], &host, 50_000_000),
            Err(ClockOffsetUnavailable::NoSyncProbe)
        );
        assert_eq!(
            best_sync_probe_offset(&[], &[probe("b", 0, 1, 2)], &host, 50_000_000),
            Err(ClockOffsetUnavailable::NoClockTriple)
        );
        assert_eq!(
            best_sync_probe_offset(&[triple], &[probe("c", 0, 1, 2)], &host, 50_000_000),
            Err(ClockOffsetUnavailable::BootMismatch)
        );
    }

    #[test]
    fn probes_in_the_interval_of_a_wall_jump_do_not_establish_a_saved_offset() {
        let (first, mut middle, host) = scene(0);
        let mut second = first.clone();
        second.boottime_ns += 10_000_000_000;
        second.realtime_ns += 40_000_000_000;
        middle.dev_boottime_ns = first.boottime_ns + 4_000_000_000;
        let triples = [first, second];
        assert_eq!(
            best_sync_probe_offset(&triples, &[middle], &host, 50_000_000),
            Err(ClockOffsetUnavailable::WallClockJump)
        );
    }

    #[test]
    fn trigger_estimate_is_wide_and_honest() {
        let triple = DeviceClockTriple {
            boot_id: "b".into(),
            boottime_ns: 100_000_000_000,
            monotonic_ns: 100_000_000_000,
            realtime_ns: 1_795_829_025_000_000_000,
        };
        let (wall_ms, precision) = parse_utc_iso8601_ms("2026-11-28T01:23:56.300Z").expect("parse");
        assert_eq!(precision, 1);
        // Device wall at boottime 110 s is …:55.000 + 10 s = …:35+…; host trigger …:56.300.
        let e =
            estimate_device_clock_offset_at_trigger(&triple, 110_000_000_000, wall_ms, precision)
                .expect("estimate");
        assert_eq!(e.source, ClockOffsetSource::TriggerEstimate);
        assert_eq!(e.offset_ms, 1_795_829_035_000 - 1_795_829_036_300);
        assert!(e.uncertainty_ms >= 1_000);
        let published =
            DeviceClockOffset::from_estimate(Some(e), ClockOffsetUnavailable::OutOfRange);
        assert_eq!(
            published.sampled_at_utc.as_deref(),
            Some("2026-11-28T01:23:56.300Z")
        );
        assert_eq!(published.reason, None);
        let none =
            DeviceClockOffset::from_estimate(None, ClockOffsetUnavailable::NoTriggerWallClock);
        assert_eq!(
            serde_json::to_value(none).expect("json"),
            serde_json::json!({"offset_ms":null,"uncertainty_ms":null,"source":null,"sampled_at_utc":null,"reason":"no_trigger_wall_clock"})
        );
    }

    #[test]
    fn utc_text_parses_with_its_resolution() {
        assert_eq!(
            parse_utc_iso8601_ms("1970-01-01T00:00:00Z"),
            Some((0, 1_000))
        );
        assert_eq!(
            parse_utc_iso8601_ms("2026-11-22T12:00:00Z"),
            Some((1_795_348_800_000, 1_000))
        );
        assert_eq!(
            parse_utc_iso8601_ms("1970-01-01T00:00:00.5Z"),
            Some((500, 100))
        );
        assert_eq!(
            parse_utc_iso8601_ms("1969-12-31T23:59:59.999Z"),
            Some((-1, 1))
        );
        for bad in [
            "",
            "2026-11-22 12:00:00Z",
            "2026-11-22T12:00:00",
            "2026-13-01T00:00:00Z",
            "2026-02-31T00:00:00Z",
            "2026-11-22T12:00:00.Z",
            "2026-11-22T12:00:00+09:00",
        ] {
            assert_eq!(parse_utc_iso8601_ms(bad), None, "{bad}");
        }
    }

    proptest::proptest! {
        #![proptest_config(if cfg!(miri) {
            proptest::test_runner::Config { failure_persistence: None, cases: 8, ..Default::default() }
        } else { proptest::test_runner::Config::default() })]

        #[test]
        fn utc_text_roundtrips(ms in -2_208_988_800_000_i64..32_503_680_000_000) {
            proptest::prop_assert_eq!(parse_utc_iso8601_ms(&format_unix_ms_iso8601(ms)), Some((ms, 1)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClockError, ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityKind,
        PcSyncProbe, RawTimestampObservation, TimeAlignment, TimestampSemantics, UncertaintyReason,
    };

    #[test]
    fn formats_millisecond_boundaries() {
        use super::format_unix_ms_iso8601 as stamp;
        assert_eq!(stamp(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(stamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(stamp(123), "1970-01-01T00:00:00.123Z");
        assert_eq!(stamp(i64::MIN), "-292275055-05-16T16:47:04.192Z");
        assert_eq!(stamp(i64::MAX), "292278994-08-17T07:12:55.807Z");
    }

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
