//! Full 11-scenario deterministic fault-injection & legacy-compatibility E2E suite for `arbd`.
//!
//! Runs the real `arbd` IPC server (`LocalIpcListener` + `ShmFrameRingWriter` + `RingStore` +
//! `Catalog` + `V2BundleWriter` + `arb-media`) against `MockDeviceSource` and real repo fixtures:
//!  1. Normal capture -> save -> inspect -> SHM scrub (`is_approximate: true -> false`) ->
//!     mid-GOP trim (0 `elst`, exact frame count & burned-in barcodes, source untouched) -> PNG export
//!  2. Delayed arrival burst (50s late old records excluded from main)
//!  3. Clock sync outage & wall rollback (`unanchored` bounded sidecar, carried over on trim)
//!  4. Partial audio stop (`StreamGap` on audio, video/logs continue)
//!  5. Disconnect -> save while disconnected -> reconnect -> save spanning gap
//!  6. Device deep sleep (`DeviceSleep` discontinuity)
//!  7. Device reboot (`boot_id` change hard boundary)
//!  8. Save failure to unwritable dest -> pin preserved -> `save.retarget` succeeds
//!  9. Concurrent edit / `bundle.trash` under active Viewer Lease -> preserved -> restore ->
//!     release lease -> `bundle.gc` purges
//! 10. Disk pressure -> `StoragePressureStop` protects saved & pinned data
//! 11. Legacy Schema 4 real repo fixture opened & inspected cleanly with zero directory mutation

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arb_adb::mock::{
    FaultScenario, FaultScenarioBuilder, FaultStep, MockDeviceSource, NormalizedTouchEvent,
    TouchAction,
};
use arb_adb::scrcpy::{
    AudioPacket, AudioPacketKind, VideoPacket, VideoPacketKind, VideoSessionMeta,
};
use arb_adb::wire::{
    BootIdSource, FrameFlags, HeartbeatPayload, HelloPayload, LogOverflowStatsPayload,
    LogRecordPayload, StreamHealthEntry, StreamId, StreamLifecycleState, TouchEventPayload,
    WireLogCompleteness,
};
use arb_core::state::{RingBudgetPolicy, SaveJobState};
use arb_core::time::{DiscontinuityKind, PcSyncProbe, StreamKind};
use arb_media::Mp4AudioTrack;
use arb_media::codec::{
    DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat, VideoCodecBackend,
};
use arb_media::mp4::{Mp4VideoTrack, inspect_mp4_atoms};
use arb_os::OsError;
use arb_os::fs::DiskSpaceInfo;
use arb_os::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcStream, read_frame_line, write_frame_line};
use arb_os::paths::ArbPaths;
use arb_os::shm::ShmFrameRingReader;
use arb_storage::bundle_io::V2BundleReader;
use arb_storage::ring::{DiskSpaceProvider, PinJournal, PinJournalRecord};
use arbd::capture::{FaultInjection, SharedDiskSpaceProvider};
use arbd::{DaemonConfig, DaemonServer};
use serde_json::{Value, json};
use tempfile::TempDir;

const WIDTH: u32 = 448;
const HEIGHT: u32 = 256;
const FRAME_DURATION_NS: i64 = 33_333_333;
const GOP_SIZE: u32 = 30;
const SEC_NS: i64 = 1_000_000_000;
const MS_NS: i64 = 1_000_000;
const BASE_WALL_NS: i64 = 1_730_000_000 * SEC_NS;

const BARCODE_SYNC_PREFIX: u16 = 0b1010;
const BARCODE_ORIGIN_X: usize = 16;
const BARCODE_ORIGIN_Y: usize = 16;
const BARCODE_CELL_PX: usize = 24;

struct TestIpcClient {
    reader: BufReader<LocalIpcStream>,
    writer: LocalIpcStream,
    next_id: u64,
}

impl TestIpcClient {
    fn connect(endpoint: &Path) -> Self {
        let stream = LocalIpcStream::connect(endpoint).expect("IPC connect must succeed");
        let writer = stream.try_clone().expect("IPC try_clone must succeed");
        Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
        }
    }

    fn call(&mut self, method: &str, params: &Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .expect("serialize RPC request");
        write_frame_line(&mut self.writer, &line, DEFAULT_MAX_IPC_LINE_BYTES)
            .expect("write RPC frame");
        self.writer.drain().expect("drain RPC frame");

        let raw = read_frame_line(&mut self.reader, DEFAULT_MAX_IPC_LINE_BYTES)
            .expect("read RPC frame")
            .expect("expected RPC response line");
        let resp: Value = serde_json::from_str(&raw).expect("parse RPC response");
        assert!(
            resp.get("error").is_none(),
            "RPC `{method}` returned unexpected error: {resp}"
        );
        resp.get("result").cloned().unwrap_or(Value::Null)
    }

    /// Calls `method` expecting a JSON-RPC error; returns `(code, message)`.
    fn call_err(&mut self, method: &str, params: &Value) -> (i64, String) {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .expect("serialize RPC request");
        write_frame_line(&mut self.writer, &line, DEFAULT_MAX_IPC_LINE_BYTES)
            .expect("write RPC frame");
        self.writer.drain().expect("drain RPC frame");
        let raw = read_frame_line(&mut self.reader, DEFAULT_MAX_IPC_LINE_BYTES)
            .expect("read RPC frame")
            .expect("expected RPC response line");
        let resp: Value = serde_json::from_str(&raw).expect("parse RPC response");
        let err = resp
            .get("error")
            .unwrap_or_else(|| panic!("RPC `{method}` unexpectedly succeeded: {resp}"));
        (
            err["code"].as_i64().expect("error code"),
            err["message"].as_str().unwrap_or_default().to_owned(),
        )
    }
}

