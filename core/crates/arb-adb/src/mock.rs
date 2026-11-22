//! Unified [`CaptureEvent`] stream, [`DeviceEventSource`] trait, and deterministic
//! mock / fault-injection harness ([`MockDeviceSource`], [`FaultScenario`],
//! [`FaultScenarioBuilder`]) covering all ARB edge-case scenarios without physical hardware.

use std::collections::VecDeque;

use arb_core::time::{
    ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind,
    PcSyncProbe,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::adb::{
    ReconnectClassification, SessionSupervisor, SupervisorConfig, VideoLivenessStatus,
};
use crate::scrcpy::{
    AudioPacket, AudioPacketKind, AudioTunnelStatus, ScrcpyError, VideoPacket, VideoPacketKind,
    VideoSessionMeta,
};
use crate::wire::{
    BootIdSource, FrameFlags, HeartbeatPayload, HelloPayload, LogOverflowStatsPayload,
    LogRecordPayload, ShutdownCode, ShutdownPayload, StreamHealthEntry, StreamId,
    StreamLifecycleState, StreamStatePayload, TouchEventPayload, WireError, WireLogCompleteness,
};

/// Errors returned while pulling events from a [`DeviceEventSource`].
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum CaptureSourceError {
    /// Companion binary wire protocol error.
    #[error("wire protocol error: {0}")]
    Wire(#[from] WireError),
    /// `scrcpy-server` video or audio tunnel parsing error.
    #[error("scrcpy tunnel error: {0}")]
    Scrcpy(#[from] ScrcpyError),
    /// Underlying transport I/O error.
    #[error("capture transport I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// High-level normalized touch gesture action.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchAction {
    /// Finger touched down on the display.
    Down,
    /// Finger moved while in contact.
    Move,
    /// Finger lifted from the display.
    Up,
    /// Gesture was cancelled by the system.
    Cancel,
}

/// Normalized high-level touch event carrying device monotonic & read timestamps and viewport context.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NormalizedTouchEvent {
    /// Kernel `CLOCK_MONOTONIC` event timestamp in nanoseconds, if present.
    pub device_mono_ns: Option<i64>,
    /// Device `CLOCK_BOOTTIME` stamped when read from `getevent`.
    pub device_read_boottime_ns: i64,
    /// Gesture phase (`Down`, `Move`, `Up`, `Cancel`).
    pub action: TouchAction,
    /// Multi-touch pointer slot / tracking identifier.
    pub pointer_id: i32,
    /// Horizontal coordinate in display pixels.
    pub x: i32,
    /// Vertical coordinate in display pixels.
    pub y: i32,
    /// Display viewport dimensions `(width, height)` in pixels, if known.
    pub viewport: Option<(u32, u32)>,
}

/// Unified capture event yielded by both live [`crate::adb::AdbSessionSource`]
/// and deterministic [`MockDeviceSource`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureEvent {
    /// Companion session handshake and reconnect classification.
    Hello {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Decoded wire `Hello` payload.
        payload: HelloPayload,
        /// Initial vs restored vs reboot vs unverified classification.
        classification: ReconnectClassification,
    },
    /// Periodic 3-clock triple and per-stream health table from Stream 0.
    Heartbeat {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Decoded wire `Heartbeat` payload.
        payload: HeartbeatPayload,
    },
    /// Completed host-to-device RTT clock synchronization probe.
    SyncProbe {
        /// Host `PcContinuousMono` timestamp when `Pong` arrived.
        pc_cont_ns: i64,
        /// Raw `arb_core::time::PcSyncProbe` fed into [`ClockTimeline`].
        probe: PcSyncProbe,
        /// Device read bracket `boottime_after_ns - boottime_before_ns` in nanoseconds.
        device_bracket_ns: u64,
    },
    /// `scrcpy` video stream geometry metadata (`width x height`).
    VideoSession {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Decoded session dimensions.
        meta: VideoSessionMeta,
    },
    /// Encoded H.264 Annex-B video packet (`Config`, `Keyframe`, or `DeltaFrame`).
    VideoFrame {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Parsed `scrcpy` H.264 packet.
        packet: VideoPacket,
    },
    /// AAC `AudioSpecificConfig` packet from the `scrcpy` audio tunnel.
    AudioConfig {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Parsed AAC config packet.
        packet: AudioPacket,
    },
    /// Encoded AAC-LC audio frame from the `scrcpy` audio tunnel.
    AudioFrame {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Parsed AAC frame packet.
        packet: AudioPacket,
    },
    /// Audio stream was unavailable (e.g., API < 33 or refused) while other streams continue.
    AudioUnavailable {
        /// Host `PcContinuousMono` time when availability was evaluated.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Explicit audio availability status.
        status: AudioTunnelStatus,
    },
    /// Binary `logcat -B` entry from Stream 1.
    LogRecord {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Wire frame flags (e.g., `FLAG_RECOVERED`).
        flags: FrameFlags,
        /// Decoded wire `LogRecord` payload.
        payload: LogRecordPayload,
    },
    /// Periodic `logcat -S` / `-g` completeness & drop statistics from Stream 1.
    LogOverflowStats {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Decoded wire `LogOverflowStats` payload.
        payload: LogOverflowStatsPayload,
    },
    /// Touch input event from Stream 2 with optional normalized coordinates.
    TouchEvent {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Wire frame flags.
        flags: FrameFlags,
        /// Decoded wire `TouchEvent` payload.
        payload: TouchEventPayload,
        /// Optional high-level normalized touch gesture representation.
        normalized: Option<NormalizedTouchEvent>,
    },
    /// Explicit per-stream lifecycle state transition.
    StreamState {
        /// Host `PcContinuousMono` ingestion time in nanoseconds.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Decoded wire `StreamState` payload.
        payload: StreamStatePayload,
    },
    /// Healthy static screen: video is silent while Beacon and Video stream remain `Running`.
    StaticHold {
        /// Host `PcContinuousMono` time of liveness evaluation.
        pc_cont_ns: i64,
        /// Active device `boot_id`.
        boot_id: String,
        /// Elapsed nanoseconds since the most recent video frame.
        silent_duration_ns: i64,
    },
    /// Timeline or transport discontinuity record (`arb_core::time::DiscontinuityEvent`).
    Discontinuity(DiscontinuityEvent),
    /// Connection was restored to the same device `boot_id` after a transport drop.
    ConnectionRestored {
        /// Host `PcContinuousMono` time of reconnect completion.
        pc_cont_ns: i64,
        /// Confirmed unchanged `boot_id`.
        boot_id: String,
        /// Reconnect attempts consumed prior to recovery.
        reconnect_attempts: u32,
    },
    /// Graceful session shutdown acknowledgement.
    Shutdown {
        /// Host `PcContinuousMono` time of shutdown.
        pc_cont_ns: i64,
        /// Decoded wire `Shutdown` payload.
        payload: ShutdownPayload,
    },
}

/// Unified pull-based event source implemented by both live [`crate::adb::AdbSessionSource`]
/// and deterministic [`MockDeviceSource`].
pub trait DeviceEventSource {
    /// Returns the ADB device serial associated with this source.
    fn serial(&self) -> &str;

    /// Pulls the next [`CaptureEvent`], returning `Ok(None)` when the session stream ends.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureSourceError`] if frame decoding or transport reading fails.
    fn next_event(&mut self) -> Result<Option<CaptureEvent>, CaptureSourceError>;

    /// Drains all remaining events into a `Vec`.
    ///
    /// # Errors
    ///
    /// Returns the first [`CaptureSourceError`] encountered while draining.
    fn drain_all(&mut self) -> Result<Vec<CaptureEvent>, CaptureSourceError> {
        let mut out = Vec::new();
        while let Some(ev) = self.next_event()? {
            out.push(ev);
        }
        Ok(out)
    }
}

/// Built-in deterministic ARB edge-case scenarios runnable without physical hardware.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultScenario {
    /// 1s GOP FHD 30fps video + AAC audio + logs + taps + clean periodic heartbeats.
    NormalFhd30,
    /// 12s silent video while Beacon stays `Running` (`StaticHold`), followed by video `Error` (`StreamGap`).
    StaticScreenVsStall,
    /// Packets carrying old native `CLOCK_MONOTONIC` PTS arriving 50s late on PC ingest clock.
    DelayedArrivalBurst,
    /// Clock sync probe outage (>10s + high RTT) + `DeviceWallJump` rollback on logcat.
    ClockSyncOutageAndWallRollback,
    /// Audio stream stops with `Error` (`StreamGap(Audio)`) while video, logs, and taps continue cleanly.
    PartialAudioStop,
    /// Transport disconnect (`ConnectionLost`) followed by reconnect with the same `boot_id` (`ConnectionRestored`).
    DisconnectAndReconnect,
    /// Device enters deep sleep (`Δ(boottime - monotonic) = 15s` -> `DeviceSleep`).
    DeviceDeepSleep,
    /// Device reboots (`boot_id` changes from `boot-A` to `boot-B` -> `DeviceReboot`).
    DeviceReboot,
    /// Logcat reports honest `Unverified` baseline followed by `OverflowDetected` + recovery.
    LogcatOverflowAndUnverified,
}

