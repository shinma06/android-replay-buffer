//! Live ADB device-agent deployment, streaming ingestion, and auto-reconnect supervision.
//!
//! Deploys `arb-companion.dex.jar` and `replay-scrcpy-audio.dex.jar` via
//! [`AdbClient`], runs concurrent companion/video/audio reader threads feeding
//! [`CapturePipeline`], performs periodic `Ping`/`Pong` sync probes, supervises
//! automatic reconnects with [`ReconnectPolicy`] (`200ms -> 500ms -> 1s -> 2s`),
//! and guarantees remote `/data/local/tmp/arb-*` and `adb forward` cleanup on stop.

use std::io::{BufRead, BufReader};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arb_adb::adb::{
    AdbClient, AdbDeviceState, DeployedAgentPaths, ReconnectClassification, SessionSupervisor,
    StreamEnable, SupervisorConfig,
};
use arb_adb::mock::CaptureEvent;
use arb_adb::scrcpy::{
    AudioCaptureMode, AudioPacketKind, AudioTunnelReader, AudioTunnelStatus, ScrcpyError,
    ScrcpyLaunchOptions, VideoRecord, VideoTunnelReader,
};
use arb_adb::wire::{
    DEFAULT_MAX_PAYLOAD_LEN, FrameFlags, PingPayload, ShutdownCode, ShutdownPayload, StreamId,
    WireFrame, WireMessage,
};
use arb_core::time::{
    ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind,
    PcSyncProbe,
};
use arb_os::clock::{HostClockSample, sample_host_clock};

use crate::capture::CapturePipeline;
use crate::error::DaemonError;
use crate::protocol::CaptureStartParams;
use crate::time_util::current_unix_millis;

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
}