fn isolated_paths(tmp: &TempDir) -> ArbPaths {
    ArbPaths {
        data_dir: tmp.path().join("data"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: tmp.path().join("run"),
    }
}

fn synthesize_barcode_yuv420p_frame(frame_index: u64) -> DecodedFrame {
    let w = usize::try_from(WIDTH).expect("WIDTH fits usize");
    let h = usize::try_from(HEIGHT).expect("HEIGHT fits usize");
    let y_len = w * h;
    let uv_len = (w / 2) * (h / 2);
    let mut data = vec![128_u8; y_len + uv_len * 2];

    let bar_x = usize::try_from((frame_index * 9) % u64::from(WIDTH.saturating_sub(32)))
        .expect("bar_x fits usize");
    for row in 0..h {
        let base = row * w;
        for col in 0..w {
            data[base + col] = if row >= 80 && col >= bar_x && col < bar_x + 28 {
                210
            } else {
                48
            };
        }
    }

    let low12 = u16::try_from(frame_index & 0x0FFF).expect("12-bit value fits u16");
    let code: u16 = (BARCODE_SYNC_PREFIX << 12) | low12;
    for bit_idx in 0..16_usize {
        let shift = 15 - bit_idx;
        let val = if ((code >> shift) & 1) == 1 { 235 } else { 20 };
        let bx = BARCODE_ORIGIN_X + bit_idx * BARCODE_CELL_PX;
        for dy in 0..BARCODE_CELL_PX {
            let row = (BARCODE_ORIGIN_Y + dy) * w;
            for dx in 0..BARCODE_CELL_PX {
                data[row + bx + dx] = val;
            }
        }
    }

    DecodedFrame {
        frame_index,
        width: WIDTH,
        height: HEIGHT,
        format: PixelFormat::Yuv420p,
        data,
    }
}

fn read_barcode_from_y(y_plane: &[u8], width: usize) -> Option<u64> {
    let mut code: u16 = 0;
    for bit_idx in 0..16_usize {
        let bx = BARCODE_ORIGIN_X + bit_idx * BARCODE_CELL_PX;
        let mut sum: u32 = 0;
        for dy in 6..18 {
            let row = (BARCODE_ORIGIN_Y + dy) * width;
            for dx in 6..18 {
                sum += u32::from(y_plane[row + bx + dx]);
            }
        }
        code = (code << 1) | u16::from(sum / 144 >= 128);
    }
    ((code >> 12) == BARCODE_SYNC_PREFIX).then_some(u64::from(code & 0x0FFF))
}

fn read_barcode_from_rgb24(rgb: &[u8], width: usize) -> Option<u64> {
    let mut code: u16 = 0;
    for bit_idx in 0..16_usize {
        let bx = BARCODE_ORIGIN_X + bit_idx * BARCODE_CELL_PX;
        let mut sum: u32 = 0;
        for dy in 6..18 {
            let row = (BARCODE_ORIGIN_Y + dy) * width * 3;
            for dx in 6..18 {
                let px = row + (bx + dx) * 3;
                let luma =
                    (u32::from(rgb[px]) + u32::from(rgb[px + 1]) + u32::from(rgb[px + 2])) / 3;
                sum += luma;
            }
        }
        code = (code << 1) | u16::from(sum / 144 >= 128);
    }
    ((code >> 12) == BARCODE_SYNC_PREFIX).then_some(u64::from(code & 0x0FFF))
}

fn make_hello(boot_id: &str) -> HelloPayload {
    HelloPayload {
        protocol_version: 1,
        api_level: 37,
        boot_count: 1,
        boot_id_source: BootIdSource::KernelProcBootId,
        is_emulator: true,
        heartbeat_interval_ms: 250,
        boot_id: boot_id.to_owned(),
        model: "arb_e2e_mock".to_owned(),
        manufacturer: "ARB".to_owned(),
        agent_version: "0.1.0".to_owned(),
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

fn make_text_log(
    read_boot_ns: i64,
    wall_sec: u32,
    subsec_ns: u32,
    tag: &str,
    msg: &str,
) -> LogRecordPayload {
    let mut payload = Vec::with_capacity(tag.len() + msg.len() + 3);
    payload.push(4);
    payload.extend_from_slice(tag.as_bytes());
    payload.push(0);
    payload.extend_from_slice(msg.as_bytes());
    payload.push(0);
    LogRecordPayload {
        device_read_boottime_ns: read_boot_ns,
        log_sec: wall_sec,
        log_nsec: subsec_ns,
        pid: 1001,
        tid: 1002,
        lid: 0,
        uid: 10_100,
        hdr_size: 28,
        priority: 4,
        record_flags: 0,
        payload,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallClockMode {
    Steady,
    Rollback30s,
}

fn push_initial_session_steps(
    builder: FaultScenarioBuilder,
    boot_id: &str,
    base_dev: i64,
) -> FaultScenarioBuilder {
    builder
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id),
        })
        .push(FaultStep::SyncProbe {
            probe: PcSyncProbe {
                boot_id: boot_id.to_owned(),
                pc_send_cont_ns: 0,
                pc_send_active_ns: 0,
                dev_boottime_ns: base_dev,
                dev_monotonic_ns: base_dev,
                pc_recv_cont_ns: 2 * MS_NS,
                pc_recv_active_ns: 2 * MS_NS,
            },
            device_bracket_ns: 400,
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 2 * MS_NS,
            payload: HeartbeatPayload {
                boottime_ns: base_dev,
                monotonic_ns: base_dev,
                realtime_ns: BASE_WALL_NS,
                streams: all_streams_running(base_dev, 1),
            },
        })
        .push(FaultStep::VideoSession {
            pc_cont_ns: 3 * MS_NS,
            meta: VideoSessionMeta {
                header_tag: 0x8000_0000,
                width: WIDTH,
                height: HEIGHT,
            },
        })
        .push(FaultStep::AudioPacket {
            pc_cont_ns: 4 * MS_NS,
            packet: AudioPacket {
                kind: AudioPacketKind::Config,
                pts_us: 0,
                data: vec![0x11, 0x90],
            },
        })
        .push(FaultStep::LogOverflowStats {
            pc_cont_ns: 5 * MS_NS,
            payload: LogOverflowStatsPayload {
                boottime_ns: base_dev + 5 * MS_NS,
                completeness: WireLogCompleteness::Verified,
                buffers_count: 8,
                pruned_delta: 0,
                pruned_cumulative: 0,
                detail: "pruned_delta=0".to_owned(),
            },
        })
}

fn push_in_window_log_and_tap(
    builder: FaultScenarioBuilder,
    base_dev: i64,
) -> FaultScenarioBuilder {
    builder
        .push(FaultStep::LogRecord {
            pc_cont_ns: SEC_NS + 10 * MS_NS,
            flags: FrameFlags::NONE,
            payload: make_text_log(
                base_dev + SEC_NS,
                1_730_000_001,
                0,
                "AppTag",
                "in-window-anchored-log",
            ),
        })
        .push(FaultStep::TouchEvent {
            pc_cont_ns: SEC_NS + 50 * MS_NS,
            flags: FrameFlags::NONE,
            payload: TouchEventPayload {
                device_read_boottime_ns: base_dev + SEC_NS + 50 * MS_NS,
                event_sec: 101,
                event_usec: 50_000,
                ev_type: 1,
                ev_code: 0x014a,
                ev_value: 1,
                device_path: "/dev/input/event1".to_owned(),
                raw_line: "[ 101.050000] EV_KEY BTN_TOUCH DOWN".to_owned(),
            },
            normalized: Some(NormalizedTouchEvent {
                device_mono_ns: Some(base_dev + SEC_NS + 50 * MS_NS),
                device_read_boottime_ns: base_dev + SEC_NS + 50 * MS_NS,
                action: TouchAction::Down,
                pointer_id: 0,
                x: 224,
                y: 128,
                viewport: Some((WIDTH, HEIGHT)),
            }),
        })
}

fn push_wall_rollback_steps(
    builder: FaultScenarioBuilder,
    boot_id: &str,
    base_dev: i64,
) -> FaultScenarioBuilder {
    let jump_dev = base_dev + 2 * SEC_NS;
    let jump_pc = 10 * MS_NS + 2 * SEC_NS;
    builder
        .push(FaultStep::SyncProbe {
            probe: PcSyncProbe {
                boot_id: boot_id.to_owned(),
                pc_send_cont_ns: jump_pc,
                pc_send_active_ns: jump_pc,
                dev_boottime_ns: jump_dev,
                dev_monotonic_ns: jump_dev,
                pc_recv_cont_ns: jump_pc + 200 * MS_NS,
                pc_recv_active_ns: jump_pc + 200 * MS_NS,
            },
            device_bracket_ns: 1_000,
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: jump_pc + 200 * MS_NS,
            payload: HeartbeatPayload {
                boottime_ns: jump_dev,
                monotonic_ns: jump_dev,
                realtime_ns: BASE_WALL_NS - 30 * SEC_NS,
                streams: all_streams_running(jump_dev, 50),
            },
        })
        .push(FaultStep::LogRecord {
            pc_cont_ns: jump_pc + 210 * MS_NS,
            flags: FrameFlags::NONE,
            payload: make_text_log(
                jump_dev - 10 * MS_NS,
                1_730_000_000 - 30,
                0,
                "ClockJump",
                "ambiguous-wall-rollback-log",
            ),
        })
}

/// Builds a deterministic barcode-stamped H.264 `MockDeviceSource` with real `OpenH264` frames.
fn build_barcode_mock_source(
    boot_id: &str,
    frame_count: usize,
    wall_mode: WallClockMode,
) -> MockDeviceSource {
    build_barcode_mock_source_audio_lead(boot_id, frame_count, wall_mode, 0)
}

/// Same as [`build_barcode_mock_source`], but every audio packet's `pts_us` is shifted by
/// `audio_lead_in_ns` on the shared device-mono clock (R4 M4 lead-in skew `D`).
fn build_barcode_mock_source_audio_lead(
    boot_id: &str,
    frame_count: usize,
    wall_mode: WallClockMode,
    audio_lead_in_ns: i64,
) -> MockDeviceSource {
    let backend = OpenH264Backend::new();
    let yuv_frames: Vec<DecodedFrame> = (0..frame_count)
        .map(|i| synthesize_barcode_yuv420p_frame(u64::try_from(i).expect("i fits u64")))
        .collect();
    let encoded = backend
        .encode_yuv420p_sequence(
            &yuv_frames,
            GOP_SIZE,
            FRAME_DURATION_NS,
            EncodeParams {
                bitrate_bps: 4_000_000,
                max_fps: 30,
            },
        )
        .expect("OpenH264 sequence encode must succeed");

    let base_dev = 100 * SEC_NS;
    let mut b = push_initial_session_steps(
        FaultScenarioBuilder::new("mock-barcode-e2e"),
        boot_id,
        base_dev,
    );

    for (idx, pkt) in encoded.into_iter().enumerate() {
        let elapsed_ns = i64::try_from(idx).expect("idx fits i64") * FRAME_DURATION_NS;
        let pc_ns = 10 * MS_NS + elapsed_ns;
        let dev_ns = base_dev + elapsed_ns;
        let pts_us = u64::try_from(dev_ns / 1_000).expect("pts_us fits u64");
        let audio_pts_us =
            u64::try_from((dev_ns + audio_lead_in_ns) / 1_000).expect("audio pts_us fits u64");
        b = b.push(FaultStep::VideoFrame {
            pc_cont_ns: pc_ns,
            packet: VideoPacket {
                kind: if pkt.is_keyframe {
                    VideoPacketKind::Keyframe
                } else {
                    VideoPacketKind::DeltaFrame
                },
                pts_us,
                data: pkt.annex_b,
            },
        });
        if idx % 10 == 0 {
            b = b.push(FaultStep::AudioPacket {
                pc_cont_ns: pc_ns + MS_NS,
                packet: AudioPacket {
                    kind: AudioPacketKind::Frame,
                    pts_us: audio_pts_us,
                    data: vec![0x21, 0x10, 0x05, u8::try_from(idx & 0xFF).unwrap_or(0)],
                },
            });
        }
    }

    b = push_in_window_log_and_tap(b, base_dev);

    let total_ns = i64::try_from(frame_count).expect("frame_count fits i64") * FRAME_DURATION_NS;
    let end_dev = base_dev + total_ns + 50 * MS_NS;
    let end_pc = 10 * MS_NS + total_ns + 50 * MS_NS;
    if wall_mode == WallClockMode::Rollback30s {
        b = push_wall_rollback_steps(b, boot_id, base_dev);
    }

    let final_wall = match wall_mode {
        WallClockMode::Rollback30s => BASE_WALL_NS - 30 * SEC_NS + (end_dev - base_dev),
        WallClockMode::Steady => BASE_WALL_NS + (end_dev - base_dev),
    };
    b.push(FaultStep::Heartbeat {
        pc_cont_ns: end_pc,
        payload: HeartbeatPayload {
            boottime_ns: end_dev,
            monotonic_ns: end_dev,
            realtime_ns: final_wall,
            streams: all_streams_running(end_dev, 100),
        },
    })
    .build()
}

fn snapshot_directory(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("read_dir must succeed") {
        let entry = entry.expect("dir entry must succeed");
        let path = entry.path();
        if path.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let bytes = fs::read(&path).expect("read file bytes must succeed");
            map.insert(name, bytes);
        }
    }
    map
}

#[derive(Debug)]
struct AtomicMockDiskSpace {
    available_bytes: Arc<AtomicU64>,
    total_bytes: u64,
}

impl DiskSpaceProvider for AtomicMockDiskSpace {
    fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
        Ok(DiskSpaceInfo {
            available_bytes: self.available_bytes.load(Ordering::Acquire),
            total_bytes: self.total_bytes,
        })
    }
}