impl FaultScenario {
    /// All 9 built-in deterministic fault-injection scenarios in canonical order.
    pub const ALL: &'static [Self] = &[
        Self::NormalFhd30,
        Self::StaticScreenVsStall,
        Self::DelayedArrivalBurst,
        Self::ClockSyncOutageAndWallRollback,
        Self::PartialAudioStop,
        Self::DisconnectAndReconnect,
        Self::DeviceDeepSleep,
        Self::DeviceReboot,
        Self::LogcatOverflowAndUnverified,
    ];

    /// Builds a ready-to-run [`MockDeviceSource`] scripted for this scenario.
    pub fn build(self) -> MockDeviceSource {
        match self {
            Self::NormalFhd30 => build_normal_fhd30(),
            Self::StaticScreenVsStall => build_static_screen_vs_stall(),
            Self::DelayedArrivalBurst => build_delayed_arrival_burst(),
            Self::ClockSyncOutageAndWallRollback => build_clock_sync_outage_and_wall_rollback(),
            Self::PartialAudioStop => build_partial_audio_stop(),
            Self::DisconnectAndReconnect => build_disconnect_and_reconnect(),
            Self::DeviceDeepSleep => build_device_deep_sleep(),
            Self::DeviceReboot => build_device_reboot(),
            Self::LogcatOverflowAndUnverified => build_logcat_overflow_and_unverified(),
        }
    }
}

/// One deterministic scripted step executed by [`MockDeviceSource`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultStep {
    /// Emit a companion `Hello` handshake at `pc_cont_ns`.
    Hello {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// `Hello` wire payload.
        payload: HelloPayload,
    },
    /// Emit a companion `Heartbeat` frame at `pc_cont_ns`.
    Heartbeat {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// `Heartbeat` wire payload.
        payload: HeartbeatPayload,
    },
    /// Emit a host-device `PcSyncProbe` completion at `probe.pc_recv_cont_ns`.
    SyncProbe {
        /// Raw sync probe readings.
        probe: PcSyncProbe,
        /// Device read bracket in nanoseconds.
        device_bracket_ns: u64,
    },
    /// Emit a `scrcpy` video session metadata header.
    VideoSession {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Video dimensions metadata.
        meta: VideoSessionMeta,
    },
    /// Emit a `scrcpy` video packet.
    VideoFrame {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Parsed video packet.
        packet: VideoPacket,
    },
    /// Emit a `scrcpy` audio packet (config or frame).
    AudioPacket {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Parsed audio packet.
        packet: AudioPacket,
    },
    /// Report audio unavailability (e.g., API < 33).
    AudioUnavailable {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Explicit audio availability status.
        status: AudioTunnelStatus,
    },
    /// Emit a Stream 1 `LogRecord`.
    LogRecord {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Frame flags (e.g., `FLAG_RECOVERED`).
        flags: FrameFlags,
        /// Decoded logcat payload.
        payload: LogRecordPayload,
    },
    /// Emit a Stream 1 `LogOverflowStats`.
    LogOverflowStats {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Decoded overflow stats payload.
        payload: LogOverflowStatsPayload,
    },
    /// Emit a Stream 2 `TouchEvent` with optional normalized coordinates.
    TouchEvent {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Frame flags.
        flags: FrameFlags,
        /// Raw wire touch payload.
        payload: TouchEventPayload,
        /// Optional normalized touch representation.
        normalized: Option<NormalizedTouchEvent>,
    },
    /// Emit a `StreamState` lifecycle transition.
    StreamState {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Decoded stream state payload.
        payload: StreamStatePayload,
    },
    /// Evaluate [`SessionSupervisor::evaluate_video_liveness`] at `pc_cont_ns`
    /// and emit [`CaptureEvent::StaticHold`] or `StreamGap` if non-active.
    EvaluateVideoLiveness {
        /// Host `PcContinuousMono` evaluation time (ns).
        pc_cont_ns: i64,
    },
    /// Inject a transport disconnect at `pc_cont_ns`.
    Disconnect {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Diagnostic disconnect reason.
        reason: String,
    },
    /// Inject a graceful shutdown at `pc_cont_ns`.
    Shutdown {
        /// Host `PcContinuousMono` timestamp (ns).
        pc_cont_ns: i64,
        /// Decoded shutdown payload.
        payload: ShutdownPayload,
    },
}

/// Fluent builder for composing custom deterministic fault-injection scripts.
#[derive(Debug, Clone)]
pub struct FaultScenarioBuilder {
    serial: String,
    supervisor_config: SupervisorConfig,
    timeline_config: ClockTimelineConfig,
    steps: Vec<FaultStep>,
}

impl FaultScenarioBuilder {
    /// Starts a new scenario builder targeting `serial`.
    pub fn new(serial: impl Into<String>) -> Self {
        Self {
            serial: serial.into(),
            supervisor_config: SupervisorConfig::new(),
            timeline_config: ClockTimelineConfig::new(),
            steps: Vec::new(),
        }
    }

    /// Overrides the [`SupervisorConfig`] used by the built [`MockDeviceSource`].
    #[must_use]
    pub fn with_supervisor_config(mut self, config: SupervisorConfig) -> Self {
        self.supervisor_config = config;
        self
    }

    /// Overrides the [`ClockTimelineConfig`] used by the built [`MockDeviceSource`].
    #[must_use]
    pub const fn with_timeline_config(mut self, config: ClockTimelineConfig) -> Self {
        self.timeline_config = config;
        self
    }

    /// Appends an arbitrary [`FaultStep`] to the script.
    #[must_use]
    pub fn push(mut self, step: FaultStep) -> Self {
        self.steps.push(step);
        self
    }

    /// Finalizes this builder into a deterministic [`MockDeviceSource`].
    pub fn build(self) -> MockDeviceSource {
        MockDeviceSource {
            steps: VecDeque::from(self.steps),
            pending: VecDeque::new(),
            supervisor: SessionSupervisor::new(self.serial, self.supervisor_config),
            timeline: ClockTimeline::new(self.timeline_config),
            suppress_next_timeline_reboot_for: None,
        }
    }
}

