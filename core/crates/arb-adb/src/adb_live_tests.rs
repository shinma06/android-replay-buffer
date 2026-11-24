//! Regression tests for the R2 review fixes in [`super`]: live multi-stream reading over
//! real loopback TCP (M1), transport-loss reporting and periodic liveness (M2),
//! `boot_id_source` continuity (S20), and subprocess / remote-state hygiene (S21).

use std::cell::RefCell;
use std::io::{Cursor, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::{Duration, Instant};

use arb_core::time::{ClockTimelineConfig, DiscontinuityKind, StreamKind};

use super::{
    AdbClient, AdbCommandOutput, AdbCommandRunner, AdbError, AdbSessionSource,
    ReconnectClassification, SessionSupervisor, StreamEnable, SupervisorConfig, SupervisorState,
    VideoLivenessStatus, run_command_with_timeout,
};
use crate::mock::{CaptureEvent, DeviceEventSource, FaultScenarioBuilder, FaultStep};
use crate::scrcpy::{
    AudioPacket, AudioPacketKind, CODEC_ID_AAC, CODEC_ID_H264, SESSION_META_TAG,
    ScrcpyLaunchOptions, VideoPacket, VideoPacketKind, VideoSessionMeta, write_audio_handshake,
    write_audio_packet, write_video_handshake, write_video_packet, write_video_session_meta,
};
use crate::wire::{
    BootIdSource, FrameFlags, HeartbeatPayload, HelloPayload, StreamHealthEntry, StreamId,
    StreamLifecycleState, WireMessage,
};

const SEC: i64 = 1_000_000_000;

fn hello(boot_id: &str, source: BootIdSource) -> HelloPayload {
    HelloPayload {
        protocol_version: 1,
        api_level: 37,
        boot_count: 1,
        boot_id_source: source,
        is_emulator: true,
        heartbeat_interval_ms: 300,
        boot_id: boot_id.to_owned(),
        model: "Pixel".into(),
        manufacturer: "Google".into(),
        agent_version: "0.1.0".into(),
        boot_id_detail: String::new(),
    }
}

/// Heartbeat whose stream table claims Control AND Video are `Running` (the companion
/// cannot see the scrcpy socket, so it keeps claiming Video is fine).
fn heartbeat(boottime_ns: i64) -> HeartbeatPayload {
    let entry = |stream_id| StreamHealthEntry {
        stream_id,
        state: StreamLifecycleState::Running,
        restart_count: 0,
        emitted_seq: 1,
        last_pts_ns: boottime_ns,
    };
    HeartbeatPayload {
        boottime_ns,
        monotonic_ns: boottime_ns,
        realtime_ns: 1_700_000_000 * SEC + boottime_ns,
        streams: vec![entry(StreamId::Control), entry(StreamId::Video)],
    }
}

fn frame_bytes(msg: &WireMessage, seq: u32) -> Vec<u8> {
    msg.to_frame(StreamId::Control, FrameFlags::URGENT, seq)
        .expect("build frame")
        .encode()
        .expect("encode frame")
}

fn real_clock() -> impl FnMut() -> i64 + Send + 'static {
    let start = Instant::now();
    move || i64::try_from(start.elapsed().as_nanos()).unwrap_or(i64::MAX)
}

fn is_disc(ev: &CaptureEvent, kind: DiscontinuityKind, stream: Option<StreamKind>) -> bool {
    matches!(ev, CaptureEvent::Discontinuity(d) if d.kind == kind && d.stream == stream)
}