fn verify_shm_scrub_and_seek(client: &mut TestIpcClient, bundle_id: &str, cache_dir: &Path) {
    let vp = client.call(
        "media.open_viewport",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    let pts = vp["frame_pts_ns"].as_array().expect("per-frame PTS");
    assert_eq!(
        u64::try_from(pts.len()).expect("length fits u64"),
        vp["frame_count"].as_u64().expect("frame count"),
        "each opened frame needs its presentation timestamp"
    );
    assert_eq!(pts[0], 0, "timestamps are relative to the first frame");
    assert!(
        pts.windows(2).all(|w| w[0].as_i64() < w[1].as_i64()),
        "fixture presentation timestamps are increasing"
    );
    let vp_id = vp["viewport_id"].as_str().expect("viewport_id");
    let shm_path = PathBuf::from(vp["shm_path"].as_str().expect("shm_path"));
    let shm_reader = ShmFrameRingReader::open(&shm_path).expect("open SHM reader");

    let fs_res = client.call("media.filmstrip", &json!({ "viewport_id": vp_id }));
    assert_eq!(
        fs_res["origin"], "generated",
        "first filmstrip must generate"
    );
    assert_eq!(fs_res["count"], 3, "filmstrip must contain 3 keyframes");
    let cache_path = PathBuf::from(fs_res["cache_path"].as_str().expect("cache_path"));
    assert!(
        cache_path.starts_with(cache_dir),
        "filmstrip cache must live under external cache_dir"
    );

    let scrub_res = client.call(
        "media.scrub",
        &json!({ "viewport_id": vp_id, "frame_index": 17 }),
    );
    assert_eq!(
        scrub_res["is_approximate"], true,
        "cold mid-GOP scrub must return approximate preview"
    );
    let mut pixels = Vec::new();
    let shm_approx = shm_reader
        .read_latest(&mut pixels)
        .expect("read SHM approx")
        .expect("SHM slot must be populated after scrub");
    assert!(
        shm_approx.is_approximate,
        "SHM slot header must report is_approximate == true during scrub"
    );

    let seek_res = client.call(
        "media.seek_frame",
        &json!({ "viewport_id": vp_id, "frame_index": 17 }),
    );
    assert_eq!(
        seek_res["is_approximate"], false,
        "seek_frame must be exact"
    );
    let shm_exact = shm_reader
        .read_latest(&mut pixels)
        .expect("read SHM exact")
        .expect("SHM slot must be populated after seek_frame");
    assert!(
        !shm_exact.is_approximate,
        "SHM slot must clear is_approximate"
    );
    assert_eq!(
        shm_exact.frame_index, 17,
        "SHM exact frame_index must be 17"
    );
    assert_eq!(shm_exact.width, WIDTH, "SHM width must match video");
    assert_eq!(shm_exact.height, HEIGHT, "SHM height must match video");

    let step_fwd = client.call(
        "media.seek_frame",
        &json!({ "viewport_id": vp_id, "frame_index": 18 }),
    );
    let step_back = client.call(
        "media.seek_frame",
        &json!({ "viewport_id": vp_id, "frame_index": 16 }),
    );
    assert!(
        step_fwd["cache_hits"].as_u64().unwrap_or(0) >= 1
            && step_back["cache_hits"].as_u64().unwrap_or(0) >= 2,
        "forward/backward frame step must hit GOP-LRU cache"
    );

    let closed = client.call("media.close_viewport", &json!({ "viewport_id": vp_id }));
    assert_eq!(closed["closed"], true, "close_viewport must return true");
}

fn verify_mid_gop_trim_and_png(client: &mut TestIpcClient, bundle_id: &str, tmp: &TempDir) {
    let trim_res = client.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": bundle_id,
            "in_frame_index": 17,
            "out_frame_index": 49,
        }),
    );
    assert_eq!(
        trim_res["trim_report"]["output_frame_count"], 33,
        "trimmed output must contain 33 frames [17..=49]"
    );
    assert_eq!(
        trim_res["trim_report"]["edts_box_count"], 0,
        "must have 0 edts"
    );
    assert_eq!(
        trim_res["trim_report"]["elst_box_count"], 0,
        "must have 0 elst"
    );

    let trimmed_dir = PathBuf::from(trim_res["bundle_path"].as_str().expect("trimmed path"));
    let trimmed_mp4 = trimmed_dir.join("video.mp4");
    let atoms = inspect_mp4_atoms(&trimmed_mp4).expect("inspect trimmed MP4 atoms");
    assert_eq!(atoms.edts_count, 0, "trimmed MP4 must have 0 edts boxes");
    assert_eq!(atoms.elst_count, 0, "trimmed MP4 must have 0 elst boxes");
    assert_eq!(atoms.avc1_count, 1, "trimmed MP4 must have single avc1");
    assert!(atoms.faststart, "trimmed MP4 must be faststart");

    let trimmed_track = Mp4VideoTrack::open(&trimmed_mp4).expect("open trimmed MP4");
    assert_eq!(
        trimmed_track.frame_count(),
        33,
        "trimmed track must have 33 frames"
    );
    let decoder = OpenH264Backend::new();
    let f0 = decoder
        .decode_frame(&trimmed_track, 0, PixelFormat::Yuv420p)
        .expect("decode trimmed frame 0");
    let f13 = decoder
        .decode_frame(&trimmed_track, 13, PixelFormat::Yuv420p)
        .expect("decode trimmed splice IDR frame 13");
    let f32 = decoder
        .decode_frame(&trimmed_track, 32, PixelFormat::Yuv420p)
        .expect("decode trimmed last frame 32");
    let width_usize = usize::try_from(WIDTH).expect("WIDTH fits usize");
    assert_eq!(
        read_barcode_from_y(f0.y_plane().expect("y"), width_usize),
        Some(17),
        "trimmed frame 0 must carry exact barcode 17 (0 pre-start frames)"
    );
    assert_eq!(
        read_barcode_from_y(f13.y_plane().expect("y"), width_usize),
        Some(30),
        "trimmed splice IDR frame 13 must carry exact barcode 30"
    );
    assert_eq!(
        read_barcode_from_y(f32.y_plane().expect("y"), width_usize),
        Some(49),
        "trimmed last frame 32 must carry exact barcode 49"
    );

    let png_path = tmp.path().join("frame43.png");
    let png_res = client.call(
        "export.png",
        &json!({
            "bundle_id_or_path": bundle_id,
            "frame_index": 43,
            "output_path": png_path,
        }),
    );
    assert_eq!(
        png_res["report"]["frame_index"], 43,
        "PNG report must be frame 43"
    );
    let png_file = File::open(&png_path).expect("open exported PNG");
    let mut reader = png::Decoder::new(BufReader::new(png_file))
        .read_info()
        .expect("decode PNG header");
    let mut img_buf = vec![0_u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut img_buf).expect("decode PNG pixels");
    assert_eq!(
        read_barcode_from_rgb24(
            &img_buf[..info.buffer_size()],
            usize::try_from(info.width).expect("png width fits usize")
        ),
        Some(43),
        "exported PNG must carry exact visual barcode 43"
    );
    assert!(
        reader
            .info()
            .uncompressed_latin1_text
            .iter()
            .any(|c| c.keyword == "arb:frame_index" && c.text == "43"),
        "PNG must include arb:frame_index=43 tEXt chunk"
    );
}

#[test]
fn scenario_01_normal_capture_save_inspect_shm_scrub_mid_gop_trim_png() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");

    let mut source = build_barcode_mock_source("boot-s01", 90, WallClockMode::Steady);
    let ingested = server
        .ingest_device_source(&mut source)
        .expect("ingest barcode source");
    assert!(
        ingested > 90,
        "must ingest all 90 frames plus anchors/logs/taps"
    );

    let mut client = TestIpcClient::connect(server.endpoint());
    let hello = client.call("system.hello", &json!({}));
    assert_eq!(hello["protocol_version"], 1, "protocol_version must be 1");
    assert_eq!(hello["schema_version"], 2, "schema_version must be 2");

    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();
    let bundle_path = PathBuf::from(saved["bundle_path"].as_str().expect("bundle_path"));

    let v2_reader = V2BundleReader::open(&bundle_path).expect("open saved v2 bundle");
    v2_reader
        .verify_integrity()
        .expect("saved v2 bundle SHA-256 integrity must verify");
    let source_snapshot_before = snapshot_directory(&bundle_path);

    let summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert_eq!(summary["frame_count"], 90, "must index all 90 frames");
    assert_eq!(
        summary["keyframe_count"], 3,
        "must have 3 keyframes (0, 30, 60)"
    );
    assert_eq!(summary["log_main_count"], 1, "must contain 1 main log");
    assert_eq!(summary["tap_main_count"], 1, "must contain 1 main tap");

    verify_shm_scrub_and_seek(&mut client, &bundle_id, &paths.cache_dir);
    verify_mid_gop_trim_and_png(&mut client, &bundle_id, &tmp);

    assert_eq!(
        snapshot_directory(&bundle_path),
        source_snapshot_before,
        "immutable source WORM bundle must never be mutated"
    );

    server.shutdown();
}

#[test]
fn scenario_02_delayed_arrival_burst_excluded_from_main() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = MockDeviceSource::from_scenario(FaultScenario::DelayedArrivalBurst);
    server
        .ingest_device_source(&mut source)
        .expect("ingest DelayedArrivalBurst");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");

    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id");
    let summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert_eq!(
        summary["frame_count"], 0,
        "all 50s-delayed frames must be excluded from the 30s main window"
    );
    assert_eq!(
        summary["unanchored_summary"]["included_count"], 0,
        "anchored delayed out-of-range frames must not leak into unanchored either"
    );

    server.shutdown();
}

#[test]
fn scenario_03_clock_sync_outage_and_wall_rollback_unanchored_carried_on_trim() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = build_barcode_mock_source("boot-s03", 90, WallClockMode::Rollback30s);
    server
        .ingest_device_source(&mut source)
        .expect("ingest wall-rollback source");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();

    let discs = client.call(
        "timeline.discontinuities",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    let disc_list = discs["discontinuities"]
        .as_array()
        .expect("discontinuities array");
    assert!(
        disc_list.iter().any(|d| d["kind"] == "device_wall_jump"),
        "must record DeviceWallJump discontinuity"
    );
    assert!(
        disc_list.iter().any(|d| d["kind"] == "clock_sync_stale"),
        "must record ClockSyncStale discontinuity"
    );

    // Wall-rollback ambiguous log MUST be quarantined in `unanchored`, NEVER mixed into `main`.
    let main_logs = client.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": bundle_id, "bucket": "main" }),
    );
    let unanchored_logs = client.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": bundle_id, "bucket": "unanchored" }),
    );
    let unanchored_arr = unanchored_logs["logs"].as_array().expect("unanchored logs");
    assert!(
        unanchored_arr
            .iter()
            .any(|l| l["message"] == "ambiguous-wall-rollback-log"),
        "wall-rollback log must be quarantined into unanchored bucket"
    );
    assert!(
        !main_logs["logs"]
            .as_array()
            .expect("main logs")
            .iter()
            .any(|l| l["message"] == "ambiguous-wall-rollback-log"),
        "wall-rollback log must never enter main timeline"
    );

    // Trim mid-GOP [15..=45] -> unanchored sidecar MUST be carried over untouched!
    let trimmed = client.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": bundle_id,
            "in_frame_index": 15,
            "out_frame_index": 45,
        }),
    );
    assert!(
        trimmed["unanchored_log_count"].as_u64().unwrap_or(0) >= 1,
        "trim must carry forward existing unanchored sidecar records"
    );
    let trimmed_id = trimmed["bundle_id"].as_str().expect("trimmed_id");
    let trimmed_unanchored = client.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": trimmed_id, "bucket": "unanchored" }),
    );
    assert!(
        trimmed_unanchored["logs"]
            .as_array()
            .expect("trimmed unanchored")
            .iter()
            .any(|l| l["message"] == "ambiguous-wall-rollback-log"),
        "carried-over wall-rollback log must remain intact in trimmed bundle's unanchored sidecar"
    );

    server.shutdown();
}

#[test]
fn scenario_04_partial_audio_stop_video_and_logs_continue() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = MockDeviceSource::from_scenario(FaultScenario::PartialAudioStop);
    server
        .ingest_device_source(&mut source)
        .expect("ingest PartialAudioStop");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id");

    let summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert_eq!(
        summary["streams"]["audio"]["status"], "partial",
        "audio stream must be honestly marked Partial after audio stop"
    );
    assert!(
        summary["frame_count"].as_u64().unwrap_or(0) > 0,
        "video frames must continue after audio stop"
    );
    assert!(
        summary["log_main_count"].as_u64().unwrap_or(0) > 0,
        "logs must continue after audio stop"
    );

    let discs = client.call(
        "timeline.discontinuities",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert!(
        discs["discontinuities"]
            .as_array()
            .expect("discs")
            .iter()
            .any(|d| d["kind"] == "stream_gap" && d["stream"] == "audio"),
        "must record StreamGap discontinuity specifically on audio stream"
    );

    server.shutdown();
}

