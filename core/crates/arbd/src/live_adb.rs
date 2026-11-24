//! Live ADB device-agent deployment, streaming ingestion, and auto-reconnect supervision.
//!
//! Deploys `arb-companion.dex.jar` and `replay-scrcpy-audio.dex.jar` via
//! [`AdbClient`], runs concurrent companion/video/audio reader threads feeding
//! [`CapturePipeline`], performs periodic `Ping`/`Pong` sync probes, supervises
//! automatic reconnects with [`arb_adb::adb::ReconnectPolicy`] (`200ms -> 500ms -> 1s -> 2s`),
//! and guarantees remote `/data/local/tmp/arb-*` and `adb forward` cleanup on stop.
//!
//! # Liveness (R3 M5 / M6)
//!
//! Every liveness decision is delegated to the R2-hardened [`SessionSupervisor`] (the same
//! state machine [`arb_adb::adb::AdbSessionSource`] uses):
//! - each video frame → [`SessionSupervisor::on_video_frame`];
//! - video connect failure / EOF / error → [`SessionSupervisor::on_media_transport_lost`]
//!   and a `StreamGap` (never a silent loss, never `StaticHold`);
//! - a periodic [`SessionSupervisor::evaluate_video_liveness`] tick → `StaticHold` /
//!   `StreamGap`, one event per episode;
//! - companion beacon older than `beacon_timeout_ns` → the session ends with
//!   `ConnectionLost` and reconnects;
//! - on `DeviceReboot` / `ContinuityUnverified` the timeline's own `DeviceReboot` for the
//!   new `boot_id` is suppressed (the supervisor already reported a real reboot, and an
//!   unverified continuity must never be asserted as one).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arb_adb::adb::{
    AdbClient, AdbDeviceState, DeployedAgentPaths, ReconnectClassification, SessionSupervisor,
    StreamEnable, SupervisorConfig, VideoLivenessStatus,
};
use arb_adb::mock::CaptureEvent;
use arb_adb::scrcpy::{
    AudioCaptureMode, AudioPacketKind, AudioTunnelReader, AudioTunnelStatus,
    MIN_AUDIO_PLAYBACK_API_LEVEL, SCRCPY_DUMMY_BYTE, ScrcpyError, ScrcpyLaunchOptions, VideoRecord,
    VideoTunnelReader, audio_unavailable_reason_for_api, unsupported_android_api_warning,
};
use arb_adb::wire::{
    DEFAULT_MAX_PAYLOAD_LEN, FrameFlags, HeartbeatPayload, HelloPayload, PingPayload, ShutdownCode,
    ShutdownPayload, StreamId, WireFrame, WireMessage,
};
use arb_core::config::CaptureMode;
use arb_core::device::{DeviceSelectionOutcome, DeviceSelectionPolicy, validate_adb_serial};
use arb_core::time::{
    ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind,
    PcSyncProbe, StreamKind,
};
use arb_os::clock::{HostClockSample, sample_host_clock};

use crate::capture::CapturePipeline;
use crate::error::DaemonError;
use crate::protocol::{AudioCaptureState, CaptureStartParams};
use crate::time_util::current_unix_millis;

/// A `Ping` without its `Pong` after this long is abandoned so sync probes resume (R3 S13).
const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// Interval between `Ping` sync probes.
const PING_INTERVAL: Duration = Duration::from_secs(1);
/// Interval of the live video liveness evaluation.
const LIVENESS_TICK_INTERVAL: Duration = Duration::from_secs(1);
/// The beacon timeout is at least this many heartbeat intervals.
const BEACON_TIMEOUT_HEARTBEATS: i64 = 6;
/// Handshake wait for `ARB_COMPANION_READY`.
const COMPANION_READY_TIMEOUT: Duration = Duration::from_secs(8);
/// Poll slice used while waiting so `capture.stop` is honoured promptly (R3 S12).
const STOP_POLL_SLICE: Duration = Duration::from_millis(100);
/// Maximum bytes retained from `scrcpy-server` stderr for HW-encoder contention diagnostics.
const MAX_SCRCPY_STDERR_BYTES: usize = 8 * 1024;

/// Returns `true` when `text` matches known Android `MediaCodec` / H.264 hardware encoder
/// initialization failure signatures (such as simultaneous Android Studio Running Devices
/// mirroring exhausting device hardware encoder instances).
#[must_use]
pub fn is_hw_encoder_contention_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "mediacodec",
        "configure failed",
        "resource_exhausted",
        "error_insufficient_resources",
        "insufficient resources",
        "codec_error",
        "start failed",
        "could not open video stream",
        "unable to create encoder",
        "createinputsurface",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Formats a `scrcpy-server` video failure with an honest Running Devices mirroring
/// hardware-encoder contention diagnostic hint when `MediaCodec` / encoder exhaustion is
/// detected in `raw_error` or `scrcpy_stderr`.
#[must_use]
pub fn classify_scrcpy_video_error(raw_error: &str, scrcpy_stderr: Option<&str>) -> String {
    let stderr_trimmed = scrcpy_stderr.map(str::trim).filter(|s| !s.is_empty());
    let combined = match stderr_trimmed {
        Some(stderr) => format!("{raw_error}\n{stderr}"),
        None => raw_error.to_owned(),
    };
    if is_hw_encoder_contention_error(&combined) {
        let detail = stderr_trimmed.map_or_else(
            || raw_error.to_owned(),
            |stderr| format!("{raw_error} (scrcpy stderr: {stderr})"),
        );
        format!(
            "scrcpy video encoder initialization failed ({detail}). Possible H.264 hardware encoder contention (for example with Android Studio Running Devices mirroring). Try lowering video_max_size to 1024 or switching capture_mode to logs_only."
        )
    } else if let Some(stderr) = stderr_trimmed {
        format!("scrcpy video unavailable: {raw_error} (scrcpy stderr: {stderr})")
    } else {
        format!("scrcpy video unavailable: {raw_error}")
    }
}

/// Handle to an active background live ADB capture supervisor thread.
#[derive(Debug)]
pub struct LiveCaptureHandle {
    stop_flag: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LiveCaptureHandle {
    /// Signals the live capture loop to stop, send graceful companion `Shutdown`,
    /// clean up remote DEX jars and ADB forwards, and waits for thread exit.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }

    /// Returns `true` once the supervisor thread has exited on its own (e.g. storage
    /// pressure stopped capture), so a new `capture.start` may replace it (R3 S12).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for LiveCaptureHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Generation-scoped writer into the shared [`CapturePipeline`].
///
/// Events are dropped once a newer session started (R3 S12), and a poisoned pipeline lock
/// stops the session instead of panicking every reader thread (R3 S15).
#[derive(Debug, Clone)]
struct LiveSink {
    pipeline: Arc<Mutex<CapturePipeline>>,
    generation: u64,
    failed: Arc<AtomicBool>,
}

impl LiveSink {
    fn push(&self, event: CaptureEvent) {
        match self.pipeline.lock() {
            // Ring write failures are recorded inside the pipeline as `StreamGap`s (R3 S5).
            Ok(mut guard) => {
                let _ = guard.ingest_event_for_session(self.generation, event);
            }
            Err(_) => self.failed.store(true, Ordering::Release),
        }
    }

    fn push_all(&self, events: Vec<CaptureEvent>) {
        for event in events {
            self.push(event);
        }
    }

    fn set_display_size(&self, width: u32, height: u32) {
        if let Ok(mut guard) = self.pipeline.lock() {
            guard.set_display_size_for_session(self.generation, width, height);
        } else {
            self.failed.store(true, Ordering::Release);
        }
    }

    fn set_package_uids(&self, uid_packages: BTreeMap<u32, String>) {
        if let Ok(mut guard) = self.pipeline.lock() {
            guard.set_package_uids_for_session(self.generation, uid_packages);
        } else {
            self.failed.store(true, Ordering::Release);
        }
    }

    fn set_last_video_error(&self, error: Option<String>) {
        if let Ok(mut guard) = self.pipeline.lock() {
            guard.set_last_video_error_for_session(self.generation, error);
        } else {
            self.failed.store(true, Ordering::Release);
        }
    }

    fn set_audio_status(&self, state: AudioCaptureState, reason: Option<String>) {
        if let Ok(mut guard) = self.pipeline.lock() {
            guard.set_audio_status_for_session(self.generation, state, reason);
        } else {
            self.failed.store(true, Ordering::Release);
        }
    }

    fn set_device_api_warning(&self, warning: Option<String>) {
        if let Ok(mut guard) = self.pipeline.lock() {
            guard.set_device_api_warning_for_session(self.generation, warning);
        } else {
            self.failed.store(true, Ordering::Release);
        }
    }

    /// `true` while this sink's session is still the current, active capture session.
    fn session_is_current(&self) -> bool {
        if self.failed.load(Ordering::Acquire) {
            return false;
        }
        if let Ok(guard) = self.pipeline.lock() {
            guard.session_generation() == self.generation && guard.is_active()
        } else {
            self.failed.store(true, Ordering::Release);
            false
        }
    }

    fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

/// Starts a supervised live ADB capture session writing events into `pipeline`.
///
/// # Errors
///
/// Returns [`DaemonError::Adb`] if `adb` cannot be located or no online device
/// matches, [`DaemonError::NotFound`] if `arb-companion.dex.jar` cannot be found, or an
/// internal error if the pipeline lock is poisoned.
pub fn start_live_adb_capture(
    params: &CaptureStartParams,
    pipeline: &Arc<Mutex<CapturePipeline>>,
) -> Result<(String, LiveCaptureHandle), DaemonError> {
    let adb = AdbClient::discover()?;
    let serial = resolve_target_serial(&adb, params.serial.as_deref())?;
    let capture_mode = params.capture_mode.unwrap_or(CaptureMode::VideoAndLogs);
    let companion_jar = resolve_agent_jar(
        params.companion_jar.as_deref(),
        "ARB_COMPANION_JAR",
        "arb-companion.dex.jar",
    )?;
    // In `logs_only` mode, never deploy or start scrcpy-server (zero video/audio MediaCodec load).
    let scrcpy_jar = if capture_mode == CaptureMode::LogsOnly {
        None
    } else {
        resolve_agent_jar(
            params.scrcpy_jar.as_deref(),
            "ARB_SCRCPY_JAR",
            "replay-scrcpy-audio.dex.jar",
        )
        .ok()
    };

    let is_video_mode = capture_mode == CaptureMode::VideoAndLogs;
    let cfg = LiveCaptureConfig {
        serial: serial.clone(),
        capture_mode,
        companion_jar,
        scrcpy_jar,
        max_size: params.max_size.unwrap_or(1024),
        max_fps: params.max_fps.unwrap_or(30),
        video_bit_rate: params.video_bit_rate.unwrap_or(4_000_000),
        audio_enabled: is_video_mode && params.audio_enabled.unwrap_or(true),
        logcat_enabled: params.logcat_enabled.unwrap_or(true),
        input_enabled: is_video_mode && params.input_enabled.unwrap_or(true),
        heartbeat_ms: params.heartbeat_ms.unwrap_or(250),
    };

    let generation = pipeline
        .lock()
        .map_err(|_| DaemonError::Internal("capture pipeline lock poisoned".to_owned()))?
        .begin_live_session_with_mode(serial.clone(), capture_mode);
    let sink = LiveSink {
        pipeline: Arc::clone(pipeline),
        generation,
        failed: Arc::new(AtomicBool::new(false)),
    };

    let stop_flag = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop_flag);

    let handle = thread::Builder::new()
        .name(format!("arbd-adb-{serial}"))
        .spawn(move || {
            run_supervised_adb_loop(&adb, &cfg, &thread_stop, &sink);
            if let Ok(mut guard) = sink.pipeline.lock() {
                guard.mark_stopped_if_current(sink.generation);
            }
        })?;

    Ok((
        serial,
        LiveCaptureHandle {
            stop_flag,
            thread: Some(handle),
        },
    ))
}

#[derive(Debug, Clone)]
struct LiveCaptureConfig {
    serial: String,
    capture_mode: CaptureMode,
    companion_jar: PathBuf,
    scrcpy_jar: Option<PathBuf>,
    max_size: u32,
    max_fps: u32,
    video_bit_rate: u32,
    audio_enabled: bool,
    logcat_enabled: bool,
    input_enabled: bool,
    heartbeat_ms: u32,
}

fn resolve_target_serial(adb: &AdbClient, requested: Option<&str>) -> Result<String, DaemonError> {
    if let Some(serial) = requested.map(str::trim).filter(|s| !s.is_empty()) {
        validate_adb_serial(serial).map_err(|err| DaemonError::InvalidParams(err.to_string()))?;
        // Strict no-auto-switch rule: once a target serial is bound, stay bound to it even if
        // another device is online so capture waits for reconnection instead of switching.
        return Ok(serial.to_owned());
    }
    let devices = adb.devices()?;
    let online: Vec<String> = devices
        .into_iter()
        .filter(|d| d.state == AdbDeviceState::Device)
        .map(|d| d.serial)
        .collect();
    match DeviceSelectionPolicy::evaluate(None, &online) {
        DeviceSelectionOutcome::Selected { serial, .. } => Ok(serial),
        DeviceSelectionOutcome::NoOnlineDevices => Err(DaemonError::NotFound(
            "no online ADB devices found".to_owned(),
        )),
        DeviceSelectionOutcome::MultipleDevicesNeedSelection { online_serials } => {
            Err(DaemonError::Conflict(format!(
                "MultipleDevicesNeedSelection: multiple online ADB devices found ({}); select a target device explicitly",
                online_serials.join(", ")
            )))
        }
        DeviceSelectionOutcome::WaitingForPreferredDevice {
            preferred_serial, ..
        } => Ok(preferred_serial),
        _ => Err(DaemonError::Internal(
            "unexpected device selection outcome".to_owned(),
        )),
    }
}

fn resolve_agent_jar(
    explicit: Option<&Path>,
    env_var: &str,
    file_name: &str,
) -> Result<PathBuf, DaemonError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(DaemonError::NotFound(format!(
            "agent jar not found at `{}`",
            path.display()
        )));
    }
    if let Some(from_env) = std::env::var_os(env_var) {
        let p = PathBuf::from(from_env);
        if p.is_file() {
            return Ok(p);
        }
    }