/// Deterministic mock [`DeviceEventSource`] executing scripted [`FaultStep`] sequences
/// through the real [`SessionSupervisor`] and [`ClockTimeline`] state machines.
#[derive(Debug, Clone)]
pub struct MockDeviceSource {
    steps: VecDeque<FaultStep>,
    pending: VecDeque<CaptureEvent>,
    supervisor: SessionSupervisor,
    timeline: ClockTimeline,
    suppress_next_timeline_reboot_for: Option<String>,
}

impl MockDeviceSource {
    /// Creates a [`MockDeviceSource`] for one of the built-in [`FaultScenario`] presets.
    pub fn from_scenario(scenario: FaultScenario) -> Self {
        scenario.build()
    }

    /// Returns a reference to the live [`SessionSupervisor`] driven by this source.
    pub const fn supervisor(&self) -> &SessionSupervisor {
        &self.supervisor
    }

    /// Returns a reference to the live [`ClockTimeline`] driven by this source.
    pub const fn timeline(&self) -> &ClockTimeline {
        &self.timeline
    }

    fn current_boot_id(&self) -> String {
        self.supervisor
            .current_boot_id()
            .unwrap_or("unknown")
            .to_owned()
    }

    fn handle_hello(&mut self, pc_cont_ns: i64, payload: HelloPayload) {
        let prev_attempts = self.supervisor.reconnect_attempts();
        let (classification, reboot_disc) = self.supervisor.on_hello(&payload, pc_cont_ns);
        let restored = matches!(
            classification,
            ReconnectClassification::ConnectionRestored { .. }
        );
        if matches!(classification, ReconnectClassification::DeviceReboot { .. }) {
            self.suppress_next_timeline_reboot_for = Some(payload.boot_id.clone());
        }
        let boot_id = payload.boot_id.clone();
        self.pending.push_back(CaptureEvent::Hello {
            pc_cont_ns,
            payload,
            classification,
        });
        if let Some(disc) = reboot_disc {
            self.pending.push_back(CaptureEvent::Discontinuity(disc));
        }
        if restored {
            self.pending.push_back(CaptureEvent::ConnectionRestored {
                pc_cont_ns,
                boot_id,
                reconnect_attempts: prev_attempts,
            });
        }
    }

    fn handle_heartbeat(&mut self, pc_cont_ns: i64, payload: HeartbeatPayload) {
        self.supervisor.on_heartbeat(&payload, pc_cont_ns);
        let boot_id = self.current_boot_id();
        let triple = DeviceClockTriple {
            boot_id: boot_id.clone(),
            boottime_ns: payload.boottime_ns,
            monotonic_ns: payload.monotonic_ns,
            realtime_ns: payload.realtime_ns,
        };
        let discs = self.timeline.ingest_device_triple(triple, pc_cont_ns);
        self.pending.push_back(CaptureEvent::Heartbeat {
            pc_cont_ns,
            boot_id: boot_id.clone(),
            payload,
        });
        self.emit_timeline_discontinuities(discs, &boot_id);
    }

    fn handle_video_liveness(&mut self, pc_cont_ns: i64) {
        let boot_id = self.current_boot_id();
        match self.supervisor.evaluate_video_liveness(pc_cont_ns) {
            VideoLivenessStatus::Active => {}
            VideoLivenessStatus::StaticHold { silent_duration_ns } => {
                self.pending.push_back(CaptureEvent::StaticHold {
                    pc_cont_ns,
                    boot_id,
                    silent_duration_ns,
                });
            }
            VideoLivenessStatus::StreamGap { reason } => {
                self.pending
                    .push_back(CaptureEvent::Discontinuity(DiscontinuityEvent {
                        kind: DiscontinuityKind::StreamGap,
                        stream: Some(arb_core::time::StreamKind::Video),
                        boot_id: Some(boot_id),
                        start_boottime_ns: None,
                        end_boottime_ns: None,
                        pc_cont_start_ns: pc_cont_ns,
                        pc_cont_end_ns: None,
                        delta_ns: None,
                        detail: Some(reason),
                    }));
            }
        }
    }

    fn push_media_step(&mut self, step: FaultStep) -> Option<FaultStep> {
        let boot_id = self.current_boot_id();
        match step {
            FaultStep::VideoSession { pc_cont_ns, meta } => {
                self.pending.push_back(CaptureEvent::VideoSession {
                    pc_cont_ns,
                    boot_id,
                    meta,
                });
                None
            }
            FaultStep::VideoFrame { pc_cont_ns, packet } => {
                if !packet.is_config() {
                    self.supervisor.on_video_frame(pc_cont_ns);
                }
                self.pending.push_back(CaptureEvent::VideoFrame {
                    pc_cont_ns,
                    boot_id,
                    packet,
                });
                None
            }
            FaultStep::AudioPacket { pc_cont_ns, packet } => {
                let event = match packet.kind {
                    AudioPacketKind::Config => CaptureEvent::AudioConfig {
                        pc_cont_ns,
                        boot_id,
                        packet,
                    },
                    AudioPacketKind::Frame => CaptureEvent::AudioFrame {
                        pc_cont_ns,
                        boot_id,
                        packet,
                    },
                };
                self.pending.push_back(event);
                None
            }
            FaultStep::AudioUnavailable { pc_cont_ns, status } => {
                self.pending.push_back(CaptureEvent::AudioUnavailable {
                    pc_cont_ns,
                    boot_id,
                    status,
                });
                None
            }
            other => Some(other),
        }
    }

    fn execute_step(&mut self, step: FaultStep) {
        let Some(step) = self.push_media_step(step) else {
            return;
        };
        let boot_id = self.current_boot_id();
        match step {
            FaultStep::Hello {
                pc_cont_ns,
                payload,
            } => self.handle_hello(pc_cont_ns, payload),
            FaultStep::Heartbeat {
                pc_cont_ns,
                payload,
            } => self.handle_heartbeat(pc_cont_ns, payload),
            FaultStep::SyncProbe {
                probe,
                device_bracket_ns,
            } => {
                let pc_cont_ns = probe.pc_recv_cont_ns;
                let probe_boot = probe.boot_id.clone();
                let discs = self.timeline.ingest_pc_sync_probe(probe.clone());
                self.pending.push_back(CaptureEvent::SyncProbe {
                    pc_cont_ns,
                    probe,
                    device_bracket_ns,
                });
                self.emit_timeline_discontinuities(discs, &probe_boot);
            }
            FaultStep::LogRecord {
                pc_cont_ns,
                flags,
                payload,
            } => self.pending.push_back(CaptureEvent::LogRecord {
                pc_cont_ns,
                boot_id,
                flags,
                payload,
            }),
            FaultStep::LogOverflowStats {
                pc_cont_ns,
                payload,
            } => self.pending.push_back(CaptureEvent::LogOverflowStats {
                pc_cont_ns,
                boot_id,
                payload,
            }),
            FaultStep::TouchEvent {
                pc_cont_ns,
                flags,
                payload,
                normalized,
            } => self.pending.push_back(CaptureEvent::TouchEvent {
                pc_cont_ns,
                boot_id,
                flags,
                payload,
                normalized,
            }),
            FaultStep::StreamState {
                pc_cont_ns,
                payload,
            } => {
                let disc = self.supervisor.on_stream_state(&payload, pc_cont_ns);
                self.pending.push_back(CaptureEvent::StreamState {
                    pc_cont_ns,
                    boot_id,
                    payload,
                });
                if let Some(d) = disc {
                    self.pending.push_back(CaptureEvent::Discontinuity(d));
                }
            }
            FaultStep::EvaluateVideoLiveness { pc_cont_ns } => {
                self.handle_video_liveness(pc_cont_ns);
            }
            FaultStep::Disconnect { pc_cont_ns, reason } => {
                let (_backoff, disc) = self.supervisor.on_disconnect(pc_cont_ns, &reason);
                self.pending.push_back(CaptureEvent::Discontinuity(disc));
            }
            FaultStep::Shutdown {
                pc_cont_ns,
                payload,
            } => {
                self.supervisor.stop();
                self.pending.push_back(CaptureEvent::Shutdown {
                    pc_cont_ns,
                    payload,
                });
            }
            FaultStep::VideoSession { .. }
            | FaultStep::VideoFrame { .. }
            | FaultStep::AudioPacket { .. }
            | FaultStep::AudioUnavailable { .. } => {}
        }
    }

