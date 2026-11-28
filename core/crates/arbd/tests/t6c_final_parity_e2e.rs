//! End-to-end integration tests for T-6C Owner-Confirmed Final v1 Parity on live `arbd`:
//! 1. Low-overhead `CaptureMode::LogsOnly` capture + `save.trigger` -> valid WORM `logs_only` bundle
//! 2. Automatic foreground per-app log extraction (`app-logs/<package>.jsonl`, verified SHA-256 in
//!    `manifest.json`, zero noisy `com.google.android.gms` / `com.android.systemui` files,
//!    `timeline.query_logs(package=...)`, `timeline.summary`, and `timeline.overview`)
//! 3. Strict multi-device & Wi-Fi ADB serial config/RPC validation
//! 4. `screenshot_dir_override` & `save.screenshot(output_dir=...)` with traversal/state-file guards
//! 5. `system.hello` `build_id` & `binary_mtime_secs` same-path upgrade metadata

#![allow(
    clippy::too_many_lines,
    reason = "end-to-end scenario tests read cleanly as linear scripts"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use arb_adb::mock::{CaptureEvent, CaptureSourceError, DeviceEventSource};
use arb_adb::wire::{BootIdSource, FrameFlags, HeartbeatPayload, HelloPayload, LogRecordPayload};
use arb_core::bundle::{BundleKind, ManifestV2};
use arb_core::config::CaptureMode;
use arb_os::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcStream, read_frame_line, write_frame_line};
use arb_os::paths::ArbPaths;
use arb_storage::sha256_hex;
use arbd::error::RPC_ERR_INVALID_PARAMS;
use arbd::screenshot::{ScreenshotProvider, SharedScreenshotProvider};
use arbd::{DaemonConfig, DaemonServer};
use serde_json::{Value, json};
use tempfile::TempDir;

const SEC_NS: i64 = 1_000_000_000;
const BASE_BT_NS: i64 = 500 * SEC_NS;

struct RpcClient {
    reader: BufReader<LocalIpcStream>,
    writer: LocalIpcStream,
    next_id: u64,
}

impl RpcClient {
    fn connect(endpoint: &Path) -> Self {
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

/// Minimal valid 1x1 RGBA PNG encoded in memory for deterministic screenshot tests.
fn valid_1x1_png() -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 1, 1);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer
        .write_image_data(&[10, 20, 30, 255])
        .expect("png data");
    drop(writer);
    out
}

#[derive(Debug)]
struct StubScreenshotProvider {
    png: Vec<u8>,
}

impl ScreenshotProvider for StubScreenshotProvider {
    fn capture_png(
        &self,
        serial: Option<&str>,
    ) -> Result<(String, Vec<u8>), arbd::error::DaemonError> {
        Ok((
            serial.unwrap_or("192.168.1.50:5555").to_owned(),
            self.png.clone(),
        ))
    }
}

/// Deterministic event source emitting ONLY companion heartbeats and logcat records (zero video).
struct LogsOnlySource {
    serial: String,
    events: std::collections::VecDeque<CaptureEvent>,
}

impl DeviceEventSource for LogsOnlySource {
    fn serial(&self) -> &str {
        &self.serial
    }

    fn next_event(&mut self) -> Result<Option<CaptureEvent>, CaptureSourceError> {
        Ok(self.events.pop_front())
    }
}

fn log_event(
    pc_cont_ns: i64,
    boottime_ns: i64,
    priority: u8,
    pid: i32,
    uid: u32,
    tag: &str,
    msg: &str,
) -> CaptureEvent {
    let mut payload_bytes = Vec::new();
    payload_bytes.push(priority);
    payload_bytes.extend_from_slice(tag.as_bytes());
    payload_bytes.push(0);
    payload_bytes.extend_from_slice(msg.as_bytes());
    payload_bytes.push(0);
    let log_sec = u32::try_from(1_700_000_000_i64 + (boottime_ns / SEC_NS)).expect("log_sec");
    CaptureEvent::LogRecord {
        pc_cont_ns,
        boot_id: "boot-t6c".to_owned(),
        flags: FrameFlags::NONE,
        payload: LogRecordPayload {
            device_read_boottime_ns: boottime_ns,
            log_sec,
            log_nsec: 0,
            pid,
            tid: pid,
            lid: 0,
            uid,
            hdr_size: 28,
            priority,
            record_flags: 0,
            payload: payload_bytes,
        },
    }
}