    let mut search_roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        search_roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        search_roots.push(dir.to_path_buf());
    }

    for root in search_roots {
        let mut cursor = Some(root.as_path());
        for _ in 0..6 {
            let Some(dir) = cursor else {
                break;
            };
            let direct = dir.join(file_name);
            if direct.is_file() {
                return Ok(direct);
            }
            let in_agent_dir = dir.join("device-agent").join(file_name);
            if in_agent_dir.is_file() {
                return Ok(in_agent_dir);
            }
            let in_build = dir.join("device-agent").join("build").join(file_name);
            if in_build.is_file() {
                return Ok(in_build);
            }
            cursor = dir.parent();
        }
    }

    Err(DaemonError::NotFound(format!(
        "device-agent artifact `{file_name}` not found (build with ./device-agent/build.sh --with-scrcpy-audio)"
    )))
}

fn sample_cont_ns() -> i64 {
    sample_host_clock().map_or(0, |s| s.continuous_mono_ns)
}

fn sample_clock_or_zero() -> HostClockSample {
    sample_host_clock().unwrap_or(HostClockSample {
        active_mono_ns: 0,
        continuous_mono_ns: 0,
        wall_epoch_ns: 0,
    })
}

/// Coarse classification of [`VideoLivenessStatus`] used to emit one event per episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LivenessKind {
    Active,
    StaticHold,
    StreamGap,
}

/// Long-lived (across reconnects) live-session bookkeeping layered on [`SessionSupervisor`].
///
/// Pure state machine: every method returns the [`CaptureEvent`]s to record, so the R3
/// M5/M6/S14 behaviour is unit-tested without a device.
#[derive(Debug)]
struct LiveTracker {
    timeline: ClockTimeline,
    suppress_reboot_for: Option<String>,
    boot_id: String,
    last_beacon_pc: Option<i64>,
    last_video_pc: Option<i64>,
    last_liveness: Option<LivenessKind>,
    video_lost: bool,
    disconnect_reported: bool,
}

impl LiveTracker {
    fn new() -> Self {
        Self {
            timeline: ClockTimeline::new(ClockTimelineConfig::new()),
            suppress_reboot_for: None,
            boot_id: "unknown".to_owned(),
            last_beacon_pc: None,
            last_video_pc: None,
            last_liveness: None,
            video_lost: false,
            disconnect_reported: false,
        }
    }

    /// Classifies a companion `Hello` and resets the per-session liveness state.
    fn on_hello(
        &mut self,
        supervisor: &mut SessionSupervisor,
        hello: HelloPayload,
        pc_cont_ns: i64,
    ) -> Vec<CaptureEvent> {
        let prev_attempts = supervisor.reconnect_attempts();
        let (classification, reboot_disc) = supervisor.on_hello(&hello, pc_cont_ns);
        if matches!(
            classification,
            ReconnectClassification::DeviceReboot { .. }
                | ReconnectClassification::ContinuityUnverified { .. }
        ) {
            // R3 M6: DeviceReboot is already reported by the supervisor; ContinuityUnverified
            // must never be asserted as a reboot (PROTOCOL §3.1).
            self.suppress_reboot_for = Some(hello.boot_id.clone());
        }
        let is_restored = matches!(
            classification,
            ReconnectClassification::ConnectionRestored { .. }
        );
        self.boot_id.clone_from(&hello.boot_id);
        self.last_beacon_pc = Some(pc_cont_ns);
        self.last_video_pc = None;
        self.last_liveness = None;
        self.video_lost = false;
        self.disconnect_reported = false;

        let boot_id = hello.boot_id.clone();
        let mut events = vec![CaptureEvent::Hello {
            pc_cont_ns,
            payload: hello,
            classification,
        }];
        if let Some(disc) = reboot_disc {
            events.push(CaptureEvent::Discontinuity(disc));
        }
        if is_restored {
            events.push(CaptureEvent::ConnectionRestored {
                pc_cont_ns,
                boot_id,
                reconnect_attempts: prev_attempts,
            });
        }
        events
    }

    fn on_heartbeat(
        &mut self,
        supervisor: &mut SessionSupervisor,
        hb: HeartbeatPayload,
        pc_cont_ns: i64,
    ) -> Vec<CaptureEvent> {
        supervisor.on_heartbeat(&hb, pc_cont_ns);
        self.last_beacon_pc = Some(pc_cont_ns);
        let triple = DeviceClockTriple {
            boot_id: self.boot_id.clone(),
            boottime_ns: hb.boottime_ns,
            monotonic_ns: hb.monotonic_ns,
            realtime_ns: hb.realtime_ns,
        };
        let discs = self.timeline.ingest_device_triple(triple, pc_cont_ns);
        let mut events = vec![CaptureEvent::Heartbeat {
            pc_cont_ns,
            boot_id: self.boot_id.clone(),
            payload: hb,
        }];
        for disc in discs {
            if disc.kind == DiscontinuityKind::DeviceReboot
                && self.suppress_reboot_for.as_deref() == Some(self.boot_id.as_str())
            {
                self.suppress_reboot_for = None;
                continue;
            }
            events.push(CaptureEvent::Discontinuity(disc));
        }
        events
    }

    fn on_video_frame(&mut self, supervisor: &mut SessionSupervisor, pc_cont_ns: i64) {
        supervisor.on_video_frame(pc_cont_ns);
        self.last_video_pc = Some(pc_cont_ns);
        if self.video_lost {
            self.video_lost = false;
            self.last_liveness = None;
        }
    }

    /// Host-side media transport loss (connect failure, EOF or error) → `StreamGap`.
    fn on_media_lost(
        &mut self,
        supervisor: &mut SessionSupervisor,
        stream: StreamId,
        pc_cont_ns: i64,
        reason: &str,
    ) -> Option<CaptureEvent> {
        if stream == StreamId::Video {
            if self.video_lost {
                return None;
            }
            self.video_lost = true;
            // The gap is reported here; the liveness tick must not re-report it.
            self.last_liveness = Some(LivenessKind::StreamGap);
        }
        let disc = supervisor.on_media_transport_lost(stream, pc_cont_ns, reason);
        Some(CaptureEvent::Discontinuity(disc))
    }

    /// Periodic video liveness evaluation: one `StaticHold` / `StreamGap` per episode.
    fn tick_liveness(
        &mut self,
        supervisor: &SessionSupervisor,
        now_pc_cont_ns: i64,
    ) -> Option<CaptureEvent> {
        if self.video_lost || self.last_beacon_pc.is_none() || self.last_video_pc.is_none() {
            return None;
        }
        let status = supervisor.evaluate_video_liveness(now_pc_cont_ns);
        let kind = match status {
            VideoLivenessStatus::StaticHold { .. } => LivenessKind::StaticHold,
            VideoLivenessStatus::StreamGap { .. } => LivenessKind::StreamGap,
            _ => LivenessKind::Active,
        };
        if self.last_liveness == Some(kind) {
            return None;
        }
        self.last_liveness = Some(kind);
        match status {
            VideoLivenessStatus::StaticHold { silent_duration_ns } => {
                Some(CaptureEvent::StaticHold {
                    pc_cont_ns: now_pc_cont_ns,
                    boot_id: self.boot_id.clone(),
                    silent_duration_ns,
                })
            }
            VideoLivenessStatus::StreamGap { reason } => {
                Some(CaptureEvent::Discontinuity(DiscontinuityEvent {
                    kind: DiscontinuityKind::StreamGap,
                    stream: Some(StreamKind::Video),
                    boot_id: Some(self.boot_id.clone()),
                    start_boottime_ns: None,
                    end_boottime_ns: None,
                    pc_cont_start_ns: now_pc_cont_ns,
                    pc_cont_end_ns: None,
                    delta_ns: None,
                    detail: Some(reason),
                }))
            }
            _ => None,
        }
    }

    /// `true` when the companion beacon is older than `beacon_timeout_ns` (R3 M5): a wedged
    /// device or USB link keeps the TCP forward open, so EOF alone never detects it.
    fn beacon_timed_out(&self, supervisor: &SessionSupervisor, now_pc_cont_ns: i64) -> bool {
        self.last_beacon_pc.is_some_and(|last| {
            now_pc_cont_ns.saturating_sub(last) > supervisor.config().beacon_timeout_ns
        })
    }

    /// Ends a session: the first failure since the last `Hello` records one
    /// `ConnectionLost`; further failed reconnect attempts only advance the backoff (S14).
    fn on_session_end(
        &mut self,
        supervisor: &mut SessionSupervisor,
        pc_cont_ns: i64,
        reason: &str,
    ) -> (Duration, Option<CaptureEvent>) {
        self.last_beacon_pc = None;
        self.last_video_pc = None;
        if self.disconnect_reported {
            return (supervisor.on_reconnect_attempt_failed(), None);
        }
        self.disconnect_reported = true;
        let (backoff, disc) = supervisor.on_disconnect(pc_cont_ns, reason);
        (backoff, Some(CaptureEvent::Discontinuity(disc)))
    }
}