    fn emit_timeline_discontinuities(&mut self, discs: Vec<DiscontinuityEvent>, boot_id: &str) {
        for disc in discs {
            if disc.kind == DiscontinuityKind::DeviceReboot
                && self.suppress_next_timeline_reboot_for.as_deref() == Some(boot_id)
            {
                self.suppress_next_timeline_reboot_for = None;
                continue;
            }
            self.pending.push_back(CaptureEvent::Discontinuity(disc));
        }
    }
}

impl DeviceEventSource for MockDeviceSource {
    fn serial(&self) -> &str {
        self.supervisor.serial()
    }

    fn next_event(&mut self) -> Result<Option<CaptureEvent>, CaptureSourceError> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(Some(ev));
            }
            let Some(step) = self.steps.pop_front() else {
                return Ok(None);
            };
            self.execute_step(step);
        }
    }
}

// ---------------- Built-in Deterministic Fault Scenario Builders ----------------

const SEC_NS: i64 = 1_000_000_000;
const MS_NS: i64 = 1_000_000;
const BASE_WALL_NS: i64 = 1_730_000_000 * SEC_NS;

fn make_hello(boot_id: &str, api_level: u32) -> HelloPayload {
    HelloPayload {
        protocol_version: 1,
        api_level,
        boot_count: 1,
        boot_id_source: BootIdSource::KernelProcBootId,
        is_emulator: true,
        heartbeat_interval_ms: 300,
        boot_id: boot_id.to_owned(),
        model: "arb_mock_fhd".into(),
        manufacturer: "ARB".into(),
        agent_version: "0.1.0".into(),
        boot_id_detail: String::new(),
    }
}

fn all_streams_running(boottime_ns: i64, seq: u32) -> Vec<StreamHealthEntry> {
    [
        StreamId::Control,
        StreamId::Logcat,
        StreamId::Input,
        StreamId::Video,
        StreamId::Audio,
    ]
    .into_iter()
    .map(|stream_id| StreamHealthEntry {
        stream_id,
        state: StreamLifecycleState::Running,
        restart_count: 0,
        emitted_seq: seq,
        last_pts_ns: boottime_ns,
    })
    .collect()
}

fn make_heartbeat(
    boottime_ns: i64,
    monotonic_ns: i64,
    realtime_ns: i64,
    streams: Vec<StreamHealthEntry>,
) -> HeartbeatPayload {
    HeartbeatPayload {
        boottime_ns,
        monotonic_ns,
        realtime_ns,
        streams,
    }
}

fn make_sync_probe(
    boot_id: &str,
    pc_send_ns: i64,
    rtt_ns: i64,
    dev_boottime_ns: i64,
    dev_monotonic_ns: i64,
) -> PcSyncProbe {
    PcSyncProbe {
        boot_id: boot_id.to_owned(),
        pc_send_cont_ns: pc_send_ns,
        pc_send_active_ns: pc_send_ns,
        dev_boottime_ns,
        dev_monotonic_ns,
        pc_recv_cont_ns: pc_send_ns + rtt_ns,
        pc_recv_active_ns: pc_send_ns + rtt_ns,
    }
}

fn make_text_log(
    read_boot_ns: i64,
    epoch_seconds: u32,
    subsec_nanos: u32,
    tag: &str,
    msg: &str,
) -> LogRecordPayload {
    let mut payload = Vec::with_capacity(tag.len() + msg.len() + 3);
    payload.push(4); // INFO
    payload.extend_from_slice(tag.as_bytes());
    payload.push(0);
    payload.extend_from_slice(msg.as_bytes());
    payload.push(0);
    LogRecordPayload {
        device_read_boottime_ns: read_boot_ns,
        log_sec: epoch_seconds,
        log_nsec: subsec_nanos,
        pid: 1200,
        tid: 1201,
        lid: 0,
        uid: 10_100,
        hdr_size: 28,
        priority: 4,
        record_flags: 0,
        payload,
    }
}

fn make_touch_step(
    pc_cont_ns: i64,
    dev_boot_ns: i64,
    mono_seconds: i64,
    mono_micros: i32,
    action: TouchAction,
    x: i32,
    y: i32,
) -> FaultStep {
    let ev_code = match action {
        TouchAction::Down | TouchAction::Up | TouchAction::Cancel => 0x014a, // BTN_TOUCH
        TouchAction::Move => 0x0035,                                         // ABS_MT_POSITION_X
    };
    let ev_type = if ev_code == 0x014a { 0x0001 } else { 0x0003 };
    let ev_value = match action {
        TouchAction::Down => 1,
        TouchAction::Up | TouchAction::Cancel => 0,
        TouchAction::Move => x,
    };
    let payload = TouchEventPayload {
        device_read_boottime_ns: dev_boot_ns,
        event_sec: mono_seconds,
        event_usec: mono_micros,
        ev_type,
        ev_code,
        ev_value,
        device_path: "/dev/input/event1".into(),
        raw_line: format!(
            "[ {mono_seconds}.{mono_micros:06}] EV_{ev_type:04x} CODE_{ev_code:04x} {ev_value:08x}"
        ),
    };
    let normalized = NormalizedTouchEvent {
        device_mono_ns: payload.event_monotonic_ns(),
        device_read_boottime_ns: dev_boot_ns,
        action,
        pointer_id: 0,
        x,
        y,
        viewport: Some((1920, 1080)),
    };
    FaultStep::TouchEvent {
        pc_cont_ns,
        flags: FrameFlags::NONE,
        payload,
        normalized: Some(normalized),
    }
}

fn push_normal_fhd30_media_loop(
    mut b: FaultScenarioBuilder,
    base_dev: i64,
) -> FaultScenarioBuilder {
    for idx in 0u32..60 {
        let frame_pts_delta_us = u64::from(idx) * 33_333;
        let frame_elapsed_ns = i64::from(idx) * 33_333_000;
        let pc_ns = 10 * MS_NS + frame_elapsed_ns;
        let dev_ns = base_dev + frame_elapsed_ns;
        let kind = if idx % 30 == 0 {
            VideoPacketKind::Keyframe
        } else {
            VideoPacketKind::DeltaFrame
        };
        let nal = if kind == VideoPacketKind::Keyframe {
            0x65
        } else {
            0x41
        };
        b = b.push(FaultStep::VideoFrame {
            pc_cont_ns: pc_ns,
            packet: VideoPacket {
                kind,
                pts_us: 100_000_000 + frame_pts_delta_us,
                data: vec![0x00, 0x00, 0x00, 0x01, nal, u8::try_from(idx).unwrap_or(0)],
            },
        });
        if idx % 3 == 0 {
            b = b.push(FaultStep::AudioPacket {
                pc_cont_ns: pc_ns + MS_NS,
                packet: AudioPacket {
                    kind: AudioPacketKind::Frame,
                    pts_us: 100_000_000 + frame_pts_delta_us,
                    data: vec![0x21, 0x10, 0x05, u8::try_from(idx).unwrap_or(0)],
                },
            });
        }
        if idx % 9 == 0 && idx > 0 {
            b = b.push(FaultStep::Heartbeat {
                pc_cont_ns: pc_ns + 2 * MS_NS,
                payload: make_heartbeat(
                    dev_ns,
                    dev_ns,
                    BASE_WALL_NS + frame_elapsed_ns,
                    all_streams_running(dev_ns, idx + 1),
                ),
            });
        }
    }
    b
}

