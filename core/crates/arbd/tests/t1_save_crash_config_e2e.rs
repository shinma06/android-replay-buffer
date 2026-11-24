//! End-to-end integration tests for T-1:
//! - Persisted `config.json` (`config.get` / `config.update`, restart reload, validation)
//! - One-click `save.screenshot` (mock provider, atomic write, PNG validation, overwrite guard)
//! - Log-only WORM v2 bundles (`save.logs` & `save.trigger` `mode = "logs_only"`, `logs.jsonl` +
//!   `unanchored.jsonl` + SHA-256 + lease/trash/restore/gc)
//! - Live `am_crash` debounced background auto-save + `STORAGE_PRESSURE_STOP` suppression
//! - `timeline.crashes` and `timeline.screens` RPCs over committed WORM bundles

use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use arb_adb::mock::{CaptureEvent, DeviceEventSource};
use arb_adb::wire::{
    BootIdSource, FrameFlags, HeartbeatPayload, HelloPayload, LogRecordPayload, StreamHealthEntry,
    StreamId, StreamLifecycleState,
};
use arb_core::analytics::{AM_CRASH_EVENT_TAG, PNG_SIGNATURE};
use arb_core::state::RingBudgetPolicy;
use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
use arb_os::OsError;
use arb_os::fs::DiskSpaceInfo;
use arb_os::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcStream, read_frame_line, write_frame_line};
use arb_os::paths::ArbPaths;
use arb_storage::bundle_io::{LOGS_JSONL_FILE, UNANCHORED_JSONL_FILE, V2BundleReader};
use arb_storage::ring::DiskSpaceProvider;
use arb_storage::sha256_hex;
use arbd::capture::SharedDiskSpaceProvider;
use arbd::error::DaemonError;
use arbd::screenshot::{ScreenshotProvider, SharedScreenshotProvider};
use arbd::{DaemonConfig, DaemonServer};
use serde_json::{Value, json};
use tempfile::TempDir;

const SEC_NS: i64 = 1_000_000_000;
const BASE_WALL_NS: i64 = 1_730_000_000 * SEC_NS;

struct RpcClient {
    reader: BufReader<LocalIpcStream>,
    writer: LocalIpcStream,
    next_id: u64,
}

impl RpcClient {
    fn connect(endpoint: &Path) -> Self {
        // Windows named pipes report `ERROR_PIPE_BUSY` ("All pipe instances are busy") while
        // the server is between accepting one client and listening for the next, so concurrent
        // clients must retry briefly (the standard `WaitNamedPipe` pattern).
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            match LocalIpcStream::connect(endpoint) {
                Ok(stream) => break stream,
                Err(err) => {
                    assert!(Instant::now() < deadline, "connect to arbd: {err:?}");
                    thread::sleep(Duration::from_millis(10));
                }
            }
        };
        let writer = stream.try_clone().expect("clone IPC stream");
        Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
        }
    }

    fn call(&mut self, method: &str, params: &Value) -> Value {
        let resp = self.send_raw(method, params);
        assert!(
            resp.get("error").is_none(),
            "RPC `{method}` returned unexpected error: {resp}"
        );
        resp.get("result").cloned().unwrap_or(Value::Null)
    }

    fn call_err(&mut self, method: &str, params: &Value) -> (i64, String) {
        let resp = self.send_raw(method, params);
        let err = resp
            .get("error")
            .unwrap_or_else(|| panic!("RPC `{method}` unexpectedly succeeded: {resp}"));
        (
            err["code"].as_i64().expect("rpc error code"),
            err["message"].as_str().unwrap_or_default().to_owned(),
        )
    }

    fn send_raw(&mut self, method: &str, params: &Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .expect("serialize request");
        write_frame_line(&mut self.writer, &line, DEFAULT_MAX_IPC_LINE_BYTES).expect("write frame");
        self.writer.drain().expect("drain frame");
        let raw = read_frame_line(&mut self.reader, DEFAULT_MAX_IPC_LINE_BYTES)
            .expect("read frame")
            .expect("response line");
        serde_json::from_str(&raw).expect("parse response JSON")
    }
}

fn isolated_paths(tmp: &TempDir) -> ArbPaths {
    ArbPaths {
        data_dir: tmp.path().join("data"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: tmp.path().join("run"),
    }
}

#[derive(Debug)]
struct MockPngProvider {
    serial: String,
    png_bytes: Vec<u8>,
}

impl ScreenshotProvider for MockPngProvider {
    fn capture_png(&self, serial: Option<&str>) -> Result<(String, Vec<u8>), DaemonError> {
        let resolved = serial.unwrap_or(&self.serial).to_owned();
        Ok((resolved, self.png_bytes.clone()))
    }
}

fn build_valid_png(width: u32, height: u32) -> Vec<u8> {
    let mut png = Vec::new();
    png.extend_from_slice(&PNG_SIGNATURE);
    png.extend_from_slice(&13_u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&width.to_be_bytes());
    png.extend_from_slice(&height.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0]);
    png.extend_from_slice(&0_u32.to_be_bytes());
    png.extend_from_slice(&0_u32.to_be_bytes());
    png.extend_from_slice(b"IEND");
    png.extend_from_slice(&0_u32.to_be_bytes());
    png
}

#[derive(Debug)]
struct VecEventSource {
    serial: String,
    events: std::vec::IntoIter<CaptureEvent>,
}

impl VecEventSource {
    fn new(serial: &str, events: Vec<CaptureEvent>) -> Self {
        Self {
            serial: serial.to_owned(),
            events: events.into_iter(),
        }
    }
}

