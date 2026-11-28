//! Full stdio integration test for `arb-cli mcp` and `arb-cli share-mp4` against a live `arbd`.

#![allow(
    clippy::too_many_lines,
    reason = "end-to-end scenario tests intentionally read as linear scripts"
)]

use std::f64::consts::PI;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use arb_core::analytics::PNG_SIGNATURE;
use arb_core::bundle::{
    AudioStreamMeta, AvOffsetStatus, BundleKind, DeviceInfo, EpochSpan, FrameRecord,
    InputStreamMeta, LogCompleteness, LogRecord, LogcatStreamMeta, Provenance, RecordBucket,
    RequestedWindow, StreamStatus, StreamsSummary, TapRecord, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::gesture::GestureKind;
use arb_core::time::{TimeAlignment, TimestampSemantics};
use arb_media::audio::{
    AacMuxInput, AacPacket, build_aac_lc_audio_specific_config, mux_aac_to_m4a,
};
use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
use arb_media::mp4::{Mp4VideoTrack, inspect_mp4_atoms, mux_raw_h264_to_mp4};
use arb_os::paths::ArbPaths;
use arb_storage::bundle_io::{MediaPayload, V2BundleDraft, V2BundleWriter};
use arbd::error::DaemonError;
use arbd::screenshot::{ScreenshotProvider, SharedScreenshotProvider};
use arbd::{DaemonConfig, DaemonServer};
use rusty_aac::{AacEncoder, AacEncoderConfig};
use serde_json::{Value, json};
use tempfile::TempDir;

const SEC_NS: i64 = 1_000_000_000;
const BASE_BT_NS: i64 = 200 * SEC_NS;
const FRAME_STEP_NS: i64 = 33_333_333;

#[derive(Debug)]
struct MockScreenshotProvider {
    png_bytes: Vec<u8>,
}

impl ScreenshotProvider for MockScreenshotProvider {
    fn capture_png(&self, serial: Option<&str>) -> Result<(String, Vec<u8>), DaemonError> {
        Ok((
            serial.unwrap_or("emu-mcp-5554").to_owned(),
            self.png_bytes.clone(),
        ))
    }
}

fn build_minimal_valid_png(width: u32, height: u32) -> Vec<u8> {
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

fn fixture_tap() -> TapRecord {
    TapRecord {
        seq: 1,
        boot_id: Some("boot-mcp".to_owned()),
        event_mono_ns: Some(BASE_BT_NS + 6 * FRAME_STEP_NS),
        pc_ingest_cont_ns: BASE_BT_NS + 6 * FRAME_STEP_NS,
        alignment: TimeAlignment::Anchored {
            boot_id: "boot-mcp".to_owned(),
            boottime_ns: BASE_BT_NS + 6 * FRAME_STEP_NS,
            half_width_ns: 500_000,
            semantics: TimestampSemantics::InputEventTime,
        },
        bucket: RecordBucket::Main,
        action: "TAP".to_owned(),
        x: Some(32.0),
        y: Some(32.0),
        pointer_id: Some(0),
        gesture_kind: GestureKind::Tap,
        end_x: None,
        end_y: None,
        end_t_offset_ns: Some(20_000_000),
        path: Vec::new(),
    }
}

fn seed_bundle(work_dir: &Path, dest_dir: &Path, bundle_id: &str) -> PathBuf {
    seed_bundle_with_events(
        work_dir,
        dest_dir,
        bundle_id,
        Vec::new(),
        vec![fixture_tap()],
    )
}

/// Seeds an inspection bundle with the specified taps and extra main-timeline log rows.
fn seed_bundle_with_events(
    work_dir: &Path,
    dest_dir: &Path,
    bundle_id: &str,
    extra: Vec<LogRecord>,
    taps: Vec<TapRecord>,
) -> PathBuf {
    let video_src = work_dir.join(format!("{bundle_id}-v.mp4"));
    let frames_raw: Vec<DecodedFrame> = (0..18_u64)
        .map(|i| {
            let luma = u8::try_from(40 + i * 10).expect("luma fits u8");
            let mut yuv = vec![luma; 64 * 64];
            yuv.resize(64 * 64 * 3 / 2, 128);
            DecodedFrame {
                frame_index: i,
                width: 64,
                height: 64,
                format: PixelFormat::Yuv420p,
                data: yuv,
            }
        })
        .collect();
    let v_pkts = OpenH264Backend::new()
        .encode_yuv420p_sequence(&frames_raw, 9, FRAME_STEP_NS, EncodeParams::default())
        .expect("encode H.264");
    mux_raw_h264_to_mp4(&video_src, 64, 64, 90_000, 3_000, &v_pkts).expect("mux video");
    let track = Mp4VideoTrack::open(&video_src).expect("open video");

    let audio_src = work_dir.join(format!("{bundle_id}-a.m4a"));
    let mut pcm = Vec::with_capacity(32_000 * 2);
    for i in 0..32_000_u32 {
        let s = (2.0 * PI * 440.0 * f64::from(i) / 48_000.0).sin() * 0.35;
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "test sine wave f64 -> f32"
        )]
        {
            pcm.push(s as f32);
            pcm.push(s as f32);
        }
    }
    let mut enc = AacEncoder::new(AacEncoderConfig::default());
    enc.push_pcm(&pcm, 2, 48_000).expect("encode AAC");
    enc.finish();
    let mut a_pkts = Vec::new();
    let mut idx = 0_i64;
    while let Ok(pkt) = enc.next_packet() {
        a_pkts.push(AacPacket {
            pts_us: idx * 1024 * 1_000_000 / 48_000,
            data: pkt.data,
        });
        idx += 1;
    }
    let asc = build_aac_lc_audio_specific_config(48_000, 2).expect("ASC");
    mux_aac_to_m4a(
        &audio_src,
        &AacMuxInput {
            audio_specific_config: asc.to_vec(),
            sample_rate_hz: 48_000,
            channels: 2,
            packets: a_pkts,
        },
    )
    .expect("mux audio");

    let frames: Vec<FrameRecord> = track
        .frames()
        .iter()
        .map(|f| {
            let bt = BASE_BT_NS + i64::try_from(f.frame_index).expect("idx") * FRAME_STEP_NS;
            FrameRecord {
                frame_index: f.frame_index,
                boot_id: "boot-mcp".to_owned(),
                pts_device_mono_ns: bt,
                pc_ingest_cont_ns: bt,
                alignment: TimeAlignment::Anchored {
                    boot_id: "boot-mcp".to_owned(),
                    boottime_ns: bt,
                    half_width_ns: 500_000,
                    semantics: TimestampSemantics::VideoEncoderInputEstimate,
                },
                is_keyframe: f.is_keyframe,
                gop_id: f.gop_id,
                mp4_sample_offset: f.offset,
                mp4_sample_size: f.size,
            }
        })
        .collect();

    let mut logs = vec![
        LogRecord {
            seq: 1,
            boot_id: Some("boot-mcp".to_owned()),
            header_epoch_ns: None,
            device_read_boottime_ns: Some(BASE_BT_NS + 2 * FRAME_STEP_NS),
            pc_ingest_cont_ns: BASE_BT_NS + 2 * FRAME_STEP_NS,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-mcp".to_owned(),
                boottime_ns: BASE_BT_NS + 2 * FRAME_STEP_NS,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::LogcatDeviceReadBoottime,
            },
            bucket: RecordBucket::Main,
            level: Some("I".to_owned()),
            tag: Some("ActivityTaskManager".to_owned()),
            pid: Some(500),
            tid: Some(500),
            uid: None,
            package_name: None,
            message: "Displayed com.example.demo/.LoginActivity: +120ms".to_owned(),
        },
        LogRecord {
            seq: 2,
            boot_id: Some("boot-mcp".to_owned()),
            header_epoch_ns: None,
            device_read_boottime_ns: Some(BASE_BT_NS + 8 * FRAME_STEP_NS),
            pc_ingest_cont_ns: BASE_BT_NS + 8 * FRAME_STEP_NS,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-mcp".to_owned(),
                boottime_ns: BASE_BT_NS + 8 * FRAME_STEP_NS,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::LogcatDeviceReadBoottime,
            },
            bucket: RecordBucket::Main,
            level: Some("E".to_owned()),
            tag: Some("AuthClient".to_owned()),
            pid: Some(1234),
            tid: Some(1235),
            uid: Some(10200),
            package_name: Some("com.example.demo".to_owned()),
            message: "Token refresh failed: 401".to_owned(),
        },
    ];
    logs.extend(extra);

    let draft = V2BundleDraft {
        memo: None,
        bundle_kind: BundleKind::Full,
        bundle_id: bundle_id.to_owned(),
        created_at_utc: "2026-11-23T09:10:00Z".to_owned(),
        provenance: Provenance::Original,
        device: DeviceInfo {
            serial: "emu-mcp-5554".to_owned(),
            model: "Pixel_8".to_owned(),
            api_level: 35,
            is_emulator: true,
        },
        requested_window: RequestedWindow::RealElapsedSec {
            post_roll_ms: 0,
            actual_post_roll_ms: 0,
            post_roll_truncated: false,
            duration_sec: 5,
            trigger_pc_wall_utc: "2026-11-23T09:10:00Z".to_owned(),
        },
        epochs: vec![EpochSpan {
            boot_id: "boot-mcp".to_owned(),
            start_boottime_ns: BASE_BT_NS,
            end_boottime_ns: BASE_BT_NS + 17 * FRAME_STEP_NS,
        }],
        streams: StreamsSummary {
            video: Some(VideoStreamMeta {
                status: StreamStatus::Present,
                frame_count: 18,
                tail_may_be_incomplete: false,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
                display_width: Some(64),
                display_height: Some(64),
                carried_in: None,
            }),
            audio: Some(AudioStreamMeta {
                status: StreamStatus::Present,
                av_offset_status: AvOffsetStatus::Calibrated,
                av_offset_ms: 0,
                audio_lead_in_offset_ns: 0,
                tail_may_be_incomplete: false,
            }),
            logcat: Some(LogcatStreamMeta {
                dropped_count: None,
                status: StreamStatus::Present,
                completeness: LogCompleteness::VerifiedNoOverflow,
                overflow_events: 0,
                tail_may_be_incomplete: false,
            }),
            input: Some(InputStreamMeta {
                input_events: None,
                status: StreamStatus::Present,
                tail_may_be_incomplete: false,
            }),
        },
        unanchored_summary: UnanchoredSummary {
            included_count: 0,
            truncated_count: 0,
        },
        video_mp4: Some(MediaPayload::File(video_src)),
        audio_m4a: Some(MediaPayload::File(audio_src)),
        share_mp4: None,
        frames,
        discontinuities: Vec::new(),
        logs,
        taps,
        host_clock_anchor: None,
        device_clock_offset: None,
        clock_anchors: Vec::new(),
    };

    let bundle_path = dest_dir.join(bundle_id);
    V2BundleWriter::write(&bundle_path, &format!("mcp-seed-{bundle_id}"), &draft)
        .expect("write seeded bundle");
    bundle_path
}