/// Outstanding `Ping` bookkeeping with expiry (R3 S13).
#[derive(Debug)]
struct PingState {
    pending: Option<(u64, HostClockSample, Instant)>,
    next_ping_at: Instant,
    seq: u32,
}

impl PingState {
    fn new(now: Instant) -> Self {
        Self {
            pending: None,
            next_ping_at: now,
            seq: 1,
        }
    }

    /// Expires a `Ping` whose `Pong` never came, then reports whether a new one is due.
    fn due(&mut self, now: Instant) -> bool {
        if let Some((_, _, sent_at)) = self.pending
            && now.saturating_duration_since(sent_at) > PING_TIMEOUT
        {
            self.pending = None;
        }
        self.pending.is_none() && now >= self.next_ping_at
    }

    fn mark_sent(&mut self, nonce: u64, sample: HostClockSample, now: Instant) {
        self.pending = Some((nonce, sample, now));
        self.seq = self.seq.wrapping_add(1);
        self.next_ping_at = now + PING_INTERVAL;
    }

    /// Returns the send sample when `nonce` answers the outstanding `Ping`.
    fn take_match(&mut self, nonce: u64) -> Option<HostClockSample> {
        let (expected, sample, _) = self.pending.take()?;
        (expected == nonce).then_some(sample)
    }
}

/// Signals from the media reader threads to the session loop that owns the supervisor.
#[derive(Debug)]
enum MediaSignal {
    VideoFrame(i64),
    Lost {
        stream: StreamId,
        pc_cont_ns: i64,
        reason: String,
    },
}

fn supervisor_config_for(cfg: &LiveCaptureConfig) -> SupervisorConfig {
    let mut config = SupervisorConfig::new();
    let heartbeat_ns = i64::from(cfg.heartbeat_ms).saturating_mul(1_000_000);
    config.beacon_timeout_ns = config
        .beacon_timeout_ns
        .max(heartbeat_ns.saturating_mul(BEACON_TIMEOUT_HEARTBEATS));
    config
}

/// Proactive audio launch plan computed from the requested audio flag and device API level.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedScrcpyAudio {
    /// Audio was disabled by user config or `logs_only` mode (`audio=false`).
    Disabled,
    /// Detected Android SDK does not support requested non-muting audio (`sdk < 33`);
    /// proactively launch `scrcpy-server` with `audio=false` (single video socket, zero audio wait).
    ProactiveUnavailable {
        /// Detected Android API level (`ro.build.version.sdk` or `Hello.api_level`).
        api_level: u32,
        /// Explicit tunnel availability status recorded in the capture stream.
        status: AudioTunnelStatus,
        /// Honest diagnostic message exposed via `CaptureStatusView.audio_reason`.
        reason: String,
    },
    /// Attempt `audio=true` with runtime fallback to `audio=false` if the device rejects audio.
    TryWithAudio {
        /// API level passed to [`ScrcpyLaunchOptions::build_server_args`] (`>= 33`).
        effective_api_level: u32,
    },
}

/// Computes whether `scrcpy-server` should proactively launch with `audio=false` or attempt
/// `audio=true` (with runtime fallback), given `audio_mode` and `detected_sdk`.
#[must_use]
pub fn plan_scrcpy_audio(
    audio_mode: AudioCaptureMode,
    detected_sdk: Option<u32>,
) -> PlannedScrcpyAudio {
    if matches!(audio_mode, AudioCaptureMode::Disabled) {
        return PlannedScrcpyAudio::Disabled;
    }
    match detected_sdk {
        Some(sdk) if sdk > 0 && sdk < MIN_AUDIO_PLAYBACK_API_LEVEL => {
            let reason = audio_unavailable_reason_for_api(sdk).unwrap_or_else(|| {
                format!(
                    "Audio capture unavailable on Android API {sdk}. Continuing with video + logs + taps."
                )
            });
            PlannedScrcpyAudio::ProactiveUnavailable {
                api_level: sdk,
                status: AudioTunnelStatus::UnavailableApiLevel {
                    api_level: sdk,
                    min_required: MIN_AUDIO_PLAYBACK_API_LEVEL,
                },
                reason,
            }
        }
        Some(sdk) if sdk >= MIN_AUDIO_PLAYBACK_API_LEVEL => PlannedScrcpyAudio::TryWithAudio {
            effective_api_level: sdk,
        },
        _ => PlannedScrcpyAudio::TryWithAudio {
            effective_api_level: MIN_AUDIO_PLAYBACK_API_LEVEL,
        },
    }
}

fn run_supervised_adb_loop(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    stop_flag: &Arc<AtomicBool>,
    sink: &LiveSink,
) {
    let mut supervisor = SessionSupervisor::new(cfg.serial.clone(), supervisor_config_for(cfg));
    let mut tracker = LiveTracker::new();
    let mut cached_sdk: Option<u32> = None;

    while !stop_flag.load(Ordering::Acquire) {
        if !sink.session_is_current() {
            break;
        }

        supervisor.begin_connecting();
        let outcome = run_single_adb_session(
            adb,
            cfg,
            stop_flag,
            sink,
            &mut supervisor,
            &mut tracker,
            &mut cached_sdk,
        );
        if stop_flag.load(Ordering::Acquire) || sink.has_failed() {
            break;
        }

        let reason = match outcome {
            Ok(()) => "session_ended".to_owned(),
            Err(err) => err.to_string(),
        };
        let (backoff, disc) = tracker.on_session_end(&mut supervisor, sample_cont_ns(), &reason);
        if let Some(disc) = disc {
            sink.push(disc);
        }

        // Sleep in small increments so capture.stop wakes up quickly.
        let deadline = Instant::now() + backoff;
        while Instant::now() < deadline && !stop_flag.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

struct SessionCleanupGuard<'a> {
    adb: &'a AdbClient,
    serial: &'a str,
    deployed: DeployedAgentPaths,
    forwarded_ports: Vec<u16>,
    companion_child: Option<Child>,
    scrcpy_child: Option<Child>,
}

impl Drop for SessionCleanupGuard<'_> {
    fn drop(&mut self) {
        kill_child(self.companion_child.take());
        kill_child(self.scrcpy_child.take());
        let _ = self
            .adb
            .cleanup_session(self.serial, &self.deployed, &self.forwarded_ports);
    }
}

fn kill_child(child: Option<Child>) {
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn run_single_adb_session(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    stop_flag: &Arc<AtomicBool>,
    sink: &LiveSink,
    supervisor: &mut SessionSupervisor,
    tracker: &mut LiveTracker,
    cached_sdk: &mut Option<u32>,
) -> Result<(), DaemonError> {
    if cached_sdk.is_none() {
        *cached_sdk = adb.query_sdk_version(&cfg.serial);
    }
    let token_raw = current_unix_millis() % 1_000_000;
    let session_token = format!("s{}-{token_raw:06}", std::process::id());
    let scrcpy_jar_for_deploy = if cfg.capture_mode == CaptureMode::LogsOnly {
        None
    } else {
        cfg.scrcpy_jar.as_deref()
    };
    let deployed = adb.deploy_session_agents(
        &cfg.serial,
        &session_token,
        &cfg.companion_jar,
        scrcpy_jar_for_deploy,
    )?;

    let mut guard = SessionCleanupGuard {
        adb,
        serial: &cfg.serial,
        deployed,
        forwarded_ports: Vec::new(),
        companion_child: None,
        scrcpy_child: None,
    };

    // 1. Forward and launch arb-companion and complete initial Hello handshake.
    let (mut companion_reader, mut companion_writer, hello) =
        launch_and_handshake_companion(adb, cfg, &session_token, &mut guard, stop_flag)?;
    let detected_sdk = (*cached_sdk).or_else(|| (hello.api_level > 0).then_some(hello.api_level));
    if cached_sdk.is_none() {
        *cached_sdk = detected_sdk;
    }
    sink.set_device_api_warning(detected_sdk.and_then(unsupported_android_api_warning));
    sink.push_all(tracker.on_hello(supervisor, hello, sample_cont_ns()));
    if let Some((display_w, display_h)) = adb.query_display_size(&cfg.serial) {
        sink.set_display_size(display_w, display_h);
    }
    let uid_packages = adb.query_package_uids(&cfg.serial);
    if !uid_packages.is_empty() {
        sink.set_package_uids(uid_packages);
    }

    // 2. Launch scrcpy-server-v5.0.1 for H.264 video (and optional AAC audio on API >= 33),
    //    UNLESS `capture_mode == CaptureMode::LogsOnly` (zero MediaCodec load on device).
    let session_stop = Arc::new(AtomicBool::new(false));
    let (media_tx, media_rx) = channel();
    let scrcpy_stderr = Arc::new(Mutex::new(String::new()));
    let (media_threads, media_sockets) = start_session_media_streams(
        adb,
        cfg,
        &mut guard,
        detected_sdk,
        stop_flag,
        &session_stop,
        sink,
        supervisor,
        tracker,
        media_tx,
        &scrcpy_stderr,
    );

    // 3. Companion control/logcat/input loop with periodic Ping/Pong RTT sync probes.
    let loop_result = run_companion_event_loop(
        &mut companion_reader,
        &mut companion_writer,
        stop_flag,
        sink,
        supervisor,
        tracker,
        &media_rx,
        &scrcpy_stderr,
    );

    session_stop.store(true, Ordering::Release);
    for sock in &media_sockets {
        let _ = sock.shutdown(Shutdown::Both);
    }
    for handle in media_threads {
        let _ = handle.join();
    }
    loop_result
}

#[expect(
    clippy::too_many_arguments,
    reason = "starts optional scrcpy media streams and records any startup video gap on the supervisor"
)]
fn start_session_media_streams(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    guard: &mut SessionCleanupGuard<'_>,
    detected_sdk: Option<u32>,
    stop_flag: &Arc<AtomicBool>,
    session_stop: &Arc<AtomicBool>,
    sink: &LiveSink,
    supervisor: &mut SessionSupervisor,
    tracker: &mut LiveTracker,
    media_tx: Sender<MediaSignal>,
    scrcpy_stderr: &Arc<Mutex<String>>,
) -> (Vec<JoinHandle<()>>, Vec<TcpStream>) {
    if cfg.capture_mode == CaptureMode::LogsOnly {
        sink.set_last_video_error(None);
        sink.set_audio_status(AudioCaptureState::Disabled, None);
        return (Vec::new(), Vec::new());
    }
    let media = launch_scrcpy_streams(
        adb,
        cfg,
        guard,
        detected_sdk,
        &tracker.boot_id,
        &MediaThreadCtx {
            session_stop: Arc::clone(session_stop),
            global_stop: Arc::clone(stop_flag),
            sink: sink.clone(),
            signals: media_tx,
        },
        scrcpy_stderr,
    );
    match media {
        Ok(started) => {
            sink.set_last_video_error(None);
            started
        }
        Err(err) => {
            // Give the stderr reader a brief slice to collect any MediaCodec error line.
            thread::sleep(Duration::from_millis(75));
            let stderr_snapshot = scrcpy_stderr
                .lock()
                .ok()
                .map(|buf| buf.clone())
                .filter(|s| !s.trim().is_empty());
            let detail = classify_scrcpy_video_error(&err.to_string(), stderr_snapshot.as_deref());
            sink.set_last_video_error(Some(detail.clone()));
            // R3 M5: no silent video loss; capture continues without video.
            if let Some(gap) =
                tracker.on_media_lost(supervisor, StreamId::Video, sample_cont_ns(), &detail)
            {
                sink.push(gap);
            }
            (Vec::new(), Vec::new())
        }
    }
}