/// Pulls events into `log` until `pred` matches one, returning its index.
fn pull_until<S: DeviceEventSource>(
    source: &mut S,
    log: &mut Vec<CaptureEvent>,
    what: &str,
    pred: impl Fn(&CaptureEvent) -> bool,
) -> usize {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        let ev = source
            .next_event()
            .expect("live next_event must not fail")
            .unwrap_or_else(|| panic!("source ended before {what}"));
        let hit = pred(&ev);
        log.push(ev);
        if hit {
            return log.len() - 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoCmd {
    StopFrames,
    Close,
}

fn spawn_companion_peer(listener: TcpListener, close: Receiver<()>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept companion");
        let hello = WireMessage::Hello(hello("boot-loopback", BootIdSource::KernelProcBootId));
        sock.write_all(&frame_bytes(&hello, 1))
            .expect("write hello");
        let start = Instant::now();
        let mut seq = 2u32;
        loop {
            match close.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            let boottime = 100 * SEC + i64::try_from(start.elapsed().as_nanos()).unwrap_or(0);
            let hb = WireMessage::Heartbeat(heartbeat(boottime));
            if sock.write_all(&frame_bytes(&hb, seq)).is_err() {
                break;
            }
            seq += 1;
            thread::sleep(Duration::from_millis(25));
        }
        // Dropping the socket closes the companion transport (EOF on the host).
    })
}

fn spawn_video_peer(listener: TcpListener, cmds: Receiver<VideoCmd>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept video");
        let mut buf = Vec::new();
        write_video_handshake(&mut buf, CODEC_ID_H264).expect("video handshake");
        write_video_session_meta(
            &mut buf,
            VideoSessionMeta {
                header_tag: SESSION_META_TAG,
                width: 1080,
                height: 2400,
            },
        )
        .expect("video meta");
        sock.write_all(&buf).expect("write video header");
        let mut pts_us = 0u64;
        let mut first = true;
        loop {
            match cmds.try_recv() {
                Ok(VideoCmd::StopFrames) => break,
                Ok(VideoCmd::Close) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {}
            }
            let mut pkt = Vec::new();
            write_video_packet(
                &mut pkt,
                &VideoPacket {
                    kind: if first {
                        VideoPacketKind::Keyframe
                    } else {
                        VideoPacketKind::DeltaFrame
                    },
                    pts_us,
                    data: vec![0, 0, 0, 1, if first { 0x65 } else { 0x41 }, 0xAA],
                },
            )
            .expect("encode video packet");
            if sock.write_all(&pkt).is_err() {
                return;
            }
            first = false;
            pts_us += 16_000;
            thread::sleep(Duration::from_millis(10));
        }
        // Silent but open (static screen) until told to die like a crashed scrcpy-server.
        while let Ok(cmd) = cmds.recv() {
            if cmd == VideoCmd::Close {
                break;
            }
        }
    })
}

fn spawn_audio_peer(listener: TcpListener, close: Receiver<()>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept audio");
        let mut buf = Vec::new();
        write_audio_handshake(&mut buf, CODEC_ID_AAC).expect("audio handshake");
        write_audio_packet(
            &mut buf,
            &AudioPacket {
                kind: AudioPacketKind::Config,
                pts_us: 0,
                data: vec![0x11, 0x90],
            },
        )
        .expect("audio config");
        sock.write_all(&buf).expect("write audio header");
        let mut pts_us = 0u64;
        loop {
            match close.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            let mut pkt = Vec::new();
            write_audio_packet(
                &mut pkt,
                &AudioPacket {
                    kind: AudioPacketKind::Frame,
                    pts_us,
                    data: vec![0x21, 0x10, 0x05],
                },
            )
            .expect("audio frame");
            if sock.write_all(&pkt).is_err() {
                break;
            }
            pts_us += 21_333;
            thread::sleep(Duration::from_millis(20));
        }
    })
}

fn bind_loopback() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind loopback listener")
}

fn connect(listener: &TcpListener) -> TcpStream {
    TcpStream::connect(listener.local_addr().expect("listener addr")).expect("connect loopback")
}