impl DeviceEventSource for VecEventSource {
    fn serial(&self) -> &str {
        &self.serial
    }

    fn next_event(&mut self) -> Result<Option<CaptureEvent>, arb_adb::mock::CaptureSourceError> {
        Ok(self.events.next())
    }
}

#[derive(Debug)]
struct ControllableDiskSpace(Arc<AtomicU64>);

impl DiskSpaceProvider for ControllableDiskSpace {
    fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
        let free = self.0.load(Ordering::Acquire);
        Ok(DiskSpaceInfo {
            available_bytes: free,
            total_bytes: free.saturating_add(1 << 40),
        })
    }
}

fn text_log_event(
    boot_id: &str,
    boottime_ns: i64,
    pc_cont_ns: i64,
    pid: i32,
    tag: &str,
    message: &str,
) -> CaptureEvent {
    let wall_ns = BASE_WALL_NS + boottime_ns;
    let wall_sec = u32::try_from(wall_ns / SEC_NS).expect("sec fits u32");
    let subsec_nanos = u32::try_from(wall_ns % SEC_NS).expect("nsec fits u32");
    let mut bytes = Vec::with_capacity(tag.len() + message.len() + 3);
    bytes.push(4); // INFO
    bytes.extend_from_slice(tag.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(message.as_bytes());
    bytes.push(0);
    CaptureEvent::LogRecord {
        pc_cont_ns,
        boot_id: boot_id.to_owned(),
        flags: FrameFlags::NONE,
        payload: LogRecordPayload {
            device_read_boottime_ns: boottime_ns,
            log_sec: wall_sec,
            log_nsec: subsec_nanos,
            pid,
            tid: pid,
            lid: 0,
            // System event tags are only stored under their real name for uid 1000 (system_server).
            uid: if matches!(
                tag,
                "wm_on_resume_called"
                    | "wm_resumed_activity"
                    | "ActivityTaskManager"
                    | "ActivityManager"
            ) {
                1000
            } else {
                10_000
            },
            hdr_size: 28,
            priority: 4,
            record_flags: 0,
            payload: bytes,
        },
    }
}

fn binary_am_crash_event(
    boot_id: &str,
    boottime_ns: i64,
    pc_cont_ns: i64,
    pid: i32,
    process: &str,
    exception: &str,
    message: &str,
) -> CaptureEvent {
    let wall_ns = BASE_WALL_NS + boottime_ns;
    let wall_sec = u32::try_from(wall_ns / SEC_NS).expect("sec fits u32");
    let subsec_nanos = u32::try_from(wall_ns % SEC_NS).expect("nsec fits u32");
    let mut raw = Vec::new();
    raw.extend_from_slice(&AM_CRASH_EVENT_TAG.to_le_bytes());
    raw.push(3); // EVENT_TYPE_LIST
    raw.push(9); // API 34+ 9-field am_crash
    let push_int = |b: &mut Vec<u8>, v: i32| {
        b.push(0);
        b.extend_from_slice(&v.to_le_bytes());
    };
    let push_str = |b: &mut Vec<u8>, s: &str| {
        b.push(2);
        b.extend_from_slice(&u32::try_from(s.len()).expect("len").to_le_bytes());
        b.extend_from_slice(s.as_bytes());
    };
    push_int(&mut raw, 0);
    push_int(&mut raw, pid);
    push_str(&mut raw, process);
    push_int(&mut raw, 8_929_190);
    push_str(&mut raw, exception);
    push_str(&mut raw, message);
    push_str(&mut raw, "MainActivity.kt");
    push_int(&mut raw, 42);
    push_int(&mut raw, 0); // fatal (recoverable = 0)

    CaptureEvent::LogRecord {
        pc_cont_ns,
        boot_id: boot_id.to_owned(),
        flags: FrameFlags::NONE,
        payload: LogRecordPayload {
            device_read_boottime_ns: boottime_ns,
            log_sec: wall_sec,
            log_nsec: subsec_nanos,
            pid: 1000,
            tid: 1000,
            lid: 2,
            uid: 1000,
            hdr_size: 28,
            priority: 0,
            record_flags: 0x01, // binary event
            payload: raw,
        },
    }
}

fn hello_and_heartbeat_events(boot_id: &str, bt_ns: i64, pc_ns: i64) -> Vec<CaptureEvent> {
    let streams = [
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
        emitted_seq: 1,
        last_pts_ns: bt_ns,
    })
    .collect();

    vec![
        CaptureEvent::Hello {
            pc_cont_ns: pc_ns,
            payload: HelloPayload {
                protocol_version: 1,
                api_level: 35,
                boot_count: 1,
                boot_id_source: BootIdSource::KernelProcBootId,
                is_emulator: true,
                heartbeat_interval_ms: 250,
                boot_id: boot_id.to_owned(),
                model: "Pixel 9".to_owned(),
                manufacturer: "Google".to_owned(),
                agent_version: "0.1.0".to_owned(),
                boot_id_detail: String::new(),
            },
            classification: arb_adb::adb::ReconnectClassification::InitialConnection {
                boot_id: boot_id.to_owned(),
                boot_id_source: BootIdSource::KernelProcBootId,
            },
        },
        CaptureEvent::Heartbeat {
            pc_cont_ns: pc_ns,
            boot_id: boot_id.to_owned(),
            payload: HeartbeatPayload {
                boottime_ns: bt_ns,
                monotonic_ns: bt_ns,
                realtime_ns: BASE_WALL_NS + bt_ns,
                streams,
            },
        },
    ]
}