fn build_disconnect_phase_a(boot_id: &str, base_dev: i64) -> MockDeviceSource {
    FaultScenarioBuilder::new("mock-s05")
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 5 * MS_NS,
            payload: HeartbeatPayload {
                boottime_ns: base_dev,
                monotonic_ns: base_dev,
                realtime_ns: BASE_WALL_NS,
                streams: all_streams_running(base_dev, 1),
            },
        })
        .push(FaultStep::LogRecord {
            pc_cont_ns: SEC_NS,
            flags: FrameFlags::NONE,
            payload: make_text_log(
                base_dev + SEC_NS,
                1_730_000_001,
                0,
                "Net",
                "before-disconnect",
            ),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 2 * SEC_NS,
            payload: HeartbeatPayload {
                boottime_ns: base_dev + 2 * SEC_NS,
                monotonic_ns: base_dev + 2 * SEC_NS,
                realtime_ns: BASE_WALL_NS + 2 * SEC_NS,
                streams: all_streams_running(base_dev + 2 * SEC_NS, 2),
            },
        })
        .push(FaultStep::Disconnect {
            pc_cont_ns: 3 * SEC_NS,
            reason: "usb_cable_unplugged".to_owned(),
        })
        .build()
}

fn build_reconnect_phase_b(boot_id: &str, base_dev: i64) -> MockDeviceSource {
    FaultScenarioBuilder::new("mock-s05")
        .push(FaultStep::Hello {
            pc_cont_ns: 6 * SEC_NS,
            payload: make_hello(boot_id),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 6 * SEC_NS + 10 * MS_NS,
            payload: HeartbeatPayload {
                boottime_ns: base_dev + 6 * SEC_NS,
                monotonic_ns: base_dev + 6 * SEC_NS,
                realtime_ns: BASE_WALL_NS + 6 * SEC_NS,
                streams: all_streams_running(base_dev + 6 * SEC_NS, 3),
            },
        })
        .push(FaultStep::LogRecord {
            pc_cont_ns: 7 * SEC_NS,
            flags: FrameFlags::NONE,
            payload: make_text_log(
                base_dev + 7 * SEC_NS,
                1_730_000_007,
                0,
                "Net",
                "after-reconnect",
            ),
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: 8 * SEC_NS,
            payload: HeartbeatPayload {
                boottime_ns: base_dev + 8 * SEC_NS,
                monotonic_ns: base_dev + 8 * SEC_NS,
                realtime_ns: BASE_WALL_NS + 8 * SEC_NS,
                streams: all_streams_running(base_dev + 8 * SEC_NS, 4),
            },
        })
        .build()
}

#[test]
fn scenario_05_disconnect_save_while_disconnected_reconnect_save_spanning_gap() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let boot_id = "boot-s05-reconnect";
    let base_dev = 300 * SEC_NS;

    let mut phase_a = build_disconnect_phase_a(boot_id, base_dev);
    server
        .ingest_device_source(&mut phase_a)
        .expect("ingest phase A");

    let mut client = TestIpcClient::connect(server.endpoint());
    let status = client.call("system.status", &json!({}));
    assert_eq!(
        status["capture"]["connected"], false,
        "must be disconnected"
    );

    let save_disc = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(
        save_disc["job"]["state"], "completed",
        "save while disconnected must still succeed from Zone-1 ring"
    );
    let b1 = save_disc["bundle_id"].as_str().expect("b1");
    let b1_discs = client.call(
        "timeline.discontinuities",
        &json!({ "bundle_id_or_path": b1 }),
    );
    assert!(
        b1_discs["discontinuities"]
            .as_array()
            .expect("b1 discs")
            .iter()
            .any(|d| d["kind"] == "connection_lost"),
        "bundle saved while disconnected must record ConnectionLost"
    );

    let mut phase_b = build_reconnect_phase_b(boot_id, base_dev);
    server
        .ingest_device_source(&mut phase_b)
        .expect("ingest phase B");

    let save_span = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(
        save_span["job"]["state"], "completed",
        "gap-spanning save must succeed"
    );
    let b2 = save_span["bundle_id"].as_str().expect("b2");
    let b2_logs = client.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": b2, "bucket": "main" }),
    );
    let msgs: Vec<&str> = b2_logs["logs"]
        .as_array()
        .expect("b2 logs")
        .iter()
        .filter_map(|l| l["message"].as_str())
        .collect();
    assert!(
        msgs.contains(&"before-disconnect") && msgs.contains(&"after-reconnect"),
        "gap-spanning bundle must include logs from both sides of the reconnect: {msgs:?}"
    );
    let b2_discs = client.call(
        "timeline.discontinuities",
        &json!({ "bundle_id_or_path": b2 }),
    );
    let b2_disc_arr = b2_discs["discontinuities"].as_array().expect("b2 discs");
    assert!(
        b2_disc_arr.iter().any(|d| d["kind"] == "connection_lost"),
        "gap-spanning bundle must preserve ConnectionLost"
    );
    assert!(
        !b2_disc_arr.iter().any(|d| d["kind"] == "device_reboot"),
        "same boot_id reconnect must NOT be misclassified as DeviceReboot"
    );

    server.shutdown();
}

#[test]
fn scenario_06_device_deep_sleep_discontinuity() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = MockDeviceSource::from_scenario(FaultScenario::DeviceDeepSleep);
    server
        .ingest_device_source(&mut source)
        .expect("ingest DeviceDeepSleep");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 60 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id");

    let discs = client.call(
        "timeline.discontinuities",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    let sleep_ev = discs["discontinuities"]
        .as_array()
        .expect("discs")
        .iter()
        .find(|d| d["kind"] == "device_sleep")
        .expect("must record DeviceSleep discontinuity");
    assert_eq!(
        sleep_ev["delta_ns"].as_i64(),
        Some(15 * SEC_NS),
        "DeviceSleep delta_ns must match 15s deep sleep"
    );

    server.shutdown();
}

#[test]
fn scenario_07_device_reboot_boot_id_hard_boundary() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = MockDeviceSource::from_scenario(FaultScenario::DeviceReboot);
    server
        .ingest_device_source(&mut source)
        .expect("ingest DeviceReboot");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id");

    let discs = client.call(
        "timeline.discontinuities",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert!(
        discs["discontinuities"]
            .as_array()
            .expect("discs")
            .iter()
            .any(|d| d["kind"] == "device_reboot"),
        "must record DeviceReboot hard boundary on boot_id change"
    );

    // Every log in the main timeline must strictly belong to the active post-reboot boot_id.
    let main_logs = client.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": bundle_id, "bucket": "main" }),
    );
    for log in main_logs["logs"].as_array().expect("main logs") {
        assert_ne!(
            log["boot_id"].as_str(),
            Some("boot-A"),
            "pre-reboot boot-A records must never pollute post-reboot main timeline"
        );
    }

    server.shutdown();
}

#[test]
fn scenario_08_save_failure_unwritable_dest_pin_preserved_retarget_succeeds() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = build_barcode_mock_source("boot-s08", 30, WallClockMode::Steady);
    server.ingest_device_source(&mut source).expect("ingest");

    // Create a regular file to act as an impossible parent directory (fails cross-OS even as root).
    let blocked_dest = tmp.path().join("unwritable-file-as-dir");
    fs::write(&blocked_dest, b"not-a-directory").expect("create blocking file");

    let mut client = TestIpcClient::connect(server.endpoint());
    let failed = client.call(
        "save.trigger",
        &json!({ "duration_sec": 30, "dest_dir": blocked_dest }),
    );
    assert_eq!(
        failed["job"]["state"], "failed_retryable",
        "save to unwritable dest must transition to FailedRetryable"
    );
    let job_id = failed["job"]["job_id"].as_str().expect("job_id").to_owned();
    let pinned_chunks = failed["job"]["pinned_chunk_ids"]
        .as_array()
        .expect("pinned_chunk_ids");
    assert!(
        !pinned_chunks.is_empty(),
        "FailedRetryable save job must keep ring chunk pins alive"
    );

    // Retarget to a valid destination directory -> must succeed from preserved pins!
    let good_dest = tmp.path().join("recovered-bundles");
    let recovered = client.call(
        "save.retarget",
        &json!({ "job_id": job_id, "dest_dir": good_dest }),
    );
    assert_eq!(
        recovered["job"]["state"], "completed",
        "save.retarget to valid directory must complete"
    );
    assert!(
        recovered["job"]["pinned_chunk_ids"]
            .as_array()
            .expect("pins")
            .is_empty(),
        "completed job must release ring chunk pins"
    );
    let recovered_path = PathBuf::from(recovered["bundle_path"].as_str().expect("bundle_path"));
    assert!(
        recovered_path.starts_with(&good_dest),
        "retargeted bundle must land in new destination"
    );
    V2BundleReader::open(&recovered_path)
        .expect("retargeted bundle must open")
        .verify_integrity()
        .expect("retargeted bundle SHA-256 must verify");

    server.shutdown();
}

#[test]
fn scenario_09_concurrent_edit_trash_under_viewer_lease_restore_release_gc() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = build_barcode_mock_source("boot-s09", 30, WallClockMode::Steady);
    server.ingest_device_source(&mut source).expect("ingest");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();
    let bundle_path = PathBuf::from(saved["bundle_path"].as_str().expect("bundle_path"));

    // Acquire Viewer Lease & persist a non-destructive edit session.
    let opened = client.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": bundle_id,
            "holder_kind": "ide_viewer",
            "holder_pid": std::process::id(),
        }),
    );
    let lease_id = opened["lease_id"].as_str().expect("lease_id").to_owned();
    let _ = client.call("bundle.lease_renew", &json!({ "lease_id": lease_id }));

    let _ = client.call(
        "edit.update_session",
        &json!({
            "session_id": "edit-s09",
            "bundle_id": bundle_id,
            "in_frame_index": 5,
            "out_frame_index": 25,
            "playhead_frame_index": 12,
            "av_offset_override_ms": -40,
        }),
    );
    let fetched_edit = client.call("edit.get_session", &json!({ "session_id": "edit-s09" }));
    assert_eq!(
        fetched_edit["session"]["in_frame_index"], 5,
        "edit session must persist outside immutable bundle"
    );

    // Trash while Viewer Lease is active -> GC MUST preserve bundle on disk!
    let trashed = client.call("bundle.trash", &json!({ "bundle_id": bundle_id }));
    assert_eq!(
        trashed["state"], "trashed",
        "bundle must enter trashed state"
    );
    let gc_blocked = client.call("bundle.gc", &json!({ "now_utc": "2099-01-01T00:00:00Z" }));
    assert!(
        gc_blocked["skipped_leased_bundle_ids"]
            .as_array()
            .expect("skipped_leased")
            .iter()
            .any(|id| id == &bundle_id),
        "GC must skip trashed bundle while Viewer Lease is active"
    );
    assert!(
        bundle_path.is_dir(),
        "leased trashed bundle must remain on disk"
    );

    // Restore -> Active!
    let restored = client.call("bundle.restore", &json!({ "bundle_id": bundle_id }));
    assert_eq!(
        restored["state"], "active",
        "bundle.restore must return to active"
    );

    // Trash again -> release lease -> GC physically purges!
    let _ = client.call("bundle.trash", &json!({ "bundle_id": bundle_id }));
    let closed = client.call("bundle.close", &json!({ "lease_id": lease_id }));
    assert_eq!(closed["released"], true, "bundle.close must release lease");

    client.call("config.update", &json!({ "gc_grace_secs": 0 }));
    let gc_purged = client.call("bundle.gc", &json!({ "now_utc": "2099-01-01T00:00:00Z" }));
    assert!(
        gc_purged["purged_bundle_ids"]
            .as_array()
            .expect("purged")
            .iter()
            .any(|id| id == &bundle_id),
        "GC must physically purge unleased trashed managed bundle"
    );
    assert!(
        !bundle_path.exists(),
        "purged bundle directory must be removed from disk"
    );

    server.shutdown();
}

