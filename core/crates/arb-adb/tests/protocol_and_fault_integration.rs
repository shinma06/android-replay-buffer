//! Public API integration tests verifying bit-exact `device-agent/PROTOCOL.md` wire
//! byte layouts, `scrcpy-server-v5.0.1` framing, `AdbClient` token safety, polymorphic
//! `DeviceEventSource` consumption, and `arb-core` selection integration.

#![expect(
    clippy::too_many_lines,
    reason = "golden byte-layout tests list every PROTOCOL.md field inline so the vectors stay reviewable against the spec"
)]

use std::io::Cursor;
use std::path::PathBuf;

use arb_adb::adb::{
    AdbClient, AdbCommandOutput, AdbCommandRunner, AdbError, AdbSessionSource, SupervisorConfig,
};
use arb_adb::mock::{CaptureEvent, DeviceEventSource, FaultScenario, MockDeviceSource};
use arb_adb::scrcpy::{
    AudioPacket, AudioPacketKind, AudioTunnelReader, CODEC_ID_AAC, CODEC_ID_H264, SESSION_META_TAG,
    ScrcpyError, VideoPacket, VideoPacketKind, VideoSessionMeta, VideoTunnelReader,
    write_audio_handshake, write_audio_packet, write_video_handshake, write_video_packet,
    write_video_session_meta,
};
use arb_adb::wire::{
    BootIdSource, DEFAULT_MAX_PAYLOAD_LEN, FrameFlags, HEADER_SIZE, HeartbeatPayload, HelloPayload,
    LogOverflowStatsPayload, LogRecordPayload, MessageType, PingPayload, PongPayload, ShutdownCode,
    ShutdownPayload, StreamHealthEntry, StreamId, StreamLifecycleState, StreamStatePayload,
    TouchEventPayload, WireError, WireFrame, WireLogCompleteness, WireMessage, decode_heartbeat,
    decode_hello, decode_log_overflow_stats, decode_log_record, decode_ping, decode_pong,
    decode_shutdown, decode_stream_state, decode_touch_event, encode_heartbeat, encode_hello,
    encode_log_overflow_stats, encode_log_record, encode_ping, encode_pong, encode_shutdown,
    encode_stream_state, encode_touch_event,
};
use arb_core::selection::{SelectionPolicy, TimestampedRecord, WindowSpec, select_records};
use arb_core::time::{ClockTimelineConfig, TimestampSemantics};

fn push_str16(buf: &mut Vec<u8>, value: &str) {
    let len = u16::try_from(value.len()).expect("test string fits u16");
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(value.as_bytes());
}