fn heartbeat_event(boot_id: &str, bt_ns: i64, pc_ns: i64) -> CaptureEvent {
    CaptureEvent::Heartbeat {
        pc_cont_ns: pc_ns,
        boot_id: boot_id.to_owned(),
        payload: HeartbeatPayload {
            boottime_ns: bt_ns,
            monotonic_ns: bt_ns,
            realtime_ns: BASE_WALL_NS + bt_ns,
            streams: Vec::new(),
        },
    }
}

fn small_encoded_h264_events(boot_id: &str, base_bt_ns: i64, base_pc_ns: i64) -> Vec<CaptureEvent> {
    let frames: Vec<DecodedFrame> = (0..6_u64)
        .map(|i| DecodedFrame {
            frame_index: i,
            width: 64,
            height: 64,
            format: PixelFormat::Yuv420p,
            data: vec![u8::try_from(32 + i * 20).expect("fits u8"); 64 * 64 * 3 / 2],
        })
        .collect();
    let packets = OpenH264Backend::new()
        .encode_yuv420p_sequence(&frames, 3, 1_000_000_000, EncodeParams::default())
        .expect("encode test H.264");

    let mut out = vec![CaptureEvent::VideoSession {
        pc_cont_ns: base_pc_ns,
        boot_id: boot_id.to_owned(),
        meta: arb_adb::scrcpy::VideoSessionMeta {
            header_tag: 0x8000_0000,
            width: 64,
            height: 64,
        },
    }];
    for (idx, pkt) in packets.into_iter().enumerate() {
        let offset = i64::try_from(idx).expect("idx") * SEC_NS;
        let bt = base_bt_ns + offset;
        let pts_us = u64::try_from(bt / 1_000).expect("pts_us");
        out.push(CaptureEvent::VideoFrame {
            pc_cont_ns: base_pc_ns + offset,
            boot_id: boot_id.to_owned(),
            packet: arb_adb::scrcpy::VideoPacket {
                kind: if pkt.is_keyframe {
                    arb_adb::scrcpy::VideoPacketKind::Keyframe
                } else {
                    arb_adb::scrcpy::VideoPacketKind::DeltaFrame
                },
                pts_us,
                data: pkt.annex_b,
            },
        });
    }
    out
}

#[test]
fn t1_config_get_update_persistence_and_validation() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let override_dir = tmp.path().join("custom-out");
    fs::create_dir_all(&override_dir).expect("create custom out dir");

    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());

    let initial = client.call("config.get", &json!({}));
    assert_eq!(initial["config"]["config_version"], 1, "config_version = 1");
    assert_eq!(initial["config"]["save_window_secs"], 300, "default 300s");
    assert_eq!(
        initial["config"]["ring_budget_bytes"], 1_610_612_736_u64,
        "default 1.5 GiB"
    );
    assert_eq!(
        initial["config"]["min_free_disk_bytes"], 3_221_225_472_u64,
        "default 3 GiB"
    );

    // Invalid ranges and private output_dir_override must fail with -32602.
    let (code_low, _) = client.call_err("config.update", &json!({ "save_window_secs": 3 }));
    assert_eq!(code_low, -32_602, "out-of-range save_window_secs rejected");

    let (code_priv, _) = client.call_err(
        "config.update",
        &json!({ "output_dir_override": paths.data_dir.join("ring") }),
    );
    assert_eq!(code_priv, -32_602, "private ring dir override rejected");

    // Valid update persists atomically to data_dir/config.json.
    let updated = client.call(
        "config.update",
        &json!({
            "save_window_secs": 90,
            "video_max_size": 1280,
            "video_max_fps": 60,
            "audio_enabled": false,
            "crash_autosave_cooldown_secs": 25,
            "output_dir_override": override_dir,
            "ide_default_save_action": "save_only",
        }),
    );
    assert_eq!(updated["config"]["save_window_secs"], 90, "updated window");
    assert_eq!(
        updated["config"]["ide_default_save_action"], "save_only",
        "updated IDE action"
    );
    assert!(
        paths.data_dir.join("config.json").is_file(),
        "config.json must be persisted on disk"
    );

    drop(client);
    server.shutdown();

    // Restart daemon and verify persisted config reloads cleanly.
    let server2 = DaemonServer::start(DaemonConfig::new(paths)).expect("restart arbd");
    let mut client2 = RpcClient::connect(server2.endpoint());
    let reloaded = client2.call("config.get", &json!({}));
    assert_eq!(
        reloaded["config"], updated["config"],
        "restarted arbd must load persisted config.json"
    );

    // Clearing output_dir_override with null works cleanly.
    let cleared = client2.call("config.update", &json!({ "output_dir_override": null }));
    assert!(
        cleared["config"]["output_dir_override"].is_null(),
        "null must clear output_dir_override"
    );

    drop(client2);
    server2.shutdown();
}