#[test]
fn scenario_10_disk_pressure_storage_pressure_stop_protects_saved_and_pinned() {
    let tmp = TempDir::new().expect("tempdir");
    let free_bytes = Arc::new(AtomicU64::new(10 * 1024 * 1024 * 1024)); // 10 GiB initially
    let disk_provider = SharedDiskSpaceProvider::new(Arc::new(AtomicMockDiskSpace {
        available_bytes: Arc::clone(&free_bytes),
        total_bytes: 50 * 1024 * 1024 * 1024,
    }));
    let policy = RingBudgetPolicy {
        max_ring_bytes: 64 * 1024 * 1024,
        max_ring_duration_ns: 600_000_000_000,
        min_free_disk_bytes: 3 * 1024 * 1024 * 1024, // 3 GiB floor
    };
    let server = DaemonServer::start(
        DaemonConfig::new(isolated_paths(&tmp))
            .with_ring_policy(policy)
            .with_disk_provider(disk_provider),
    )
    .expect("start arbd");

    // 1. Ingest data while disk is healthy -> commit one saved bundle AND create one pinned FailedRetryable job.
    let mut source1 = build_barcode_mock_source("boot-s10", 30, WallClockMode::Steady);
    server
        .ingest_device_source(&mut source1)
        .expect("ingest initial");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    let saved_path = PathBuf::from(saved["bundle_path"].as_str().expect("saved path"));

    let blocked_file = tmp.path().join("blocked-dest");
    fs::write(&blocked_file, b"x").expect("write blocking file");
    let pinned_failed = client.call(
        "save.trigger",
        &json!({ "duration_sec": 30, "dest_dir": blocked_file }),
    );
    assert_eq!(
        pinned_failed["job"]["state"], "failed_retryable",
        "pinned job must stay in FailedRetryable"
    );
    let pinned_job_id = pinned_failed["job"]["job_id"]
        .as_str()
        .expect("pinned job_id")
        .to_owned();

    // 2. Simulate host free space dropping to 1 GiB (< 3 GiB floor) and ingest more events.
    free_bytes.store(1024 * 1024 * 1024, Ordering::Release);
    let mut source2 = MockDeviceSource::from_scenario(FaultScenario::NormalFhd30);
    server
        .ingest_device_source(&mut source2)
        .expect("storage pressure must halt cleanly rather than panic/error");

    let status = client.call("system.status", &json!({}));
    assert_eq!(
        status["capture"]["storage_pressure_stopped"], true,
        "status must report storage_pressure_stopped == true"
    );
    assert!(
        server
            .live_discontinuities()
            .iter()
            .any(|d| d.kind == DiscontinuityKind::StoragePressureStop),
        "live discontinuities must record StoragePressureStop"
    );

    // 3. Verify saved bundle AND pinned job data remain 100% protected and recoverable!
    V2BundleReader::open(&saved_path)
        .expect("saved bundle must survive StoragePressureStop")
        .verify_integrity()
        .expect("saved bundle integrity must remain intact");

    let retarget_dir = tmp.path().join("after-pressure-dest");
    let retargeted = client.call(
        "save.retarget",
        &json!({ "job_id": pinned_job_id, "dest_dir": retarget_dir }),
    );
    assert_eq!(
        retargeted["job"]["state"], "completed",
        "pinned chunks must survive StoragePressureStop and allow save.retarget to succeed"
    );

    server.shutdown();
}

#[test]
fn scenario_12_pressure_stop_merged_into_later_bundle_and_startup_pin_leak_recovered() {
    let tmp = TempDir::new().expect("tempdir");
    let free_bytes = Arc::new(AtomicU64::new(10 * 1024 * 1024 * 1024));
    let make_config = |free: &Arc<AtomicU64>| {
        DaemonConfig::new(isolated_paths(&tmp))
            .with_ring_policy(RingBudgetPolicy {
                max_ring_bytes: 64 * 1024 * 1024,
                max_ring_duration_ns: 600_000_000_000,
                min_free_disk_bytes: 3 * 1024 * 1024 * 1024,
            })
            .with_disk_provider(SharedDiskSpaceProvider::new(Arc::new(
                AtomicMockDiskSpace {
                    available_bytes: Arc::clone(free),
                    total_bytes: 50 * 1024 * 1024 * 1024,
                },
            )))
    };
    let server = DaemonServer::start(make_config(&free_bytes)).expect("start arbd");

    let mut source1 = build_barcode_mock_source("boot-s12", 30, WallClockMode::Steady);
    server
        .ingest_device_source(&mut source1)
        .expect("ingest initial");

    // S18: pressure stop must be drained from the ring and merged into later saved bundles.
    free_bytes.store(1024 * 1024 * 1024, Ordering::Release);
    let mut source2 = build_barcode_mock_source("boot-s12", 2, WallClockMode::Steady);
    server
        .ingest_device_source(&mut source2)
        .expect("pressure must halt cleanly");
    let pressure_events = server
        .live_discontinuities()
        .iter()
        .filter(|d| d.kind == DiscontinuityKind::StoragePressureStop)
        .count();
    assert_eq!(
        pressure_events, 1,
        "exactly one StoragePressureStop per pressure episode"
    );

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();
    let discs = client.call(
        "timeline.discontinuities",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert!(
        discs["discontinuities"]
            .as_array()
            .expect("discontinuities")
            .iter()
            .any(|d| d["kind"] == "storage_pressure_stop"),
        "saved bundle must carry StoragePressureStop discontinuity: {discs}"
    );

    // S13: a pin journal left behind for an unknown job must be released on restart.
    drop(client);
    server.shutdown();
    let journal = PinJournal::open(tmp.path().join("data")).expect("open journal");
    journal
        .pin(&PinJournalRecord {
            job_id: "job-leaked-s13".to_owned(),
            destination_path: tmp.path().join("nowhere"),
            pinned_chunk_ids: vec!["chunk-does-not-matter".to_owned()],
            created_at_utc: "2026-01-01T00:00:00Z".to_owned(),
        })
        .expect("write leaked journal");
    let server = DaemonServer::start(make_config(&free_bytes)).expect("restart arbd");
    let mut client = TestIpcClient::connect(server.endpoint());
    let status = client.call("system.status", &json!({}));
    assert_eq!(
        status["recovery"]["released_pin_journals"], 1,
        "startup must reconcile (release) the leaked pin journal: {status}"
    );
    assert!(
        journal.load("job-leaked-s13").is_err(),
        "leaked pin journal must be removed"
    );
    server.shutdown();
}

#[test]
fn scenario_11_legacy_schema4_real_repo_fixture_readonly_zero_mutation() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir = manifest_dir
        .join("..")
        .join("..")
        .join("..")
        .join("fixtures")
        .join("legacy-schema4")
        .join("replay-trim-parity")
        .join("replay-20240101-000000-manual-feedface");
    assert!(
        fixture_dir.is_dir(),
        "real repo Legacy Schema 4 fixture must exist at {}",
        fixture_dir.display()
    );

    let before_snapshot = snapshot_directory(&fixture_dir);

    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");
    let mut client = TestIpcClient::connect(server.endpoint());

    let opened = client.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": fixture_dir.to_string_lossy(),
            "holder_kind": "cli",
            "holder_pid": std::process::id(),
        }),
    );
    assert_eq!(
        opened["is_legacy"], true,
        "fixture must be detected as legacy"
    );
    assert_eq!(
        opened["format"], "legacy_schema4",
        "fixture must be detected as legacy_schema4"
    );
    assert!(
        opened["summary"]["frame_count"].as_u64().unwrap_or(0) > 0,
        "legacy fixture must expose indexed frames"
    );
    let lease_id = opened["lease_id"].as_str().expect("lease_id");
    let bundle_id = opened["bundle_id"].as_str().expect("bundle_id");

    let logs = client.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert!(
        !logs["logs"].as_array().expect("logs").is_empty(),
        "legacy fixture must expose logcat rows"
    );
    let _taps = client.call(
        "timeline.query_taps",
        &json!({ "bundle_id_or_path": bundle_id }),
    );

    // Even if someone tries to trash + GC a legacy bundle, GC MUST NEVER delete it!
    let _ = client.call("bundle.close", &json!({ "lease_id": lease_id }));
    let _ = client.call("bundle.trash", &json!({ "bundle_id": bundle_id }));
    let gc = client.call("bundle.gc", &json!({ "now_utc": "2099-01-01T00:00:00Z" }));
    assert!(
        gc["skipped_unmanaged_or_legacy_bundle_ids"]
            .as_array()
            .expect("skipped legacy")
            .iter()
            .any(|id| id == bundle_id),
        "GC must NEVER delete legacy read-only directories"
    );

    let after_snapshot = snapshot_directory(&fixture_dir);
    assert_eq!(
        after_snapshot, before_snapshot,
        "opening and inspecting legacy Schema 4 fixture must produce ZERO directory mutations"
    );

    server.shutdown();
}

#[test]
fn assert_stream_kind_matches_expected_audio_variant() {
    assert_eq!(
        serde_json::to_value(StreamKind::Audio).expect("serialize"),
        json!("audio"),
        "StreamKind::Audio JSON tag must be snake_case"
    );
    assert_eq!(
        serde_json::to_value(SaveJobState::FailedRetryable).expect("serialize"),
        json!("failed_retryable"),
        "SaveJobState::FailedRetryable JSON tag must be snake_case"
    );
    assert_eq!(
        serde_json::to_value(DiscontinuityKind::StoragePressureStop).expect("serialize"),
        json!("storage_pressure_stop"),
        "DiscontinuityKind::StoragePressureStop JSON tag must be snake_case"
    );
}