#[test]
fn t6c_logs_only_capture_foreground_app_logs_and_hello_build_id() {
    let tmp = TempDir::new().expect("tempdir");
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).expect("start arbd");
    let mut rpc = RpcClient::connect(server.endpoint());

    let digest = sha256_hex(&fs::read(std::env::current_exe().unwrap()).unwrap());
    assert_eq!(
        rpc.call("system.hello", &json!({}))["build_id"],
        format!("{}:sha256:{digest}", env!("CARGO_PKG_VERSION"))
    );

    // 1. system.hello exposes build_id + binary_mtime_secs for same-path upgrade checks.
    let hello = rpc.call("system.hello", &json!({}));
    assert!(
        hello["build_id"].as_str().is_some_and(|s| !s.is_empty()),
        "system.hello must expose non-empty build_id: {hello}"
    );
    assert!(
        hello["binary_mtime_secs"].as_u64().is_some(),
        "system.hello must expose binary_mtime_secs: {hello}"
    );

    // 2. Build a realistic logs_only stream (zero video frames):
    //    - foreground app: com.example.shop (uid 10200, pid 4200)
    //    - system UI: com.android.systemui (uid 10050, pid 2100) -> MUST NOT create app-logs file
    //    - noisy background GMS: com.google.android.gms (uid 10100, pid 3100) -> MUST NOT create file
    //    - crashed background helper: com.example.worker (pid 5100) -> MUST create app-logs file
    let mut events = std::collections::VecDeque::new();
    events.push_back(CaptureEvent::Hello {
        pc_cont_ns: BASE_BT_NS,
        payload: HelloPayload {
            protocol_version: 1,
            api_level: 35,
            boot_count: 1,
            boot_id_source: BootIdSource::KernelProcBootId,
            is_emulator: false,
            heartbeat_interval_ms: 250,
            boot_id: "boot-t6c".to_owned(),
            model: "Pixel 9".to_owned(),
            manufacturer: "Google".to_owned(),
            agent_version: "0.1.0".to_owned(),
            boot_id_detail: String::new(),
        },
        classification: arb_adb::adb::ReconnectClassification::InitialConnection {
            boot_id: "boot-t6c".to_owned(),
            boot_id_source: BootIdSource::KernelProcBootId,
        },
    });
    for sec in 0..=4 {
        let t = BASE_BT_NS + sec * SEC_NS;
        events.push_back(CaptureEvent::Heartbeat {
            pc_cont_ns: t,
            boot_id: "boot-t6c".to_owned(),
            payload: HeartbeatPayload {
                boottime_ns: t,
                monotonic_ns: t,
                realtime_ns: 1_700_000_000 * SEC_NS + t,
                streams: Vec::new(),
            },
        });
    }
    events.push_back(log_event(
        BASE_BT_NS + 200_000_000,
        BASE_BT_NS + 200_000_000,
        4,
        1000,
        1000,
        "wm_resumed_activity",
        "[0,111,com.android.systemui/.ShadeActivity]",
    ));
    events.push_back(log_event(
        BASE_BT_NS + 500_000_000,
        BASE_BT_NS + 500_000_000,
        4,
        1000,
        1000,
        "wm_resumed_activity",
        "[0,222,com.example.shop/.CartActivity]",
    ));
    events.push_back(log_event(
        BASE_BT_NS + 1_000_000_000,
        BASE_BT_NS + 1_000_000_000,
        6,
        4200,
        10200,
        "ShopCart",
        "checkout request failed with HTTP 503",
    ));
    events.push_back(log_event(
        BASE_BT_NS + 1_200_000_000,
        BASE_BT_NS + 1_200_000_000,
        4,
        3100,
        10100,
        "GmsCore",
        "background sync heartbeat",
    ));
    events.push_back(log_event(
        BASE_BT_NS + 1_400_000_000,
        BASE_BT_NS + 1_400_000_000,
        4,
        2100,
        10050,
        "SystemUI",
        "status bar clock tick",
    ));
    events.push_back(log_event(
        BASE_BT_NS + 2_000_000_000,
        BASE_BT_NS + 2_000_000_000,
        6,
        5100,
        10300,
        "WorkerSync",
        "fatal null token",
    ));
    events.push_back(log_event(
        BASE_BT_NS + 2_100_000_000,
        BASE_BT_NS + 2_100_000_000,
        7,
        1000,
        1000,
        "am_crash",
        "[0,5100,com.example.worker,8912896,java.lang.NullPointerException,token is null,Worker.kt,42]",
    ));

    let mut source = LogsOnlySource {
        serial: "192.168.1.50:5555".to_owned(),
        events,
    };
    server
        .ingest_device_source_with_mode(&mut source, CaptureMode::LogsOnly)
        .expect("ingest logs_only source");
    let mut uid_map = BTreeMap::new();
    uid_map.insert(10200, "com.example.shop".to_owned());
    uid_map.insert(10100, "com.google.android.gms".to_owned());
    uid_map.insert(10050, "com.android.systemui".to_owned());
    uid_map.insert(10300, "com.example.worker".to_owned());
    server
        .set_capture_package_uids(uid_map)
        .expect("set uid_packages");

    // 3. system.status reports capture_mode == logs_only.
    let status = rpc.call("system.status", &json!({}));
    assert_eq!(status["capture"]["capture_mode"], "logs_only");
    assert_eq!(status["capture"]["video_frames"], 0);

    // 4. Triggering `save.trigger` while in `logs_only` capture mode succeeds cleanly
    //    and writes a valid WORM `logs_only` bundle!
    let saved = rpc.call("save.trigger", &json!({ "duration_sec": 30 }));
    let bundle_id = saved["bundle_id"]
        .as_str()
        .expect("save.trigger in logs_only mode must produce a bundle_id")
        .to_owned();
    let bundle_path = Path::new(
        saved["bundle_path"]
            .as_str()
            .expect("bundle_path must be present"),
    );

    let manifest_raw =
        fs::read_to_string(bundle_path.join("manifest.json")).expect("read manifest");
    let manifest: ManifestV2 = serde_json::from_str(&manifest_raw).expect("parse manifest");
    manifest
        .validate()
        .expect("manifest must pass full validation");
    assert_eq!(manifest.bundle_kind, BundleKind::LogsOnly);
    assert!(
        !bundle_path.join("video.mp4").exists(),
        "logs_only bundle must have no video.mp4"
    );

    // 5. Verify foreground + crashed per-app extraction (`app-logs/<package>.log`) and ZERO GMS/SystemUI noise.
    let app_log_pkgs: Vec<&str> = manifest
        .app_logs
        .iter()
        .map(|entry| entry.package_name.as_str())
        .collect();
    assert_eq!(app_log_pkgs, vec!["com.example.shop", "com.example.worker"]);
    for entry in &manifest.app_logs {
        assert!(
            std::path::Path::new(&entry.relative_path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("log")),
            "per-app log sidecar must use human-readable .log extension: {}",
            entry.relative_path
        );
        let on_disk = bundle_path.join(&entry.relative_path);
        assert!(
            on_disk.is_file(),
            "extracted file must exist: {}",
            on_disk.display()
        );
        let raw_bytes = fs::read(&on_disk).expect("read app log");
        let actual_sha = sha256_hex(&raw_bytes);
        assert_eq!(
            actual_sha, entry.sha256,
            "app-log SHA-256 must match manifest"
        );
    }
    assert!(
        bundle_path.join("logcat.log").is_file(),
        "human-readable logcat.log must be written alongside timeline.sqlite"
    );
    assert!(
        !bundle_path
            .join("app-logs/com.google.android.gms.log")
            .exists()
    );
    assert!(
        !bundle_path
            .join("app-logs/com.android.systemui.log")
            .exists()
    );

    // 6. Verify `timeline.summary`, `timeline.overview`, and `timeline.query_logs(package=...)`.
    let summary = rpc.call(
        "timeline.summary",
        &json!({ "bundle_id_or_path": bundle_id }),
    );
    assert_eq!(summary["bundle_kind"], "logs_only");
    assert_eq!(summary["app_logs"].as_array().map(Vec::len), Some(2));

    let overview = rpc.call("timeline.overview", &json!({ "bundle_id": bundle_id }));
    assert_eq!(overview["app_logs"].as_array().map(Vec::len), Some(2));

    let shop_logs = rpc.call(
        "timeline.query_logs",
        &json!({ "bundle_id_or_path": bundle_id, "package": "com.example.shop" }),
    );
    let shop_rows = shop_logs["logs"].as_array().expect("logs array");
    assert!(
        shop_rows.iter().any(|row| row["message"]
            .as_str()
            .is_some_and(|m| m.contains("HTTP 503"))),
        "package filter must include com.example.shop app log: {shop_rows:?}"
    );
    assert!(
        !shop_rows.iter().any(|row| row["message"]
            .as_str()
            .is_some_and(|m| m.contains("background sync heartbeat"))),
        "package filter must exclude com.google.android.gms logs"
    );

    server.shutdown();
}