fn push_normal_fhd30_tail(b: FaultScenarioBuilder, base_dev: i64) -> FaultScenarioBuilder {
    b.push(FaultStep::LogRecord {
        pc_cont_ns: 500 * MS_NS,
        flags: FrameFlags::NONE,
        payload: make_text_log(
            base_dev + 500 * MS_NS,
            1_730_000_000,
            500_000_000,
            "MainActivity",
            "onResume",
        ),
    })
    .push(make_touch_step(
        600 * MS_NS,
        base_dev + 600 * MS_NS,
        100,
        600_000,
        TouchAction::Down,
        540,
        960,
    ))
    .push(make_touch_step(
        650 * MS_NS,
        base_dev + 650 * MS_NS,
        100,
        650_000,
        TouchAction::Move,
        600,
        960,
    ))
    .push(make_touch_step(
        700 * MS_NS,
        base_dev + 700 * MS_NS,
        100,
        700_000,
        TouchAction::Up,
        600,
        960,
    ))
    .push(FaultStep::Shutdown {
        pc_cont_ns: 2 * SEC_NS + 50 * MS_NS,
        payload: ShutdownPayload {
            boottime_ns: base_dev + 2 * SEC_NS,
            code: ShutdownCode::Normal,
            reason: "scenario_complete".into(),
        },
    })
}

fn build_normal_fhd30() -> MockDeviceSource {
    let boot_id = "boot-normal-001";
    let base_dev = 100 * SEC_NS;
    let b = FaultScenarioBuilder::new("mock-normal-fhd30")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id, 37),
        })
        .push(FaultStep::SyncProbe {
            probe: make_sync_probe(boot_id, 0, 2 * MS_NS, base_dev, base_dev),
            device_bracket_ns: 500,
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 2 * MS_NS,
            payload: make_heartbeat(
                base_dev,
                base_dev,
                BASE_WALL_NS,
                all_streams_running(base_dev, 1),
            ),
        })
        .push(FaultStep::VideoSession {
            pc_cont_ns: 5 * MS_NS,
            meta: VideoSessionMeta {
                header_tag: 0x8000_0000,
                width: 1920,
                height: 1080,
            },
        })
        .push(FaultStep::VideoFrame {
            pc_cont_ns: 6 * MS_NS,
            packet: VideoPacket {
                kind: VideoPacketKind::Config,
                pts_us: 0,
                data: vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xE0, 0x28],
            },
        })
        .push(FaultStep::AudioPacket {
            pc_cont_ns: 7 * MS_NS,
            packet: AudioPacket {
                kind: AudioPacketKind::Config,
                pts_us: 0,
                data: vec![0x11, 0x90],
            },
        })
        .push(FaultStep::LogOverflowStats {
            pc_cont_ns: 10 * MS_NS,
            payload: LogOverflowStatsPayload {
                boottime_ns: base_dev + 10 * MS_NS,
                completeness: WireLogCompleteness::Verified,
                buffers_count: 8,
                pruned_delta: 0,
                pruned_cumulative: 0,
                detail: "logcat-S:pruned_delta=0".into(),
            },
        });

    let b = push_normal_fhd30_media_loop(b, base_dev);
    push_normal_fhd30_tail(b, base_dev).build()
}

fn build_static_screen_vs_stall() -> MockDeviceSource {
    let boot_id = "boot-static-001";
    let base_dev = 50 * SEC_NS;
    let mut b = FaultScenarioBuilder::new("mock-static-vs-stall")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id, 37),
        })
        .push(FaultStep::VideoSession {
            pc_cont_ns: MS_NS,
            meta: VideoSessionMeta {
                header_tag: 0x8000_0000,
                width: 1920,
                height: 1080,
            },
        })
        .push(FaultStep::VideoFrame {
            pc_cont_ns: 2 * MS_NS,
            packet: VideoPacket {
                kind: VideoPacketKind::Keyframe,
                pts_us: 50_000_000,
                data: vec![0, 0, 0, 1, 0x65],
            },
        });

    // 12 seconds of silent video while Beacon & Video remain Running (every 300ms).
    for step in 1i64..=40 {
        let t_ns = step * 300 * MS_NS; // reaches 12.0s at step 40
        b = b.push(FaultStep::Heartbeat {
            pc_cont_ns: t_ns,
            payload: make_heartbeat(
                base_dev + t_ns,
                base_dev + t_ns,
                BASE_WALL_NS + t_ns,
                all_streams_running(base_dev + t_ns, u32::try_from(step).unwrap_or(1)),
            ),
        });
    }
    // Evaluate liveness at 12s -> MUST emit StaticHold, NOT StreamGap!
    b = b.push(FaultStep::EvaluateVideoLiveness {
        pc_cont_ns: 12 * SEC_NS,
    });

    // At 13s, video pipeline encounters a real Error -> MUST emit StreamGap(Video)!
    let err_t = 13 * SEC_NS;
    b = b.push(FaultStep::StreamState {
        pc_cont_ns: err_t,
        payload: StreamStatePayload {
            target_stream_id: StreamId::Video,
            state: StreamLifecycleState::Error,
            restart_count: 0,
            boottime_ns: base_dev + err_t,
            emitted_seq: 1,
            reason: "surface_encoder_stalled".into(),
        },
    });
    b.build()
}

fn build_delayed_arrival_burst() -> MockDeviceSource {
    let boot_id = "boot-delayed-001";
    let base_dev = 100 * SEC_NS;
    let mut b = FaultScenarioBuilder::new("mock-delayed-arrival")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id, 37),
        })
        .push(FaultStep::SyncProbe {
            probe: make_sync_probe(boot_id, 0, 2 * MS_NS, base_dev, base_dev),
            device_bracket_ns: 400,
        });

    // Clean heartbeats every 10s from t=0s to t=60s so clock triples bracket t=10s cleanly.
    for sec in (0i64..=60).step_by(10) {
        let t = sec * SEC_NS;
        b = b.push(FaultStep::Heartbeat {
            pc_cont_ns: t + MS_NS,
            payload: make_heartbeat(
                base_dev + t,
                base_dev + t,
                BASE_WALL_NS + t,
                all_streams_running(base_dev + t, u32::try_from(sec + 1).unwrap_or(1)),
            ),
        });
    }

    // At pc_cont_ns = 60s, a burst of 3 packets carrying old native monotonic PTS from t = 10s
    // arrives 50 seconds late!
    let ingest_t = 60 * SEC_NS + 100 * MS_NS;
    for i in 0u64..3 {
        let old_pts_us = 110_000_000 + i * 33_333; // base_dev (100s) + 10s = 110s
        b = b.push(FaultStep::VideoFrame {
            pc_cont_ns: ingest_t + i64::try_from(i).unwrap_or(0) * MS_NS,
            packet: VideoPacket {
                kind: if i == 0 {
                    VideoPacketKind::Keyframe
                } else {
                    VideoPacketKind::DeltaFrame
                },
                pts_us: old_pts_us,
                data: vec![0, 0, 0, 1, 0x65],
            },
        });
    }
    b.build()
}