fn send_mcp_request(
    stdin: &mut impl Write,
    stdout: &mut BufReader<impl std::io::Read>,
    req: &Value,
) -> Value {
    let line = serde_json::to_string(req).expect("serialize MCP request");
    writeln!(stdin, "{line}").expect("write MCP request");
    stdin.flush().expect("flush MCP request");

    let mut resp_line = String::new();
    let n = stdout.read_line(&mut resp_line).expect("read MCP response");
    assert!(n > 0, "MCP server closed stdout unexpectedly");
    let parsed: Value = serde_json::from_str(resp_line.trim()).unwrap_or_else(|err| {
        panic!("stdout emitted non-JSON-RPC line `{resp_line}`: {err}");
    });
    assert_eq!(
        parsed["jsonrpc"], "2.0",
        "every stdout frame must be jsonrpc 2.0"
    );
    parsed
}

#[test]
fn arb_cli_mcp_full_stdio_workflow_and_share_mp4_cli_e2e() {
    let tmp = TempDir::new().expect("temp dir");
    let paths = ArbPaths {
        data_dir: tmp.path().join("data"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: tmp.path().join("run"),
    };
    let mock_png = build_minimal_valid_png(120, 240);
    let cfg = DaemonConfig::new(paths.clone()).with_screenshot_provider(
        SharedScreenshotProvider::new(Arc::new(MockScreenshotProvider {
            png_bytes: mock_png,
        })),
    );
    let server = DaemonServer::start(cfg).expect("start live arbd");

    let managed_bundles = paths.data_dir.join("bundles");
    let bundle_dir = seed_bundle(tmp.path(), &managed_bundles, "bundle-mcp-e2e");

    // Register seeded bundle in catalog via `arb-cli bundles open` -> `close`.
    let cli_bin = env!("CARGO_BIN_EXE_arb-cli");
    let open_out = Command::new(cli_bin)
        .args([
            "--data-dir",
            paths.data_dir.to_str().expect("utf8"),
            "--cache-dir",
            paths.cache_dir.to_str().expect("utf8"),
            "--runtime-dir",
            paths.runtime_dir.to_str().expect("utf8"),
            "--json",
            "bundles",
            "open",
            bundle_dir.to_str().expect("utf8"),
        ])
        .output()
        .expect("run arb-cli bundles open");
    assert!(
        open_out.status.success(),
        "bundles open failed: {}",
        String::from_utf8_lossy(&open_out.stderr)
    );
    let opened: Value = serde_json::from_slice(&open_out.stdout).expect("parse open json");
    let bundle_id = opened["bundle_id"].as_str().expect("bundle_id").to_owned();

    // Spawn real `arb-cli mcp` child process with piped stdio.
    let mut child = Command::new(cli_bin)
        .args([
            "--data-dir",
            paths.data_dir.to_str().expect("utf8"),
            "--cache-dir",
            paths.cache_dir.to_str().expect("utf8"),
            "--runtime-dir",
            paths.runtime_dir.to_str().expect("utf8"),
            "mcp",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arb-cli mcp");

    let mut stdin = child.stdin.take().expect("mcp stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("mcp stdout"));

    // 1. `initialize`
    let init = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": version::ProtocolVersion::V2025_03_26.as_str(),
                "capabilities": {},
                "clientInfo": { "name": "test-host", "version": "1.0" }
            }
        }),
    );
    assert_eq!(init["id"], 1, "init id");
    assert_eq!(
        init["result"]["protocolVersion"],
        version::ProtocolVersion::V2025_03_26.as_str(),
        "protocolVersion"
    );
    assert_eq!(
        init["result"]["serverInfo"]["name"], "android-replay-buffer",
        "serverInfo name"
    );

    // 2. `notifications/initialized` (MUST emit no response!)
    writeln!(
        stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .expect("write notification");
    stdin.flush().expect("flush notification");

    // 3. `ping` (confirms notification emitted zero stray frames ahead of ping!)
    let ping = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }),
    );
    assert_eq!(
        ping["id"], 2,
        "ping id matches immediately after silent notification"
    );

    // 4. `tools/list`
    let tools_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {} }),
    );
    let tools = tools_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert_eq!(tools.len(), 21, "expected all 21 MCP tools");
    for required in [
        "arb_jobs",
        "arb_status",
        "arb_devices",
        "arb_capture_start",
        "arb_capture_stop",
        "arb_config_get",
        "arb_config_update",
        "arb_save",
        "arb_screenshot",
        "arb_save_logs",
        "arb_save_and_inspect",
        "arb_list_bundles",
        "arb_trash_bundle",
        "arb_restore_bundle",
        "arb_gc_bundles",
        "arb_forget_bundle",
        "arb_timeline_overview",
        "arb_query_logs",
        "arb_inspect_frames",
        "arb_export_trim",
        "arb_export_frame_png",
    ] {
        assert!(
            tools.iter().any(|t| t["name"] == required),
            "missing required MCP tool `{required}`"
        );
    }

    // 5. `tools/call` -> `arb_status`
    let status_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "arb_status", "arguments": {} }
        }),
    );
    assert_eq!(
        status_resp["result"]["isError"], false,
        "arb_status ok: {status_resp}"
    );

    // 6. `tools/call` -> `arb_list_bundles`
    let list_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": { "name": "arb_list_bundles", "arguments": {} }
        }),
    );
    assert_eq!(list_resp["result"]["isError"], false, "arb_list_bundles ok");
    let list_json: Value = serde_json::from_str(
        list_resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("parse list json");
    assert!(
        list_json["bundles"]
            .as_array()
            .expect("bundles")
            .iter()
            .any(|b| b["bundle_id"] == bundle_id),
        "seeded bundle must appear in arb_list_bundles"
    );

    // 7. `tools/call` -> `arb_timeline_overview`
    let ov_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "arb_timeline_overview",
                "arguments": { "bundle_id": bundle_id }
            }
        }),
    );
    assert_eq!(
        ov_resp["result"]["isError"], false,
        "overview ok: {ov_resp}"
    );
    let ov_json: Value = serde_json::from_str(
        ov_resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("parse overview");
    assert_eq!(ov_json["frame_count"], 18, "overview frame_count");
    assert_eq!(
        ov_json["screens"][0]["short_name"], "LoginActivity",
        "overview screen"
    );
    assert_eq!(ov_json["taps"][0]["frame_index"], 6, "overview tap frame");
    assert_eq!(
        ov_json["app_logs"][0]["package_name"], "com.example.demo",
        "overview must expose extracted foreground app_logs"
    );

    // 7b. `tools/call` -> `arb_query_logs` with `package` filter (T-6C)
    let pkg_logs_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 601,
            "method": "tools/call",
            "params": {
                "name": "arb_query_logs",
                "arguments": {
                    "bundle_id": bundle_id,
                    "package": "com.example.demo"
                }
            }
        }),
    );
    assert_eq!(
        pkg_logs_resp["result"]["isError"], false,
        "arb_query_logs(package) ok: {pkg_logs_resp}"
    );
    let pkg_logs_json: Value = serde_json::from_str(
        pkg_logs_resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("parse pkg logs");
    assert!(
        pkg_logs_json["logs"]
            .as_array()
            .expect("logs")
            .iter()
            .any(|row| row["tag"] == "AuthClient"),
        "package filter must return com.example.demo AuthClient log"
    );
    assert_eq!(
        ov_json["error_log_highlights"][0]["tag"], "AuthClient",
        "overview error highlight"
    );

    // 8. `tools/call` -> `arb_inspect_frames` (`contact_sheet` -> MCP native `image/png`!)
    let sheet_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "arb_inspect_frames",
                "arguments": {
                    "bundle_id": bundle_id,
                    "mode": "contact_sheet", "include_images":true,
                    "max_tiles": 4,
                    "include_tap_marker": true
                }
            }
        }),
    );
    assert_eq!(
        sheet_resp["result"]["isError"], false,
        "inspect contact_sheet ok: {sheet_resp}"
    );
    let content = sheet_resp["result"]["content"]
        .as_array()
        .expect("content blocks");
    assert_eq!(content.len(), 2, "inspect_frames returns [text, image]");
    assert_eq!(
        content[1]["type"], "image",
        "second block must be MCP image"
    );
    assert_eq!(
        content[1]["mimeType"], "image/png",
        "mimeType must be image/png"
    );
    assert!(
        content[1]["data"]
            .as_str()
            .is_some_and(|b64| b64.starts_with("iVBORw0KGgo")),
        "base64 data must encode a valid PNG header"
    );
    let sheet_meta: Value =
        serde_json::from_str(content[0]["text"].as_str().expect("meta text")).expect("meta json");
    assert_eq!(
        sheet_meta["frame_indices_rendered"]
            .as_array()
            .expect("indices")
            .len(),
        4,
        "4 tiles rendered"
    );
    assert!(
        sheet_meta.get("png_base64").is_none(),
        "text metadata should not duplicate giant png_base64"
    );

    // 9. `tools/call` -> `arb_export_trim` (`format: "share_mp4"` with `burn_taps: true`)
    let mut catalog = arb_os::rpc::RpcClient::connect_endpoint(&paths.ipc_endpoint()).unwrap();
    let list_before = catalog.call("bundle.list", json!({})).unwrap();
    let dirs_before = fs::read_dir(&managed_bundles).unwrap().count();
    let dest = tmp.path().join("share-exports");
    let mcp_mp4 = dest.join("mcp-share.mp4");
    let trim_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "arb_export_trim",
                "arguments": {
                    "bundle_id": bundle_id,
                    "format": "share_mp4",
                    "in_frame_index": 2,
                    "out_frame_index": 12,
                    "burn_taps": true,
                    "dest_dir": dest,
                    "output_path": mcp_mp4
                }
            }
        }),
    );
    assert_eq!(
        trim_resp["result"]["isError"], false,
        "arb_export_trim ok: {trim_resp}"
    );
    let trim_json: Value = serde_json::from_str(
        trim_resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("parse trim json");
    assert_eq!(
        trim_json["video_frame_count"], 11,
        "frames 2..=12 = 11 frames"
    );
    assert_eq!(trim_json["edts_box_count"], 0, "zero edts");
    assert_eq!(trim_json["elst_box_count"], 0, "zero elst");
    assert_eq!(trim_json["audio_included"], true, "audio included");
    assert_eq!(catalog.call("bundle.list", json!({})).unwrap(), list_before);
    assert_eq!(fs::read_dir(&managed_bundles).unwrap().count(), dirs_before);
    assert_eq!(fs::read_dir(&dest).unwrap().count(), 1);
    assert!(trim_json.get("trimmed_bundle_id").is_none());
    assert!(mcp_mp4.is_file(), "share.mp4 written on disk");
    let atoms = inspect_mp4_atoms(&mcp_mp4).expect("inspect mcp share.mp4");
    assert_eq!(atoms.edts_count, 0, "disk edts=0");
    assert_eq!(atoms.elst_count, 0, "disk elst=0");

    // 9a. `tools/call` -> `arb_export_trim` (`format: "share_mp4"`, full bundle without frame range)
    // Must match `arb-cli share-mp4` / `export.share_mp4`: no redundant duplicate trimmed bundle.
    let mcp_full_mp4 = tmp.path().join("mcp-full-share.mp4");
    let full_share_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 80,
            "method": "tools/call",
            "params": {
                "name": "arb_export_trim",
                "arguments": {
                    "bundle_id": bundle_id,
                    "format": "share_mp4",
                    "full_range": true,
                    "output_path": mcp_full_mp4
                }
            }
        }),
    );
    assert_eq!(
        full_share_resp["result"]["isError"], false,
        "full share_mp4 ok: {full_share_resp}"
    );
    let full_share_json: Value = serde_json::from_str(
        full_share_resp["result"]["content"][0]["text"]
            .as_str()
            .expect("full share text"),
    )
    .expect("parse full share json");
    assert_eq!(full_share_json["video_frame_count"], 18, "all 18 frames");
    assert!(
        full_share_json["trimmed_bundle_id"].is_null(),
        "full-bundle share_mp4 must not create a redundant trimmed bundle: {full_share_json}"
    );
    assert!(mcp_full_mp4.is_file(), "full share.mp4 written on disk");

    // 9b. `tools/call` -> `arb_export_trim` (`format: "bundle"`, `include_share_mp4: true`, `burn_taps: true`)
    let trim_bundle_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 81,
            "method": "tools/call",
            "params": {
                "name": "arb_export_trim",
                "arguments": {
                    "bundle_id": bundle_id,
                    "format": "bundle",
                    "in_frame_index": 2,
                    "out_frame_index": 12,
                    "include_share_mp4": true,
                    "trash_original": true,
                    "burn_taps": true
                }
            }
        }),
    );
    assert_eq!(
        trim_bundle_resp["result"]["isError"], false,
        "arb_export_trim bundle ok: {trim_bundle_resp}"
    );
    let trim_bundle_json: Value = serde_json::from_str(
        trim_bundle_resp["result"]["content"][0]["text"]
            .as_str()
            .expect("bundle text"),
    )
    .expect("parse trim bundle json");
    assert_eq!(trim_bundle_json["burn_taps_applied"], true);
    assert_eq!(trim_bundle_json["original_trashed"], true);
    let share_sidecar = PathBuf::from(
        trim_bundle_json["share_mp4_path"]
            .as_str()
            .expect("share_mp4_path"),
    );
    assert!(
        share_sidecar.is_file(),
        "trimmed bundle share-taps.mp4 must exist"
    );

    // 10. `tools/call` -> `arb_screenshot` (returns MCP `image/png` + text metadata!)
    let shot_out = tmp.path().join("mcp-shot.png");
    let shot_resp = send_mcp_request(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "arb_screenshot",
                "arguments": {
                    "output_path": shot_out,
                    "include_image": true
                }
            }
        }),
    );
    assert_eq!(
        shot_resp["result"]["isError"], false,
        "arb_screenshot ok: {shot_resp}"
    );
    let shot_content = shot_resp["result"]["content"]
        .as_array()
        .expect("shot content");
    assert_eq!(shot_content.len(), 2, "screenshot returns [text, image]");
    assert_eq!(shot_content[1]["type"], "image", "screenshot image block");
    assert_eq!(
        shot_content[1]["mimeType"], "image/png",
        "screenshot image/png"
    );
    assert!(
        fs::read(&shot_out)
            .expect("read shot")
            .starts_with(&PNG_SIGNATURE),
        "screenshot PNG file valid"
    );

    // Close stdin and verify clean exit with ZERO trailing non-JSON-RPC stdout bytes.
    drop(stdin);
    let mut leftover = String::new();
    let extra = stdout.read_line(&mut leftover).expect("read EOF");
    assert_eq!(
        extra, 0,
        "no trailing stdout data after stdin EOF: {leftover:?}"
    );
    let status = child.wait().expect("wait mcp process");
    assert!(status.success(), "arb-cli mcp must exit 0 cleanly");

    // Also smoke-test `arb-cli share-mp4 --json` subcommand directly.
    // An explicit standalone MP4 does not use the configured bundle directory.
    let blocked = tmp.path().join("not-a-directory");
    fs::write(&blocked, b"file").unwrap();
    arb_os::rpc::RpcClient::connect_endpoint(&paths.ipc_endpoint())
        .unwrap()
        .call("config.update", json!({"output_dir_override":blocked}))
        .unwrap();
    let cli_mp4 = tmp.path().join("cli-share.mp4");
    let share_out = Command::new(cli_bin)
        .args([
            "--data-dir",
            paths.data_dir.to_str().expect("utf8"),
            "--cache-dir",
            paths.cache_dir.to_str().expect("utf8"),
            "--runtime-dir",
            paths.runtime_dir.to_str().expect("utf8"),
            "--json",
            "share-mp4",
            &bundle_id,
            "--out",
            cli_mp4.to_str().expect("utf8"),
        ])
        .output()
        .expect("run arb-cli share-mp4");
    assert!(
        share_out.status.success(),
        "arb-cli share-mp4 failed: {}",
        String::from_utf8_lossy(&share_out.stderr)
    );
    let cli_share_json: Value =
        serde_json::from_slice(&share_out.stdout).expect("parse share-mp4 json");
    assert_eq!(
        cli_share_json["video_frame_count"], 18,
        "CLI share-mp4 full 18 frames"
    );
    assert_eq!(
        cli_share_json["edts_box_count"], 0,
        "CLI share-mp4 zero edts"
    );
    assert!(cli_mp4.is_file(), "CLI share-mp4 output written");

    server.shutdown();
}