#[test]
fn t1_save_screenshot_atomic_validation_and_overwrite_guard() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let png = build_valid_png(1080, 2400);
    let expected_sha = sha256_hex(&png);

    let provider = SharedScreenshotProvider::new(Arc::new(MockPngProvider {
        serial: "emulator-5554".to_owned(),
        png_bytes: png.clone(),
    }));
    let server = DaemonServer::start(DaemonConfig::new(paths).with_screenshot_provider(provider))
        .expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());

    // 1. Default output path when capture is idle.
    let shot1 = client.call("save.screenshot", &json!({}));
    assert_eq!(shot1["width"], 1080, "width");
    assert_eq!(shot1["height"], 2400, "height");
    assert_eq!(shot1["sha256"], expected_sha, "sha256");
    assert_eq!(shot1["serial"], "emulator-5554", "serial");
    assert!(
        shot1["captured_at_unix_ms"].as_u64().unwrap_or(0) > 0,
        "captured_at_unix_ms must be populated"
    );
    let default_path = Path::new(shot1["path"].as_str().expect("path string"));
    assert!(default_path.is_file(), "default screenshot file must exist");
    assert_eq!(
        fs::read(default_path).expect("read default png"),
        png,
        "written bytes must match"
    );

    // 2. Explicit output_path refuses overwrite by default, succeeds with overwrite: true.
    let explicit = tmp.path().join("explicit-shot.png");
    let shot2 = client.call(
        "save.screenshot",
        &json!({ "output_path": explicit, "serial": "usb-device-9" }),
    );
    assert_eq!(shot2["serial"], "usb-device-9", "explicit serial honored");
    assert!(explicit.is_file(), "explicit screenshot created");

    let (conflict_code, _) =
        client.call_err("save.screenshot", &json!({ "output_path": explicit }));
    assert_eq!(
        conflict_code, -32_002,
        "must refuse overwrite without overwrite=true"
    );

    let shot3 = client.call(
        "save.screenshot",
        &json!({ "output_path": explicit, "overwrite": true }),
    );
    assert_eq!(shot3["sha256"], expected_sha, "overwrite succeeded");

    drop(client);
    server.shutdown();
}

#[test]
fn t1_save_logs_and_logs_only_trigger_keep_video_running_and_honor_worm_leases() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths)).expect("start arbd");

    let mut events = hello_and_heartbeat_events("boot-logs", 10 * SEC_NS, 100 * SEC_NS);
    events.extend(small_encoded_h264_events(
        "boot-logs",
        10 * SEC_NS,
        100 * SEC_NS,
    ));
    events.push(text_log_event(
        "boot-logs",
        11 * SEC_NS,
        101 * SEC_NS,
        2000,
        "AppTag",
        "main window log line",
    ));
    events.push(heartbeat_event("boot-logs", 16 * SEC_NS, 106 * SEC_NS));
    let mut source = VecEventSource::new("emu-logs", events);
    server
        .ingest_device_source(&mut source)
        .expect("ingest source");

    let mut client = RpcClient::connect(server.endpoint());

    // 1. save.logs while capture is active must NOT stop capture and must NOT write video.mp4.
    let saved = client.call(
        "save.logs",
        &json!({ "window_secs": 30, "drain_timeout_ms": 50 }),
    );
    let bundle_id = saved["bundle_id"].as_str().expect("bundle_id").to_owned();
    let bundle_path = Path::new(saved["bundle_path"].as_str().expect("bundle_path"));

    let status = client.call("system.status", &json!({}));
    assert_eq!(
        status["capture"]["active"], true,
        "save.logs must NOT stop active capture"
    );

    assert!(
        !bundle_path.join("video.mp4").exists(),
        "logs_only bundle must not contain video.mp4"
    );
    assert!(
        !bundle_path.join("audio.m4a").exists(),
        "logs_only bundle must not contain audio.m4a"
    );
    assert!(
        bundle_path.join(LOGS_JSONL_FILE).is_file(),
        "logs.jsonl must exist"
    );
    assert!(
        bundle_path.join(UNANCHORED_JSONL_FILE).is_file(),
        "unanchored.jsonl must exist"
    );

    let reader = V2BundleReader::open(bundle_path).expect("open logs_only bundle");
    reader
        .verify_integrity()
        .expect("logs_only bundle must pass SHA-256 verification");
    assert_eq!(
        reader.manifest().bundle_kind,
        arb_core::bundle::BundleKind::LogsOnly,
        "manifest must be logs_only"
    );

    // 2. Also verify `save.trigger` with `mode = "logs_only"`.
    let saved2 = client.call(
        "save.trigger",
        &json!({ "duration_sec": 30, "mode": "logs_only", "drain_timeout_ms": 50 }),
    );
    let bundle2_path = Path::new(saved2["bundle_path"].as_str().expect("bundle2_path"));
    assert!(
        !bundle2_path.join("video.mp4").exists(),
        "save.trigger mode=logs_only must not write video.mp4"
    );

    // 3. Lease protection + non-destructive Trash/Restore/GC on logs_only bundle.
    let opened = client.call(
        "bundle.open",
        &json!({ "bundle_id_or_path": bundle_id, "holder_kind": "cli", "holder_pid": std::process::id() }),
    );
    let lease_id = opened["lease_id"].as_str().expect("lease_id").to_owned();
    let _ = client.call("bundle.trash", &json!({ "bundle_id": bundle_id }));
    let gc_leased = client.call("bundle.gc", &json!({}));
    assert!(
        gc_leased["skipped_leased_bundle_ids"]
            .as_array()
            .expect("array")
            .iter()
            .any(|v| v.as_str() == Some(&bundle_id)),
        "leased logs_only bundle must be protected from GC"
    );
    assert!(bundle_path.is_dir(), "trashed leased bundle stays on disk");

    let _ = client.call("bundle.restore", &json!({ "bundle_id": bundle_id }));
    let _ = client.call("bundle.close", &json!({ "lease_id": lease_id }));
    let _ = client.call("bundle.trash", &json!({ "bundle_id": bundle_id }));
    let gc_purged = client.call("bundle.gc", &json!({}));
    assert!(
        gc_purged["purged_bundle_ids"]
            .as_array()
            .expect("array")
            .iter()
            .any(|v| v.as_str() == Some(&bundle_id)),
        "unleased trashed logs_only bundle must purge on GC"
    );

    drop(client);
    server.shutdown();
}