#[test]
fn t6c_screenshot_dir_override_output_dir_and_config_wifi_serial() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let data_dir = paths.data_dir.clone();
    let cfg = DaemonConfig::new(paths).with_screenshot_provider(SharedScreenshotProvider::new(
        Arc::new(StubScreenshotProvider {
            png: valid_1x1_png(),
        }),
    ));
    let server = DaemonServer::start(cfg).expect("start arbd");
    let mut rpc = RpcClient::connect(server.endpoint());

    // 1. Update config with capture_mode, Wi-Fi ADB serial, and screenshot_dir_override.
    let logcat_shots_dir = tmp.path().join("logcat-screenshots");
    let updated = rpc.call(
        "config.update",
        &json!({
            "capture_mode": "logs_only",
            "device_serial": "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp",
            "screenshot_dir_override": logcat_shots_dir,
        }),
    );
    assert_eq!(updated["config"]["capture_mode"], "logs_only");
    assert_eq!(
        updated["config"]["device_serial"],
        "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp"
    );
    assert_eq!(
        updated["config"]["screenshot_dir_override"],
        logcat_shots_dir.to_string_lossy().as_ref()
    );

    // 2. Reject invalid serial & `..` traversal & data_dir state overwrite.
    let (code, _) = rpc.call_err("config.update", &json!({ "device_serial": "bad;rm -rf /" }));
    assert_eq!(code, i64::from(RPC_ERR_INVALID_PARAMS));

    let (code, _) = rpc.call_err(
        "config.update",
        &json!({ "screenshot_dir_override": tmp.path().join("..").join("escape") }),
    );
    assert_eq!(code, i64::from(RPC_ERR_INVALID_PARAMS));

    let (code, _) = rpc.call_err(
        "config.update",
        &json!({ "screenshot_dir_override": data_dir }),
    );
    assert_eq!(code, i64::from(RPC_ERR_INVALID_PARAMS));

    // 3. `save.screenshot` with no explicit output falls back to `screenshot_dir_override`
    //    and inherits `config.device_serial`.
    let shot_default = rpc.call("save.screenshot", &json!({}));
    let shot_path = Path::new(shot_default["path"].as_str().expect("path"));
    assert_eq!(shot_path.parent(), Some(logcat_shots_dir.as_path()));
    assert!(shot_path.is_file());
    assert_eq!(
        shot_default["serial"],
        "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp"
    );

    // 4. `save.screenshot` with explicit `output_dir` overrides `screenshot_dir_override`.
    let explicit_dir = tmp.path().join("explicit-shots");
    let shot_explicit = rpc.call(
        "save.screenshot",
        &json!({ "output_dir": explicit_dir, "serial": "192.168.1.50:5555" }),
    );
    let explicit_path = Path::new(shot_explicit["path"].as_str().expect("path"));
    assert_eq!(explicit_path.parent(), Some(explicit_dir.as_path()));
    assert!(explicit_path.is_file());
    assert_eq!(shot_explicit["serial"], "192.168.1.50:5555");

    // 5. Passing both `output_path` and `output_dir` is rejected cleanly.
    let (code, _) = rpc.call_err(
        "save.screenshot",
        &json!({
            "output_path": explicit_dir.join("a.png"),
            "output_dir": explicit_dir,
        }),
    );
    assert_eq!(code, i64::from(RPC_ERR_INVALID_PARAMS));

    server.shutdown();
}