#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end loopback session walks every live transport phase in order"
)]
#[test]
fn live_loopback_tcp_reads_streams_concurrently_and_reports_transport_loss() {
    let comp_l = bind_loopback();
    let vid_l = bind_loopback();
    let aud_l = bind_loopback();
    let (comp_close_tx, comp_close_rx): (Sender<()>, _) = channel();
    let (vid_tx, vid_rx) = channel();
    let (aud_close_tx, aud_close_rx): (Sender<()>, _) = channel();

    let comp_sock = connect(&comp_l);
    let vid_sock = connect(&vid_l);
    let aud_sock = connect(&aud_l);
    let peers = [
        spawn_companion_peer(comp_l, comp_close_rx),
        spawn_video_peer(vid_l, vid_rx),
        spawn_audio_peer(aud_l, aud_close_rx),
    ];

    let config = SupervisorConfig {
        static_hold_threshold_ns: 300_000_000,
        beacon_timeout_ns: 10 * SEC,
        ..SupervisorConfig::new()
    };
    let mut source = AdbSessionSource::from_tcp_streams(
        "emulator-5554",
        comp_sock,
        Some(vid_sock),
        Some(aud_sock),
        config,
        ClockTimelineConfig::new(),
        real_clock(),
    )
    .expect("loopback handshake")
    .with_liveness_tick_interval(Duration::from_millis(20));

    // ---- Phase 1 (M1): video/audio flow WHILE the companion stays open. ----
    let mut log = Vec::new();
    let count = |log: &[CaptureEvent], pred: &dyn Fn(&CaptureEvent) -> bool| {
        log.iter().filter(|e| pred(e)).count()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while count(&log, &|e| matches!(e, CaptureEvent::VideoFrame { .. })) < 8
        || count(&log, &|e| matches!(e, CaptureEvent::Heartbeat { .. })) < 5
        || count(&log, &|e| matches!(e, CaptureEvent::AudioFrame { .. })) < 3
    {
        assert!(Instant::now() < deadline, "live streams must all flow");
        pull_until(&mut source, &mut log, "next live event", |_| true);
    }
    assert!(
        !log.iter()
            .any(|e| is_disc(e, DiscontinuityKind::ConnectionLost, None)),
        "companion is still open: no ConnectionLost yet"
    );
    let video_idx: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, CaptureEvent::VideoFrame { .. }))
        .map(|(i, _)| i)
        .collect();
    let hb_idx: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, CaptureEvent::Heartbeat { .. }))
        .map(|(i, _)| i)
        .collect();
    let (first_v, last_v) = (video_idx[0], video_idx[video_idx.len() - 1]);
    let (first_h, last_h) = (hb_idx[0], hb_idx[hb_idx.len() - 1]);
    assert!(
        hb_idx.iter().any(|&h| h > first_v && h < last_v)
            && video_idx.iter().any(|&v| v > first_h && v < last_h),
        "video frames and heartbeats must interleave while the companion is open: \
         video={video_idx:?} heartbeats={hb_idx:?}"
    );
    let video_pcs: Vec<i64> = log
        .iter()
        .filter_map(|e| match e {
            CaptureEvent::VideoFrame { pc_cont_ns, .. } => Some(*pc_cont_ns),
            _ => None,
        })
        .collect();
    assert!(
        video_pcs.windows(2).all(|w| w[0] < w[1]),
        "per-stream pc_cont_ns is stamped at read completion and strictly increases"
    );

    // ---- Phase 2 (M2 liveness tick): open-but-silent video is a StaticHold. ----
    vid_tx.send(VideoCmd::StopFrames).expect("stop frames");
    let hold = pull_until(&mut source, &mut log, "StaticHold", |e| {
        matches!(e, CaptureEvent::StaticHold { .. })
    });
    assert!(
        !log[..hold].iter().any(|e| is_disc(
            e,
            DiscontinuityKind::StreamGap,
            Some(StreamKind::Video)
        )),
        "a healthy silent video socket must never be a StreamGap"
    );

    // ---- Phase 3 (M2): scrcpy video death is a StreamGap(Video), NOT StaticHold. ----
    vid_tx.send(VideoCmd::Close).expect("close video");
    let gap = pull_until(&mut source, &mut log, "StreamGap(Video)", |e| {
        is_disc(e, DiscontinuityKind::StreamGap, Some(StreamKind::Video))
    });
    // Heartbeats keep claiming Video=Running; the host-observed loss must win.
    let hb_after = log.len();
    while count(&log[hb_after..], &|e| {
        matches!(e, CaptureEvent::Heartbeat { .. })
    }) < 4
    {
        pull_until(&mut source, &mut log, "heartbeat after video death", |_| {
            true
        });
    }
    assert!(
        !log[gap..]
            .iter()
            .any(|e| matches!(e, CaptureEvent::StaticHold { .. })),
        "a dead scrcpy video socket must never be reported as StaticHold"
    );
    assert_eq!(
        source.supervisor().stream_state(StreamId::Video),
        Some(StreamLifecycleState::Error),
        "video stream must be Error after its socket closed, despite Running heartbeats"
    );
    assert!(
        matches!(
            source.supervisor().evaluate_video_liveness(i64::MAX / 2),
            VideoLivenessStatus::StreamGap { .. }
        ),
        "liveness must classify the dead video as StreamGap"
    );
    assert!(
        !log.iter()
            .any(|e| is_disc(e, DiscontinuityKind::ConnectionLost, None)),
        "video death must not be reported as a companion disconnect"
    );

    // ---- Phase 4: audio socket death is a StreamGap(Audio). ----
    aud_close_tx.send(()).expect("close audio");
    pull_until(&mut source, &mut log, "StreamGap(Audio)", |e| {
        is_disc(e, DiscontinuityKind::StreamGap, Some(StreamKind::Audio))
    });

    // ---- Phase 5 (M2): companion EOF is ConnectionLost and drives the supervisor. ----
    comp_close_tx.send(()).expect("close companion");
    pull_until(&mut source, &mut log, "ConnectionLost", |e| {
        is_disc(e, DiscontinuityKind::ConnectionLost, None)
    });
    assert!(
        matches!(
            source.supervisor().state(),
            SupervisorState::Reconnecting { attempt: 1, .. }
        ),
        "companion EOF must invoke SessionSupervisor::on_disconnect"
    );
    let rest = source.drain_all().expect("drain to end");
    assert!(
        rest.iter()
            .all(|e| !matches!(e, CaptureEvent::Discontinuity(_))),
        "each transport end is reported exactly once"
    );
    for peer in peers {
        peer.join().expect("peer thread");
    }
}