fn build_crash_and_screen_events() -> Vec<CaptureEvent> {
    let mut events = hello_and_heartbeat_events("boot-crash", 10 * SEC_NS, 100 * SEC_NS);
    events.extend(small_encoded_h264_events(
        "boot-crash",
        10 * SEC_NS,
        100 * SEC_NS,
    ));
    events.push(text_log_event(
        "boot-crash",
        10 * SEC_NS,
        100 * SEC_NS,
        1000,
        "wm_resumed_activity",
        "[0,111,com.example.shop/.HomeActivity]",
    ));
    events.push(text_log_event(
        "boot-crash",
        10 * SEC_NS + 50_000_000,
        100 * SEC_NS + 50_000_000,
        1000,
        "ActivityTaskManager",
        "Displayed com.example.shop/.HomeActivity for user 0: +50ms",
    ));
    events.push(text_log_event(
        "boot-crash",
        12 * SEC_NS,
        102 * SEC_NS,
        1000,
        "wm_on_resume_called",
        "[222,com.example.shop/.CheckoutActivity,RESUME_ACTIVITY,4]",
    ));
    events.push(binary_am_crash_event(
        "boot-crash",
        11 * SEC_NS,
        101 * SEC_NS,
        4201,
        "com.example.shop",
        "java.lang.NullPointerException",
        "cart was null",
    ));
    events.push(binary_am_crash_event(
        "boot-crash",
        14 * SEC_NS,
        104 * SEC_NS,
        4202,
        "com.example.shop",
        "java.lang.NullPointerException",
        "crash loop debounced",
    ));
    events.push(heartbeat_event("boot-crash", 30 * SEC_NS, 120 * SEC_NS));
    events.push(binary_am_crash_event(
        "boot-crash",
        30 * SEC_NS,
        120 * SEC_NS,
        4203,
        "com.example.shop:worker",
        "java.lang.IllegalStateException",
        "post cooldown crash",
    ));
    events.push(heartbeat_event("boot-crash", 31 * SEC_NS, 121 * SEC_NS));
    events
}

#[test]
fn t1_crash_autosave_debounce_and_timeline_crashes_screens() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths)).expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());

    let _ = client.call(
        "config.update",
        &json!({ "save_window_secs": 60, "crash_autosave_enabled": true, "crash_autosave_cooldown_secs": 15 }),
    );

    let mut events = build_crash_and_screen_events();
    let mut unknown = binary_am_crash_event(
        "boot-crash",
        31 * SEC_NS,
        121 * SEC_NS,
        4204,
        "unknown",
        "UnknownException",
        "compat",
    );
    if let CaptureEvent::LogRecord { payload, .. } = &mut unknown {
        payload.uid = arb_adb::wire::UNKNOWN_U32;
    }
    events.push(unknown);
    events.push(heartbeat_event("boot-crash", 200 * SEC_NS, 290 * SEC_NS));
    let mut source = VecEventSource::new("emu-crash", events);
    server
        .ingest_device_source(&mut source)
        .expect("ingest crash scenario");

    let deadline = Instant::now() + Duration::from_secs(10);
    let final_status = loop {
        let st = client.call("system.status", &json!({}));
        let ca = &st["crash_autosave"];
        if ca["detected_crashes"].as_u64() == Some(3)
            && ca["in_flight"].as_bool() == Some(false)
            && ca["last_bundle_id"].is_string()
        {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for crash auto-save: {st}"
        );
        thread::sleep(Duration::from_millis(50));
    };

    let ca = &final_status["crash_autosave"];
    assert_eq!(ca["detected_crashes"], 3, "3 live crashes detected");
    assert_eq!(ca["triggered_saves"], 2, "2 saves triggered (1 debounced)");
    assert_eq!(ca["suppressed_cooldown"], 1, "1 crash debounced");
    assert_eq!(ca["suppressed_storage_pressure"], 0, "no storage pressure");

    let jobs = client.call("save.list_jobs", &json!({}));
    let crash_jobs = jobs["jobs"]
        .as_array()
        .expect("jobs array")
        .iter()
        .filter(|j| j["trigger"] == "crash_auto_save")
        .count();
    assert_eq!(
        crash_jobs, 2,
        "save.list_jobs must expose both crash_auto_save jobs"
    );

    let last_bundle_id = ca["last_bundle_id"].as_str().expect("last_bundle_id");
    let crashes_resp = client.call(
        "timeline.crashes",
        &json!({ "bundle_id_or_path": last_bundle_id }),
    );
    let crashes = crashes_resp["crashes"].as_array().expect("crashes array");
    assert_eq!(crashes.len(), 3, "bundle must index all 3 crash markers");
    assert_eq!(crashes[0]["process_name"], "com.example.shop", "proc 0");
    assert_eq!(crashes[0]["package_name"], "com.example.shop", "pkg 0");
    assert_eq!(
        crashes[0]["exception_class"], "java.lang.NullPointerException",
        "exception 0"
    );
    assert!(
        crashes[0]["frame_index_hint"].is_u64(),
        "crash marker must carry frame_index_hint"
    );

    let screens_resp = client.call(
        "timeline.screens",
        &json!({ "bundle_id_or_path": last_bundle_id }),
    );
    let screens = screens_resp["screens"].as_array().expect("screens array");
    assert_eq!(
        screens.len(),
        2,
        "collapsed HomeActivity + CheckoutActivity spans"
    );
    assert_eq!(screens[0]["short_name"], "HomeActivity", "screen 0");
    assert_eq!(screens[0]["package_name"], "com.example.shop", "pkg 0");
    assert_eq!(screens[1]["short_name"], "CheckoutActivity", "screen 1");
    assert!(
        screens[0]["start_frame_index"].is_u64() && screens[0]["end_frame_index"].is_u64(),
        "screen span must carry video frame bounds"
    );

    drop(client);
    server.shutdown();
}