fn launch_and_handshake_companion(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    session_token: &str,
    guard: &mut SessionCleanupGuard<'_>,
    stop_flag: &Arc<AtomicBool>,
) -> Result<(TcpStream, TcpStream, HelloPayload), DaemonError> {
    let companion_socket = format!("arb-companion-{session_token}");
    let companion_port = adb.forward_tcp_to_abstract(&cfg.serial, &companion_socket)?;
    guard.forwarded_ports.push(companion_port);

    let logcat_en = if cfg.logcat_enabled {
        StreamEnable::Enabled
    } else {
        StreamEnable::Disabled
    };
    let input_en = if cfg.input_enabled {
        StreamEnable::Enabled
    } else {
        StreamEnable::Disabled
    };
    let companion_cmd = AdbClient::<arb_adb::adb::SystemAdbRunner>::companion_launch_command(
        &guard.deployed.companion_jar,
        &companion_socket,
        cfg.heartbeat_ms,
        logcat_en,
        input_en,
    );
    let companion_child = Command::new(adb.adb_path())
        .args(["-s", &cfg.serial, "shell", &companion_cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    // R3 M4: the guard owns the child BEFORE any fallible wait, so a handshake timeout
    // kills the host `adb shell` (and with it the on-device companion).
    adopt_child_then_wait_ready(
        &mut guard.companion_child,
        companion_child,
        COMPANION_READY_TIMEOUT,
        stop_flag,
    )?;

    let mut companion_reader =
        connect_tcp_with_retry(companion_port, Duration::from_secs(5), stop_flag)?;
    companion_reader.set_read_timeout(Some(Duration::from_millis(250)))?;
    let companion_writer = companion_reader.try_clone()?;
    let hello = wait_for_hello_frame(&mut companion_reader, stop_flag, Duration::from_secs(5))?;
    Ok((companion_reader, companion_writer, hello))
}

/// Stores `child` in `slot` (the cleanup guard) first, then waits for `ARB_COMPANION_READY`
/// on its stdout (R3 M4). Any error leaves the child in `slot` so the guard kills it.
fn adopt_child_then_wait_ready(
    slot: &mut Option<Child>,
    mut child: Child,
    timeout: Duration,
    stop_flag: &AtomicBool,
) -> Result<(), DaemonError> {
    let stdout = child.stdout.take();
    *slot = Some(child);
    match stdout {
        Some(out) => wait_for_companion_ready(out, timeout, stop_flag),
        None => Ok(()),
    }
}

struct MediaThreadCtx {
    session_stop: Arc<AtomicBool>,
    global_stop: Arc<AtomicBool>,
    sink: LiveSink,
    signals: Sender<MediaSignal>,
}

impl MediaThreadCtx {
    fn should_stop(&self) -> bool {
        self.session_stop.load(Ordering::Acquire)
            || self.global_stop.load(Ordering::Acquire)
            || self.sink.has_failed()
    }

    fn clone_for_thread(&self) -> Self {
        Self {
            session_stop: Arc::clone(&self.session_stop),
            global_stop: Arc::clone(&self.global_stop),
            sink: self.sink.clone(),
            signals: self.signals.clone(),
        }
    }

    /// Reports an unexpected transport end (never during a deliberate stop).
    fn report_lost(&self, stream: StreamId, reason: String) {
        if !self.should_stop() {
            let _ = self.signals.send(MediaSignal::Lost {
                stream,
                pc_cont_ns: sample_cont_ns(),
                reason,
            });
        }
    }
}

fn spawn_scrcpy_stderr_collector(stderr: ChildStderr, buf: Arc<Mutex<String>>) {
    let _ = thread::Builder::new()
        .name("arbd-scrcpy-stderr".to_owned())
        .spawn(move || collect_scrcpy_stderr(BufReader::new(stderr), &buf));
}

/// Drains `reader` to EOF, retaining at most [`MAX_SCRCPY_STDERR_BYTES`] of its text in `buf`.
///
/// Reads raw bytes (lossily decoded) so invalid UTF-8 never stops the drain: the child's stderr
/// is a pipe, and an undrained pipe would eventually block `scrcpy-server`'s writes.
fn collect_scrcpy_stderr<R: BufRead>(mut reader: R, buf: &Mutex<String>) {
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if let Ok(mut guard) = buf.lock()
            && guard.len() < MAX_SCRCPY_STDERR_BYTES
        {
            let line = String::from_utf8_lossy(&raw);
            let remaining = MAX_SCRCPY_STDERR_BYTES - guard.len();
            let take = line.floor_char_boundary(remaining.min(line.len()));
            guard.push_str(&line[..take]);
        }
    }
}

/// Launches scrcpy and its reader threads, proactively disabling audio below Android 13
/// (`sdk < 33`, including Android 9 API 28) and degrading cleanly to `audio=false` if a
/// runtime `audio=true` launch fails during handshake.
///
/// # Errors
///
/// Returns an error when no scrcpy jar is deployed or the video tunnel cannot be connected
/// even with `audio=false`; the caller records that as a video `StreamGap` (R3 M5) and
/// continues without video.
fn launch_scrcpy_streams(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    guard: &mut SessionCleanupGuard<'_>,
    detected_sdk: Option<u32>,
    boot_id: &str,
    ctx: &MediaThreadCtx,
    scrcpy_stderr: &Arc<Mutex<String>>,
) -> Result<(Vec<JoinHandle<()>>, Vec<TcpStream>), DaemonError> {
    let Some(remote_scrcpy) = guard.deployed.scrcpy_jar.clone() else {
        return Err(DaemonError::NotFound(
            "replay-scrcpy-audio.dex.jar was not deployed".to_owned(),
        ));
    };

    let requested_audio = if cfg.audio_enabled {
        AudioCaptureMode::Enabled
    } else {
        AudioCaptureMode::Disabled
    };
    match plan_scrcpy_audio(requested_audio, detected_sdk) {
        PlannedScrcpyAudio::Disabled => {
            ctx.sink.set_audio_status(AudioCaptureState::Disabled, None);
            launch_scrcpy_attempt(
                adb,
                cfg,
                guard,
                &remote_scrcpy,
                detected_sdk.unwrap_or(0),
                AudioCaptureMode::Disabled,
                0,
                boot_id,
                ctx,
                scrcpy_stderr,
            )
        }
        PlannedScrcpyAudio::ProactiveUnavailable {
            api_level,
            status,
            reason,
        } => {
            ctx.sink
                .set_audio_status(AudioCaptureState::Unavailable, Some(reason));
            ctx.sink.push(CaptureEvent::AudioUnavailable {
                pc_cont_ns: sample_cont_ns(),
                boot_id: boot_id.to_owned(),
                status,
            });
            launch_scrcpy_attempt(
                adb,
                cfg,
                guard,
                &remote_scrcpy,
                api_level,
                AudioCaptureMode::Disabled,
                0,
                boot_id,
                ctx,
                scrcpy_stderr,
            )
        }
        PlannedScrcpyAudio::TryWithAudio {
            effective_api_level,
        } => launch_scrcpy_with_audio_fallback(
            adb,
            cfg,
            guard,
            &remote_scrcpy,
            effective_api_level,
            boot_id,
            ctx,
            scrcpy_stderr,
        ),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "retries a failed audio=true scrcpy launch cleanly with audio=false"
)]
fn launch_scrcpy_with_audio_fallback(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    guard: &mut SessionCleanupGuard<'_>,
    remote_scrcpy: &str,
    effective_api_level: u32,
    boot_id: &str,
    ctx: &MediaThreadCtx,
    scrcpy_stderr: &Arc<Mutex<String>>,
) -> Result<(Vec<JoinHandle<()>>, Vec<TcpStream>), DaemonError> {
    match launch_scrcpy_attempt(
        adb,
        cfg,
        guard,
        remote_scrcpy,
        effective_api_level,
        AudioCaptureMode::Enabled,
        0,
        boot_id,
        ctx,
        scrcpy_stderr,
    ) {
        Ok(started) => Ok(started),
        Err(first_err) if !ctx.should_stop() => {
            // Tear down the failed `audio=true` scrcpy attempt and degrade cleanly
            // to `audio=false` (single video socket) so video + logs + taps continue.
            kill_child(guard.scrcpy_child.take());
            thread::sleep(Duration::from_millis(50));
            let stderr_note = scrcpy_stderr
                .lock()
                .ok()
                .map(|mut buf| {
                    let snap = buf.trim().to_owned();
                    buf.clear();
                    snap
                })
                .filter(|s| !s.is_empty());
            let retry = launch_scrcpy_attempt(
                adb,
                cfg,
                guard,
                remote_scrcpy,
                effective_api_level,
                AudioCaptureMode::Disabled,
                1,
                boot_id,
                ctx,
                scrcpy_stderr,
            );
            // Only blame audio once the audio=false retry has proved video can start.
            let retry_err = retry.as_ref().err().map(ToString::to_string);
            let detail = audio_fallback_reason(
                &first_err.to_string(),
                stderr_note.as_deref(),
                retry_err.as_deref(),
            );
            ctx.sink
                .set_audio_status(AudioCaptureState::Unavailable, Some(detail));
            ctx.sink.push(CaptureEvent::AudioUnavailable {
                pc_cont_ns: sample_cont_ns(),
                boot_id: boot_id.to_owned(),
                status: AudioTunnelStatus::DeviceError,
            });
            retry
        }
        Err(err) => Err(err),
    }
}

/// Derives the 8-hex-digit `scrcpy-server` `scid` for one launch attempt.
///
/// `scrcpy-server` parses `scid` with Java `Integer.parseInt(hex)` and rejects values above
/// `0x7FFF_FFFF`, so bit 31 is always cleared before the `0x1000_0000` marker is applied.
fn scrcpy_scid_hex(unix_millis: u64, salt: u32) -> String {
    let low = u32::try_from(unix_millis & 0xFFFF_FFFF)
        .unwrap_or(0)
        .wrapping_add(salt);
    format!("{:08x}", (low & 0x7FFF_FFFF) | 0x1000_0000)
}

/// Honest `audio_reason` after an `audio=true` scrcpy launch failed and the `audio=false`
/// retry finished (`retry_err` is `Some` when the retry failed too).
fn audio_fallback_reason(first_err: &str, stderr: Option<&str>, retry_err: Option<&str>) -> String {
    let first = match stderr {
        Some(stderr) => format!("{first_err}; stderr: {stderr}"),
        None => first_err.to_owned(),
    };
    match retry_err {
        None => format!(
            "Audio capture failed during scrcpy startup ({first}); degraded to video + logs + taps."
        ),
        Some(retry_err) => format!(
            "scrcpy-server failed to start with audio=true ({first}) and with audio=false ({retry_err}); no video or audio is captured, logs + taps continue."
        ),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "single scrcpy launch attempt binds adb, config, cleanup guard, audio mode, and reader thread context"
)]
fn launch_scrcpy_attempt(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    guard: &mut SessionCleanupGuard<'_>,
    remote_scrcpy: &str,
    api_level: u32,
    audio_mode: AudioCaptureMode,
    scid_salt: u32,
    boot_id: &str,
    ctx: &MediaThreadCtx,
    scrcpy_stderr: &Arc<Mutex<String>>,
) -> Result<(Vec<JoinHandle<()>>, Vec<TcpStream>), DaemonError> {
    let mut media_threads: Vec<JoinHandle<()>> = Vec::new();
    let mut media_sockets: Vec<TcpStream> = Vec::new();

    let scid = scrcpy_scid_hex(current_unix_millis(), scid_salt);
    let scrcpy_abstract = format!("scrcpy_{scid}");
    let scrcpy_port = adb.forward_tcp_to_abstract(&cfg.serial, &scrcpy_abstract)?;
    guard.forwarded_ports.push(scrcpy_port);

    let scrcpy_opts = ScrcpyLaunchOptions {
        scid,
        max_size: cfg.max_size,
        max_fps: cfg.max_fps,
        video_bit_rate: cfg.video_bit_rate,
        audio_bit_rate: 128_000,
        audio_mode,
    };
    let (_server_args, planned_audio) = scrcpy_opts.build_server_args(api_level);
    let expect_audio_socket = matches!(planned_audio, AudioTunnelStatus::Available { .. });

    let scrcpy_cmd = AdbClient::<arb_adb::adb::SystemAdbRunner>::scrcpy_launch_command(
        remote_scrcpy,
        &scrcpy_opts,
        api_level,
    );
    let mut scrcpy_child = Command::new(adb.adb_path())
        .args(["-s", &cfg.serial, "shell", &scrcpy_cmd])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(stderr) = scrcpy_child.stderr.take() {
        spawn_scrcpy_stderr_collector(stderr, Arc::clone(scrcpy_stderr));
    }
    guard.scrcpy_child = Some(scrcpy_child);

    let audio_socket_mode = if expect_audio_socket {
        AudioSocketMode::Enabled
    } else {
        AudioSocketMode::Disabled
    };
    let global_stop = Arc::clone(&ctx.global_stop);
    let ScrcpyTunnels {
        video_reader,
        video_shutdown,
        audio_stream,
        audio_connect_error,
    } = match connect_scrcpy_tunnels(
        scrcpy_port,
        audio_socket_mode,
        Duration::from_secs(8),
        &global_stop,
    ) {
        Ok(tunnels) => tunnels,
        Err(err) => {
            if let Some(pos) = guard
                .forwarded_ports
                .iter()
                .rposition(|&p| p == scrcpy_port)
            {
                let port = guard.forwarded_ports.remove(pos);
                let _ = adb.remove_forward(&cfg.serial, port);
            }
            return Err(err);
        }
    };
    media_sockets.push(video_shutdown);
    let v_ctx = ctx.clone_for_thread();
    let v_boot = boot_id.to_owned();
    media_threads.push(thread::spawn(move || {
        run_video_reader_loop(video_reader, &v_boot, &v_ctx);
    }));

    if let Some(audio_sock) = audio_stream {
        if let Ok(audio_shutdown) = audio_sock.try_clone() {
            media_sockets.push(audio_shutdown);
        }
        let a_ctx = ctx.clone_for_thread();
        let a_boot = boot_id.to_owned();
        media_threads.push(thread::spawn(move || {
            run_audio_reader_loop(audio_sock, &a_boot, &a_ctx);
        }));
    } else if let Some(err) = audio_connect_error {
        let reason =
            format!("Audio tunnel could not connect ({err}). Continuing with video + logs + taps.");
        ctx.sink
            .set_audio_status(AudioCaptureState::Unavailable, Some(reason));
        ctx.sink.push(CaptureEvent::AudioUnavailable {
            pc_cont_ns: sample_cont_ns(),
            boot_id: boot_id.to_owned(),
            status: AudioTunnelStatus::DeviceError,
        });
        ctx.report_lost(StreamId::Audio, format!("audio connect failed: {err}"));
    }

    Ok((media_threads, media_sockets))
}

#[expect(
    clippy::too_many_arguments,
    reason = "session event loop takes the companion sockets, supervisor state, and media signal channel"
)]
fn run_companion_event_loop(
    companion_reader: &mut TcpStream,
    companion_writer: &mut TcpStream,
    stop_flag: &Arc<AtomicBool>,
    sink: &LiveSink,
    supervisor: &mut SessionSupervisor,
    tracker: &mut LiveTracker,
    media_rx: &Receiver<MediaSignal>,
    scrcpy_stderr: &Arc<Mutex<String>>,
) -> Result<(), DaemonError> {
    let mut ping = PingState::new(Instant::now());
    let mut next_liveness_tick = Instant::now() + LIVENESS_TICK_INTERVAL;

    loop {
        if stop_flag.load(Ordering::Acquire) || sink.has_failed() {
            if let Ok(frame) = WireMessage::Shutdown(ShutdownPayload {
                boottime_ns: 0,
                code: ShutdownCode::Normal,
                reason: "arbd_capture_stop".to_owned(),
            })
            .to_frame(StreamId::Control, FrameFlags::URGENT, ping.seq)
            {
                let _ = frame.write_to(companion_writer);
            }
            return Ok(());
        }

        let now = Instant::now();
        if ping.due(now) {
            let nonce = (0xA5A5_0000_u64 << 32) | u64::from(ping.seq);
            let send_sample = sample_clock_or_zero();
            if let Ok(frame) = WireMessage::Ping(PingPayload { nonce }).to_frame(
                StreamId::Control,
                FrameFlags::NONE,
                ping.seq,
            ) && frame.write_to(companion_writer).is_ok()
            {
                ping.mark_sent(nonce, send_sample, now);
            }
        }

        match WireFrame::read_from(companion_reader, DEFAULT_MAX_PAYLOAD_LEN) {
            Ok(Some(frame)) => {
                let recv_sample = sample_clock_or_zero();
                let keep_going = dispatch_companion_frame(
                    &frame,
                    recv_sample,
                    &mut ping,
                    sink,
                    supervisor,
                    tracker,
                )?;
                if !keep_going {
                    return Ok(());
                }
            }
            Ok(None) => return Ok(()),
            Err(arb_adb::wire::WireError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(err) => return Err(DaemonError::CaptureSource(err.into())),
        }

        drain_media_signals(media_rx, sink, supervisor, tracker, scrcpy_stderr);

        let now_pc = sample_cont_ns();
        if Instant::now() >= next_liveness_tick {
            next_liveness_tick = Instant::now() + LIVENESS_TICK_INTERVAL;
            if let Some(event) = tracker.tick_liveness(supervisor, now_pc) {
                sink.push(event);
            }
        }
        if tracker.beacon_timed_out(supervisor, now_pc) {
            return Err(DaemonError::Internal(format!(
                "companion beacon timed out (no heartbeat for more than {} ms)",
                supervisor.config().beacon_timeout_ns / 1_000_000
            )));
        }
    }
}

fn drain_media_signals(
    media_rx: &Receiver<MediaSignal>,
    sink: &LiveSink,
    supervisor: &mut SessionSupervisor,
    tracker: &mut LiveTracker,
    scrcpy_stderr: &Arc<Mutex<String>>,
) {
    loop {
        match media_rx.try_recv() {
            Ok(MediaSignal::VideoFrame(pc)) => tracker.on_video_frame(supervisor, pc),
            Ok(MediaSignal::Lost {
                stream,
                pc_cont_ns,
                reason,
            }) => {
                let effective_reason = if stream == StreamId::Video {
                    let stderr_snapshot = scrcpy_stderr
                        .lock()
                        .ok()
                        .map(|buf| buf.clone())
                        .filter(|s| !s.trim().is_empty());
                    let classified =
                        classify_scrcpy_video_error(&reason, stderr_snapshot.as_deref());
                    sink.set_last_video_error(Some(classified.clone()));
                    classified
                } else {
                    reason
                };
                if let Some(event) =
                    tracker.on_media_lost(supervisor, stream, pc_cont_ns, &effective_reason)
                {
                    sink.push(event);
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
        }
    }
}

fn dispatch_companion_frame(
    frame: &WireFrame,
    recv_sample: HostClockSample,
    ping_state: &mut PingState,
    sink: &LiveSink,
    supervisor: &mut SessionSupervisor,
    tracker: &mut LiveTracker,
) -> Result<bool, DaemonError> {
    let pc_cont_ns = recv_sample.continuous_mono_ns;
    let boot_id = tracker.boot_id.clone();
    match WireMessage::decode_frame(frame)? {
        WireMessage::Heartbeat(hb) => {
            sink.push_all(tracker.on_heartbeat(supervisor, hb, pc_cont_ns));
        }
        WireMessage::Pong(pong) => {
            if let Some(send_sample) = ping_state.take_match(pong.nonce) {
                let probe = PcSyncProbe {
                    boot_id,
                    pc_send_cont_ns: send_sample.continuous_mono_ns,
                    pc_send_active_ns: send_sample.active_mono_ns,
                    dev_boottime_ns: pong.midpoint_boottime_ns(),
                    dev_monotonic_ns: pong.monotonic_ns,
                    pc_recv_cont_ns: recv_sample.continuous_mono_ns,
                    pc_recv_active_ns: recv_sample.active_mono_ns,
                };
                let discs = tracker.timeline.ingest_pc_sync_probe(probe.clone());
                sink.push(CaptureEvent::SyncProbe {
                    pc_cont_ns: recv_sample.continuous_mono_ns,
                    probe,
                    device_bracket_ns: pong.device_bracket_ns().unwrap_or(0),
                });
                for disc in discs {
                    sink.push(CaptureEvent::Discontinuity(disc));
                }
            }
        }
        WireMessage::StreamState(st) => {
            let disc = supervisor.on_stream_state(&st, pc_cont_ns);
            sink.push(CaptureEvent::StreamState {
                pc_cont_ns,
                boot_id,
                payload: st,
            });
            if let Some(d) = disc {
                sink.push(CaptureEvent::Discontinuity(d));
            }
        }
        WireMessage::LogRecord(rec) => sink.push(CaptureEvent::LogRecord {
            pc_cont_ns,
            boot_id,
            flags: frame.header.flags,
            payload: rec,
        }),
        WireMessage::LogOverflowStats(stats) => sink.push(CaptureEvent::LogOverflowStats {
            pc_cont_ns,
            boot_id,
            payload: stats,
        }),
        WireMessage::TouchEvent(touch) => sink.push(CaptureEvent::TouchEvent {
            pc_cont_ns,
            boot_id,
            flags: frame.header.flags,
            payload: touch,
            normalized: None,
        }),
        WireMessage::Shutdown(shutdown) => {
            supervisor.stop();
            sink.push(CaptureEvent::Shutdown {
                pc_cont_ns,
                payload: shutdown,
            });
            return Ok(false);
        }
        _ => {}
    }
    Ok(true)
}

fn run_video_reader_loop(
    mut reader: VideoTunnelReader<TcpStream>,
    boot_id: &str,
    ctx: &MediaThreadCtx,
) {
    while !ctx.should_stop() {
        match reader.read_next() {
            Ok(Some(VideoRecord::SessionMeta(meta))) => {
                ctx.sink.push(CaptureEvent::VideoSession {
                    pc_cont_ns: sample_cont_ns(),
                    boot_id: boot_id.to_owned(),
                    meta,
                });
            }
            Ok(Some(VideoRecord::Packet(packet))) => {
                let pc_cont_ns = sample_cont_ns();
                let is_frame = !packet.is_config();
                ctx.sink.push(CaptureEvent::VideoFrame {
                    pc_cont_ns,
                    boot_id: boot_id.to_owned(),
                    packet,
                });
                if is_frame {
                    let _ = ctx.signals.send(MediaSignal::VideoFrame(pc_cont_ns));
                }
            }
            Err(ScrcpyError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Ok(None) => {
                ctx.report_lost(StreamId::Video, "scrcpy_video_eof".to_owned());
                break;
            }
            Err(err) => {
                ctx.report_lost(StreamId::Video, format!("scrcpy_video_error: {err}"));
                break;
            }
            _ => {}
        }
    }
}

fn run_audio_reader_loop(stream: TcpStream, boot_id: &str, ctx: &MediaThreadCtx) {
    // Allow sufficient time for device-side AudioRecord + AAC MediaCodec initialization
    // before reading the 4-byte audio codec ID, then switch to short poll timeouts.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let timeout_ctl = stream.try_clone().ok();
    let mut reader = match AudioTunnelReader::handshake(stream) {
        Ok(r) => {
            if let Some(sock) = timeout_ctl {
                let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
            }
            ctx.sink
                .set_audio_status(AudioCaptureState::Available, None);
            r
        }
        Err(ScrcpyError::AudioDisabled) => {
            ctx.sink.set_audio_status(
                AudioCaptureState::Unavailable,
                Some(
                    "Device disabled or refused audio capture (codec_id = 0). Continuing with video + logs + taps."
                        .to_owned(),
                ),
            );
            ctx.sink.push(CaptureEvent::AudioUnavailable {
                pc_cont_ns: sample_cont_ns(),
                boot_id: boot_id.to_owned(),
                status: AudioTunnelStatus::DisabledByDevice,
            });
            return;
        }
        Err(ScrcpyError::AudioServerError) => {
            ctx.sink.set_audio_status(
                AudioCaptureState::Unavailable,
                Some(
                    "Device audio encoder failed to initialize (codec_id = 1). Continuing with video + logs + taps."
                        .to_owned(),
                ),
            );
            ctx.sink.push(CaptureEvent::AudioUnavailable {
                pc_cont_ns: sample_cont_ns(),
                boot_id: boot_id.to_owned(),
                status: AudioTunnelStatus::DeviceError,
            });
            return;
        }
        Err(err) => {
            ctx.sink.set_audio_status(
                AudioCaptureState::Unavailable,
                Some(format!(
                    "Audio tunnel closed during handshake ({err}). Continuing with video + logs + taps."
                )),
            );
            ctx.sink.push(CaptureEvent::AudioUnavailable {
                pc_cont_ns: sample_cont_ns(),
                boot_id: boot_id.to_owned(),
                status: AudioTunnelStatus::DeviceError,
            });
            ctx.report_lost(StreamId::Audio, format!("audio handshake failed: {err}"));
            return;
        }
    };

    while !ctx.should_stop() {
        match reader.read_next() {
            Ok(Some(packet)) => {
                let pc_cont_ns = sample_cont_ns();
                let ev = match packet.kind {
                    AudioPacketKind::Config => CaptureEvent::AudioConfig {
                        pc_cont_ns,
                        boot_id: boot_id.to_owned(),
                        packet,
                    },
                    AudioPacketKind::Frame => CaptureEvent::AudioFrame {
                        pc_cont_ns,
                        boot_id: boot_id.to_owned(),
                        packet,
                    },
                    _ => continue,
                };
                ctx.sink.push(ev);
            }
            Ok(None) => {
                ctx.report_lost(StreamId::Audio, "scrcpy_audio_eof".to_owned());
                break;
            }
            Err(ScrcpyError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(err) => {
                ctx.report_lost(
                    StreamId::Audio,
                    format!("audio tunnel closed unexpectedly: {err}"),
                );
                break;
            }
        }
    }
}

/// Waits for `ARB_COMPANION_READY` on `stdout`, polling `stop_flag` so `capture.stop` is
/// never blocked for the whole handshake timeout (R3 S12).
fn wait_for_companion_ready<R: Read + Send + 'static>(
    stdout: R,
    timeout: Duration,
    stop_flag: &AtomicBool,
) -> Result<(), DaemonError> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while let Ok(n) = reader.read_line(&mut line) {
            if n == 0 {
                let _ = tx.send(false);
                return;
            }
            if line.trim_start().starts_with("ARB_COMPANION_READY") {
                let _ = tx.send(true);
                return;
            }
            line.clear();
        }
        let _ = tx.send(false);
    });

    let deadline = Instant::now() + timeout;
    loop {
        if stop_flag.load(Ordering::Acquire) {
            return Err(DaemonError::Internal(
                "capture stopped while waiting for ARB_COMPANION_READY".to_owned(),
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DaemonError::Internal(
                "timed out waiting for ARB_COMPANION_READY".to_owned(),
            ));
        }
        match rx.recv_timeout(remaining.min(STOP_POLL_SLICE)) {
            Ok(true) => return Ok(()),
            Ok(false) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(DaemonError::Internal(
                    "arb-companion exited before printing ARB_COMPANION_READY".to_owned(),
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn connect_tcp_with_retry(
    port: u16,
    timeout: Duration,
    stop_flag: &Arc<AtomicBool>,
) -> Result<TcpStream, DaemonError> {
    let deadline = Instant::now() + timeout;
    let addr = format!("127.0.0.1:{port}");
    loop {
        if stop_flag.load(Ordering::Acquire) {
            return Err(DaemonError::Internal(
                "capture stopped during connect".to_owned(),
            ));
        }
        match TcpStream::connect(&addr) {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(DaemonError::Io(err));
                }
                thread::sleep(Duration::from_millis(80));
            }
        }
    }
}

#[derive(Clone, Copy)]
enum AudioSocketMode {
    Enabled,
    Disabled,
}

struct ScrcpyTunnels {
    video_reader: VideoTunnelReader<TcpStream>,
    video_shutdown: TcpStream,
    audio_stream: Option<TcpStream>,
    audio_connect_error: Option<DaemonError>,
}

/// Connects to `scrcpy-server-v5.0.1` (`tunnel_forward=true`, `control=false`).
///
/// Protocol order: `scrcpy-server` accepts socket 1 (video), writes a single `0x00` dummy byte,
/// and when `audio=true` blocks in `LocalServerSocket.accept()` waiting for socket 2 (audio)
/// **before** initializing encoders or writing the 4-byte `"h264"` / `"\0aac"` codec IDs.
/// Connecting the audio socket immediately after receiving the dummy byte prevents a deadlock.
fn connect_scrcpy_tunnels(
    port: u16,
    audio_mode: AudioSocketMode,
    timeout: Duration,
    stop_flag: &Arc<AtomicBool>,
) -> Result<ScrcpyTunnels, DaemonError> {
    let (video_sock, video_shutdown) =
        connect_scrcpy_video_dummy_with_retry(port, timeout, stop_flag)?;

    let (audio_stream, audio_connect_error) = match audio_mode {
        AudioSocketMode::Enabled => {
            match connect_tcp_with_retry(port, Duration::from_secs(3), stop_flag) {
                Ok(sock) => (Some(sock), None),
                Err(err) => (None, Some(err)),
            }
        }
        AudioSocketMode::Disabled => (None, None),
    };

    let _ = video_sock.set_read_timeout(Some(Duration::from_secs(5)));
    let video_reader = VideoTunnelReader::handshake_without_dummy(video_sock)
        .map_err(|err| DaemonError::CaptureSource(err.into()))?;
    let _ = video_shutdown.set_read_timeout(Some(Duration::from_millis(300)));

    Ok(ScrcpyTunnels {
        video_reader,
        video_shutdown,
        audio_stream,
        audio_connect_error,
    })
}

fn connect_scrcpy_video_dummy_with_retry(
    port: u16,
    timeout: Duration,
    stop_flag: &Arc<AtomicBool>,
) -> Result<(TcpStream, TcpStream), DaemonError> {
    let deadline = Instant::now() + timeout;
    let addr = format!("127.0.0.1:{port}");
    loop {
        if stop_flag.load(Ordering::Acquire) {
            return Err(DaemonError::Internal(
                "capture stopped during scrcpy handshake".to_owned(),
            ));
        }
        match TcpStream::connect(&addr) {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let mut dummy = [0u8; 1];
                match stream.read_exact(&mut dummy) {
                    Ok(()) if dummy[0] == SCRCPY_DUMMY_BYTE => {
                        let shutdown_handle = stream.try_clone()?;
                        return Ok((stream, shutdown_handle));
                    }
                    Ok(()) => {
                        let err = ScrcpyError::InvalidDummyByte { found: dummy[0] };
                        if Instant::now() >= deadline {
                            return Err(DaemonError::CaptureSource(err.into()));
                        }
                        thread::sleep(Duration::from_millis(120));
                    }
                    Err(err) => {
                        if Instant::now() >= deadline {
                            return Err(DaemonError::Io(err));
                        }
                        thread::sleep(Duration::from_millis(120));
                    }
                }
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(DaemonError::Io(err));
                }
                thread::sleep(Duration::from_millis(120));
            }
        }
    }
}

fn wait_for_hello_frame(
    reader: &mut TcpStream,
    stop_flag: &Arc<AtomicBool>,
    timeout: Duration,
) -> Result<HelloPayload, DaemonError> {
    let deadline = Instant::now() + timeout;
    loop {
        if stop_flag.load(Ordering::Acquire) {
            return Err(DaemonError::Internal(
                "capture stopped before Hello frame".to_owned(),
            ));
        }
        // R3 C8: the deadline also applies while non-Hello frames keep arriving.
        if Instant::now() >= deadline {
            return Err(DaemonError::Internal(
                "timed out waiting for companion Hello frame".to_owned(),
            ));
        }
        match WireFrame::read_from(reader, DEFAULT_MAX_PAYLOAD_LEN) {
            Ok(Some(frame)) => {
                if let WireMessage::Hello(hello) = WireMessage::decode_frame(&frame)? {
                    return Ok(hello);
                }
            }
            Ok(None) => {
                return Err(DaemonError::Internal(
                    "companion closed connection before Hello".to_owned(),
                ));
            }
            Err(arb_adb::wire::WireError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(err) => return Err(DaemonError::CaptureSource(err.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use arb_adb::adb::{ReconnectClassification, SessionSupervisor, SupervisorConfig};
    use arb_adb::mock::CaptureEvent;
    use arb_adb::wire::{
        BootIdSource, HeartbeatPayload, HelloPayload, StreamHealthEntry, StreamId,
        StreamLifecycleState,
    };
    use arb_core::time::{DiscontinuityKind, StreamKind};
    use arb_os::clock::HostClockSample;

    use super::{LiveTracker, PING_TIMEOUT, PingState, adopt_child_then_wait_ready, kill_child};

    const SEC: i64 = 1_000_000_000;

    fn hello(boot_id: &str, source: BootIdSource) -> HelloPayload {
        HelloPayload {
            protocol_version: 1,
            api_level: 37,
            boot_count: 1,
            boot_id_source: source,
            is_emulator: true,
            heartbeat_interval_ms: 250,
            boot_id: boot_id.to_owned(),
            model: "test".to_owned(),
            manufacturer: "ARB".to_owned(),
            agent_version: "0.1.0".to_owned(),
            boot_id_detail: String::new(),
        }
    }

    fn heartbeat(boottime_ns: i64, video: StreamLifecycleState) -> HeartbeatPayload {
        HeartbeatPayload {
            boottime_ns,
            monotonic_ns: boottime_ns,
            realtime_ns: 1_730_000_000 * SEC + boottime_ns,
            streams: [
                (StreamId::Control, StreamLifecycleState::Running),
                (StreamId::Video, video),
            ]
            .into_iter()
            .map(|(stream_id, state)| StreamHealthEntry {
                stream_id,
                state,
                restart_count: 0,
                emitted_seq: 1,
                last_pts_ns: boottime_ns,
            })
            .collect(),
        }
    }

    fn reboot_count(events: &[CaptureEvent]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, CaptureEvent::Discontinuity(d) if d.kind == DiscontinuityKind::DeviceReboot))
            .count()
    }

    fn supervisor() -> SessionSupervisor {
        SessionSupervisor::new("emulator-5554", SupervisorConfig::new())
    }

    #[test]
    fn r3_m5_live_video_loss_emits_stream_gap_never_static_hold() {
        let mut sup = supervisor();
        let mut tracker = LiveTracker::new();
        let _ = tracker.on_hello(&mut sup, hello("boot-a", BootIdSource::KernelProcBootId), 0);
        let _ = tracker.on_heartbeat(
            &mut sup,
            heartbeat(10 * SEC, StreamLifecycleState::Running),
            SEC / 10,
        );
        tracker.on_video_frame(&mut sup, SEC / 5);

        let lost = tracker
            .on_media_lost(&mut sup, StreamId::Video, SEC / 4, "scrcpy_video_eof")
            .expect("video EOF must be reported");
        match lost {
            CaptureEvent::Discontinuity(d) => {
                assert_eq!(
                    d.kind,
                    DiscontinuityKind::StreamGap,
                    "video loss must be a StreamGap"
                );
                assert_eq!(
                    d.stream,
                    Some(StreamKind::Video),
                    "gap must be on the video stream"
                );
            }
            other => panic!("expected a StreamGap discontinuity, got {other:?}"),
        }
        assert!(
            tracker
                .on_media_lost(&mut sup, StreamId::Video, SEC / 3, "again")
                .is_none(),
            "the same video loss must be reported once"
        );
        // Heartbeats keep arriving (companion alive) while the video socket is dead: the
        // liveness tick must never turn this into a StaticHold.
        let _ = tracker.on_heartbeat(
            &mut sup,
            heartbeat(13 * SEC, StreamLifecycleState::Running),
            3 * SEC,
        );
        assert!(
            tracker.tick_liveness(&sup, 3 * SEC + 1).is_none(),
            "a dead video transport must not be re-reported, and never as StaticHold"
        );
    }

    #[test]
    fn r3_m5_silent_but_alive_video_emits_one_static_hold_and_beacon_timeout_detected() {
        let mut sup = supervisor();
        let mut tracker = LiveTracker::new();
        let _ = tracker.on_hello(&mut sup, hello("boot-a", BootIdSource::KernelProcBootId), 0);
        let _ = tracker.on_heartbeat(
            &mut sup,
            heartbeat(10 * SEC, StreamLifecycleState::Running),
            SEC / 10,
        );
        tracker.on_video_frame(&mut sup, SEC / 10);
        let _ = tracker.on_heartbeat(
            &mut sup,
            heartbeat(13 * SEC, StreamLifecycleState::Running),
            3 * SEC,
        );

        let hold = tracker.tick_liveness(&sup, 3 * SEC + 1);
        assert!(
            matches!(hold, Some(CaptureEvent::StaticHold { .. })),
            "silent video with a live beacon must be StaticHold, got {hold:?}"
        );
        assert!(
            tracker.tick_liveness(&sup, 3 * SEC + 2).is_none(),
            "one StaticHold per episode"
        );
        assert!(
            !tracker.beacon_timed_out(&sup, 3 * SEC + SEC / 2),
            "a fresh beacon must not time out"
        );
        assert!(
            tracker.beacon_timed_out(&sup, 3 * SEC + 2 * SEC),
            "a beacon older than beacon_timeout_ns must end the session"
        );
    }

    #[test]
    fn r3_m6_continuity_unverified_never_emits_device_reboot() {
        let mut sup = supervisor();
        let mut tracker = LiveTracker::new();
        let first = tracker.on_hello(
            &mut sup,
            hello("rand-1", BootIdSource::FallbackRandomUuid),
            0,
        );
        assert_eq!(
            reboot_count(&first),
            0,
            "initial connection is not a reboot"
        );
        let hb1 = tracker.on_heartbeat(
            &mut sup,
            heartbeat(10 * SEC, StreamLifecycleState::Running),
            SEC,
        );

        let (_, lost) = tracker.on_session_end(&mut sup, 2 * SEC, "companion_eof");
        assert!(lost.is_some(), "first disconnect records ConnectionLost");
        let second = tracker.on_hello(
            &mut sup,
            hello("rand-2", BootIdSource::FallbackRandomUuid),
            3 * SEC,
        );
        assert!(
            second.iter().any(|e| matches!(
                e,
                CaptureEvent::Hello {
                    classification: ReconnectClassification::ContinuityUnverified { .. },
                    ..
                }
            )),
            "a random-UUID boot_id change must be ContinuityUnverified"
        );
        let hb2 = tracker.on_heartbeat(
            &mut sup,
            heartbeat(11 * SEC, StreamLifecycleState::Running),
            4 * SEC,
        );
        assert_eq!(
            reboot_count(&hb1) + reboot_count(&second) + reboot_count(&hb2),
            0,
            "ContinuityUnverified must never be asserted as a DeviceReboot"
        );
    }

    #[test]
    fn r3_m6_real_reboot_reported_exactly_once() {
        let mut sup = supervisor();
        let mut tracker = LiveTracker::new();
        let _ = tracker.on_hello(&mut sup, hello("boot-a", BootIdSource::KernelProcBootId), 0);
        let _ = tracker.on_heartbeat(
            &mut sup,
            heartbeat(100 * SEC, StreamLifecycleState::Running),
            SEC,
        );
        let _ = tracker.on_session_end(&mut sup, 2 * SEC, "companion_eof");
        let hello_events = tracker.on_hello(
            &mut sup,
            hello("boot-b", BootIdSource::KernelProcBootId),
            30 * SEC,
        );
        let hb = tracker.on_heartbeat(
            &mut sup,
            heartbeat(5 * SEC, StreamLifecycleState::Running),
            31 * SEC,
        );
        assert_eq!(
            reboot_count(&hello_events) + reboot_count(&hb),
            1,
            "supervisor reports the reboot; the timeline duplicate must be suppressed"
        );
    }

    #[test]
    fn r3_s14_repeated_reconnect_failures_coalesce_into_one_connection_lost() {
        let mut sup = supervisor();
        let mut tracker = LiveTracker::new();
        let _ = tracker.on_hello(&mut sup, hello("boot-a", BootIdSource::KernelProcBootId), 0);
        let mut lost = 0;
        for attempt in 0..10_i64 {
            let (_, event) = tracker.on_session_end(&mut sup, SEC + attempt, "device offline");
            lost += usize::from(event.is_some());
        }
        assert_eq!(
            lost, 1,
            "an unplugged device must record one ConnectionLost, not one per retry"
        );
        assert!(
            sup.reconnect_attempts() >= 10,
            "backoff must still advance per attempt"
        );
        let _ = tracker.on_hello(
            &mut sup,
            hello("boot-a", BootIdSource::KernelProcBootId),
            20 * SEC,
        );
        let (_, event) = tracker.on_session_end(&mut sup, 21 * SEC, "eof");
        assert!(
            event.is_some(),
            "a new disconnect after reconnecting is recorded again"
        );
    }

    #[test]
    fn r3_s13_expired_pending_ping_recovers_sync_probes() {
        let start = Instant::now();
        let mut ping = PingState::new(start);
        assert!(ping.due(start), "first ping is due immediately");
        let sample = HostClockSample {
            active_mono_ns: 1,
            continuous_mono_ns: 1,
            wall_epoch_ns: 1,
        };
        ping.mark_sent(42, sample, start);
        assert!(
            !ping.due(start + Duration::from_millis(1500)),
            "pong still awaited"
        );
        // The Pong was lost: after the timeout a new Ping must be allowed.
        assert!(
            ping.due(start + PING_TIMEOUT + Duration::from_millis(10)),
            "a lost Pong must not disable sync probes for the rest of the session"
        );
        ping.mark_sent(43, sample, start + PING_TIMEOUT);
        assert!(ping.take_match(99).is_none(), "mismatched nonce is ignored");
        assert!(
            ping.due(start + PING_TIMEOUT + Duration::from_secs(2)),
            "a mismatched Pong clears the pending ping"
        );
    }

    /// Helper child process body for the M4 test: blocks without printing anything.
    #[test]
    #[ignore = "helper process for r3_m4_companion_child_owned_by_guard_before_handshake_wait"]
    fn helper_block_without_ready_line() {
        if std::env::var_os("ARB_TEST_BLOCK_CHILD").is_some() {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn r3_m4_companion_child_owned_by_guard_before_handshake_wait() {
        let exe = std::env::current_exe().expect("current test executable");
        let child = Command::new(exe)
            .args([
                "--exact",
                "live_adb::tests::helper_block_without_ready_line",
                "--ignored",
                "--nocapture",
            ])
            .env("ARB_TEST_BLOCK_CHILD", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn helper child");
        let stop = AtomicBool::new(false);
        let mut slot = None;
        let started = Instant::now();
        let result =
            adopt_child_then_wait_ready(&mut slot, child, Duration::from_millis(400), &stop);
        assert!(result.is_err(), "no ARB_COMPANION_READY line must time out");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "wait must honour its timeout"
        );
        let mut owned = slot.expect("the guard slot must own the child even after a timeout");
        assert!(
            owned.try_wait().expect("try_wait").is_none(),
            "child is still running and owned by the guard"
        );
        kill_child(Some(owned));
    }

    #[test]
    fn r3_s12_companion_ready_wait_honours_stop_flag() {
        let exe = std::env::current_exe().expect("current test executable");
        let child = Command::new(exe)
            .args([
                "--exact",
                "live_adb::tests::helper_block_without_ready_line",
                "--ignored",
                "--nocapture",
            ])
            .env("ARB_TEST_BLOCK_CHILD", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn helper child");
        let stop = AtomicBool::new(true);
        let mut slot = None;
        let started = Instant::now();
        let result = adopt_child_then_wait_ready(&mut slot, child, Duration::from_secs(8), &stop);
        assert!(
            result.is_err(),
            "a stop request must abort the handshake wait"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "capture.stop must not wait for the 8 s handshake timeout"
        );
        kill_child(slot.take());
    }

    #[test]
    fn scrcpy_dual_socket_handshake_opens_audio_before_reading_video_codec_id() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::thread;

        use arb_adb::scrcpy::{AudioTunnelReader, CODEC_ID_AAC, CODEC_ID_H264, SCRCPY_DUMMY_BYTE};

        use super::{AudioSocketMode, connect_scrcpy_tunnels};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock scrcpy listener");
        let port = listener.local_addr().expect("local addr").port();

        // Mirrors scrcpy-server-v5.0.1 DesktopConnection.open(video=true, audio=true, control=false):
        // accept(video) -> send 0x00 dummy byte -> accept(audio) -> send "h264" and "\0aac".
        let server = thread::spawn(move || {
            let (mut video, _) = listener.accept().expect("accept video socket");
            video.write_all(&[SCRCPY_DUMMY_BYTE]).expect("write dummy");
            video.flush().expect("flush dummy");

            let (mut audio, _) = listener.accept().expect("accept audio socket");
            video
                .write_all(&CODEC_ID_H264.to_be_bytes())
                .expect("write h264 codec id");
            audio
                .write_all(&CODEC_ID_AAC.to_be_bytes())
                .expect("write aac codec id");
        });

        let stop = Arc::new(AtomicBool::new(false));
        let tunnels = connect_scrcpy_tunnels(
            port,
            AudioSocketMode::Enabled,
            Duration::from_secs(5),
            &stop,
        )
        .expect("dual-socket scrcpy handshake must not deadlock");
        assert_eq!(tunnels.video_reader.codec_id(), CODEC_ID_H264);
        assert!(tunnels.audio_connect_error.is_none());
        let audio_sock = tunnels.audio_stream.expect("audio socket must connect");
        let audio_reader =
            AudioTunnelReader::handshake(audio_sock).expect("audio handshake must read AAC");
        assert_eq!(audio_reader.codec_id(), CODEC_ID_AAC);
        server.join().expect("mock scrcpy server must exit cleanly");
    }

    #[test]
    fn t6c_hw_encoder_contention_classifier_detects_mediacodec_and_suggests_workarounds() {
        assert!(super::is_hw_encoder_contention_error(
            "android.media.MediaCodec$CodecException: Configure failed with 0xfffffff4"
        ));
        assert!(super::is_hw_encoder_contention_error(
            "ERROR_INSUFFICIENT_RESOURCES while creating video/avc encoder"
        ));
        assert!(!super::is_hw_encoder_contention_error(
            "socket timed out waiting for dummy byte"
        ));

        let hint = super::classify_scrcpy_video_error(
            "scrcpy_video_eof",
            Some("MediaCodec.configure failed: RESOURCE_EXHAUSTED"),
        );
        assert!(hint.contains("Running Devices mirroring"), "hint={hint}");
        assert!(hint.contains("1024"), "hint={hint}");
        assert!(hint.contains("logs_only"), "hint={hint}");

        let plain = super::classify_scrcpy_video_error("connection reset by peer", None);
        assert!(plain.contains("connection reset by peer"));
        assert!(!plain.contains("Running Devices mirroring"));
    }

    #[test]
    fn t6c_scrcpy_stderr_collector_keeps_draining_after_invalid_utf8_and_caps_bytes() {
        use std::io::Cursor;
        use std::sync::Mutex;

        let mut raw = b"first\n\xff\xfe broken\n".to_vec();
        raw.extend_from_slice(b"MediaCodec configure failed\n");
        let reader = Cursor::new(raw);
        let buf = Mutex::new(String::new());
        super::collect_scrcpy_stderr(reader, &buf);
        let text = buf.lock().expect("buf").clone();
        assert!(text.contains("first"), "text={text:?}");
        assert!(
            text.contains("MediaCodec configure failed"),
            "collector must keep draining past invalid UTF-8 (pipe must never back up): {text:?}"
        );

        let mut cursor_consumed = Cursor::new(vec![b'x'; super::MAX_SCRCPY_STDERR_BYTES * 3]);
        let capped = Mutex::new(String::new());
        super::collect_scrcpy_stderr(&mut cursor_consumed, &capped);
        assert!(capped.lock().expect("capped").len() <= super::MAX_SCRCPY_STDERR_BYTES);
        assert_eq!(
            usize::try_from(cursor_consumed.position()).expect("pos"),
            super::MAX_SCRCPY_STDERR_BYTES * 3,
            "collector must drain the whole stream to EOF"
        );
    }

    #[test]
    fn t6e_plan_scrcpy_audio_proactively_disables_on_aos9_and_tries_fallback_on_api33_or_unknown() {
        use arb_adb::scrcpy::{AudioCaptureMode, AudioTunnelStatus};

        use super::{PlannedScrcpyAudio, plan_scrcpy_audio};

        assert_eq!(
            plan_scrcpy_audio(AudioCaptureMode::Disabled, Some(34)),
            PlannedScrcpyAudio::Disabled
        );

        // Android 9 (API 28) -> proactive audio=false with honest diagnostic!
        match plan_scrcpy_audio(AudioCaptureMode::Enabled, Some(28)) {
            PlannedScrcpyAudio::ProactiveUnavailable {
                api_level,
                status,
                reason,
            } => {
                assert_eq!(api_level, 28);
                assert_eq!(
                    status,
                    AudioTunnelStatus::UnavailableApiLevel {
                        api_level: 28,
                        min_required: 33,
                    }
                );
                assert_eq!(
                    reason,
                    "Audio capture requires Android 13+ (API 33+, non-muting playback capture); detected API 28. Continuing with video + logs + taps."
                );
            }
            other => panic!("expected ProactiveUnavailable on API 28, got {other:?}"),
        }

        // Android 10 (API 29) -> proactive audio=false
        assert!(matches!(
            plan_scrcpy_audio(AudioCaptureMode::Enabled, Some(29)),
            PlannedScrcpyAudio::ProactiveUnavailable { api_level: 29, .. }
        ));

        // Android 12 (API 32) -> proactive audio=false (playback dup requires API 33)
        assert!(matches!(
            plan_scrcpy_audio(AudioCaptureMode::Enabled, Some(32)),
            PlannedScrcpyAudio::ProactiveUnavailable { api_level: 32, .. }
        ));

        // Android 13 (API 33) and unknown SDK (None) -> TryWithAudio (runtime fallback armed)
        assert_eq!(
            plan_scrcpy_audio(AudioCaptureMode::Enabled, Some(33)),
            PlannedScrcpyAudio::TryWithAudio {
                effective_api_level: 33
            }
        );
        assert_eq!(
            plan_scrcpy_audio(AudioCaptureMode::Enabled, None),
            PlannedScrcpyAudio::TryWithAudio {
                effective_api_level: 33
            }
        );
    }

    #[test]
    fn t6e_single_video_socket_handshake_never_waits_for_audio_socket_on_aos9() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::thread;

        use arb_adb::scrcpy::{CODEC_ID_H264, SCRCPY_DUMMY_BYTE};

        use super::{AudioSocketMode, connect_scrcpy_tunnels};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock aos9 scrcpy listener");
        let port = listener.local_addr().expect("local addr").port();

        // Mirrors scrcpy-server launched with `audio=false`: accepts ONLY socket 1 (video),
        // writes 0x00 dummy byte + "h264" immediately, and closes the listener.
        let server = thread::spawn(move || {
            let (mut video, _) = listener.accept().expect("accept single video socket");
            video.write_all(&[SCRCPY_DUMMY_BYTE]).expect("write dummy");
            video
                .write_all(&CODEC_ID_H264.to_be_bytes())
                .expect("write h264 codec id");
            video.flush().expect("flush");
        });

        let stop = Arc::new(AtomicBool::new(false));
        let tunnels = connect_scrcpy_tunnels(
            port,
            AudioSocketMode::Disabled,
            Duration::from_secs(5),
            &stop,
        )
        .expect("single-socket handshake must succeed without audio socket");
        assert_eq!(tunnels.video_reader.codec_id(), CODEC_ID_H264);
        assert!(tunnels.audio_stream.is_none());
        assert!(tunnels.audio_connect_error.is_none());
        server.join().expect("mock server thread");
    }

    #[test]
    fn t6e_audio_reader_degrades_honestly_when_audio_socket_closes_immediately() {
        use std::net::{TcpListener, TcpStream};
        use std::sync::{Arc, Mutex, mpsc};
        use std::thread;

        use arb_core::state::RingBudgetPolicy;
        use tempfile::TempDir;

        use crate::capture::{CapturePipeline, SharedDiskSpaceProvider};
        use crate::protocol::AudioCaptureState;

        let tmp = TempDir::new().expect("tempdir");
        let mut pipe = CapturePipeline::open(
            tmp.path(),
            RingBudgetPolicy::default(),
            SharedDiskSpaceProvider::os_default(),
        )
        .expect("open pipeline");
        let generation = pipe.begin_live_session("aos9-fallback");
        let pipeline = Arc::new(Mutex::new(pipe));

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock audio listener");
        let addr = listener.local_addr().expect("addr");
        let server = thread::spawn(move || {
            // Immediately close the audio socket (simulates scrcpy rejecting audio on init).
            let (_sock, _) = listener.accept().expect("accept");
        });
        let stream = TcpStream::connect(addr).expect("connect");

        let (tx, _rx) = mpsc::channel();
        let ctx = super::MediaThreadCtx {
            session_stop: Arc::new(AtomicBool::new(false)),
            global_stop: Arc::new(AtomicBool::new(false)),
            sink: super::LiveSink {
                pipeline: Arc::clone(&pipeline),
                generation,
                failed: Arc::new(AtomicBool::new(false)),
            },
            signals: tx,
        };
        super::run_audio_reader_loop(stream, "boot-1", &ctx);
        server.join().expect("server");

        let status = pipeline.lock().expect("lock").status_view();
        assert_eq!(status.audio_state, AudioCaptureState::Unavailable);
        let reason = status
            .audio_reason
            .expect("must record honest audio fallback reason");
        assert!(
            reason.contains("Continuing with video + logs + taps"),
            "reason={reason}"
        );
    }

    #[test]
    fn opus_review_scrcpy_scid_never_exceeds_int_max_for_high_bit_millis() {
        // scrcpy-server v5.0.1 parses `scid` with `Integer.parseInt(hex)` and rejects values
        // above 0x7FFFFFFF, so bit 31 of the wall-clock millis must never reach the scid.
        for (millis, salt) in [
            (0x0000_0000_u64, 0_u32),
            (0x0AF9_A1B2, 0),
            (0x7FFF_FFFF, 1),
            (0x8000_0000, 0),
            (0x1_9ABC_DEF0, 0),
            (0xFFFF_FFFF, 0),
            (0xFFFF_FFFF, 1),
        ] {
            let scid = super::scrcpy_scid_hex(millis, salt);
            assert_eq!(scid.len(), 8, "scid={scid}");
            let value = u32::from_str_radix(&scid, 16).expect("hex scid");
            assert!(
                value <= 0x7FFF_FFFF,
                "scid {scid} (millis={millis:#x}, salt={salt}) exceeds Java int range"
            );
            assert!(
                value >= 0x1000_0000,
                "scid {scid} keeps the 0x10000000 marker"
            );
        }
        assert_ne!(
            super::scrcpy_scid_hex(0x8000_0000, 0),
            super::scrcpy_scid_hex(0x8000_0000, 1),
            "the audio=false retry salt must yield a different scid"
        );
    }

    #[test]
    fn opus_review_audio_fallback_reason_does_not_blame_audio_when_retry_also_fails() {
        let degraded = super::audio_fallback_reason("handshake eof", Some("audio init"), None);
        assert!(degraded.contains("Audio capture failed"), "{degraded}");
        assert!(
            degraded.contains("degraded to video + logs + taps"),
            "{degraded}"
        );

        let both_failed = super::audio_fallback_reason(
            "handshake eof",
            Some("MediaCodec busy"),
            Some("video tunnel timed out"),
        );
        assert!(
            !both_failed.contains("degraded to video"),
            "video did not start, so the reason must not claim video continues: {both_failed}"
        );
        assert!(
            both_failed.contains("scrcpy-server failed to start"),
            "{both_failed}"
        );
        assert!(
            both_failed.contains("video tunnel timed out"),
            "{both_failed}"
        );
    }
}