fn build_clock_sync_outage_and_wall_rollback() -> MockDeviceSource {
    let boot_id = "boot-wall-001";
    let base_dev = 200 * SEC_NS;
    FaultScenarioBuilder::new("mock-sync-wall-rollback")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id, 37),
        })
        // Initial healthy sync probe (RTT 2ms).
        .push(FaultStep::SyncProbe {
            probe: make_sync_probe(boot_id, 0, 2 * MS_NS, base_dev, base_dev),
            device_bracket_ns: 400,
        })
        // Initial heartbeat before wall rollback.
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 10 * MS_NS,
            payload: make_heartbeat(
                base_dev,
                base_dev,
                BASE_WALL_NS,
                all_streams_running(base_dev, 1),
            ),
        })
        // 15s later (> 10s anchor age) with bad 200ms RTT -> ClockSyncStale!
        .push(FaultStep::SyncProbe {
            probe: make_sync_probe(
                boot_id,
                15 * SEC_NS,
                200 * MS_NS,
                base_dev + 15 * SEC_NS,
                base_dev + 15 * SEC_NS,
            ),
            device_bracket_ns: 1_000,
        })
        // Wall clock rolls backward by 120 seconds while boottime advances +16s -> DeviceWallJump!
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 16 * SEC_NS,
            payload: make_heartbeat(
                base_dev + 16 * SEC_NS,
                base_dev + 16 * SEC_NS,
                BASE_WALL_NS + 16 * SEC_NS - 120 * SEC_NS,
                all_streams_running(base_dev + 16 * SEC_NS, 2),
            ),
        })
        // Logcat line arriving during rollback window with device_read_boottime_ns ceiling.
        .push(FaultStep::LogRecord {
            pc_cont_ns: 16 * SEC_NS + 10 * MS_NS,
            flags: FrameFlags::NONE,
            payload: make_text_log(
                base_dev + 16 * SEC_NS + 5 * MS_NS,
                1_729_999_900,
                0,
                "WallClock",
                "ntp_rollback_log",
            ),
        })
        .build()
}

fn degraded_audio_streams(boottime_ns: i64) -> Vec<StreamHealthEntry> {
    all_streams_running(boottime_ns, 5)
        .into_iter()
        .map(|mut entry| {
            if entry.stream_id == StreamId::Audio {
                entry.state = StreamLifecycleState::Error;
            }
            entry
        })
        .collect()
}

fn build_partial_audio_stop() -> MockDeviceSource {
    let boot_id = "boot-audio-stop-001";
    let base_dev = 300 * SEC_NS;

    FaultScenarioBuilder::new("mock-partial-audio-stop")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id, 37),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 5 * MS_NS,
            payload: make_heartbeat(
                base_dev,
                base_dev,
                BASE_WALL_NS,
                all_streams_running(base_dev, 1),
            ),
        })
        .push(FaultStep::VideoFrame {
            pc_cont_ns: 10 * MS_NS,
            packet: VideoPacket {
                kind: VideoPacketKind::Keyframe,
                pts_us: 300_000_000,
                data: vec![0, 0, 0, 1, 0x65],
            },
        })
        .push(FaultStep::AudioPacket {
            pc_cont_ns: 15 * MS_NS,
            packet: AudioPacket {
                kind: AudioPacketKind::Frame,
                pts_us: 300_000_000,
                data: vec![0x21, 0x10],
            },
        })
        // Audio fails while Video, Logcat, and Input continue uninterrupted.
        .push(FaultStep::StreamState {
            pc_cont_ns: SEC_NS,
            payload: StreamStatePayload {
                target_stream_id: StreamId::Audio,
                state: StreamLifecycleState::Error,
                restart_count: 0,
                boottime_ns: base_dev + SEC_NS,
                emitted_seq: 1,
                reason: "audio_record_died".into(),
            },
        })
        .push(FaultStep::VideoFrame {
            pc_cont_ns: 2 * SEC_NS,
            packet: VideoPacket {
                kind: VideoPacketKind::Keyframe,
                pts_us: 302_000_000,
                data: vec![0, 0, 0, 1, 0x65],
            },
        })
        .push(FaultStep::LogRecord {
            pc_cont_ns: 2 * SEC_NS + 5 * MS_NS,
            flags: FrameFlags::NONE,
            payload: make_text_log(
                base_dev + 2 * SEC_NS,
                1_730_000_002,
                0,
                "App",
                "still_alive",
            ),
        })
        .push(make_touch_step(
            2 * SEC_NS + 10 * MS_NS,
            base_dev + 2 * SEC_NS + 10 * MS_NS,
            302,
            10_000,
            TouchAction::Down,
            100,
            200,
        ))
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 2 * SEC_NS + 20 * MS_NS,
            payload: make_heartbeat(
                base_dev + 2 * SEC_NS,
                base_dev + 2 * SEC_NS,
                BASE_WALL_NS + 2 * SEC_NS,
                degraded_audio_streams(base_dev + 2 * SEC_NS),
            ),
        })
        .build()
}

fn build_disconnect_and_reconnect() -> MockDeviceSource {
    let boot_id = "boot-reconnect-001";
    let base_dev = 400 * SEC_NS;
    FaultScenarioBuilder::new("mock-disconnect-reconnect")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id, 37),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 10 * MS_NS,
            payload: make_heartbeat(
                base_dev,
                base_dev,
                BASE_WALL_NS,
                all_streams_running(base_dev, 1),
            ),
        })
        .push(FaultStep::Disconnect {
            pc_cont_ns: SEC_NS,
            reason: "usb_cable_bump".into(),
        })
        // Reconnect 200ms later with SAME boot_id -> ConnectionRestored (no DeviceReboot).
        .push(FaultStep::Hello {
            pc_cont_ns: SEC_NS + 200 * MS_NS,
            payload: make_hello(boot_id, 37),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: SEC_NS + 210 * MS_NS,
            payload: make_heartbeat(
                base_dev + SEC_NS + 210 * MS_NS,
                base_dev + SEC_NS + 210 * MS_NS,
                BASE_WALL_NS + SEC_NS + 210 * MS_NS,
                all_streams_running(base_dev + SEC_NS + 210 * MS_NS, 2),
            ),
        })
        .build()
}

fn build_device_deep_sleep() -> MockDeviceSource {
    let boot_id = "boot-sleep-001";
    let base_dev = 500 * SEC_NS;
    let sleep_ns = 15 * SEC_NS;
    FaultScenarioBuilder::new("mock-device-deep-sleep")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id, 37),
        })
        // Before deep sleep: boottime == monotonic (offset = 0).
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 0,
            payload: make_heartbeat(
                base_dev,
                base_dev,
                BASE_WALL_NS,
                all_streams_running(base_dev, 1),
            ),
        })
        // After 15s deep sleep + 1s awake: boottime += 16s, monotonic += 1s -> Δ(boot - mono) = 15s!
        .push(FaultStep::Heartbeat {
            pc_cont_ns: sleep_ns + SEC_NS,
            payload: make_heartbeat(
                base_dev + sleep_ns + SEC_NS,
                base_dev + SEC_NS,
                BASE_WALL_NS + sleep_ns + SEC_NS,
                all_streams_running(base_dev + sleep_ns + SEC_NS, 2),
            ),
        })
        .build()
}

fn build_device_reboot() -> MockDeviceSource {
    let base_a = 600 * SEC_NS;
    let base_b = 5 * SEC_NS; // Reset near 0 after reboot
    FaultScenarioBuilder::new("mock-device-reboot")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello("boot-A", 37),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 10 * MS_NS,
            payload: make_heartbeat(base_a, base_a, BASE_WALL_NS, all_streams_running(base_a, 1)),
        })
        .push(FaultStep::Disconnect {
            pc_cont_ns: SEC_NS,
            reason: "device_rebooting".into(),
        })
        .push(FaultStep::Hello {
            pc_cont_ns: 20 * SEC_NS,
            payload: make_hello("boot-B", 37),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 20 * SEC_NS + 10 * MS_NS,
            payload: make_heartbeat(
                base_b,
                base_b,
                BASE_WALL_NS + 20 * SEC_NS,
                all_streams_running(base_b, 1),
            ),
        })
        .build()
}