#[test]
fn golden_protocol_md_byte_layouts_match_rust_codec_bit_for_bit() {
    // 1. Hello golden wire layout directly from PROTOCOL.md §3.1
    let mut expected_hello = Vec::new();
    expected_hello.extend_from_slice(&1u16.to_le_bytes()); // protocol_version
    expected_hello.extend_from_slice(&37u32.to_le_bytes()); // api_level
    expected_hello.extend_from_slice(&(-1i32).to_le_bytes()); // boot_count (-1 unreadable)
    expected_hello.push(1); // boot_id_source = FallbackBootCountBtime
    expected_hello.push(0); // is_emulator = false
    expected_hello.extend_from_slice(&250u32.to_le_bytes()); // heartbeat_interval_ms
    push_str16(&mut expected_hello, "fallback:bc=-1:btime=1700000000");
    push_str16(&mut expected_hello, "Pixel 9");
    push_str16(&mut expected_hello, "Google");
    push_str16(&mut expected_hello, "0.1.0");
    push_str16(&mut expected_hello, "selinux_denied");

    let decoded_hello = decode_hello(&expected_hello).expect("decode golden hello");
    assert_eq!(
        decoded_hello,
        HelloPayload {
            protocol_version: 1,
            api_level: 37,
            boot_count: -1,
            boot_id_source: BootIdSource::FallbackBootCountBtime,
            is_emulator: false,
            heartbeat_interval_ms: 250,
            boot_id: "fallback:bc=-1:btime=1700000000".into(),
            model: "Pixel 9".into(),
            manufacturer: "Google".into(),
            agent_version: "0.1.0".into(),
            boot_id_detail: "selinux_denied".into(),
        },
        "golden Hello decode must match PROTOCOL.md §3.1"
    );
    assert_eq!(
        encode_hello(&decoded_hello).expect("re-encode hello"),
        expected_hello,
        "re-encoded Hello must match golden bytes bit-for-bit"
    );

    // 2. Heartbeat golden wire layout directly from PROTOCOL.md §3.2
    let mut expected_hb = Vec::new();
    expected_hb.extend_from_slice(&1000i64.to_le_bytes()); // boottime_ns
    expected_hb.extend_from_slice(&900i64.to_le_bytes()); // monotonic_ns
    expected_hb.extend_from_slice(&2000i64.to_le_bytes()); // realtime_ns
    expected_hb.push(1); // stream_count = 1
    expected_hb.push(3); // stream_id = Video
    expected_hb.push(1); // state = Running
    expected_hb.extend_from_slice(&2u16.to_le_bytes()); // restart_count
    expected_hb.extend_from_slice(&77u32.to_le_bytes()); // emitted_seq
    expected_hb.extend_from_slice(&999i64.to_le_bytes()); // last_pts_ns
    expected_hb.extend_from_slice(&0u32.to_le_bytes()); // reserved
    assert_eq!(expected_hb.len(), 45, "25 + 1*20 = 45 bytes");

    let decoded_hb = decode_heartbeat(&expected_hb).expect("decode golden heartbeat");
    assert_eq!(
        decoded_hb,
        HeartbeatPayload {
            boottime_ns: 1000,
            monotonic_ns: 900,
            realtime_ns: 2000,
            streams: vec![StreamHealthEntry {
                stream_id: StreamId::Video,
                state: StreamLifecycleState::Running,
                restart_count: 2,
                emitted_seq: 77,
                last_pts_ns: 999,
            }],
        },
        "golden Heartbeat decode must match PROTOCOL.md §3.2"
    );
    assert_eq!(
        encode_heartbeat(&decoded_hb).expect("re-encode hb"),
        expected_hb,
        "re-encoded Heartbeat must match golden bytes bit-for-bit"
    );

    // 3 & 4. Ping / Pong golden layouts (§3.3 / §3.4)
    let ping_wire = 0x0102_0304_0506_0708u64.to_le_bytes().to_vec();
    let ping_val = decode_ping(&ping_wire).expect("decode golden ping");
    assert_eq!(
        ping_val,
        PingPayload {
            nonce: 0x0102_0304_0506_0708
        },
        "golden Ping decode must match PROTOCOL.md §3.3"
    );
    assert_eq!(
        encode_ping(ping_val),
        ping_wire,
        "re-encoded Ping must match golden bytes"
    );

    let mut reply_wire = Vec::new();
    reply_wire.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
    reply_wire.extend_from_slice(&10_000i64.to_le_bytes());
    reply_wire.extend_from_slice(&9_500i64.to_le_bytes());
    reply_wire.extend_from_slice(&1_700_000i64.to_le_bytes());
    reply_wire.extend_from_slice(&10_400i64.to_le_bytes());
    let reply_val = decode_pong(&reply_wire).expect("decode golden pong");
    assert_eq!(
        reply_val,
        PongPayload {
            nonce: 0x0102_0304_0506_0708,
            boottime_before_ns: 10_000,
            monotonic_ns: 9_500,
            realtime_ns: 1_700_000,
            boottime_after_ns: 10_400,
        },
        "golden Pong decode must match PROTOCOL.md §3.4"
    );
    assert_eq!(
        encode_pong(reply_val),
        reply_wire,
        "re-encoded Pong must match golden bytes"
    );
}