#[test]
fn live_source_after_shutdown_treats_transport_ends_as_graceful() {
    let mut comp = Vec::new();
    comp.extend(frame_bytes(
        &WireMessage::Hello(hello("boot-s", BootIdSource::KernelProcBootId)),
        1,
    ));
    comp.extend(frame_bytes(
        &WireMessage::Shutdown(crate::wire::ShutdownPayload {
            boottime_ns: 0,
            code: crate::wire::ShutdownCode::Normal,
            reason: "normal".into(),
        }),
        2,
    ));
    let mut tick = 0i64;
    let mut source = AdbSessionSource::<_, std::io::Empty, std::io::Empty>::new(
        "emulator-5554",
        Cursor::new(comp),
        None,
        None,
        SupervisorConfig::new(),
        ClockTimelineConfig::new(),
        move || {
            tick += 1_000;
            tick
        },
    );
    let events = source.drain_all().expect("drain");
    assert!(
        events
            .iter()
            .all(|e| !matches!(e, CaptureEvent::Discontinuity(_))),
        "EOF after an explicit Shutdown is teardown, not ConnectionLost: {events:?}"
    );
}

// ---------------- S20: boot_id_source continuity ----------------

fn reboot_count(events: &[CaptureEvent]) -> usize {
    events
        .iter()
        .filter(|e| is_disc(e, DiscontinuityKind::DeviceReboot, None))
        .count()
}