fn build_logcat_overflow_and_unverified() -> MockDeviceSource {
    let boot_id = "boot-log-overflow-001";
    let base_dev = 700 * SEC_NS;
    FaultScenarioBuilder::new("mock-logcat-overflow")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id, 37),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 5 * MS_NS,
            payload: make_heartbeat(
                base_dev,
                base_dev,
                BASE_WALL_NS,
                all_streams_running(base_dev, 1),
            ),
        })
        // 1. Honest Unverified baseline (existing boot pruning, matching VER-04 emulator behavior).
        .push(FaultStep::LogOverflowStats {
            pc_cont_ns: 10 * MS_NS,
            payload: LogOverflowStatsPayload {
                boottime_ns: base_dev + 10 * MS_NS,
                completeness: WireLogCompleteness::Unverified,
                buffers_count: 8,
                pruned_delta: -1,
                pruned_cumulative: 2023,
                detail: "baseline:existing_ring_pruning".into(),
            },
        })
        .push(FaultStep::LogRecord {
            pc_cont_ns: 20 * MS_NS,
            flags: FrameFlags::NONE,
            payload: make_text_log(
                base_dev + 20 * MS_NS,
                1_730_000_000,
                20_000_000,
                "ARB",
                "before_kill",
            ),
        })
        // 2. OverflowDetected + fault-isolated logcat Error -> Restarting -> Running + FLAG_RECOVERED.
        .push(FaultStep::LogOverflowStats {
            pc_cont_ns: SEC_NS,
            payload: LogOverflowStatsPayload {
                boottime_ns: base_dev + SEC_NS,
                completeness: WireLogCompleteness::OverflowDetected,
                buffers_count: 8,
                pruned_delta: 512,
                pruned_cumulative: 2535,
                detail: "process_exit:137_ring_overflow".into(),
            },
        })
        .push(FaultStep::StreamState {
            pc_cont_ns: SEC_NS + MS_NS,
            payload: StreamStatePayload {
                target_stream_id: StreamId::Logcat,
                state: StreamLifecycleState::Error,
                restart_count: 0,
                boottime_ns: base_dev + SEC_NS + MS_NS,
                emitted_seq: 1,
                reason: "process_exit:137".into(),
            },
        })
        .push(FaultStep::StreamState {
            pc_cont_ns: SEC_NS + 500 * MS_NS,
            payload: StreamStatePayload {
                target_stream_id: StreamId::Logcat,
                state: StreamLifecycleState::Running,
                restart_count: 1,
                boottime_ns: base_dev + SEC_NS + 500 * MS_NS,
                emitted_seq: 1,
                reason: "restarted".into(),
            },
        })
        .push(FaultStep::LogRecord {
            pc_cont_ns: SEC_NS + 510 * MS_NS,
            flags: FrameFlags::RECOVERED,
            payload: make_text_log(
                base_dev + SEC_NS + 510 * MS_NS,
                1_730_000_001,
                510_000_000,
                "ARB",
                "after_recovery",
            ),
        })
        .build()
}

#[cfg(test)]
mod tests {
    use arb_core::time::{
        DiscontinuityKind, StreamKind, TimeAlignment, TimestampSemantics, UncertaintyReason,
    };

    use super::{CaptureEvent, DeviceEventSource, FaultScenario, MockDeviceSource, TouchAction};
    use crate::adb::ReconnectClassification;
    use crate::wire::{FrameFlags, WireLogCompleteness};