#[test]
fn r3_m1_parent_sync_failure_publishes_and_catalogs_with_warning() {
    let tmp = TempDir::new().expect("tempdir");
    let faults = Arc::new(FaultInjection::default());
    let server = DaemonServer::start(
        DaemonConfig::new(isolated_paths(&tmp)).with_fault_injection(Arc::clone(&faults)),
    )
    .expect("start arbd");

    let mut source = build_barcode_mock_source("boot-r3-m1a", 60, WallClockMode::Steady);
    server
        .ingest_device_source(&mut source)
        .expect("ingest source");

    let mut client = TestIpcClient::connect(server.endpoint());

    // 1. Inject `CommittedParentSyncFailed` on `save.trigger` (R3 M1).
    faults.fail_parent_sync_once.store(true, Ordering::Release);
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(
        saved["job"]["state"], "completed",
        "CommittedParentSyncFailed must still complete the save job: {saved}"
    );
    let bundle_id = saved["bundle_id"]
        .as_str()
        .expect("committed bundle_id must be returned")
        .to_owned();
    let bundle_path = PathBuf::from(saved["bundle_path"].as_str().expect("bundle_path"));
    assert!(
        saved["warning"]
            .as_str()
            .is_some_and(|w| w.contains("parent directory")),
        "warning must surface the parent-sync failure: {saved}"
    );
    V2BundleReader::open(&bundle_path)
        .expect("published bundle must open")
        .verify_integrity()
        .expect("published bundle integrity must verify");
    let bundles = client.call("bundle.list", &json!({}));
    assert!(
        bundles["bundles"]
            .as_array()
            .expect("bundles")
            .iter()
            .any(|b| b["bundle_id"] == bundle_id),
        "published bundle must be cataloged and never orphaned"
    );

    // 2. Inject `CommittedParentSyncFailed` on `export.trim` (R3 S8a).
    faults.fail_parent_sync_once.store(true, Ordering::Release);
    let trimmed = client.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": bundle_id,
            "in_frame_index": 5,
            "out_frame_index": 35,
        }),
    );
    let trim_id = trimmed["bundle_id"]
        .as_str()
        .expect("trimmed bundle_id")
        .to_owned();
    assert!(
        trimmed["warning"]
            .as_str()
            .is_some_and(|w| w.contains("parent directory")),
        "trim must surface parent-sync warning without failing: {trimmed}"
    );
    let bundles_after = client.call("bundle.list", &json!({}));
    assert!(
        bundles_after["bundles"]
            .as_array()
            .expect("bundles")
            .iter()
            .any(|b| b["bundle_id"] == trim_id),
        "trimmed bundle must be cataloged"
    );

    server.shutdown();
}

#[test]
fn r3_m1_crash_and_catalog_error_after_register_retry_idempotently() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let faults = Arc::new(FaultInjection::default());
    let server = DaemonServer::start(
        DaemonConfig::new(paths.clone()).with_fault_injection(Arc::clone(&faults)),
    )
    .expect("start arbd");

    let mut source = build_barcode_mock_source("boot-r3-m1b", 30, WallClockMode::Steady);
    server
        .ingest_device_source(&mut source)
        .expect("ingest source");

    let mut client = TestIpcClient::connect(server.endpoint());

    // (a) Catalog `complete_job` failure after `register_bundle`: job must move to
    // `FailedRetryable` (NEVER stranded in `Materializing`), and `save.retry` must succeed
    // idempotently via `register_bundle_idempotent`.
    faults.fail_complete_job_once.store(true, Ordering::Release);
    let failed = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(
        failed["job"]["state"], "failed_retryable",
        "post-commit catalog failure must transition to FailedRetryable, not leave Materializing: {failed}"
    );
    let job_a = failed["job"]["job_id"].as_str().expect("job_a").to_owned();
    let retried_a = client.call("save.retry", &json!({ "job_id": job_a }));
    assert_eq!(
        retried_a["job"]["state"], "completed",
        "idempotent retry after post-register failure must complete: {retried_a}"
    );
    V2BundleReader::open(Path::new(
        retried_a["bundle_path"].as_str().expect("bundle_path"),
    ))
    .expect("open retried bundle")
    .verify_integrity()
    .expect("verify retried bundle");

    // (b) Simulated crash between `register_bundle` and `complete_job`: daemon restarts,
    // startup recovery moves the interrupted `Materializing` job to `FailedRetryable`, and
    // `save.retry` completes idempotently without PK conflict.
    faults
        .crash_after_register_once
        .store(true, Ordering::Release);
    let (_code, msg) = client.call_err("save.trigger", &json!({ "duration_sec": 30 }));
    assert!(
        msg.contains("injected crash"),
        "expected injected crash error, got `{msg}`"
    );
    drop(client);
    server.shutdown();

    // The rename already committed the bundle. Its retry must not depend on decoding any ring file.
    for entry in fs::read_dir(paths.data_dir.join("ring")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "chunk") {
            fs::remove_file(path).unwrap();
        }
    }
    let restarted = DaemonServer::start(DaemonConfig::new(paths)).expect("restart arbd");
    let mut client2 = TestIpcClient::connect(restarted.endpoint());
    let status = client2.call("system.status", &json!({}));
    assert_eq!(
        status["recovery"]["recovered_jobs"], 1,
        "startup recovery must transition interrupted Materializing job to FailedRetryable: {status}"
    );
    let jobs = client2.call("save.list_jobs", &json!({}));
    let recovered_job_id = jobs["jobs"]
        .as_array()
        .expect("jobs array")
        .iter()
        .find(|j| j["state"] == "failed_retryable")
        .and_then(|j| j["job_id"].as_str())
        .expect("recovered failed_retryable job")
        .to_owned();
    let retried_b = client2.call("save.retry", &json!({ "job_id": recovered_job_id }));
    assert_eq!(
        retried_b["job"]["state"], "completed",
        "idempotent retry after crash-between-register-and-complete must succeed: {retried_b}"
    );
    assert!(
        retried_b["job"]["pinned_chunk_ids"]
            .as_array()
            .expect("pins")
            .is_empty(),
        "completed retry must release its pins"
    );
    restarted.shutdown();
}

fn build_chunked_heartbeat_source(
    boot_id: &str,
    base_dev: i64,
    sec_range: std::ops::Range<i64>,
) -> MockDeviceSource {
    let mut b = FaultScenarioBuilder::new("mock-r3-m2");
    if sec_range.start == 0 {
        b = b.push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id),
        });
    }
    for sec in sec_range {
        let pc = sec * SEC_NS;
        let dev = base_dev + pc;
        b = b
            .push(FaultStep::LogRecord {
                pc_cont_ns: pc,
                flags: FrameFlags::NONE,
                payload: make_text_log(
                    dev,
                    u32::try_from(1_730_000_000 + sec).expect("wall sec"),
                    0,
                    "Ring",
                    &format!("log-sec-{sec}"),
                ),
            })
            .push(FaultStep::Heartbeat {
                pc_cont_ns: pc + 900 * MS_NS,
                payload: HeartbeatPayload {
                    boottime_ns: dev + 900 * MS_NS,
                    monotonic_ns: dev + 900 * MS_NS,
                    realtime_ns: BASE_WALL_NS + pc + 900 * MS_NS,
                    streams: all_streams_running(
                        dev + 900 * MS_NS,
                        u32::try_from(sec + 1).expect("seq"),
                    ),
                },
            });
    }
    b.build()
}

#[test]
fn r3_m2_selective_window_pinning_leaves_older_chunks_evictable() {
    let tmp = TempDir::new().expect("tempdir");
    // Cap ring duration to 60s so pushing new chunks past 60s forces eviction of unpinned chunks.
    let policy = RingBudgetPolicy {
        max_ring_bytes: 64 * 1024 * 1024,
        max_ring_duration_ns: 60_000_000_000,
        min_free_disk_bytes: 0,
    };
    let server =
        DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp)).with_ring_policy(policy))
            .expect("start arbd");

    let base_dev = 1_000 * SEC_NS;
    let mut source = build_chunked_heartbeat_source("boot-r3-m2", base_dev, 0..50);
    server
        .ingest_device_source(&mut source)
        .expect("ingest 50s");

    let mut client = TestIpcClient::connect(server.endpoint());
    let total_chunks = client.call("system.status", &json!({}))["ring_chunk_count"]
        .as_u64()
        .expect("ring_chunk_count");
    assert!(
        total_chunks >= 45,
        "expected >= 45 ring chunks across 50s, got {total_chunks}"
    );

    // Trigger a 10s save to an unwritable file so the job stays `FailedRetryable` holding pins.
    let blocked_file = tmp.path().join("blocked-m2");
    fs::write(&blocked_file, b"file").expect("create blocking file");
    let failed = client.call(
        "save.trigger",
        &json!({ "duration_sec": 10, "dest_dir": blocked_file }),
    );
    assert_eq!(
        failed["job"]["state"], "failed_retryable",
        "blocked save must be FailedRetryable"
    );
    let job_id = failed["job"]["job_id"].as_str().expect("job_id").to_owned();
    let pinned_count = u64::try_from(
        failed["job"]["pinned_chunk_ids"]
            .as_array()
            .expect("pinned_chunk_ids")
            .len(),
    )
    .expect("fits u64");
    assert!(
        pinned_count > 0 && pinned_count < total_chunks / 2,
        "selective pinning must pin only the ~10s window (+margin/anchor), not the whole ring: pinned={pinned_count}, total={total_chunks}"
    );

    // Push 35 more seconds (t = 50..85s): total span exceeds the 60s ring budget, so older
    // unpinned chunks MUST be evicted while the FailedRetryable job's pins remain protected!
    let mut source2 = build_chunked_heartbeat_source("boot-r3-m2", base_dev, 50..85);
    server
        .ingest_device_source(&mut source2)
        .expect("ingest more chunks while pins held");

    let during = client.call("system.status", &json!({}));
    assert_eq!(
        during["capture"]["storage_pressure_stopped"], false,
        "unpinned chunks must evict cleanly without triggering RingFullPinned"
    );
    assert!(
        during["ring_chunk_count"].as_u64().expect("chunks") < total_chunks + 35,
        "older unpinned chunks must have been evicted"
    );

    let recovered = client.call(
        "save.retarget",
        &json!({ "job_id": job_id, "dest_dir": tmp.path().join("m2-retargeted") }),
    );
    assert_eq!(
        recovered["job"]["state"], "completed",
        "pinned window chunks must remain intact for save.retarget: {recovered}"
    );
    server.shutdown();
}