#[test]
fn screenshot_fallback_supports_relative_data_and_managed_bundle_output_directories() {
    let cwd = std::env::current_dir().unwrap();
    let tmp = TempDir::new_in(&cwd).unwrap();
    let mut paths = isolated_paths(&tmp);
    let data = paths.data_dir.clone();
    paths.data_dir = data.strip_prefix(&cwd).unwrap().join("..").join("data");
    // Keep the IPC endpoint short regardless of the checkout path.
    let ipc = TempDir::new().unwrap();
    paths.runtime_dir = ipc.path().join("run");
    let config = DaemonConfig::new(paths).with_screenshot_provider(SharedScreenshotProvider::new(
        Arc::new(StubScreenshotProvider {
            png: valid_1x1_png(),
        }),
    ));
    let server = DaemonServer::start(config).unwrap();
    let mut rpc = RpcClient::connect(server.endpoint());
    let screenshot = |rpc: &mut RpcClient| {
        let shot = rpc.call("save.screenshot", &json!({}));
        let path = PathBuf::from(shot["path"].as_str().unwrap());
        assert!(path.is_file());
        path
    };
    assert_eq!(
        screenshot(&mut rpc).parent(),
        Some(data.join("screenshots").canonicalize().unwrap().as_path())
    );
    let bundles = data.join("bundles");
    rpc.call("config.update", &json!({"output_dir_override": bundles}));
    assert_eq!(screenshot(&mut rpc).parent(), Some(bundles.as_path()));
    server.shutdown();
}

#[test]
fn invalid_shutdown_keeps_connection_alive() {
    let tmp = TempDir::new().unwrap();
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).unwrap();
    let mut client = RpcClient::connect(server.endpoint());
    let invalid = json!({"jsonrpc":"1.0","method":"system.shutdown","id":0,"params":{}});
    write_frame_line(
        &mut client.writer,
        &invalid.to_string(),
        DEFAULT_MAX_IPC_LINE_BYTES,
    )
    .unwrap();
    let line = read_frame_line(&mut client.reader, DEFAULT_MAX_IPC_LINE_BYTES)
        .unwrap()
        .unwrap();
    assert!(serde_json::from_str::<Value>(&line).unwrap()["error"].is_object());
    assert!(client.call("system.hello", &json!({})).is_object());
    client.call("system.shutdown", &json!({}));
    assert!(server.is_shutting_down());
}

#[test]
fn shutdown_flag_precedes_client_read_of_final_reply() {
    let tmp = TempDir::new().unwrap();
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).unwrap();
    let mut client = RpcClient::connect(server.endpoint());
    let shutdown = json!({"jsonrpc":"2.0","method":"system.shutdown","id":1,"params":{}});
    write_frame_line(
        &mut client.writer,
        &shutdown.to_string(),
        DEFAULT_MAX_IPC_LINE_BYTES,
    )
    .unwrap();
    // In particular, Windows FlushFileBuffers can wait for this client to consume its reply.
    await_condition(|| server.is_shutting_down());
    let line = read_frame_line(&mut client.reader, DEFAULT_MAX_IPC_LINE_BYTES)
        .unwrap()
        .unwrap();
    assert!(serde_json::from_str::<Value>(&line).unwrap()["result"].is_object());
}

#[test]
fn legacy_ancestors_protect_screenshot_destinations() {
    let tmp = TempDir::new().unwrap();
    let legacy = tmp.path().join("external");
    fs::create_dir(&legacy).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/legacy-schema4/replay-trim-parity/replay-20240101-000000-manual-feedface");
    for entry in fs::read_dir(fixture).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), legacy.join(entry.file_name())).unwrap();
    }
    let hashes = |dir: &Path| {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name(),
                    sha256_hex(&fs::read(entry.path()).unwrap()),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let schema3 = tmp.path().join("schema3");
    fs::create_dir(&schema3).unwrap();
    fs::write(
        schema3.join("session.json"),
        json!({"schema":3,"save_id":"test","parts":[]}).to_string(),
    )
    .unwrap();
    let server = DaemonServer::start(
        DaemonConfig::new(isolated_paths(&tmp)).with_screenshot_provider(
            SharedScreenshotProvider::new(Arc::new(StubScreenshotProvider {
                png: valid_1x1_png(),
            })),
        ),
    )
    .unwrap();
    let mut rpc = RpcClient::connect(server.endpoint());
    for (dir, manifest) in [(&legacy, "replay.json"), (&schema3, "session.json")] {
        let before = hashes(dir);
        for params in [
            json!({"output_dir":dir}),
            json!({"output_path":dir.join(manifest),"overwrite":true}),
        ] {
            assert_eq!(
                rpc.call_err("save.screenshot", &params).0,
                i64::from(RPC_ERR_INVALID_PARAMS)
            );
        }
        assert_eq!(before, hashes(dir));
    }
}