#[test]
fn golden_protocol_md_data_and_shutdown_layouts_match_bit_for_bit() {
    // 5. StreamState (§3.5)
    let mut expected_st = Vec::new();
    expected_st.push(1); // Logcat
    expected_st.push(4); // Restarting
    expected_st.extend_from_slice(&3u16.to_le_bytes());
    expected_st.extend_from_slice(&55_000i64.to_le_bytes());
    expected_st.extend_from_slice(&12u32.to_le_bytes());
    push_str16(&mut expected_st, "backoff_ms:500");
    let decoded_st = decode_stream_state(&expected_st).expect("decode golden stream state");
    assert_eq!(
        decoded_st,
        StreamStatePayload {
            target_stream_id: StreamId::Logcat,
            state: StreamLifecycleState::Restarting,
            restart_count: 3,
            boottime_ns: 55_000,
            emitted_seq: 12,
            reason: "backoff_ms:500".into(),
        },
        "golden StreamState decode must match PROTOCOL.md §3.5"
    );
    assert_eq!(
        encode_stream_state(&decoded_st).expect("re-encode st"),
        expected_st,
        "re-encoded StreamState must match golden bytes"
    );

    // 6. LogRecord (§3.6)
    let mut expected_log = Vec::new();
    expected_log.extend_from_slice(&99_000i64.to_le_bytes());
    expected_log.extend_from_slice(&1_700_000_000u32.to_le_bytes());
    expected_log.extend_from_slice(&500_000u32.to_le_bytes());
    expected_log.extend_from_slice(&100i32.to_le_bytes());
    expected_log.extend_from_slice(&101i32.to_le_bytes());
    expected_log.extend_from_slice(&0u32.to_le_bytes());
    expected_log.extend_from_slice(&10_000u32.to_le_bytes());
    expected_log.extend_from_slice(&28u16.to_le_bytes());
    expected_log.push(5); // WARN
    expected_log.push(0); // text
    let body = b"\x05T\0M\0";
    expected_log.extend_from_slice(&5u32.to_le_bytes());
    expected_log.extend_from_slice(body);
    let decoded_log = decode_log_record(&expected_log).expect("decode golden log");
    assert_eq!(
        decoded_log,
        LogRecordPayload {
            device_read_boottime_ns: 99_000,
            log_sec: 1_700_000_000,
            log_nsec: 500_000,
            pid: 100,
            tid: 101,
            lid: 0,
            uid: 10_000,
            hdr_size: 28,
            priority: 5,
            record_flags: 0,
            payload: body.to_vec(),
        },
        "golden LogRecord decode must match PROTOCOL.md §3.6"
    );
    assert_eq!(
        encode_log_record(&decoded_log).expect("re-encode log"),
        expected_log,
        "re-encoded LogRecord must match golden bytes"
    );

    // 7. LogOverflowStats (§3.7)
    let mut expected_stats = Vec::new();
    expected_stats.extend_from_slice(&88_000i64.to_le_bytes());
    expected_stats.push(2); // Unverified
    expected_stats.push(0); // reserved
    expected_stats.extend_from_slice(&0u16.to_le_bytes());
    expected_stats.extend_from_slice(&(-1i64).to_le_bytes());
    expected_stats.extend_from_slice(&(-1i64).to_le_bytes());
    push_str16(&mut expected_stats, "no_counter");
    let decoded_stats = decode_log_overflow_stats(&expected_stats).expect("decode golden stats");
    assert_eq!(
        decoded_stats,
        LogOverflowStatsPayload {
            boottime_ns: 88_000,
            completeness: WireLogCompleteness::Unverified,
            buffers_count: 0,
            pruned_delta: -1,
            pruned_cumulative: -1,
            detail: "no_counter".into(),
        },
        "golden LogOverflowStats decode must match PROTOCOL.md §3.7"
    );
    assert_eq!(
        encode_log_overflow_stats(&decoded_stats).expect("re-encode stats"),
        expected_stats,
        "re-encoded LogOverflowStats must match golden bytes"
    );

    // 8. TouchEvent (§3.8)
    let mut expected_touch = Vec::new();
    expected_touch.extend_from_slice(&77_000i64.to_le_bytes());
    expected_touch.extend_from_slice(&12i64.to_le_bytes());
    expected_touch.extend_from_slice(&345_678i32.to_le_bytes());
    expected_touch.extend_from_slice(&3u16.to_le_bytes());
    expected_touch.extend_from_slice(&0x0036u16.to_le_bytes());
    expected_touch.extend_from_slice(&900i32.to_le_bytes());
    push_str16(&mut expected_touch, "/dev/input/event2");
    push_str16(
        &mut expected_touch,
        "[ 12.345678] EV_ABS ABS_MT_POSITION_Y 00000384",
    );
    let decoded_touch = decode_touch_event(&expected_touch).expect("decode golden touch");
    assert_eq!(
        decoded_touch,
        TouchEventPayload {
            device_read_boottime_ns: 77_000,
            event_sec: 12,
            event_usec: 345_678,
            ev_type: 3,
            ev_code: 0x0036,
            ev_value: 900,
            device_path: "/dev/input/event2".into(),
            raw_line: "[ 12.345678] EV_ABS ABS_MT_POSITION_Y 00000384".into(),
        },
        "golden TouchEvent decode must match PROTOCOL.md §3.8"
    );
    assert_eq!(
        encode_touch_event(&decoded_touch).expect("re-encode touch"),
        expected_touch,
        "re-encoded TouchEvent must match golden bytes"
    );

    // 9. Shutdown (§3.9)
    let mut shutdown_wire = Vec::new();
    shutdown_wire.extend_from_slice(&0i64.to_le_bytes());
    shutdown_wire.extend_from_slice(&1u16.to_le_bytes()); // HostDisconnect
    push_str16(&mut shutdown_wire, "bye");
    let shutdown_val = decode_shutdown(&shutdown_wire).expect("decode golden shutdown");
    assert_eq!(
        shutdown_val,
        ShutdownPayload {
            boottime_ns: 0,
            code: ShutdownCode::HostDisconnect,
            reason: "bye".into(),
        },
        "golden Shutdown decode must match PROTOCOL.md §3.9"
    );
    assert_eq!(
        encode_shutdown(&shutdown_val).expect("re-encode shutdown"),
        shutdown_wire,
        "re-encoded Shutdown must match golden bytes"
    );
}