#[test]
fn t1_crash_autosave_suppressed_under_storage_pressure_stop() {
    let tmp_p = TempDir::new().expect("tempdir pressure");
    let paths_p = isolated_paths(&tmp_p);
    let free_bytes = Arc::new(AtomicU64::new(10 * 1024 * 1024)); // 10 MiB < 100 MiB floor
    let disk_provider = SharedDiskSpaceProvider::new(Arc::new(ControllableDiskSpace(free_bytes)));
    let policy = RingBudgetPolicy {
        max_ring_bytes: 16 * 1024 * 1024,
        max_ring_duration_ns: 600_000_000_000,
        min_free_disk_bytes: 100 * 1024 * 1024,
    };
    let server_p = DaemonServer::start(
        DaemonConfig::new(paths_p)
            .with_ring_policy(policy)
            .with_disk_provider(disk_provider),
    )
    .expect("start pressure arbd");

    let mut pressure_events = hello_and_heartbeat_events("boot-p", 10 * SEC_NS, 100 * SEC_NS);
    pressure_events.push(binary_am_crash_event(
        "boot-p",
        11 * SEC_NS,
        101 * SEC_NS,
        7777,
        "com.example.pressure",
        "java.lang.OutOfMemoryError",
        "must be suppressed",
    ));
    let mut source_p = VecEventSource::new("emu-p", pressure_events);
    server_p
        .ingest_device_source(&mut source_p)
        .expect("ingest under pressure");

    let mut client_p = RpcClient::connect(server_p.endpoint());
    let deadline_p = Instant::now() + Duration::from_secs(5);
    let st_p = loop {
        let st = client_p.call("system.status", &json!({}));
        if st["crash_autosave"]["detected_crashes"].as_u64() == Some(1) {
            break st;
        }
        assert!(
            Instant::now() < deadline_p,
            "timed out waiting for pressure crash telemetry: {st}"
        );
        thread::sleep(Duration::from_millis(25));
    };

    assert_eq!(
        st_p["capture"]["storage_pressure_stopped"], true,
        "storage pressure stop must be active"
    );
    assert_eq!(
        st_p["crash_autosave"]["suppressed_storage_pressure"], 1,
        "crash auto-save must be strictly suppressed under STORAGE_PRESSURE_STOP"
    );
    assert_eq!(
        st_p["crash_autosave"]["triggered_saves"], 0,
        "zero auto-saves may fire under STORAGE_PRESSURE_STOP"
    );

    drop(client_p);
    server_p.shutdown();
}

#[test]
fn t1_review_concurrent_config_updates_never_lose_fields_or_corrupt_config_json() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let endpoint = server.endpoint().to_path_buf();

    let patches = [
        json!({ "save_window_secs": 111 }),
        json!({ "video_max_fps": 45 }),
        json!({ "video_max_size": 1000 }),
        json!({ "crash_autosave_cooldown_secs": 33 }),
        json!({ "audio_enabled": false }),
        json!({ "ide_default_save_action": "save_only" }),
    ];
    let handles: Vec<_> = patches
        .into_iter()
        .map(|patch| {
            let endpoint = endpoint.clone();
            thread::spawn(move || {
                let mut client = RpcClient::connect(&endpoint);
                for _ in 0..10 {
                    let _ = client.call("config.update", &patch);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("updater thread");
    }

    let mut client = RpcClient::connect(&endpoint);
    let cfg = client.call("config.get", &json!({}))["config"].clone();
    assert_eq!(cfg["save_window_secs"], 111, "no lost update: window");
    assert_eq!(cfg["video_max_fps"], 45, "no lost update: fps");
    assert_eq!(cfg["video_max_size"], 1000, "no lost update: size");
    assert_eq!(
        cfg["crash_autosave_cooldown_secs"], 33,
        "no lost update: cooldown"
    );
    assert_eq!(cfg["audio_enabled"], false, "no lost update: audio");
    assert_eq!(
        cfg["ide_default_save_action"], "save_only",
        "no lost update: ide"
    );

    drop(client);
    server.shutdown();

    // The persisted file must be intact: a restart loads exactly the merged config.
    let server2 = DaemonServer::start(DaemonConfig::new(paths)).expect("restart arbd");
    let mut client2 = RpcClient::connect(server2.endpoint());
    let reloaded = client2.call("config.get", &json!({}))["config"].clone();
    assert_eq!(reloaded, cfg, "restart must reload the merged config.json");
    drop(client2);
    server2.shutdown();
}

#[test]
fn t1_review_screenshot_cannot_overwrite_arbd_state_files_in_data_dir() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let png = build_valid_png(100, 200);
    let provider = SharedScreenshotProvider::new(Arc::new(MockPngProvider {
        serial: "emu".to_owned(),
        png_bytes: png,
    }));
    let server =
        DaemonServer::start(DaemonConfig::new(paths.clone()).with_screenshot_provider(provider))
            .expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());
    // Make sure config.json exists so a successful overwrite would be observable.
    let _ = client.call("config.update", &json!({ "video_max_fps": 50 }));
    let config_before = fs::read(paths.data_dir.join("config.json")).expect("config.json");

    for victim in ["config.json", "catalog.sqlite", "new-file.png"] {
        let (code, msg) = client.call_err(
            "save.screenshot",
            &json!({ "output_path": paths.data_dir.join(victim), "overwrite": true }),
        );
        assert_eq!(code, -32_602, "{victim} must be rejected: {msg}");
    }
    assert_eq!(
        fs::read(paths.data_dir.join("config.json")).expect("config.json"),
        config_before,
        "config.json must be untouched"
    );

    // `..` segments must not smuggle a path out of `screenshots/` into state files.
    let traversal = paths
        .data_dir
        .join("screenshots")
        .join("nope")
        .join("..")
        .join("..")
        .join("catalog.sqlite");
    let (code, msg) = client.call_err(
        "save.screenshot",
        &json!({ "output_path": traversal, "overwrite": true }),
    );
    assert_eq!(code, -32_602, "`..` traversal must be rejected: {msg}");
    assert!(
        !paths.data_dir.join("screenshots").join("nope").exists(),
        "rejected request must not create directories"
    );
    let (code, _) = client.call_err(
        "save.logs",
        &json!({ "dest_dir": paths.data_dir.join("screenshots").join("..").join("ring") }),
    );
    assert_eq!(code, -32_602, "`..` in dest_dir rejected");

    // The dedicated screenshots directory inside data_dir stays allowed.
    let ok = client.call(
        "save.screenshot",
        &json!({ "output_path": paths.data_dir.join("screenshots").join("ok.png") }),
    );
    assert_eq!(ok["width"], 100, "screenshots/ target allowed");

    drop(client);
    server.shutdown();
}

#[test]
fn t1_review_invalid_config_json_is_set_aside_and_arbd_still_starts() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    fs::create_dir_all(&paths.data_dir).expect("data dir");
    fs::write(paths.data_dir.join("config.json"), b"{ this is not json")
        .expect("write corrupt config");

    let server = DaemonServer::start(DaemonConfig::new(paths.clone()))
        .expect("arbd must start despite an invalid config.json");
    let mut client = RpcClient::connect(server.endpoint());
    let status = client.call("system.status", &json!({}));
    let warning = status["config_warning"]
        .as_str()
        .expect("config_warning set");
    assert!(
        warning.contains("config.json"),
        "warning names the file: {warning}"
    );
    assert_eq!(
        client.call("config.get", &json!({}))["config"]["save_window_secs"],
        300,
        "defaults in use"
    );
    let aside: Vec<_> = fs::read_dir(&paths.data_dir)
        .expect("read data dir")
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.json.invalid-")
        })
        .collect();
    assert_eq!(aside.len(), 1, "invalid config preserved exactly once");

    // A subsequent valid update works and the warning remains informational.
    let _ = client.call("config.update", &json!({ "save_window_secs": 60 }));
    drop(client);
    server.shutdown();
}