#[test]
fn mcp_reports_retryable_saves_with_recoverable_job_and_pins() {
    use arb_adb::mock::FaultScenario;
    use arb_os::rpc::RpcClient;
    use arbd::capture::FaultInjection;
    use std::sync::atomic::Ordering;

    let tmp = TempDir::new().unwrap();
    let paths = ArbPaths {
        data_dir: tmp.path().join("data"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: tmp.path().join("run"),
    };
    let faults = Arc::new(FaultInjection::default());
    let server = DaemonServer::start(
        DaemonConfig::new(paths.clone()).with_fault_injection(Arc::clone(&faults)),
    )
    .unwrap();
    server
        .ingest_device_source_with_mode(
            &mut FaultScenario::NormalFhd30.build(),
            arb_core::config::CaptureMode::LogsOnly,
        )
        .unwrap();
    let mut rpc = RpcClient::connect_endpoint(&paths.ipc_endpoint()).unwrap();
    for version in version::ProtocolVersion::ALL {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arb-cli"))
            .arg("--data-dir")
            .arg(&paths.data_dir)
            .arg("--cache-dir")
            .arg(&paths.cache_dir)
            .arg("--runtime-dir")
            .arg(&paths.runtime_dir)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        send_mcp_request(
            &mut stdin,
            &mut stdout,
            &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":version.as_str(),"capabilities":{},"clientInfo":{"name":"arb-test","version":"1"}}}),
        );
        let schema = send_mcp_request(
            &mut stdin,
            &mut stdout,
            &json!({"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"arb://schemas/arb_jobs"}}),
        );
        let jobs_schema: Value =
            serde_json::from_str(schema["result"]["contents"][0]["text"].as_str().unwrap())
                .unwrap();
        let mut call = |name: &str, args: Value| {
            let response = send_mcp_request(
                &mut stdin,
                &mut stdout,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}),
            );
            assert!(
                !response.to_string().contains("pinned_chunk_ids"),
                "{name}: {response}"
            );
            let result = &response["result"];
            let body: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            if name == "arb_jobs" {
                stdio_contract::assert_valid_jobs_response(&jobs_schema, &body);
            }
            (result["isError"] == true, body)
        };
        for (name, action) in [
            ("arb_save", "retry"),
            ("arb_save_logs", "retarget"),
            ("arb_save_and_inspect", "discard"),
        ] {
            if action == "retry" {
                faults.fail_complete_job_once.store(true, Ordering::Release);
            } else {
                faults
                    .deny_output_after_pin_once
                    .store(true, Ordering::Release);
            }
            let (failed, body) = call(name, json!({}));
            assert!(failed, "{name}: {body}");
            assert_eq!(
                body["error_code"],
                if action == "retry" {
                    "FAILED_RETRYABLE"
                } else {
                    "SAVE_NEEDS_RETARGET"
                }
            );
            assert_eq!(body["retryable"], action == "retry");
            let job = &body["job"];
            assert_eq!(job["state"], "failed_retryable");
            assert!(job["pinned_chunk_count"].as_u64().unwrap() > 0);
            let id = &job["job_id"];
            let (error, list) = call("arb_jobs", json!({"action":"list"}));
            assert!(!error);
            assert!(list["jobs"].as_array().unwrap().iter().any(|j| j == job));
            let raw = rpc.call("save.list_jobs", json!({})).unwrap();
            assert!(
                raw["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|j| &j["job_id"] == id && j["pinned_chunk_ids"].is_array())
            );
            let (error, result) = call("arb_jobs", json!({"action":action,"job_id":id}));
            assert!(!error, "{action}: {result}");
            assert_eq!(&result["job"]["job_id"], id);
            assert_eq!(
                result["job"]["state"],
                if action == "discard" {
                    "discarded"
                } else {
                    "completed"
                }
            );
        }
        drop(stdin);
        assert!(child.wait().unwrap().success());
    }
    drop(rpc);
    drop(server);
}
#[cfg(test)]
mod stdio_contract;
#[path = "../src/mcp/version.rs"]
#[expect(
    dead_code,
    reason = "the external wire client only needs the protocol version names"
)]
mod version;