fn live_companion_reconnect(first: &HelloPayload, second: &HelloPayload) -> Vec<CaptureEvent> {
    let mut comp = Vec::new();
    comp.extend(frame_bytes(&WireMessage::Hello(first.clone()), 1));
    comp.extend(frame_bytes(&WireMessage::Heartbeat(heartbeat(10 * SEC)), 2));
    comp.extend(frame_bytes(&WireMessage::Hello(second.clone()), 3));
    comp.extend(frame_bytes(&WireMessage::Heartbeat(heartbeat(11 * SEC)), 4));
    let mut tick = 0i64;
    let mut source = AdbSessionSource::<_, std::io::Empty, std::io::Empty>::new(
        "emulator-5554",
        Cursor::new(comp),
        None,
        None,
        SupervisorConfig::new(),
        ClockTimelineConfig::new(),
        move || {
            tick += 100_000_000;
            tick
        },
    );
    source.drain_all().expect("drain live reconnect")
}

fn mock_companion_reconnect(first: &HelloPayload, second: &HelloPayload) -> Vec<CaptureEvent> {
    let mut source = FaultScenarioBuilder::new("emulator-5554")
        .push(FaultStep::Hello {
            pc_cont_ns: SEC,
            payload: first.clone(),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 2 * SEC,
            payload: heartbeat(10 * SEC),
        })
        .push(FaultStep::Hello {
            pc_cont_ns: 3 * SEC,
            payload: second.clone(),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 4 * SEC,
            payload: heartbeat(11 * SEC),
        })
        .build();
    source.drain_all().expect("drain mock reconnect")
}

#[test]
fn boot_id_source_change_is_continuity_unverified_not_device_reboot() {
    let kernel = hello(
        "6f1c2a7e-0000-4000-8000-000000000001",
        BootIdSource::KernelProcBootId,
    );
    let fallback = hello(
        "fallback:bc=1:btime=1700000000",
        BootIdSource::FallbackBootCountBtime,
    );

    let mut sup = SessionSupervisor::new("emulator-5554", SupervisorConfig::new());
    let _ = sup.on_hello(&kernel, 1);
    let (cls, disc) = sup.on_hello(&fallback, 2);
    assert!(
        matches!(cls, ReconnectClassification::ContinuityUnverified { .. }),
        "reliable source change (Kernel -> BootCount) must be ContinuityUnverified, got {cls:?}"
    );
    assert!(
        disc.is_none(),
        "a source change must never fabricate DeviceReboot"
    );

    // Same reliable source with a different id is still a genuine reboot.
    let mut sup2 = SessionSupervisor::new("emulator-5554", SupervisorConfig::new());
    let _ = sup2.on_hello(&kernel, 1);
    let mut rebooted = kernel.clone();
    rebooted.boot_id = "6f1c2a7e-0000-4000-8000-000000000002".into();
    let (cls2, disc2) = sup2.on_hello(&rebooted, 2);
    assert!(
        matches!(cls2, ReconnectClassification::DeviceReboot { .. }),
        "same-source boot_id change must remain DeviceReboot"
    );
    assert!(disc2.is_some(), "genuine reboot must emit DeviceReboot");

    assert_eq!(
        reboot_count(&live_companion_reconnect(&kernel, &fallback)),
        0,
        "live source: source change must not leak a timeline DeviceReboot"
    );
    assert_eq!(
        reboot_count(&mock_companion_reconnect(&kernel, &fallback)),
        0,
        "mock source: source change must not leak a timeline DeviceReboot"
    );
    assert_eq!(
        reboot_count(&live_companion_reconnect(&kernel, &rebooted)),
        1,
        "live source: genuine reboot is reported exactly once (supervisor, not timeline)"
    );
}

#[test]
fn fallback_random_uuid_reconnect_suppresses_timeline_device_reboot() {
    let kernel = hello("boot-A", BootIdSource::KernelProcBootId);
    let random = hello("fallback:random:9b2d0c5e", BootIdSource::FallbackRandomUuid);
    for (label, events) in [
        ("live", live_companion_reconnect(&kernel, &random)),
        ("mock", mock_companion_reconnect(&kernel, &random)),
    ] {
        assert!(
            events.iter().any(|e| matches!(
                e,
                CaptureEvent::Hello {
                    classification: ReconnectClassification::ContinuityUnverified { .. },
                    ..
                }
            )),
            "{label}: random-UUID reconnect must classify ContinuityUnverified"
        );
        assert_eq!(
            reboot_count(&events),
            0,
            "{label}: ContinuityUnverified must not leak a timeline DeviceReboot"
        );
    }
}