#[test]
fn r3_m7_client_disconnect_closes_viewports_cap_and_startup_shm_sweep() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    fs::create_dir_all(paths.shm_dir()).expect("create shm dir");
    let stale_shm = paths.shm_dir().join("vp-crashed-previous.shm");
    fs::write(&stale_shm, b"stale shm bytes").expect("write stale shm");

    let server = DaemonServer::start(DaemonConfig::new(paths)).expect("start arbd");
    assert!(
        !stale_shm.exists(),
        "DaemonServer::start must sweep stale *.shm files after acquiring the instance lock"
    );

    let mut source = build_barcode_mock_source("boot-r3-m7", 30, WallClockMode::Steady);
    server.ingest_device_source(&mut source).expect("ingest");

    let mut admin = TestIpcClient::connect(server.endpoint());
    let saved = admin.call("save.trigger", &json!({ "duration_sec": 30 }));
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();

    // 1. Open a viewport on a separate client and drop the socket without `media.close_viewport`.
    let leaked_shm = {
        let mut transient = TestIpcClient::connect(server.endpoint());
        let vp = transient.call(
            "media.open_viewport",
            &json!({ "bundle_id_or_path": bundle_id }),
        );
        let shm_path = PathBuf::from(vp["shm_path"].as_str().expect("shm_path"));
        assert!(
            shm_path.is_file(),
            "SHM file must exist while client is connected"
        );
        assert_eq!(
            admin.call("system.status", &json!({}))["active_viewports"],
            1,
            "active_viewports must be 1"
        );
        shm_path
    };

    // Wait briefly for the daemon thread to observe EOF and close the client's viewports.
    let mut cleaned = false;
    for _ in 0..40 {
        let count = admin.call("system.status", &json!({}))["active_viewports"]
            .as_u64()
            .unwrap_or(99);
        if count == 0 && !leaked_shm.exists() {
            cleaned = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        cleaned,
        "disconnecting client must automatically close its SHM viewport and remove {}",
        leaked_shm.display()
    );

    // 2. Enforce global `MAX_VIEWPORTS` cap (returning Conflict on the 9th viewport).
    let mut open_ids = Vec::new();
    for _ in 0..arbd::protocol::MAX_VIEWPORTS {
        let vp = admin.call(
            "media.open_viewport",
            &json!({ "bundle_id_or_path": bundle_id }),
        );
        open_ids.push(vp["viewport_id"].as_str().expect("vp_id").to_owned());
    }
    let (code, msg) = admin.call_err(
        "media.open_viewport",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert_eq!(
        code,
        i64::from(arbd::error::RPC_ERR_CONFLICT),
        "exceeding MAX_VIEWPORTS must return Conflict ({msg})"
    );
    for id in open_ids {
        let _ = admin.call("media.close_viewport", &json!({ "viewport_id": id }));
    }

    server.shutdown();
}

#[test]
fn r3_m8_s7_daemon_rejects_relative_and_forbidden_paths_and_bundle_png_mutation() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");

    let mut source = build_barcode_mock_source("boot-r3-m8", 30, WallClockMode::Steady);
    server.ingest_device_source(&mut source).expect("ingest");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();
    let bundle_path = PathBuf::from(saved["bundle_path"].as_str().expect("bundle_path"));
    let bundle_before = snapshot_directory(&bundle_path);

    let invalid = i64::from(arbd::error::RPC_ERR_INVALID_PARAMS);

    // M8: relative paths must be rejected by arbd with InvalidParams.
    let (code, _) = client.call_err(
        "save.trigger",
        &json!({ "duration_sec": 30, "dest_dir": "rel/dir" }),
    );
    assert_eq!(code, invalid, "relative save dest_dir must be rejected");

    let (code, _) = client.call_err(
        "export.png",
        &json!({ "bundle_id_or_path": bundle_id, "frame_index": 0, "output_path": "rel.png" }),
    );
    assert_eq!(
        code, invalid,
        "relative export.png output_path must be rejected"
    );

    let (code, _) = client.call_err(
        "timeline.summary",
        &json!({ "bundle_id_or_path": "./relative-bundle" }),
    );
    assert_eq!(code, invalid, "relative bundle_id_or_path must be rejected");

    let (code, _) = client.call_err(
        "capture.start",
        &json!({ "companion_jar": "relative/arb-companion.dex.jar" }),
    );
    assert_eq!(code, invalid, "relative companion_jar must be rejected");

    // C3: destinations inside ring/, cache_dir, or an existing bundle must be rejected.
    let (code, _) = client.call_err(
        "save.trigger",
        &json!({ "duration_sec": 30, "dest_dir": paths.data_dir.join("ring").join("bad") }),
    );
    assert_eq!(code, invalid, "dest_dir inside ring/ must be rejected");

    let (code, _) = client.call_err(
        "save.trigger",
        &json!({ "duration_sec": 30, "dest_dir": paths.cache_dir.join("bad") }),
    );
    assert_eq!(code, invalid, "dest_dir inside cache_dir must be rejected");

    let (code, _) = client.call_err(
        "save.trigger",
        &json!({ "duration_sec": 30, "dest_dir": bundle_path.join("nested") }),
    );
    assert_eq!(
        code, invalid,
        "dest_dir inside existing bundle must be rejected"
    );

    // S7: export.png into a WORM bundle (including nested subdir) must be rejected BEFORE
    // `create_dir_all`, leaving the bundle completely untouched.
    let nested_png = bundle_path.join("forbidden-subdir").join("frame0.png");
    let (code, _) = client.call_err(
        "export.png",
        &json!({
            "bundle_id_or_path": bundle_id,
            "frame_index": 0,
            "output_path": nested_png,
        }),
    );
    assert_eq!(
        code, invalid,
        "export.png inside WORM bundle must be rejected"
    );
    assert!(
        !bundle_path.join("forbidden-subdir").exists(),
        "validation must run before create_dir_all so no directory is created inside the bundle"
    );
    assert_eq!(
        snapshot_directory(&bundle_path),
        bundle_before,
        "WORM bundle must remain 100% unmodified"
    );

    server.shutdown();
}

#[test]
fn r3_s1_s2_disconnected_save_tail_flag_and_unanchored_video_recorded() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let boot_id = "boot-r3-s1s2";
    let base_dev = 500 * SEC_NS;
    let mut source = FaultScenarioBuilder::new("mock-s1s2")
        // Send video frames BEFORE any heartbeat so they have NoClockAnchor (unanchored, R3 S2).
        .push(FaultStep::Hello {
            pc_cont_ns: 0,
            payload: make_hello(boot_id),
        })
        .push(FaultStep::VideoFrame {
            pc_cont_ns: 100 * MS_NS,
            packet: VideoPacket {
                kind: VideoPacketKind::Keyframe,
                pts_us: 100_000,
                data: vec![0, 0, 0, 1, 0x65, 0x01],
            },
        })
        .push(FaultStep::VideoFrame {
            pc_cont_ns: 133 * MS_NS,
            packet: VideoPacket {
                kind: VideoPacketKind::DeltaFrame,
                pts_us: 133_000,
                data: vec![0, 0, 0, 1, 0x41, 0x02],
            },
        })
        .push(FaultStep::Heartbeat {
            pc_cont_ns: SEC_NS,
            payload: HeartbeatPayload {
                boottime_ns: base_dev + SEC_NS,
                monotonic_ns: base_dev + SEC_NS,
                realtime_ns: BASE_WALL_NS + SEC_NS,
                streams: all_streams_running(base_dev + SEC_NS, 1),
            },
        })
        .push(FaultStep::Disconnect {
            pc_cont_ns: 2 * SEC_NS,
            reason: "cable_pulled".to_owned(),
        })
        .build();
    server.ingest_device_source(&mut source).expect("ingest");

    let mut client = TestIpcClient::connect(server.endpoint());
    // R3 S1: capture is active (`begin_session`) but disconnected (`Disconnect` ingested) ->
    // `save.trigger` must mark `tail_may_be_incomplete == true`.
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");
    assert_eq!(
        saved["tail_may_be_incomplete"], true,
        "active-but-disconnected capture must report tail_may_be_incomplete=true (S1): {saved}"
    );
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();

    // R3 S2: the 2 unanchored video frames must not disappear silently — both a video
    // `StreamGap` discontinuity and `unanchored_summary.truncated_count` must record them.
    let discs = client.call(
        "timeline.discontinuities",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert!(
        discs["discontinuities"]
            .as_array()
            .expect("discs")
            .iter()
            .any(|d| {
                d["kind"] == "stream_gap"
                    && d["stream"] == "video"
                    && d["detail"]
                        .as_str()
                        .is_some_and(|s| s.contains("2 unanchored video frames"))
            }),
        "excluded unanchored video frames must be recorded as a video StreamGap (S2): {discs}"
    );
    assert!(
        saved["unanchored_summary"]["truncated_count"]
            .as_u64()
            .unwrap_or(0)
            >= 2,
        "unanchored_summary.truncated_count must reflect excluded unanchored video frames: {saved}"
    );

    server.shutdown();
}

fn build_disconnected_biglog_source(boot_id: &str) -> MockDeviceSource {
    let backend = OpenH264Backend::new();
    let yuv: Vec<DecodedFrame> = (0..60_u64).map(synthesize_barcode_yuv420p_frame).collect();
    let encoded = backend
        .encode_yuv420p_sequence(
            &yuv,
            GOP_SIZE,
            FRAME_DURATION_NS,
            EncodeParams {
                bitrate_bps: 4_000_000,
                max_fps: 30,
            },
        )
        .expect("encode");
    let base_dev = 200 * SEC_NS;
    let mut b =
        push_initial_session_steps(FaultScenarioBuilder::new("mock-r3-s6"), boot_id, base_dev);
    for (idx, pkt) in encoded.into_iter().enumerate() {
        let elapsed = i64::try_from(idx).expect("idx") * FRAME_DURATION_NS;
        let pc = 10 * MS_NS + elapsed;
        let dev = base_dev + elapsed;
        b = b.push(FaultStep::VideoFrame {
            pc_cont_ns: pc,
            packet: VideoPacket {
                kind: if pkt.is_keyframe {
                    VideoPacketKind::Keyframe
                } else {
                    VideoPacketKind::DeltaFrame
                },
                pts_us: u64::try_from(dev / 1_000).expect("pts"),
                data: pkt.annex_b,
            },
        });
    }
    let big_msg = "X".repeat(60_000);
    for i in 0..85_i64 {
        b = b.push(FaultStep::LogRecord {
            pc_cont_ns: 20 * MS_NS + i * MS_NS,
            flags: FrameFlags::NONE,
            payload: make_text_log(
                base_dev + 20 * MS_NS + i * MS_NS,
                1_730_000_000,
                u32::try_from(i * 1_000).expect("subsec"),
                "BigLog",
                &big_msg,
            ),
        });
    }
    let end_pc = 3 * SEC_NS;
    b.push(FaultStep::Heartbeat {
        pc_cont_ns: end_pc,
        payload: HeartbeatPayload {
            boottime_ns: base_dev + end_pc,
            monotonic_ns: base_dev + end_pc,
            realtime_ns: BASE_WALL_NS + end_pc,
            streams: all_streams_running(base_dev + end_pc, 99),
        },
    })
    .push(FaultStep::Disconnect {
        pc_cont_ns: end_pc + 10 * MS_NS,
        reason: "tail_disconnect".to_owned(),
    })
    .build()
}