#[test]
fn t1_review_explicit_save_windows_are_range_checked() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());
    for (method, params) in [
        ("save.trigger", json!({ "duration_sec": 3 })),
        ("save.trigger", json!({ "duration_sec": 601 })),
        ("save.logs", json!({ "window_secs": 4 })),
        ("save.logs", json!({ "window_secs": 100_000 })),
    ] {
        let (code, msg) = client.call_err(method, &params);
        assert_eq!(
            code, -32_602,
            "{method} {params} must be InvalidParams: {msg}"
        );
    }
    drop(client);
    server.shutdown();
}

#[test]
fn t1_review_crash_autosave_checks_host_free_space_before_pinning() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let free_bytes = Arc::new(AtomicU64::new(1 << 40));
    let disk_provider =
        SharedDiskSpaceProvider::new(Arc::new(ControllableDiskSpace(Arc::clone(&free_bytes))));
    let server = DaemonServer::start(DaemonConfig::new(paths).with_disk_provider(disk_provider))
        .expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());
    // Disk becomes full after startup, before any ring write has failed and latched the stop.
    free_bytes.store(1024, Ordering::Release);

    let mut events = hello_and_heartbeat_events("boot-free", 10 * SEC_NS, 100 * SEC_NS);
    events.push(binary_am_crash_event(
        "boot-free",
        11 * SEC_NS,
        101 * SEC_NS,
        5150,
        "com.example.full",
        "java.lang.OutOfMemoryError",
        "disk almost full",
    ));
    let mut source = VecEventSource::new("emu-free", events);
    server
        .ingest_device_source(&mut source)
        .expect("ingest crash");

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        let st = client.call("system.status", &json!({}));
        if st["crash_autosave"]["detected_crashes"].as_u64() == Some(1) {
            break st;
        }
        assert!(Instant::now() < deadline, "timed out: {st}");
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        status["crash_autosave"]["triggered_saves"], 0,
        "no save on a full disk"
    );
    assert_eq!(
        status["crash_autosave"]["suppressed_storage_pressure"], 1,
        "suppressed by the free-space floor"
    );
    drop(client);
    server.shutdown();
}

#[derive(Debug)]
struct PathAwareDisk {
    low_under: std::path::PathBuf,
}

impl DiskSpaceProvider for PathAwareDisk {
    fn disk_space(&self, path: &Path) -> Result<DiskSpaceInfo, OsError> {
        let low = path.starts_with(&self.low_under);
        let free: u64 = if low { 1024 } else { 1 << 40 };
        Ok(DiskSpaceInfo {
            available_bytes: free,
            total_bytes: 1 << 41,
        })
    }
}