impl Drop for LiveCaptureHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts a supervised live ADB capture session writing events into `pipeline`.
///
/// # Errors
///
/// Returns [`DaemonError::Adb`] if `adb` cannot be located or no online device
/// matches, or [`DaemonError::NotFound`] if `arb-companion.dex.jar` cannot be found.
///
/// # Panics
///
/// Panics if the internal `pipeline` mutex is poisoned.
pub fn start_live_adb_capture(
    params: &CaptureStartParams,
    pipeline: &Arc<Mutex<CapturePipeline>>,
) -> Result<(String, LiveCaptureHandle), DaemonError> {
    let adb = AdbClient::discover()?;
    let serial = resolve_target_serial(&adb, params.serial.as_deref())?;
    let companion_jar = resolve_agent_jar(
        params.companion_jar.as_deref(),
        "ARB_COMPANION_JAR",
        "arb-companion.dex.jar",
    )?;
    let scrcpy_jar = resolve_agent_jar(
        params.scrcpy_jar.as_deref(),
        "ARB_SCRCPY_JAR",
        "replay-scrcpy-audio.dex.jar",
    )
    .ok();

    let cfg = LiveCaptureConfig {
        serial: serial.clone(),
        companion_jar,
        scrcpy_jar,
        max_size: params.max_size.unwrap_or(1024),
        max_fps: params.max_fps.unwrap_or(30),
        video_bit_rate: params.video_bit_rate.unwrap_or(4_000_000),
        audio_enabled: params.audio_enabled.unwrap_or(true),
        logcat_enabled: params.logcat_enabled.unwrap_or(true),
        input_enabled: params.input_enabled.unwrap_or(true),
        heartbeat_ms: params.heartbeat_ms.unwrap_or(250),
    };

    {
        let mut guard = pipeline.lock().expect("capture pipeline lock poisoned");
        guard.begin_session(serial.clone());
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop_flag);
    let thread_pipeline = Arc::clone(pipeline);

    let handle = thread::Builder::new()
        .name(format!("arbd-adb-{serial}"))
        .spawn(move || {
            run_supervised_adb_loop(&adb, &cfg, &thread_stop, &thread_pipeline);
            let mut guard = thread_pipeline
                .lock()
                .expect("capture pipeline lock poisoned");
            guard.mark_stopped();
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
    if let Some(serial) = requested.filter(|s| !s.trim().is_empty()) {
        return Ok(serial.to_owned());
    }
    let devices = adb.devices()?;
    let online: Vec<String> = devices
        .into_iter()
        .filter(|d| d.state == AdbDeviceState::Device)
        .map(|d| d.serial)
        .collect();
    match online.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(DaemonError::NotFound(
            "no online ADB devices found".to_owned(),
        )),
        many => Err(DaemonError::InvalidParams(format!(
            "multiple online ADB devices found ({}); pass --serial explicitly",
            many.join(", ")
        ))),
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

fn run_supervised_adb_loop(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    stop_flag: &Arc<AtomicBool>,
    pipeline: &Arc<Mutex<CapturePipeline>>,
) {
    let mut supervisor = SessionSupervisor::new(cfg.serial.clone(), SupervisorConfig::new());
    let mut timeline = ClockTimeline::new(ClockTimelineConfig::new());

    while !stop_flag.load(Ordering::Acquire) {
        {
            let guard = pipeline.lock().expect("capture pipeline lock poisoned");
            if !guard.is_active() {
                break;
            }
        }

        supervisor.begin_connecting();
        let outcome = run_single_adb_session(
            adb,
            cfg,
            stop_flag,
            pipeline,
            &mut supervisor,
            &mut timeline,
        );
        if stop_flag.load(Ordering::Acquire) {
            break;
        }

        let reason = match outcome {
            Ok(()) => "session_ended".to_owned(),
            Err(err) => err.to_string(),
        };
        let (backoff, disc) = supervisor.on_disconnect(sample_cont_ns(), &reason);
        push_event(pipeline, CaptureEvent::Discontinuity(disc));

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
        if let Some(mut child) = self.companion_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.scrcpy_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = self
            .adb
            .cleanup_session(self.serial, &self.deployed, &self.forwarded_ports);
    }
}

fn run_single_adb_session(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    stop_flag: &Arc<AtomicBool>,
    pipeline: &Arc<Mutex<CapturePipeline>>,
    supervisor: &mut SessionSupervisor,
    timeline: &mut ClockTimeline,
) -> Result<(), DaemonError> {
    let token_raw = current_unix_millis() % 1_000_000;
    let session_token = format!("s{}-{token_raw:06}", std::process::id());
    let deployed = adb.deploy_session_agents(
        &cfg.serial,
        &session_token,
        &cfg.companion_jar,
        cfg.scrcpy_jar.as_deref(),
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
    let api_level = hello.api_level;
    let boot_id = hello.boot_id.clone();
    record_hello_handshake(supervisor, pipeline, hello);

    // 2. Launch scrcpy-server-v5.0.1 for H.264 video (and optional AAC audio on API >= 33).
    let session_stop = Arc::new(AtomicBool::new(false));
    let (media_threads, media_sockets) = launch_scrcpy_streams(
        adb,
        cfg,
        &mut guard,
        api_level,
        &boot_id,
        &session_stop,
        stop_flag,
        pipeline,
    )?;

    // 3. Companion control/logcat/input loop with periodic Ping/Pong RTT sync probes.
    let loop_result = run_companion_event_loop(
        &mut companion_reader,
        &mut companion_writer,
        &boot_id,
        stop_flag,
        pipeline,
        supervisor,
        timeline,
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

fn launch_and_handshake_companion(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    session_token: &str,
    guard: &mut SessionCleanupGuard<'_>,
    stop_flag: &Arc<AtomicBool>,
) -> Result<(TcpStream, TcpStream, arb_adb::wire::HelloPayload), DaemonError> {
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
    let mut companion_child = Command::new(adb.adb_path())
        .args(["-s", &cfg.serial, "shell", &companion_cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    if let Some(stdout) = companion_child.stdout.take() {
        wait_for_companion_ready(stdout, Duration::from_secs(8))?;
    }
    guard.companion_child = Some(companion_child);

    let mut companion_reader =
        connect_tcp_with_retry(companion_port, Duration::from_secs(5), stop_flag)?;
    companion_reader.set_read_timeout(Some(Duration::from_millis(250)))?;
    let companion_writer = companion_reader.try_clone()?;
    let hello = wait_for_hello_frame(&mut companion_reader, stop_flag, Duration::from_secs(5))?;
    Ok((companion_reader, companion_writer, hello))
}

fn record_hello_handshake(
    supervisor: &mut SessionSupervisor,
    pipeline: &Arc<Mutex<CapturePipeline>>,
    hello: arb_adb::wire::HelloPayload,
) {
    let prev_attempts = supervisor.reconnect_attempts();
    let now_cont = sample_cont_ns();
    let boot_id = hello.boot_id.clone();
    let (classification, reboot_disc) = supervisor.on_hello(&hello, now_cont);
    let is_restored = matches!(
        classification,
        ReconnectClassification::ConnectionRestored { .. }
    );
    push_event(
        pipeline,
        CaptureEvent::Hello {
            pc_cont_ns: now_cont,
            payload: hello,
            classification,
        },
    );
    if let Some(disc) = reboot_disc {
        push_event(pipeline, CaptureEvent::Discontinuity(disc));
    }
    if is_restored {
        push_event(
            pipeline,
            CaptureEvent::ConnectionRestored {
                pc_cont_ns: now_cont,
                boot_id,
                reconnect_attempts: prev_attempts,
            },
        );
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "scrcpy launch helper threads session guard, stop flags, and pipeline handle"
)]
fn launch_scrcpy_streams(
    adb: &AdbClient,
    cfg: &LiveCaptureConfig,
    guard: &mut SessionCleanupGuard<'_>,
    api_level: u32,
    boot_id: &str,
    session_stop: &Arc<AtomicBool>,
    stop_flag: &Arc<AtomicBool>,
    pipeline: &Arc<Mutex<CapturePipeline>>,
) -> Result<(Vec<JoinHandle<()>>, Vec<TcpStream>), DaemonError> {
    let mut media_threads: Vec<JoinHandle<()>> = Vec::new();
    let mut media_sockets: Vec<TcpStream> = Vec::new();

    let Some(remote_scrcpy) = guard.deployed.scrcpy_jar.clone() else {
        return Ok((media_threads, media_sockets));
    };

    let scid_low = u32::try_from(current_unix_millis() & 0xFFFF_FFFF).unwrap_or(0);
    let scid = format!("{:08x}", scid_low | 0x1000_0000);
    let scrcpy_abstract = format!("scrcpy_{scid}");
    let scrcpy_port = adb.forward_tcp_to_abstract(&cfg.serial, &scrcpy_abstract)?;
    guard.forwarded_ports.push(scrcpy_port);

    let scrcpy_opts = ScrcpyLaunchOptions {
        scid,
        max_size: cfg.max_size,
        max_fps: cfg.max_fps,
        video_bit_rate: cfg.video_bit_rate,
        audio_bit_rate: 128_000,
        audio_mode: if cfg.audio_enabled {
            AudioCaptureMode::Enabled
        } else {
            AudioCaptureMode::Disabled
        },
    };
    let (_server_args, planned_audio) = scrcpy_opts.build_server_args(api_level);
    let expect_audio_socket = matches!(planned_audio, AudioTunnelStatus::Available { .. });
    if !expect_audio_socket && cfg.audio_enabled {
        push_event(
            pipeline,
            CaptureEvent::AudioUnavailable {
                pc_cont_ns: sample_cont_ns(),
                boot_id: boot_id.to_owned(),
                status: planned_audio,
            },
        );
    }

    let scrcpy_cmd = AdbClient::<arb_adb::adb::SystemAdbRunner>::scrcpy_launch_command(
        &remote_scrcpy,
        &scrcpy_opts,
        api_level,
    );
    let scrcpy_child = Command::new(adb.adb_path())
        .args(["-s", &cfg.serial, "shell", &scrcpy_cmd])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    guard.scrcpy_child = Some(scrcpy_child);

    if let Ok((video_reader, video_shutdown_handle)) =
        connect_scrcpy_video_with_retry(scrcpy_port, Duration::from_secs(8), stop_flag)
    {
        media_sockets.push(video_shutdown_handle);
        let v_stop = Arc::clone(session_stop);
        let v_global_stop = Arc::clone(stop_flag);
        let v_pipe = Arc::clone(pipeline);
        let v_boot = boot_id.to_owned();
        media_threads.push(thread::spawn(move || {
            run_video_reader_loop(video_reader, &v_boot, &v_stop, &v_global_stop, &v_pipe);
        }));

        if expect_audio_socket
            && let Ok(audio_sock) =
                connect_tcp_with_retry(scrcpy_port, Duration::from_secs(3), stop_flag)
        {
            let _ = audio_sock.set_read_timeout(Some(Duration::from_millis(500)));
            if let Ok(audio_shutdown) = audio_sock.try_clone() {
                media_sockets.push(audio_shutdown);
            }
            let a_stop = Arc::clone(session_stop);
            let a_global_stop = Arc::clone(stop_flag);
            let a_pipe = Arc::clone(pipeline);
            let a_boot = boot_id.to_owned();
            media_threads.push(thread::spawn(move || {
                run_audio_reader_loop(audio_sock, &a_boot, &a_stop, &a_global_stop, &a_pipe);
            }));
        }
    }

    Ok((media_threads, media_sockets))
}

fn run_companion_event_loop(
    companion_reader: &mut TcpStream,
    companion_writer: &mut TcpStream,
    boot_id: &str,
    stop_flag: &Arc<AtomicBool>,
    pipeline: &Arc<Mutex<CapturePipeline>>,
    supervisor: &mut SessionSupervisor,
    timeline: &mut ClockTimeline,
) -> Result<(), DaemonError> {
    let mut pending_ping: Option<(u64, HostClockSample)> = None;
    let mut next_ping_at = Instant::now();
    let mut ping_seq: u32 = 1;

    loop {
        if stop_flag.load(Ordering::Acquire) {
            if let Ok(frame) = WireMessage::Shutdown(ShutdownPayload {
                boottime_ns: 0,
                code: ShutdownCode::Normal,
                reason: "arbd_capture_stop".to_owned(),
            })
            .to_frame(StreamId::Control, FrameFlags::URGENT, ping_seq)
            {
                let _ = frame.write_to(companion_writer);
            }
            return Ok(());
        }

        if pending_ping.is_none() && Instant::now() >= next_ping_at {
            let nonce = (0xA5A5_0000_u64 << 32) | u64::from(ping_seq);
            let send_sample = sample_clock_or_zero();
            if let Ok(frame) = WireMessage::Ping(PingPayload { nonce }).to_frame(
                StreamId::Control,
                FrameFlags::NONE,
                ping_seq,
            ) && frame.write_to(companion_writer).is_ok()
            {
                pending_ping = Some((nonce, send_sample));
                ping_seq = ping_seq.wrapping_add(1);
                next_ping_at = Instant::now() + Duration::from_secs(1);
            }
        }

        match WireFrame::read_from(companion_reader, DEFAULT_MAX_PAYLOAD_LEN) {
            Ok(Some(frame)) => {
                let recv_sample = sample_clock_or_zero();
                let keep_going = dispatch_companion_frame(
                    &frame,
                    recv_sample,
                    boot_id,
                    &mut pending_ping,
                    pipeline,
                    supervisor,
                    timeline,
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
    }
}

fn dispatch_companion_frame(
    frame: &WireFrame,
    recv_sample: HostClockSample,
    boot_id: &str,
    pending_ping: &mut Option<(u64, HostClockSample)>,
    pipeline: &Arc<Mutex<CapturePipeline>>,
    supervisor: &mut SessionSupervisor,
    timeline: &mut ClockTimeline,
) -> Result<bool, DaemonError> {
    let pc_cont_ns = recv_sample.continuous_mono_ns;
    match WireMessage::decode_frame(frame)? {
        WireMessage::Heartbeat(hb) => {
            supervisor.on_heartbeat(&hb, pc_cont_ns);
            let triple = DeviceClockTriple {
                boot_id: boot_id.to_owned(),
                boottime_ns: hb.boottime_ns,
                monotonic_ns: hb.monotonic_ns,
                realtime_ns: hb.realtime_ns,
            };
            let discs = timeline.ingest_device_triple(triple, pc_cont_ns);
            push_event(
                pipeline,
                CaptureEvent::Heartbeat {
                    pc_cont_ns,
                    boot_id: boot_id.to_owned(),
                    payload: hb,
                },
            );
            for disc in discs {
                push_event(pipeline, CaptureEvent::Discontinuity(disc));
            }
        }
        WireMessage::Pong(pong) => {
            handle_pong_frame(pong, recv_sample, boot_id, pending_ping, pipeline, timeline);
        }
        WireMessage::StreamState(st) => {
            let disc = supervisor.on_stream_state(&st, pc_cont_ns);
            push_event(
                pipeline,
                CaptureEvent::StreamState {
                    pc_cont_ns,
                    boot_id: boot_id.to_owned(),
                    payload: st,
                },
            );
            if let Some(d) = disc {
                push_event(pipeline, CaptureEvent::Discontinuity(d));
            }
        }
        WireMessage::LogRecord(rec) => push_event(
            pipeline,
            CaptureEvent::LogRecord {
                pc_cont_ns,
                boot_id: boot_id.to_owned(),
                flags: frame.header.flags,
                payload: rec,
            },
        ),
        WireMessage::LogOverflowStats(stats) => push_event(
            pipeline,
            CaptureEvent::LogOverflowStats {
                pc_cont_ns,
                boot_id: boot_id.to_owned(),
                payload: stats,
            },
        ),
        WireMessage::TouchEvent(touch) => push_event(
            pipeline,
            CaptureEvent::TouchEvent {
                pc_cont_ns,
                boot_id: boot_id.to_owned(),
                flags: frame.header.flags,
                payload: touch,
                normalized: None,
            },
        ),
        WireMessage::Shutdown(shutdown) => {
            push_event(
                pipeline,
                CaptureEvent::Shutdown {
                    pc_cont_ns,
                    payload: shutdown,
                },
            );
            return Ok(false);
        }
        _ => {}
    }
    Ok(true)
}

fn handle_pong_frame(
    pong: arb_adb::wire::PongPayload,
    recv_sample: HostClockSample,
    boot_id: &str,
    pending_ping: &mut Option<(u64, HostClockSample)>,
    pipeline: &Arc<Mutex<CapturePipeline>>,
    timeline: &mut ClockTimeline,
) {
    let Some((expected_nonce, send_sample)) = pending_ping.take() else {
        return;
    };
    if pong.nonce != expected_nonce {
        return;
    }
    let probe = PcSyncProbe {
        boot_id: boot_id.to_owned(),
        pc_send_cont_ns: send_sample.continuous_mono_ns,
        pc_send_active_ns: send_sample.active_mono_ns,
        dev_boottime_ns: pong.midpoint_boottime_ns(),
        dev_monotonic_ns: pong.monotonic_ns,
        pc_recv_cont_ns: recv_sample.continuous_mono_ns,
        pc_recv_active_ns: recv_sample.active_mono_ns,
    };
    let discs = timeline.ingest_pc_sync_probe(probe.clone());
    push_event(
        pipeline,
        CaptureEvent::SyncProbe {
            pc_cont_ns: recv_sample.continuous_mono_ns,
            probe,
            device_bracket_ns: pong.device_bracket_ns().unwrap_or(0),
        },
    );
    for disc in discs {
        push_event(pipeline, CaptureEvent::Discontinuity(disc));
    }
}

fn run_video_reader_loop(
    mut reader: VideoTunnelReader<TcpStream>,
    boot_id: &str,
    session_stop: &Arc<AtomicBool>,
    global_stop: &Arc<AtomicBool>,
    pipeline: &Arc<Mutex<CapturePipeline>>,
) {
    while !session_stop.load(Ordering::Acquire) && !global_stop.load(Ordering::Acquire) {
        match reader.read_next() {
            Ok(Some(VideoRecord::SessionMeta(meta))) => {
                push_event(
                    pipeline,
                    CaptureEvent::VideoSession {
                        pc_cont_ns: sample_cont_ns(),
                        boot_id: boot_id.to_owned(),
                        meta,
                    },
                );
            }
            Ok(Some(VideoRecord::Packet(packet))) => {
                push_event(
                    pipeline,
                    CaptureEvent::VideoFrame {
                        pc_cont_ns: sample_cont_ns(),
                        boot_id: boot_id.to_owned(),
                        packet,
                    },
                );
            }
            Err(ScrcpyError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Ok(None) | Err(_) => break,
            _ => {}
        }
    }
}

fn run_audio_reader_loop(
    stream: TcpStream,
    boot_id: &str,
    session_stop: &Arc<AtomicBool>,
    global_stop: &Arc<AtomicBool>,
    pipeline: &Arc<Mutex<CapturePipeline>>,
) {
    let mut reader = match AudioTunnelReader::handshake(stream) {
        Ok(r) => r,
        Err(ScrcpyError::AudioDisabled) => {
            push_event(
                pipeline,
                CaptureEvent::AudioUnavailable {
                    pc_cont_ns: sample_cont_ns(),
                    boot_id: boot_id.to_owned(),
                    status: AudioTunnelStatus::DisabledByDevice,
                },
            );
            return;
        }
        Err(ScrcpyError::AudioServerError) => {
            push_event(
                pipeline,
                CaptureEvent::AudioUnavailable {
                    pc_cont_ns: sample_cont_ns(),
                    boot_id: boot_id.to_owned(),
                    status: AudioTunnelStatus::DeviceError,
                },
            );
            return;
        }
        Err(_) => return,
    };

    while !session_stop.load(Ordering::Acquire) && !global_stop.load(Ordering::Acquire) {
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
                push_event(pipeline, ev);
            }
            Ok(None) => break,
            Err(ScrcpyError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                push_event(
                    pipeline,
                    CaptureEvent::Discontinuity(DiscontinuityEvent {
                        kind: DiscontinuityKind::StreamGap,
                        stream: Some(arb_core::time::StreamKind::Audio),
                        boot_id: Some(boot_id.to_owned()),
                        start_boottime_ns: None,
                        end_boottime_ns: None,
                        pc_cont_start_ns: sample_cont_ns(),
                        pc_cont_end_ns: None,
                        delta_ns: None,
                        detail: Some("audio tunnel closed unexpectedly".to_owned()),
                    }),
                );
                break;
            }
        }
    }
}

fn push_event(pipeline: &Arc<Mutex<CapturePipeline>>, event: CaptureEvent) {
    let mut guard = pipeline.lock().expect("capture pipeline lock poisoned");
    let _ = guard.ingest_event(event);
}

fn wait_for_companion_ready(
    stdout: std::process::ChildStdout,
    timeout: Duration,
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

    match rx.recv_timeout(timeout) {
        Ok(true) => Ok(()),
        Ok(false) => Err(DaemonError::Internal(
            "arb-companion exited before printing ARB_COMPANION_READY".to_owned(),
        )),
        Err(_) => Err(DaemonError::Internal(
            "timed out waiting for ARB_COMPANION_READY".to_owned(),
        )),
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

fn connect_scrcpy_video_with_retry(
    port: u16,
    timeout: Duration,
    stop_flag: &Arc<AtomicBool>,
) -> Result<(VideoTunnelReader<TcpStream>, TcpStream), DaemonError> {
    let deadline = Instant::now() + timeout;
    let addr = format!("127.0.0.1:{port}");
    loop {
        if stop_flag.load(Ordering::Acquire) {
            return Err(DaemonError::Internal(
                "capture stopped during scrcpy handshake".to_owned(),
            ));
        }
        match TcpStream::connect(&addr) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let shutdown_handle = stream.try_clone()?;
                match VideoTunnelReader::handshake(stream) {
                    Ok(reader) => {
                        let _ = shutdown_handle.set_read_timeout(Some(Duration::from_millis(300)));
                        return Ok((reader, shutdown_handle));
                    }
                    Err(err) => {
                        if Instant::now() >= deadline {
                            return Err(DaemonError::CaptureSource(err.into()));
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
) -> Result<arb_adb::wire::HelloPayload, DaemonError> {
    let deadline = Instant::now() + timeout;
    loop {
        if stop_flag.load(Ordering::Acquire) {
            return Err(DaemonError::Internal(
                "capture stopped before Hello frame".to_owned(),
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
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(DaemonError::Internal(
                        "timed out waiting for companion Hello frame".to_owned(),
                    ));
                }
            }
            Err(err) => return Err(DaemonError::CaptureSource(err.into())),
        }
    }
}