#[test]
fn rejects_adversarial_wire_scrcpy_and_session_token_inputs() {
    // Unknown stream_id discriminant in header.
    let mut bad_stream = [0u8; HEADER_SIZE];
    bad_stream[0..2].copy_from_slice(b"AR");
    bad_stream[2] = 1;
    bad_stream[3] = 99;
    bad_stream[4] = MessageType::Ping.to_wire();
    assert!(
        matches!(
            WireFrame::read_from(&mut Cursor::new(bad_stream), DEFAULT_MAX_PAYLOAD_LEN),
            Err(WireError::UnknownDiscriminant {
                kind: "stream_id",
                value: 99
            })
        ),
        "unknown stream_id must be rejected"
    );

    // Invalid UTF-8 in Shutdown reason.
    let mut bad_utf8 = Vec::new();
    bad_utf8.extend_from_slice(&0i64.to_le_bytes());
    bad_utf8.extend_from_slice(&0u16.to_le_bytes());
    bad_utf8.extend_from_slice(&2u16.to_le_bytes());
    bad_utf8.extend_from_slice(&[0xFF, 0xFE]);
    assert!(
        matches!(
            decode_shutdown(&bad_utf8),
            Err(WireError::InvalidUtf8 { .. })
        ),
        "invalid UTF-8 in str16 must fail cleanly"
    );

    // Negative scrcpy session dimensions rejected.
    let mut bad_dims = Vec::new();
    write_video_handshake(&mut bad_dims, CODEC_ID_H264).expect("handshake");
    bad_dims.extend_from_slice(&SESSION_META_TAG.to_be_bytes());
    bad_dims.extend_from_slice(&(-1920i32).to_be_bytes());
    bad_dims.extend_from_slice(&1080i32.to_be_bytes());
    let mut vr = VideoTunnelReader::handshake(Cursor::new(bad_dims)).expect("handshake");
    assert!(
        matches!(
            vr.read_next(),
            Err(ScrcpyError::InvalidSessionDimensions {
                width: -1920,
                height: 1080
            })
        ),
        "negative video dimensions must be rejected"
    );

    // Shell metacharacters / path traversal in session token rejected before running adb.
    let client = AdbClient::with_runner(PathBuf::from("/usr/bin/adb"), RefusingRunner);
    for bad_token in ["", "../escape", "abc;rm -rf /", "has space"] {
        assert!(
            matches!(
                client.deploy_session_agents(
                    "emulator-5554",
                    bad_token,
                    std::path::Path::new("/tmp/a.jar"),
                    None
                ),
                Err(AdbError::InvalidSessionToken { .. })
            ),
            "unsafe session token '{bad_token}' must be rejected"
        );
    }
}

/// Runner that refuses every invocation: any call surfaces as an I/O error, so a test
/// expecting `InvalidSessionToken` fails if validation ever lets adb run.
#[derive(Debug)]
struct RefusingRunner;

impl AdbCommandRunner for RefusingRunner {
    fn run(
        &self,
        _program: &std::path::Path,
        _args: &[String],
    ) -> Result<AdbCommandOutput, std::io::Error> {
        Err(std::io::Error::other(
            "runner must not be called for invalid session token",
        ))
    }
}

fn collect_event_count<S: DeviceEventSource>(source: &mut S) -> usize {
    source.drain_all().expect("source drain must succeed").len()
}