#[test]
fn t1_review_crash_autosave_checks_free_space_of_the_override_destination() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let override_dir = tmp.path().join("override-out");
    fs::create_dir_all(&override_dir).expect("override dir");
    let disk = SharedDiskSpaceProvider::new(Arc::new(PathAwareDisk {
        low_under: override_dir.clone(),
    }));
    let server =
        DaemonServer::start(DaemonConfig::new(paths).with_disk_provider(disk)).expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());
    let _ = client.call(
        "config.update",
        &json!({ "output_dir_override": override_dir, "crash_autosave_enabled": true }),
    );

    let mut events = hello_and_heartbeat_events("boot-dest", 10 * SEC_NS, 100 * SEC_NS);
    events.push(binary_am_crash_event(
        "boot-dest",
        11 * SEC_NS,
        101 * SEC_NS,
        6001,
        "com.example.dest",
        "java.lang.IllegalStateException",
        "override disk is full",
    ));
    let mut source = VecEventSource::new("emu-dest", events);
    server.ingest_device_source(&mut source).expect("ingest");

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        let st = client.call("system.status", &json!({}));
        if st["crash_autosave"]["detected_crashes"].as_u64() == Some(1) {
            break st;
        }
        assert!(Instant::now() < deadline, "timed out: {st}");
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        status["capture"]["storage_pressure_stopped"], false,
        "the ring volume is healthy; only the override destination is full"
    );
    assert_eq!(
        status["crash_autosave"]["triggered_saves"], 0,
        "no save to a full disk"
    );
    assert_eq!(status["crash_autosave"]["suppressed_storage_pressure"], 1);
    drop(client);
    server.shutdown();
}

#[test]
fn t1_review_app_text_log_cannot_forge_a_crash_and_unreadable_config_blocks_updates() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    // A directory named config.json is unreadable (I/O error, not parse error) on every OS.
    fs::create_dir_all(paths.data_dir.join("config.json")).expect("config.json dir");
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());

    let status = client.call("system.status", &json!({}));
    assert!(
        status["config_warning"]
            .as_str()
            .is_some_and(|w| w.contains("could not be read")),
        "unreadable config reported: {status}"
    );
    let (code, _) = client.call_err("config.update", &json!({ "save_window_secs": 60 }));
    assert_eq!(
        code, -32_002,
        "config.update refused while config.json is unreadable"
    );
    assert!(
        paths.data_dir.join("config.json").is_dir(),
        "the user's file must be left untouched"
    );

    let mut events = hello_and_heartbeat_events("boot-forge", 10 * SEC_NS, 100 * SEC_NS);
    events.push(text_log_event(
        "boot-forge",
        11 * SEC_NS,
        101 * SEC_NS,
        4242,
        "am_crash",
        "[0,1,com.evil,0,java.lang.Error,fake,F.kt,1]",
    ));
    events.push(heartbeat_event("boot-forge", 12 * SEC_NS, 102 * SEC_NS));
    let mut source = VecEventSource::new("emu-forge", events);
    server.ingest_device_source(&mut source).expect("ingest");
    thread::sleep(Duration::from_millis(300));
    let after = client.call("system.status", &json!({}));
    assert_eq!(
        after["crash_autosave"]["detected_crashes"], 0,
        "an app's text log line must never count as a crash"
    );
    drop(client);
    server.shutdown();
}

#[test]
fn t1_review_newer_version_config_json_is_preserved_and_blocks_updates() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    fs::create_dir_all(&paths.data_dir).expect("data dir");
    let newer = br#"{"config_version": 999, "future_field": true}"#;
    fs::write(paths.data_dir.join("config.json"), newer).expect("write newer config");

    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());
    let status = client.call("system.status", &json!({}));
    assert!(
        status["config_warning"].is_string(),
        "newer-version config reported: {status}"
    );
    let (code, _) = client.call_err("config.update", &json!({ "save_window_secs": 60 }));
    assert_eq!(
        code, -32_002,
        "update refused so the newer file is never replaced"
    );
    assert_eq!(
        fs::read(paths.data_dir.join("config.json")).expect("config.json"),
        newer,
        "a newer arbd's config.json must survive untouched"
    );
    drop(client);
    server.shutdown();
}

#[test]
fn t1_review_newer_schema_with_unknown_enum_value_is_not_quarantined() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    fs::create_dir_all(&paths.data_dir).expect("data dir");
    let newer = br#"{"config_version": 2, "ide_default_save_action": "brand_new_action"}"#;
    fs::write(paths.data_dir.join("config.json"), newer).expect("write newer config");
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());
    let (code, _) = client.call_err("config.update", &json!({ "save_window_secs": 60 }));
    assert_eq!(code, -32_002, "update refused");
    assert_eq!(
        fs::read(paths.data_dir.join("config.json")).expect("config.json"),
        newer,
        "newer-schema file untouched"
    );
    let moved = fs::read_dir(&paths.data_dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".invalid-"))
        .count();
    assert_eq!(moved, 0, "must not be quarantined");
    drop(client);
    server.shutdown();
}

#[test]
fn t1_review_older_or_invalid_config_version_is_quarantined_and_updates_still_work() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    fs::create_dir_all(&paths.data_dir).expect("data dir");
    fs::write(
        paths.data_dir.join("config.json"),
        br#"{"config_version": 0}"#,
    )
    .expect("write old config");
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut client = RpcClient::connect(server.endpoint());
    let updated = client.call("config.update", &json!({ "save_window_secs": 60 }));
    assert_eq!(
        updated["config"]["save_window_secs"], 60,
        "update works after quarantine"
    );
    let moved = fs::read_dir(&paths.data_dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.json.invalid-")
        })
        .count();
    assert_eq!(moved, 1, "older/invalid version is set aside");
    drop(client);
    server.shutdown();
}