#[test]
fn r3_s6_s8c_s10_s11_leases_trim_tail_oversized_response_and_shutdown_gate() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = build_disconnected_biglog_source("boot-r3-s6");
    server.ingest_device_source(&mut source).expect("ingest");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(
        saved["tail_may_be_incomplete"], true,
        "source must have incomplete tail"
    );
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();

    // S6: `bundle.open` must reject omitted `holder_pid` and dead `holder_pid`.
    let invalid = i64::from(arbd::error::RPC_ERR_INVALID_PARAMS);
    let (code, _) = client.call_err("bundle.open", &json!({ "bundle_id_or_path": bundle_id }));
    assert_eq!(
        code, invalid,
        "bundle.open without holder_pid must be rejected"
    );
    let (code, _) = client.call_err(
        "bundle.open",
        &json!({ "bundle_id_or_path": bundle_id, "holder_pid": u32::MAX - 7 }),
    );
    assert_eq!(
        code, invalid,
        "bundle.open with dead holder_pid must be rejected"
    );

    // S8c: trim including the last frame (59) inherits `tail_may_be_incomplete == true`;
    // trim ending earlier (40) has `tail_may_be_incomplete == false`.
    let trim_tail = client.call(
        "export.trim",
        &json!({ "bundle_id_or_path": bundle_id, "in_frame_index": 10, "out_frame_index": 59 }),
    );
    let tail_summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": trim_tail["bundle_id"].as_str().expect("id") }),
    );
    assert_eq!(
        tail_summary["streams"]["video"]["tail_may_be_incomplete"], true,
        "trim including source's incomplete tail must propagate tail_may_be_incomplete=true (S8c)"
    );

    let trim_mid = client.call(
        "export.trim",
        &json!({ "bundle_id_or_path": bundle_id, "in_frame_index": 10, "out_frame_index": 40 }),
    );
    let mid_summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": trim_mid["bundle_id"].as_str().expect("id") }),
    );
    assert_eq!(
        mid_summary["streams"]["video"]["tail_may_be_incomplete"], false,
        "trim excluding source's last frame must have tail_may_be_incomplete=false (S8c)"
    );

    // S10: response exceeding 4 MiB must return structured RPC_ERR_RESPONSE_TOO_LARGE (-32010)
    // WITHOUT dropping the IPC connection!
    let (code, msg) = client.call_err(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert_eq!(
        code,
        i64::from(arbd::error::RPC_ERR_RESPONSE_TOO_LARGE),
        "oversized response must return -32010 instead of dropping socket ({msg})"
    );
    let small = client.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": bundle_id, "limit": 2 }),
    );
    assert_eq!(
        small["logs"].as_array().expect("logs").len(),
        2,
        "connection must remain healthy after oversized response error"
    );

    let cursor = small["logs"][1]["seq"].as_u64().expect("sequence cursor");
    let next = client.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": bundle_id, "limit": 2, "after_seq": cursor }),
    );
    let rows = next["logs"].as_array().expect("next page");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row["seq"].as_u64().expect("seq") > cursor)
    );

    // S11: once shutdown is triggered, idle client connections exit promptly so shutdown completes.
    let shutdown_handle = server.shutdown_handle();
    let mut idle_client = TestIpcClient::connect(server.endpoint());
    let _ = idle_client.call("system.hello", &json!({}));
    shutdown_handle.trigger();
    let started = std::time::Instant::now();
    server.shutdown();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "shutdown must complete promptly even with idle client connections open"
    );
}

#[test]
fn w11_audio_m4a_muxed_trimmed_and_sha256_verified() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    let mut source = build_barcode_mock_source("boot-w11", 90, WallClockMode::Steady);
    server
        .ingest_device_source(&mut source)
        .expect("ingest audio source");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();
    let bundle_path = PathBuf::from(saved["bundle_path"].as_str().expect("bundle_path"));

    // W11: `audio.m4a` must be written, listed in `manifest.files` and SHA-256 verified.
    let m4a_path = bundle_path.join("audio.m4a");
    assert!(m4a_path.is_file(), "saved bundle must contain audio.m4a");
    let reader = V2BundleReader::open(&bundle_path).expect("open saved bundle");
    assert!(
        reader
            .manifest()
            .files
            .iter()
            .any(|f| f.path == "audio.m4a"),
        "audio.m4a must be listed in manifest.files"
    );
    reader
        .verify_integrity()
        .expect("saved audio.m4a SHA-256 integrity must verify");

    let summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert_eq!(
        summary["streams"]["audio"]["status"], "present",
        "gap-free AAC-LC capture must report audio.status == present"
    );
    assert_eq!(
        summary["audio_path"].as_str(),
        m4a_path.to_str(),
        "timeline.summary must expose the audio.m4a path"
    );

    let track = Mp4AudioTrack::open(&m4a_path).expect("open audio.m4a track");
    assert_eq!(track.sample_rate_hz(), 48_000, "ASC must be 48 kHz");
    assert_eq!(track.channels(), 2, "ASC must be stereo");
    assert_eq!(
        track.packet_count(),
        9,
        "all 9 audio packets (idx % 10) in the window must be muxed"
    );

    // Trimming the bundle must produce a trimmed `audio.m4a` covering the same range.
    let trimmed = client.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": bundle_id,
            "in_frame_index": 17,
            "out_frame_index": 49,
        }),
    );
    let trimmed_dir = PathBuf::from(trimmed["bundle_path"].as_str().expect("trimmed path"));
    let trimmed_m4a = trimmed_dir.join("audio.m4a");
    assert!(
        trimmed_m4a.is_file(),
        "trimmed bundle must contain audio.m4a"
    );
    let trimmed_reader = V2BundleReader::open(&trimmed_dir).expect("open trimmed bundle");
    trimmed_reader
        .verify_integrity()
        .expect("trimmed audio.m4a SHA-256 integrity must verify");

    let trimmed_track = Mp4AudioTrack::open(&trimmed_m4a).expect("open trimmed audio.m4a");
    assert!(
        trimmed_track.packet_count() >= 1 && trimmed_track.packet_count() < track.packet_count(),
        "trimmed audio must hold a strict in-window subset of packets"
    );
    assert_eq!(
        trimmed_track
            .packet(0)
            .expect("first trimmed packet")
            .pts_ns,
        0,
        "trimmed audio.m4a must be rebased to pts 0"
    );

    let trimmed_summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": trimmed["bundle_id"].as_str().expect("trimmed id") }),
    );
    assert_eq!(
        trimmed_summary["streams"]["audio"]["status"], "present",
        "trimmed audio.m4a must keep audio.status == present"
    );

    server.shutdown();
}

/// R4 M4 + S10 end-to-end: audio packets report a `+500 ms` lead-in skew `D` on the shared
/// device clock (audio tunnel opens late). The saved bundle must persist the measured
/// `av_offset_ms == 500` / exact `audio_lead_in_offset_ns`, and `export.trim` must cut
/// `audio.m4a` in translated `[video - D]` coordinates so the trimmed file covers exactly
/// the audio segment aligned to the trimmed video — re-deriving the new bundle's own
/// lead-in instead of blindly copying `D`.
#[test]
fn r4_m4_audio_lead_in_offset_saved_and_trim_aligned() {
    const LEAD_NS: i64 = 500 * MS_NS;

    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");

    // 90 video frames (~3 s) with sparse audio packets every 10th frame, each stamped
    // `video_pts + 500 ms` on the shared device-mono clock → D = +500 ms.
    let mut source =
        build_barcode_mock_source_audio_lead("boot-m4", 90, WallClockMode::Steady, LEAD_NS);
    server
        .ingest_device_source(&mut source)
        .expect("ingest lead-in audio source");

    let mut client = TestIpcClient::connect(server.endpoint());
    let saved = client.call("save.trigger", &json!({ "duration_sec": 30 }));
    assert_eq!(saved["job"]["state"], "completed", "save must complete");
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();

    let summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert_eq!(
        summary["streams"]["audio"]["status"], "present",
        "audio stream must be present"
    );
    assert_eq!(
        summary["streams"]["audio"]["av_offset_ms"], 500,
        "saved bundle must persist the measured +500 ms lead-in as av_offset_ms"
    );
    assert_eq!(
        summary["streams"]["audio"]["audio_lead_in_offset_ns"], LEAD_NS,
        "saved bundle must persist the exact lead-in skew in nanoseconds"
    );
    assert_eq!(
        summary["streams"]["audio"]["av_offset_status"], "uncalibrated",
        "lead-in is measured at save, but A/V lag stays honestly uncalibrated"
    );

    // Trim to video frames 17..=49. Audio-axis window = [v_start - D, v_end - D] on the
    // source `audio.m4a` (pts0 = first audio packet). Mock audio packets sit at source
    // pts ≈ {0, 333, 667, 1000, 1333, ...} ms, so exactly the 4 packets intersecting
    // [~67 ms, ~1167 ms) — i.e. pts {0, 333, 667, 1000} ms — must be selected.
    let trimmed = client.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": bundle_id,
            "in_frame_index": 17,
            "out_frame_index": 49,
        }),
    );
    let trimmed_dir = PathBuf::from(trimmed["bundle_path"].as_str().expect("trimmed path"));
    let trimmed_m4a = trimmed_dir.join("audio.m4a");
    assert!(
        trimmed_m4a.is_file(),
        "trimmed bundle must contain the translated audio segment"
    );
    let trimmed_track = Mp4AudioTrack::open(&trimmed_m4a).expect("open trimmed audio.m4a");
    assert_eq!(
        trimmed_track.packet_count(),
        4,
        "trimmed audio.m4a must cover exactly the aligned segment \
         (source pts {{0, 333, 667, 1000}} ms intersect [67 ms, 1167 ms))"
    );
    assert_eq!(
        trimmed_track
            .packet(0)
            .expect("first trimmed packet")
            .pts_ns,
        0,
        "trimmed audio.m4a must be rebased to pts 0"
    );

    // The first selected packet straddles the new video origin (its source pts 0 + D
    // lands ~67 ms before the trimmed video start), so the trimmed bundle's honest
    // lead-in is a small negative skew — NOT the source's +500 ms.
    let trimmed_summary = client.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": trimmed["bundle_id"].as_str().expect("trimmed id") }),
    );
    let lead_in_ns = trimmed_summary["streams"]["audio"]["audio_lead_in_offset_ns"]
        .as_i64()
        .expect("trimmed audio_lead_in_offset_ns");
    assert!(
        (-70_000_000..-60_000_000).contains(&lead_in_ns),
        "trimmed lead-in must be re-derived on the shared clock (~-67 ms), got {lead_in_ns}"
    );
    assert_eq!(
        trimmed_summary["streams"]["audio"]["av_offset_ms"].as_i64(),
        Some(lead_in_ns / 1_000_000),
        "av_offset_ms must stay the millisecond truncation of the exact lead-in"
    );
    assert_eq!(
        trimmed_summary["streams"]["audio"]["status"], "present",
        "trimmed audio.m4a must keep audio.status == present"
    );

    server.shutdown();
}