#[test]
fn live_adb_session_source_and_mock_source_share_device_event_source_trait() {
    // Live AdbSessionSource over companion + video + audio streams.
    let mut comp = Vec::new();
    let hello = WireMessage::Hello(HelloPayload {
        protocol_version: 1,
        api_level: 37,
        boot_count: 1,
        boot_id_source: BootIdSource::KernelProcBootId,
        is_emulator: true,
        heartbeat_interval_ms: 300,
        boot_id: "boot-poly".into(),
        model: "Pixel".into(),
        manufacturer: "Google".into(),
        agent_version: "0.1.0".into(),
        boot_id_detail: String::new(),
    });
    comp.extend_from_slice(
        &hello
            .to_frame(StreamId::Control, FrameFlags::URGENT, 1)
            .expect("frame")
            .encode()
            .expect("encode"),
    );

    let mut vid = Vec::new();
    write_video_handshake(&mut vid, CODEC_ID_H264).expect("vid hs");
    write_video_session_meta(
        &mut vid,
        VideoSessionMeta {
            header_tag: SESSION_META_TAG,
            width: 1080,
            height: 2400,
        },
    )
    .expect("vid meta");
    write_video_packet(
        &mut vid,
        &VideoPacket {
            kind: VideoPacketKind::Keyframe,
            pts_us: 1000,
            data: vec![0, 0, 0, 1, 0x65],
        },
    )
    .expect("vid pkt");

    let mut aud = Vec::new();
    write_audio_handshake(&mut aud, CODEC_ID_AAC).expect("aud hs");
    write_audio_packet(
        &mut aud,
        &AudioPacket {
            kind: AudioPacketKind::Config,
            pts_us: 0,
            data: vec![0x11, 0x90],
        },
    )
    .expect("aud cfg");
    write_audio_packet(
        &mut aud,
        &AudioPacket {
            kind: AudioPacketKind::Frame,
            pts_us: 1000,
            data: vec![0x21, 0x10],
        },
    )
    .expect("aud frame");

    let mut tick = 0i64;
    let mut live_source = AdbSessionSource::new(
        "emulator-5554",
        Cursor::new(comp),
        Some(VideoTunnelReader::handshake(Cursor::new(vid)).expect("vid reader")),
        Some(AudioTunnelReader::handshake_for_api(Cursor::new(aud), 37).expect("aud reader")),
        SupervisorConfig::new(),
        ClockTimelineConfig::new(),
        move || {
            tick += 1_000;
            tick
        },
    );
    let mut mock_source = MockDeviceSource::from_scenario(FaultScenario::NormalFhd30);

    let live_events = live_source.drain_all().expect("live drain must succeed");
    let data_events = live_events
        .iter()
        .filter(|ev| {
            matches!(
                ev,
                CaptureEvent::Hello { .. }
                    | CaptureEvent::VideoSession { .. }
                    | CaptureEvent::VideoFrame { .. }
                    | CaptureEvent::AudioConfig { .. }
                    | CaptureEvent::AudioFrame { .. }
            )
        })
        .count();
    assert_eq!(
        data_events, 5,
        "live source must yield Hello + VideoSession + VideoFrame + AudioConfig + AudioFrame"
    );
    // Every transport ends without a Shutdown: each end must be reported (M2), never swallowed.
    let ends = live_events
        .iter()
        .filter(|ev| matches!(ev, CaptureEvent::Discontinuity(_)))
        .count();
    assert_eq!(
        ends, 3,
        "companion EOF (ConnectionLost) + video EOF + audio EOF (StreamGap) must be reported"
    );
    assert!(
        collect_event_count(&mut mock_source) > 80,
        "NormalFhd30 mock source must yield full multi-stream capture sequence"
    );
}