    #[test]
    fn scenario_1_normal_fhd30_emits_1s_gop_audio_logs_taps_and_zero_discontinuities() {
        let mut src = MockDeviceSource::from_scenario(FaultScenario::NormalFhd30);
        let events = src.drain_all().expect("drain NormalFhd30");

        let video_frames: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                CaptureEvent::VideoFrame { packet, .. } => Some(packet),
                _ => None,
            })
            .collect();
        let keyframes = video_frames.iter().filter(|p| p.is_keyframe()).count();
        let configs = video_frames.iter().filter(|p| p.is_config()).count();
        let audio_frames = events
            .iter()
            .filter(|e| matches!(e, CaptureEvent::AudioFrame { .. }))
            .count();
        let logs = events
            .iter()
            .filter(|e| matches!(e, CaptureEvent::LogRecord { .. }))
            .count();
        let touches: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                CaptureEvent::TouchEvent { normalized, .. } => normalized.as_ref(),
                _ => None,
            })
            .collect();
        let discontinuities = events
            .iter()
            .filter(|e| matches!(e, CaptureEvent::Discontinuity(_)))
            .count();

        assert_eq!(configs, 1, "must emit 1 H.264 config packet");
        assert_eq!(keyframes, 2, "2s at 1s GOP must emit 2 IDR keyframes");
        assert_eq!(video_frames.len(), 61, "1 config + 60 video frames");
        assert!(audio_frames >= 10, "must emit AAC audio frames");
        assert_eq!(logs, 1, "must emit log record");
        assert_eq!(
            touches.len(),
            3,
            "must emit Down -> Move -> Up tap sequence"
        );
        assert_eq!(touches[0].action, TouchAction::Down, "first tap is Down");
        assert_eq!(touches[2].action, TouchAction::Up, "last tap is Up");
        assert_eq!(
            discontinuities, 0,
            "NormalFhd30 must have zero discontinuities"
        );
    }

    #[test]
    fn scenario_2_static_screen_vs_stall_separates_static_hold_from_stream_gap() {
        let mut src = MockDeviceSource::from_scenario(FaultScenario::StaticScreenVsStall);
        let events = src.drain_all().expect("drain StaticScreenVsStall");

        let static_hold_idx = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    CaptureEvent::StaticHold {
                        silent_duration_ns,
                        ..
                    } if *silent_duration_ns >= 11_900_000_000
                )
            })
            .expect("must emit StaticHold after 12s silent video with healthy beacon");

        let gap_idx = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    CaptureEvent::Discontinuity(d)
                        if d.kind == DiscontinuityKind::StreamGap
                            && d.stream == Some(StreamKind::Video)
                )
            })
            .expect("must emit StreamGap(Video) after video Error");

        assert!(
            static_hold_idx < gap_idx,
            "StaticHold must precede StreamGap transition"
        );
    }

    #[test]
    fn scenario_3_delayed_arrival_burst_preserves_native_pts_50s_behind_pc_ingest() {
        let mut src = MockDeviceSource::from_scenario(FaultScenario::DelayedArrivalBurst);
        let events = src.drain_all().expect("drain DelayedArrivalBurst");

        let delayed_frames: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                CaptureEvent::VideoFrame {
                    pc_cont_ns,
                    boot_id,
                    packet,
                } => Some((*pc_cont_ns, boot_id.as_str(), packet)),
                _ => None,
            })
            .collect();
        assert_eq!(delayed_frames.len(), 3, "must emit 3 delayed burst frames");

        for (pc_cont_ns, boot_id, packet) in delayed_frames {
            let mono_ns = packet.pts_monotonic_ns().expect("valid PTS ns");
            let aligned = src.timeline().map_device_monotonic(
                boot_id,
                mono_ns,
                TimestampSemantics::VideoEncoderInputEstimate,
                pc_cont_ns,
            );
            assert!(
                matches!(
                    aligned,
                    TimeAlignment::Anchored { boottime_ns, .. }
                        if (110_000_000_000..111_000_000_000).contains(&boottime_ns)
                ),
                "delayed packet must align to native ~110s boottime, NOT 60s ingest time: {aligned:?}"
            );
            assert!(
                pc_cont_ns >= 60_000_000_000,
                "PC ingest timestamp must preserve the 50s late arrival at >= 60s"
            );
        }
    }

    #[test]
    fn scenario_4_clock_sync_outage_and_wall_rollback_degrades_logcat_honestly() {
        let mut src =
            MockDeviceSource::from_scenario(FaultScenario::ClockSyncOutageAndWallRollback);
        let events = src
            .drain_all()
            .expect("drain ClockSyncOutageAndWallRollback");

        let kinds: Vec<DiscontinuityKind> = events
            .iter()
            .filter_map(|e| match e {
                CaptureEvent::Discontinuity(d) => Some(d.kind),
                _ => None,
            })
            .collect();
        assert!(
            kinds.contains(&DiscontinuityKind::ClockSyncStale),
            "must detect ClockSyncStale during sync probe outage: {kinds:?}"
        );
        assert!(
            kinds.contains(&DiscontinuityKind::DeviceWallJump),
            "must detect DeviceWallJump on -120s rollback: {kinds:?}"
        );

        let log = events
            .iter()
            .find_map(|e| match e {
                CaptureEvent::LogRecord {
                    pc_cont_ns,
                    boot_id,
                    payload,
                    ..
                } => Some((*pc_cont_ns, boot_id.as_str(), payload)),
                _ => None,
            })
            .expect("must emit rollback logcat entry");
        let aligned = src.timeline().map_logcat_wall(
            log.1,
            log.2.header_epoch_ns().expect("header epoch"),
            Some(log.2.device_read_boottime_ns),
            log.0,
        );
        assert!(
            !matches!(aligned, TimeAlignment::Anchored { .. }),
            "rollback logcat entry must degrade to Bounded/Unanchored rather than fabricated Anchored: {aligned:?}"
        );
    }

    #[test]
    fn scenario_5_partial_audio_stop_isolates_audio_gap_while_other_streams_continue() {
        let mut src = MockDeviceSource::from_scenario(FaultScenario::PartialAudioStop);
        let events = src.drain_all().expect("drain PartialAudioStop");

        let audio_gap_idx = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    CaptureEvent::Discontinuity(d)
                        if d.kind == DiscontinuityKind::StreamGap
                            && d.stream == Some(StreamKind::Audio)
                )
            })
            .expect("must emit StreamGap(Audio)");

        let after = &events[audio_gap_idx + 1..];
        assert!(
            after
                .iter()
                .any(|e| matches!(e, CaptureEvent::VideoFrame { .. })),
            "video must continue after audio stop"
        );
        assert!(
            after
                .iter()
                .any(|e| matches!(e, CaptureEvent::LogRecord { .. })),
            "logcat must continue after audio stop"
        );
        assert!(
            after
                .iter()
                .any(|e| matches!(e, CaptureEvent::TouchEvent { .. })),
            "touch input must continue after audio stop"
        );
    }

    #[test]
    fn scenario_6_disconnect_and_reconnect_same_boot_id_emits_restored_without_reboot() {
        let mut src = MockDeviceSource::from_scenario(FaultScenario::DisconnectAndReconnect);
        let events = src.drain_all().expect("drain DisconnectAndReconnect");

        assert!(
            events.iter().any(|e| matches!(
                e,
                CaptureEvent::Discontinuity(d) if d.kind == DiscontinuityKind::ConnectionLost
            )),
            "must emit ConnectionLost"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                CaptureEvent::ConnectionRestored { boot_id, .. } if boot_id == "boot-reconnect-001"
            )),
            "must emit ConnectionRestored with same boot_id"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                CaptureEvent::Discontinuity(d) if d.kind == DiscontinuityKind::DeviceReboot
            )),
            "same boot_id reconnect must NEVER emit DeviceReboot"
        );
    }

    #[test]
    fn scenario_7_device_deep_sleep_detects_15s_boottime_monotonic_divergence() {
        let mut src = MockDeviceSource::from_scenario(FaultScenario::DeviceDeepSleep);
        let events = src.drain_all().expect("drain DeviceDeepSleep");

        let sleep = events
            .iter()
            .find_map(|e| match e {
                CaptureEvent::Discontinuity(d) if d.kind == DiscontinuityKind::DeviceSleep => {
                    Some(d)
                }
                _ => None,
            })
            .expect("must emit DeviceSleep discontinuity");
        assert_eq!(
            sleep.delta_ns,
            Some(15_000_000_000),
            "sleep delta must equal 15s (15_000_000_000 ns)"
        );

        // Monotonic PTS spanning the sleep interval degrades honestly to Bounded(DeviceSleepSpanned).
        let spanned = src.timeline().map_device_monotonic(
            "boot-sleep-001",
            500_500_000_000,
            TimestampSemantics::VideoEncoderInputEstimate,
            16_000_000_000,
        );
        assert!(
            matches!(
                spanned,
                TimeAlignment::Bounded {
                    reason: UncertaintyReason::DeviceSleepSpanned,
                    ..
                }
            ),
            "monotonic timestamp across sleep must degrade to Bounded(DeviceSleepSpanned): {spanned:?}"
        );
    }

    #[test]
    fn scenario_8_device_reboot_detects_boot_a_to_boot_b_exactly_once() {
        let mut src = MockDeviceSource::from_scenario(FaultScenario::DeviceReboot);
        let events = src.drain_all().expect("drain DeviceReboot");

        let reboot_events: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    CaptureEvent::Discontinuity(d) if d.kind == DiscontinuityKind::DeviceReboot
                )
            })
            .collect();
        assert_eq!(
            reboot_events.len(),
            1,
            "boot-A -> boot-B must emit exactly one DeviceReboot discontinuity"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                CaptureEvent::Hello {
                    classification: ReconnectClassification::DeviceReboot {
                        previous_boot_id,
                        new_boot_id
                    },
                    ..
                } if previous_boot_id == "boot-A" && new_boot_id == "boot-B"
            )),
            "second Hello must carry DeviceReboot(boot-A -> boot-B) classification"
        );
    }

    #[test]
    fn scenario_9_logcat_overflow_and_unverified_emits_both_plus_recovered_flag() {
        let mut src = MockDeviceSource::from_scenario(FaultScenario::LogcatOverflowAndUnverified);
        let events = src.drain_all().expect("drain LogcatOverflowAndUnverified");

        let completeness_seq: Vec<WireLogCompleteness> = events
            .iter()
            .filter_map(|e| match e {
                CaptureEvent::LogOverflowStats { payload, .. } => Some(payload.completeness),
                _ => None,
            })
            .collect();
        assert_eq!(
            completeness_seq,
            vec![
                WireLogCompleteness::Unverified,
                WireLogCompleteness::OverflowDetected,
            ],
            "must emit Unverified baseline followed by OverflowDetected"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                CaptureEvent::Discontinuity(d)
                    if d.kind == DiscontinuityKind::StreamGap
                        && d.stream == Some(StreamKind::Logcat)
            )),
            "logcat Error must emit StreamGap(Logcat)"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                CaptureEvent::LogRecord { flags, .. } if flags.contains(FrameFlags::RECOVERED)
            )),
            "post-recovery logcat frame must carry FLAG_RECOVERED"
        );
    }

    #[test]
    fn all_nine_scenarios_list_is_complete_and_deterministic() {
        assert_eq!(
            FaultScenario::ALL.len(),
            9,
            "FaultScenario::ALL must expose all 9 built-in scenarios"
        );
        for &scenario in FaultScenario::ALL {
            let run1 = MockDeviceSource::from_scenario(scenario)
                .drain_all()
                .expect("run 1 must succeed");
            let run2 = MockDeviceSource::from_scenario(scenario)
                .drain_all()
                .expect("run 2 must succeed");
            assert_eq!(
                run1, run2,
                "scenario {scenario:?} must be 100% deterministic across repeated runs"
            );
        }
    }
}