// ---------------- S21: subprocess timeout, remote cleanup, shell args ----------------

const SLEEP_CHILD_ENV: &str = "ARB_ADB_TEST_SLEEP_CHILD";

/// Helper re-executed as a child process by the timeout test; a no-op otherwise.
#[test]
fn subprocess_sleep_helper() {
    if std::env::var_os(SLEEP_CHILD_ENV).is_some() {
        thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn run_command_with_timeout_kills_wedged_subprocess() {
    let exe = std::env::current_exe().expect("current test exe");
    let mut wedged = Command::new(&exe);
    wedged
        .args([
            "--exact",
            "adb::live_tests::subprocess_sleep_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SLEEP_CHILD_ENV, "1");
    let started = Instant::now();
    let err = run_command_with_timeout(wedged, Duration::from_millis(300))
        .expect_err("wedged child must time out");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::TimedOut,
        "timeout must surface as TimedOut"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "timeout must return promptly instead of waiting for the 60s child"
    );

    let mut quick = Command::new(&exe);
    quick.args(["--list"]);
    let out = run_command_with_timeout(quick, Duration::from_secs(60)).expect("quick child");
    assert!(out.is_success(), "a quick child must complete normally");
    assert!(
        out.stdout.contains("subprocess_sleep_helper"),
        "stdout must be captured"
    );
}

#[derive(Debug, Default)]
struct ScriptedRunner {
    invocations: RefCell<Vec<Vec<String>>>,
    responses: RefCell<Vec<AdbCommandOutput>>,
}

impl AdbCommandRunner for ScriptedRunner {
    fn run(&self, _program: &Path, args: &[String]) -> Result<AdbCommandOutput, std::io::Error> {
        self.invocations.borrow_mut().push(args.to_vec());
        let mut responses = self.responses.borrow_mut();
        Ok(if responses.is_empty() {
            out(0, "")
        } else {
            responses.remove(0)
        })
    }
}

fn out(code: i32, stdout: &str) -> AdbCommandOutput {
    AdbCommandOutput {
        exit_code: Some(code),
        stdout: stdout.to_owned(),
        stderr: if code == 0 {
            String::new()
        } else {
            "boom".into()
        },
    }
}

fn scripted(responses: Vec<AdbCommandOutput>) -> AdbClient<ScriptedRunner> {
    AdbClient::with_runner(
        PathBuf::from("/usr/bin/adb"),
        ScriptedRunner {
            invocations: RefCell::new(Vec::new()),
            responses: RefCell::new(responses),
        },
    )
}

#[test]
fn partial_deploy_failure_removes_already_pushed_remote_jars() {
    let client = scripted(vec![out(0, "pushed"), out(1, "")]);
    let err = client
        .deploy_session_agents(
            "emulator-5554",
            "sess-x",
            Path::new("/tmp/arb-companion.dex.jar"),
            Some(Path::new("/tmp/replay-scrcpy-audio.dex.jar")),
        )
        .expect_err("scrcpy push failure must fail the deploy");
    assert!(
        matches!(err, AdbError::CommandFailed { .. }),
        "the original push error is returned"
    );
    let calls = client.runner.invocations.borrow();
    let cleanup = calls.last().expect("cleanup call");
    assert!(
        cleanup.iter().any(|a| a == "rm")
            && cleanup
                .iter()
                .any(|a| a == "/data/local/tmp/arb-sess-x-companion.dex.jar")
            && cleanup
                .iter()
                .any(|a| a == "/data/local/tmp/arb-sess-x-scrcpy-audio.dex.jar"),
        "already-pushed companion jar (and partial scrcpy jar) must be removed: {cleanup:?}"
    );
}

#[test]
fn unparsable_forward_output_removes_orphan_forward() {
    let client = scripted(vec![
        out(0, "not-a-port\n"),
        out(
            0,
            "emulator-5554 tcp:41234 localabstract:arb-companion-x\n\
             emulator-5556 tcp:41235 localabstract:arb-companion-x\n\
             emulator-5554 tcp:41236 localabstract:other\n",
        ),
        out(0, ""),
    ]);
    let err = client
        .forward_tcp_to_abstract("emulator-5554", "arb-companion-x")
        .expect_err("garbage port must fail");
    assert!(
        matches!(err, AdbError::InvalidForwardedPort { .. }),
        "parse failure must still be reported"
    );
    let calls = client.runner.invocations.borrow();
    let removed: Vec<&Vec<String>> = calls
        .iter()
        .filter(|c| c.iter().any(|a| a == "--remove"))
        .collect();
    assert_eq!(removed.len(), 1, "exactly the orphan forward is removed");
    assert!(
        removed[0].iter().any(|a| a == "tcp:41234"),
        "orphan tcp:41234 for this serial/target must be removed: {removed:?}"
    );
}

#[test]
fn remote_shell_arguments_are_strictly_validated() {
    for bad in ["sock;reboot", "$(id)", "a b", "", "x`y`", "sock'name"] {
        assert!(
            matches!(
                AdbClient::<ScriptedRunner>::try_companion_launch_command(
                    "/data/local/tmp/arb-s-companion.dex.jar",
                    bad,
                    300,
                    StreamEnable::Enabled,
                    StreamEnable::Enabled,
                ),
                Err(AdbError::InvalidShellArgument {
                    name: "socket_name",
                    ..
                })
            ),
            "unsafe socket_name {bad:?} must be rejected"
        );
        assert!(
            matches!(
                AdbClient::<ScriptedRunner>::try_companion_launch_command(
                    bad,
                    "arb-sock",
                    300,
                    StreamEnable::Enabled,
                    StreamEnable::Enabled,
                ),
                Err(AdbError::InvalidShellArgument {
                    name: "remote_jar",
                    ..
                })
            ),
            "unsafe remote_jar {bad:?} must be rejected"
        );
    }
    for bad_scid in ["1234ABCD", "1234abc", "1234abcde", "12;4abcd", "$(id)xx"] {
        assert!(
            matches!(
                AdbClient::<ScriptedRunner>::try_scrcpy_launch_command(
                    "/data/local/tmp/arb-s-scrcpy-audio.dex.jar",
                    &ScrcpyLaunchOptions::fhd30(bad_scid),
                    37,
                ),
                Err(AdbError::InvalidShellArgument { name: "scid", .. })
            ),
            "invalid scid {bad_scid:?} must be rejected"
        );
    }
    let ok = AdbClient::<ScriptedRunner>::try_scrcpy_launch_command(
        "/data/local/tmp/arb-s-scrcpy-audio.dex.jar",
        &ScrcpyLaunchOptions::fhd30("0a1b2c3d"),
        37,
    )
    .expect("valid scid");
    assert!(ok.contains("scid=0a1b2c3d"), "valid scid passes through");

    // The infallible legacy form never interpolates an unsafe value verbatim.
    let quoted = AdbClient::<ScriptedRunner>::companion_launch_command(
        "/data/local/tmp/j.jar",
        "x; reboot",
        300,
        StreamEnable::Enabled,
        StreamEnable::Enabled,
    );
    assert!(
        quoted.contains("--socket 'x; reboot'"),
        "unsafe value must be single-quoted: {quoted}"
    );

    let client = scripted(Vec::new());
    assert!(
        matches!(
            client.forward_tcp_to_abstract("emulator-5554", "a;b"),
            Err(AdbError::InvalidShellArgument { .. })
        ),
        "unsafe abstract socket name must be rejected before running adb"
    );
    assert!(
        client.runner.invocations.borrow().is_empty(),
        "validation happens before any adb invocation"
    );
}