#[test]
fn delayed_arrival_burst_integrates_with_arb_core_selection_to_reject_50s_late_packets() {
    let mut src = MockDeviceSource::from_scenario(FaultScenario::DelayedArrivalBurst);
    let events = src.drain_all().expect("drain DelayedArrivalBurst");

    let candidates: Vec<TimestampedRecord<u64>> = events
        .iter()
        .enumerate()
        .filter_map(|(idx, ev)| match ev {
            CaptureEvent::VideoFrame {
                pc_cont_ns,
                boot_id,
                packet,
            } => {
                let mono_ns = packet.pts_monotonic_ns()?;
                let alignment = src.timeline().map_device_monotonic(
                    boot_id,
                    mono_ns,
                    TimestampSemantics::VideoEncoderInputEstimate,
                    *pc_cont_ns,
                );
                Some(TimestampedRecord {
                    payload: u64::try_from(idx).unwrap_or(0),
                    alignment,
                    pc_ingest_cont_ns: *pc_cont_ns,
                    estimated_bytes: packet.data.len(),
                })
            }
            _ => None,
        })
        .collect();

    assert_eq!(
        candidates.len(),
        3,
        "must align all 3 delayed video frames from DelayedArrivalBurst"
    );

    // Save window covering the last 30s of device boottime [130s, 160s] (PC ingest [30s, 60.5s]).
    let window = WindowSpec::RealElapsed {
        boot_id: "boot-delayed-001".into(),
        start_boottime_ns: 130_000_000_000,
        end_boottime_ns: 160_000_000_000,
        ingest_start_cont_ns: 30_000_000_000,
        ingest_end_cont_ns: 60_500_000_000,
    };
    let result = select_records(candidates, &window, &SelectionPolicy::new());

    assert!(
        result.main.is_empty(),
        "50s-late packets with native ~110s PTS must NEVER enter [130s, 160s] main window"
    );
    assert!(
        result.unanchored.is_empty(),
        "cleanly anchored old packets must be OutOfRange, not Unanchored"
    );
    assert_eq!(
        result.excluded_count, 3,
        "all 3 delayed packets must be strictly excluded as OutOfRange"
    );
    assert_eq!(
        result.unanchored_truncated_count, 0,
        "zero unanchored truncations expected"
    );
}

#[test]
fn golden_protocol_md_r1_truncated_flag_utf8_and_writer_queue_overflow() {
    // 1. PROTOCOL.md R1 contract: str16 truncated on a valid UTF-8 code point boundary
    //    carries FrameFlags::TRUNCATED (0x02) on the enclosing 20-byte frame header.
    let truncated_state = WireMessage::StreamState(StreamStatePayload {
        target_stream_id: StreamId::Input,
        state: StreamLifecycleState::Error,
        restart_count: 1,
        boottime_ns: 987_654_321,
        emitted_seq: 42,
        reason: "入力エラー:日本語境界テスト".into(),
    });
    let encoded_frame = truncated_state
        .to_frame(StreamId::Input, FrameFlags::TRUNCATED, 99)
        .expect("build truncated StreamState frame")
        .encode()
        .expect("encode truncated frame");

    let parsed_frame =
        WireFrame::read_from(&mut Cursor::new(&encoded_frame), DEFAULT_MAX_PAYLOAD_LEN)
            .expect("read truncated frame")
            .expect("frame must be present");
    assert!(
        parsed_frame.header.flags.is_truncated(),
        "FLAG_TRUNCATED (0x02) must survive wire round-trip"
    );
    assert_eq!(
        parsed_frame.header.flags.bits(),
        0x02,
        "FLAG_TRUNCATED bit value must equal 0x02"
    );
    assert_eq!(
        WireMessage::decode_frame(&parsed_frame).expect("decode UTF-8 StreamState"),
        truncated_state,
        "UTF-8 codepoint-boundary truncated str16 must round-trip losslessly"
    );

    // 2. PROTOCOL.md R1 contract: Stream 1 queue overflow emits
    //    LogOverflowStats(OverflowDetected, detail="writer_queue_overflow:dropped=N").
    let overflow = LogOverflowStatsPayload {
        boottime_ns: 1_234_567_890,
        completeness: WireLogCompleteness::OverflowDetected,
        buffers_count: 0,
        pruned_delta: 17,
        pruned_cumulative: 17,
        detail: "writer_queue_overflow:dropped=17".into(),
    };
    let overflow_frame = WireMessage::LogOverflowStats(overflow.clone())
        .to_frame(StreamId::Logcat, FrameFlags::URGENT, 100)
        .expect("build overflow frame")
        .encode()
        .expect("encode overflow frame");
    let decoded_overflow_frame =
        WireFrame::read_from(&mut Cursor::new(&overflow_frame), DEFAULT_MAX_PAYLOAD_LEN)
            .expect("read overflow frame")
            .expect("overflow frame must be present");
    assert!(
        decoded_overflow_frame.header.flags.is_urgent(),
        "urgent overflow notification must preserve FLAG_URGENT"
    );
    assert_eq!(
        WireMessage::decode_frame(&decoded_overflow_frame).expect("decode overflow stats"),
        WireMessage::LogOverflowStats(overflow),
        "writer_queue_overflow LogOverflowStats must round-trip bit-for-bit"
    );
}