#[test]
fn armed_capture_survives_restart_and_explicit_stop() {
    let tmp = TempDir::new().unwrap();
    {
        let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).unwrap();
        let mut rpc = RpcClient::connect(server.endpoint());
        let cfg = rpc.call(
            "config.update",
            &json!({"capture_enabled":true,"device_serial":"absent-test-device"}),
        );
        assert_eq!(cfg["config"]["capture_enabled"], true);
        server.shutdown();
    }
    let server = DaemonServer::start(DaemonConfig::new(isolated_paths(&tmp))).unwrap();
    let mut rpc = RpcClient::connect(server.endpoint());
    assert_eq!(
        rpc.call("system.status", &json!({}))["capture_enabled"],
        true
    );
    rpc.call("capture.stop", &json!({}));
    assert_eq!(
        rpc.call("config.get", &json!({}))["config"]["capture_enabled"],
        false
    );
}

struct ChildDaemon(std::process::Child);
impl Drop for ChildDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn await_condition(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(12);
    while !condition() {
        assert!(Instant::now() < end, "timed out waiting for daemon");
        thread::sleep(Duration::from_millis(25));
    }
}

fn compile_fake_adb(tmp: &TempDir) -> PathBuf {
    use std::process::Command;
    let fake = tmp
        .path()
        .join(format!("fake-adb{}", std::env::consts::EXE_SUFFIX));
    assert!(
        Command::new("rustc")
            .args(["--edition", "2024"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_adb.rs"))
            .arg("-o")
            .arg(&fake)
            .status()
            .expect("run rustc on fake ADB")
            .success(),
        "compile fake ADB"
    );
    fake
}

fn spawn_fake_daemon(paths: &ArbPaths, fake: &Path, dir: &Path) -> ChildDaemon {
    use std::process::{Command, Stdio};
    ChildDaemon(
        Command::new(env!("CARGO_BIN_EXE_arbd"))
            .env("ARB_ADB_PATH", fake)
            .env("ARB_COMPANION_JAR", fake)
            .env("FAKE_DEVICE_STATE", dir)
            .env("ARB_DATA_DIR", &paths.data_dir)
            .env("ARB_CACHE_DIR", &paths.cache_dir)
            .env("ARB_RUNTIME_DIR", &paths.runtime_dir)
            .stdout(Stdio::null())
            .spawn()
            .expect("start isolated arbd with fake ADB"),
    )
}

#[test]
fn explicit_start_survives_process_restart_and_auto_attaches_to_new_device() {
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let fake = compile_fake_adb(&tmp);
    let mut daemon = spawn_fake_daemon(&paths, &fake, tmp.path());
    let mut rpc = RpcClient::connect(&paths.ipc_endpoint());
    rpc.call(
        "config.update",
        &json!({"device_serial":"A","capture_mode":"logs_only","capture_enabled":false}),
    );
    // #583: only a device that was actually selected becomes the persisted pin.
    fs::write(tmp.path().join("devices"), "B device\n").unwrap();
    let start = rpc.call("capture.start", &json!({"serial":"B"}));
    assert_eq!(start["started"], true, "{start}");
    rpc.call("system.shutdown", &json!({}));
    drop(rpc);
    assert!(daemon.0.wait().unwrap().success());
    fs::write(tmp.path().join("devices"), "A device\nB device\n").unwrap();
    let _restarted = spawn_fake_daemon(&paths, &fake, tmp.path());
    let mut rpc = RpcClient::connect(&paths.ipc_endpoint());
    await_condition(|| rpc.call("system.status", &json!({}))["capture"]["active"] == true);
    let status = rpc.call("system.status", &json!({}));
    assert_eq!(status["capture"]["serial"], "B", "{status}");
    assert_eq!(status["configured_device_serial"], "B");
    assert_eq!(status["capture_enabled"], true);
}

#[test]
fn capture_wait_times_out_offline_then_connects_after_discovery() {
    use arb_adb::mock::FaultScenario;
    use arb_adb::wire::{StreamId, WireMessage};
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let fake = compile_fake_adb(&tmp);
    let _daemon = spawn_fake_daemon(&paths, &fake, tmp.path());
    let mut rpc = RpcClient::connect(&paths.ipc_endpoint());
    fs::write(tmp.path().join("devices"), "A offline\n").unwrap();
    let started = Instant::now();
    let result = rpc.send_raw("capture.start", &json!({"serial":"A","capture_mode":"logs_only","wait_connected":true,"wait_timeout_ms":250}));
    assert!(started.elapsed() >= Duration::from_millis(250), "{result}");
    // #583: an explicit serial that never became `device` is an error and persists nothing.
    let data = &result["error"]["data"];
    assert_eq!(data["error_code"], "DEVICE_NOT_FOUND", "{result}");
    assert_eq!(data["details"]["wait_timed_out"], true, "{result}");
    assert_eq!(data["details"]["config_changed"], json!({}));
    let cfg = rpc.call("config.get", &json!({}))["config"].clone();
    assert_eq!(cfg["device_serial"], Value::Null);
    assert_eq!(cfg["capture_enabled"], false);
    // Reserving an offline device ahead of time is `config.update { device_serial }`.
    rpc.call("config.update", &json!({"device_serial":"A"}));

    let Some(CaptureEvent::Hello { payload, .. }) =
        FaultScenario::NormalFhd30.build().next_event().unwrap()
    else {
        panic!("expected fixture Hello")
    };
    WireMessage::Hello(payload)
        .to_frame(StreamId::Control, FrameFlags::NONE, 1)
        .unwrap()
        .write_to(&mut fs::File::create(tmp.path().join("hello.bin")).unwrap())
        .unwrap();
    let endpoint = paths.ipc_endpoint();
    let waiter = thread::spawn(move || {
        RpcClient::connect(&endpoint).call(
            "capture.start",
            &json!({"capture_mode":"logs_only","wait_connected":true,"wait_timeout_ms":5000}),
        )
    });
    await_condition(|| tmp.path().join("discovered").exists());
    fs::write(tmp.path().join("devices"), "A device\n").unwrap();
    let result = waiter.join().unwrap();
    assert_eq!(result["serial"], "A");
    assert_eq!(result["serial_source"], "config");
    assert_eq!(
        result["config_changed"],
        json!({"capture_enabled":[false,true]})
    );
    assert_eq!(result["started"], true);
    assert_eq!(result["connected"], true, "{result}");
    assert_eq!(result["wait_timed_out"], false);
    rpc.call("capture.stop", &json!({}));
}

#[test]
fn capture_intent_rows_match_daemon_responses_and_persisted_config() {
    let table: Value = serde_json::from_str(include_str!(
        "../../../../docs/specs/device-selection-policy-table.json"
    ))
    .unwrap();
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let fake = compile_fake_adb(&tmp);
    let _daemon = spawn_fake_daemon(&paths, &fake, tmp.path());
    let mut rpc = RpcClient::connect(&paths.ipc_endpoint());
    let devices = tmp.path().join("devices");
    let rows = table["capture_intent_rows"].as_array().unwrap();
    assert_eq!(rows.len(), 9);
    for row in rows {
        let id = &row["id"];
        let online: Vec<&str> = row["online"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        let listing: String = online.iter().flat_map(|s| [s, " device\n"]).collect();
        fs::write(&devices, listing).unwrap();
        rpc.call(
            "config.update",
            &json!({"device_serial":row["configured"],"capture_enabled":false,"capture_mode":"logs_only"}),
        );
        if let Some(active) = row["active"].as_str() {
            let setup = rpc.call("capture.start", &json!({"serial":active}));
            assert_eq!(setup["started"], true, "{id}: {setup}");
        } else {
            rpc.call("config.update", &json!({"capture_enabled":row["enabled"]}));
        }
        let cfg = rpc.call("config.get", &json!({}))["config"].clone();
        assert_eq!(cfg["capture_enabled"], row["enabled"], "{id}: precondition");
        assert_eq!(
            cfg["device_serial"], row["configured"],
            "{id}: precondition"
        );
        let before = rpc.call("system.status", &json!({}))["capture"].clone();
        let mut params = json!({"serial":row["argument"]});
        if row["wait_connected"] == true {
            params["wait_connected"] = json!(true);
            params["wait_timeout_ms"] = json!(500);
        }
        let start = rpc.send_raw("capture.start", &params);
        match row["outcome"].as_str().unwrap() {
            outcome @ ("device_not_found" | "conflict") => {
                let data = &start["error"]["data"];
                assert_eq!(data["error_code"], row["error_code"], "{id}: {start}");
                assert_eq!(data["details"]["config_changed"], json!({}), "{id}");
                for (key, value) in row["error_details"].as_object().unwrap() {
                    assert_eq!(&data["details"][key], value, "{id}: details.{key}");
                }
                let hint = data["hint"].as_str().unwrap();
                let needle = if outcome == "conflict" {
                    "arb_capture_stop"
                } else {
                    "arb_devices"
                };
                assert!(hint.contains(needle), "{id}: {hint}");
                let after = rpc.call("system.status", &json!({}))["capture"].clone();
                assert_eq!(after["active"], before["active"], "{id}: session kept");
                assert_eq!(after["serial"], before["serial"], "{id}: session kept");
            }
            outcome => {
                let start = start["result"].clone();
                assert!(start.is_object(), "{id}: {start}");
                assert_eq!(start["config_changed"], row["config_changed"], "{id}");
                assert_eq!(start["serial_source"], row["serial_source"], "{id}");
                assert_eq!(
                    start["started"],
                    json!(outcome != "waiting"),
                    "{id}: {start}"
                );
                assert_eq!(
                    start.get("already_active").cloned().unwrap_or(json!(false)),
                    json!(outcome == "already_active"),
                    "{id}"
                );
                let codes: Vec<Value> = start
                    .get("warnings")
                    .and_then(Value::as_array)
                    .map_or_else(Vec::new, |w| w.iter().map(|w| w["code"].clone()).collect());
                assert_eq!(json!(codes), row["warning_codes"], "{id}");
            }
        }
        let cfg = rpc.call("config.get", &json!({}))["config"].clone();
        assert_eq!(cfg["device_serial"], row["persisted"], "{id}");
        assert_eq!(cfg["capture_enabled"], row["persisted_enabled"], "{id}");
        let status = rpc.call("system.status", &json!({}));
        assert_eq!(
            status.get("configured_device_serial"),
            Some(&row["persisted"]),
            "{id}"
        );
        assert_eq!(status["serial_mismatch"], false, "{id}");
        let stop_changes = if row["persisted_enabled"] == true {
            json!({"capture_enabled":[true,false]})
        } else {
            json!({})
        };
        assert_eq!(
            rpc.call("capture.stop", &json!({}))["config_changed"],
            stop_changes,
            "{id}"
        );
        assert_eq!(
            rpc.call("capture.stop", &json!({}))["config_changed"],
            json!({})
        );
        assert_eq!(
            rpc.call("config.get", &json!({}))["config"]["device_serial"],
            row["persisted"]
        );
    }
}

/// #583 end to end: a typo serial while recording, then while stopped, then a real device.
#[test]
fn issue_583_unknown_serial_never_persists_and_never_blocks_recording() {
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let fake = compile_fake_adb(&tmp);
    let _daemon = spawn_fake_daemon(&paths, &fake, tmp.path());
    let mut rpc = RpcClient::connect(&paths.ipc_endpoint());
    fs::write(tmp.path().join("devices"), "A device\n").unwrap();
    rpc.call("config.update", &json!({"capture_mode":"logs_only"}));
    let config = |rpc: &mut RpcClient| rpc.call("config.get", &json!({}))["config"].clone();

    // (1) Recording on A (auto-selected), then an unknown serial B: CONFLICT, A keeps recording.
    let first = rpc.call("capture.start", &json!({}));
    assert_eq!(first["serial"], "A", "{first}");
    let saved = config(&mut rpc);
    let conflict = rpc.send_raw("capture.start", &json!({"serial":"1.2.3.4:5555"}));
    let data = &conflict["error"]["data"];
    assert_eq!(data["error_code"], "CONFLICT", "{conflict}");
    assert_eq!(data["details"]["active_serial"], "A");
    assert_eq!(data["details"]["requested_serial"], "1.2.3.4:5555");
    assert!(data["hint"].as_str().unwrap().contains("arb_capture_stop"));
    let status = rpc.call("system.status", &json!({}));
    assert_eq!(status["capture"]["active"], true);
    assert_eq!(status["capture"]["serial"], "A");
    assert_eq!(
        config(&mut rpc),
        saved,
        "CONFLICT leaves config.json untouched"
    );
    // The same serial (or none) is an idempotent success that keeps the session.
    let again = rpc.call("capture.start", &json!({}));
    assert_eq!(again["already_active"], true, "{again}");
    assert_eq!(again["started"], true);
    rpc.call("capture.stop", &json!({}));

    // (2) Stopped, unknown serial: DEVICE_NOT_FOUND, nothing persisted, A still starts.
    let before = config(&mut rpc);
    assert_eq!(before["device_serial"], Value::Null);
    let missing = rpc.send_raw("capture.start", &json!({"serial":"1.2.3.4:5555"}));
    let data = &missing["error"]["data"];
    assert_eq!(data["error_code"], "DEVICE_NOT_FOUND", "{missing}");
    assert_eq!(data["retryable"], true);
    assert_eq!(data["details"]["serial"], "1.2.3.4:5555");
    assert_eq!(
        data["details"]["selection"]["other_online_serials"],
        json!(["A"])
    );
    assert!(data["hint"].as_str().unwrap().contains("[A]"), "{missing}");
    assert_eq!(
        config(&mut rpc),
        before,
        "DEVICE_NOT_FOUND leaves config.json untouched"
    );
    let resumed = rpc.call("capture.start", &json!({}));
    assert_eq!(resumed["started"], true, "{resumed}");
    assert_eq!(resumed["serial"], "A");
    assert_eq!(resumed["serial_source"], "auto");
    rpc.call("capture.stop", &json!({}));

    // (3) Explicit A that is online: persisted only after the capture started.
    let pinned = rpc.call("capture.start", &json!({"serial":"A"}));
    assert_eq!(pinned["started"], true, "{pinned}");
    assert_eq!(
        pinned["config_changed"],
        json!({"capture_enabled":[false,true],"device_serial":[null,"A"]})
    );
    let after = config(&mut rpc);
    assert_eq!(after["device_serial"], "A");
    assert_eq!(after["capture_enabled"], true);
    // The confirmed pin survives an unplug/replug: auto-attach returns to A, never to B.
    fs::write(tmp.path().join("devices"), "B device\n").unwrap();
    rpc.call("capture.stop", &json!({}));
    rpc.call("config.update", &json!({"capture_enabled":true}));
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        rpc.call("system.status", &json!({}))["capture"]["active"],
        false,
        "DS-07: never switches to the other online device"
    );
    fs::write(tmp.path().join("devices"), "A device\nB device\n").unwrap();
    await_condition(|| rpc.call("system.status", &json!({}))["capture"]["active"] == true);
    assert_eq!(
        rpc.call("system.status", &json!({}))["capture"]["serial"],
        "A"
    );
    rpc.call("capture.stop", &json!({}));
}

/// A start racing an auto-attach discovery queues behind it instead of cancelling it.
#[test]
fn capture_start_waits_for_auto_attach_reservation_instead_of_cancelling_it() {
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let fake = compile_fake_adb(&tmp);
    let dir = tmp.path();
    fs::write(dir.join("devices"), "A device\n").unwrap();
    let _daemon = spawn_fake_daemon(&paths, &fake, dir);
    let mut rpc = RpcClient::connect(&paths.ipc_endpoint());
    rpc.call(
        "config.update",
        &json!({"capture_mode":"logs_only","device_serial":"A"}),
    );
    // Block the next `adb devices`, then arm so that auto-attach reserves and enters discovery.
    fs::write(dir.join("block"), []).unwrap();
    rpc.call("config.update", &json!({"capture_enabled":true}));
    await_condition(|| dir.join("discovery-entered").exists());
    let endpoint = paths.ipc_endpoint();
    let start = thread::spawn(move || {
        RpcClient::connect(&endpoint).send_raw("capture.start", &json!({"serial":"A"}))
    });
    thread::sleep(Duration::from_millis(300));
    fs::remove_file(dir.join("block")).unwrap();
    let result = start.join().unwrap();
    let result = &result["result"];
    // Auto-attach finished its own attach on A; the explicit start then saw it as the same
    // session and succeeded idempotently, pinning A.
    assert_eq!(result["started"], true, "{result}");
    assert_eq!(result["serial"], "A");
    assert_eq!(result["already_active"], true, "{result}");
    let status = rpc.call("system.status", &json!({}));
    assert_eq!(status["capture"]["serial"], "A");
    assert_eq!(status["capture_enabled"], true);
    rpc.call("capture.stop", &json!({}));
}

#[test]
fn live_supervisor_waits_selects_reconnects_and_cancels_slow_discovery() {
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let fake = compile_fake_adb(&tmp);
    let dir = tmp.path();
    let devices = dir.join("devices");
    let ledger = paths.data_dir.join("adb-sessions");
    fs::create_dir_all(&ledger).unwrap();
    let write_ledger = |pid| {
        let token = format!("s-100-{pid}-1");
        let record = json!({"pid":pid,"serial":"A","deployed":{"session_token":token,
            "companion_jar":format!("/data/local/tmp/arb-{token}-companion.dex.jar"),"scrcpy_jar":null},
            "forwards":[[4321,format!("arb-companion-{token}")],[4322,format!("arb-companion-{token}")]]});
        let path = ledger.join(format!("{token}.json"));
        fs::write(&path, record.to_string()).unwrap();
        (token, path)
    };
    let (token, stale) = write_ledger(2_000_000_001);
    let (_, live) = write_ledger(std::process::id());
    fs::write(
        dir.join("forwards"),
        format!(
            "A tcp:4321 localabstract:arb-companion-{token}\nA tcp:4322 localabstract:other-tool\n"
        ),
    )
    .unwrap();
    let mut daemon = spawn_fake_daemon(&paths, &fake, dir);
    let endpoint = paths.ipc_endpoint();
    let mut rpc = RpcClient::connect(&endpoint);
    await_condition(|| !stale.exists());
    assert!(live.exists());
    let cleaned = fs::read_to_string(dir.join("reaped")).unwrap();
    assert!(cleaned.contains("forward --remove tcp:4321"));
    assert!(cleaned.contains(&format!("arb-{token}-companion.dex.jar")));
    assert!(!cleaned.contains("4322"));
    let armed = rpc.call("capture.start", &json!({"mode":"logs_only"}));
    assert_eq!(armed["started"], false);
    assert_eq!(armed["selection"]["status"], "no_online_devices");
    fs::write(&devices, "A device\nB device\n").unwrap();
    let multiple = rpc.call("capture.start", &json!({"mode":"logs_only"}));
    assert_eq!(
        multiple["selection"]["status"],
        "multiple_devices_need_selection"
    );
    rpc.call("config.update", &json!({"device_serial":"A"}));
    fs::write(&devices, "B device\n").unwrap();
    let waiting = rpc.call("capture.start", &json!({"mode":"logs_only"}));
    assert_eq!(waiting["started"], false);
    assert_eq!(
        rpc.call("system.status", &json!({}))["capture_enabled"],
        true
    );
    fs::write(&devices, "A device\nB device\n").unwrap();
    await_condition(|| rpc.call("system.status", &json!({}))["capture"]["active"] == true);
    assert_eq!(
        rpc.call("system.status", &json!({}))["capture"]["serial"],
        "A"
    );
    fs::write(&devices, []).unwrap();
    await_condition(|| rpc.call("system.status", &json!({}))["capture"]["active"] == false);
    assert_eq!(
        rpc.call("system.status", &json!({}))["capture_enabled"],
        true
    );
    fs::write(&devices, "A device\n").unwrap();
    await_condition(|| rpc.call("system.status", &json!({}))["capture"]["active"] == true);
    rpc.call("capture.stop", &json!({}));
    fs::write(dir.join("block"), []).unwrap();
    let other_endpoint = endpoint.clone();
    let start = thread::spawn(move || {
        RpcClient::connect(&other_endpoint).send_raw("capture.start", &json!({}))
    });
    await_condition(|| dir.join("discovery-entered").exists());
    let (done, result) = std::sync::mpsc::channel();
    let stop = thread::spawn(move || {
        done.send(RpcClient::connect(&endpoint).call("capture.stop", &json!({})))
            .unwrap();
    });
    let stopped = result.recv_timeout(Duration::from_secs(2));
    fs::remove_file(dir.join("block")).unwrap();
    assert!(stopped.is_ok(), "stop waited for ADB discovery");
    stop.join().unwrap();
    assert!(start.join().unwrap()["error"].is_object());
    let status = rpc.call("system.status", &json!({}));
    assert_eq!(status["capture"]["active"], false);
    assert_eq!(status["capture_enabled"], false);
    rpc.call("system.shutdown", &json!({}));
    assert!(daemon.0.wait().unwrap().success());
}
