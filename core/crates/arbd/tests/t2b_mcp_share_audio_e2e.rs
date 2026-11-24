//! End-to-end integration tests for T-2B RPCs on live `arbd`:
//! - `media.decode_audio_pcm`: real AAC-LC -> cached s16le RIFF/WAV + clean no-audio response
//! - `export.share_mp4`: full & mid-GOP trimmed faststart zero-`elst` MP4 + non-destructive
//!   `trash_original` + refusal on `LegacyRo` + never trash on error
//! - `timeline.overview` & `timeline.query_logs` filters (frame range, level, tag, keyword)
//! - `media.inspect_frames`: `contact_sheet`, `single`, `tap_crop` returning valid PNGs

#![allow(
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools,
    reason = "end-to-end scenario tests intentionally read as linear scripts"
)]

use std::f64::consts::PI;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use arb_core::analytics::PNG_SIGNATURE;
use arb_core::bundle::{
    AudioStreamMeta, AvOffsetStatus, BundleKind, DeviceInfo, EpochSpan, FrameRecord,
    InputStreamMeta, LogCompleteness, LogRecord, LogcatStreamMeta, Provenance, RecordBucket,
    RequestedWindow, StreamStatus, StreamsSummary, TapRecord, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::gesture::GestureKind;
use arb_core::time::{DiscontinuityEvent, DiscontinuityKind, TimeAlignment, TimestampSemantics};
use arb_media::audio::{
    AacMuxInput, AacPacket, Mp4AudioTrack, build_aac_lc_audio_specific_config, mux_aac_to_m4a,
};
use arb_media::audio_decode::inspect_wav_header;
use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
use arb_media::mp4::{Mp4VideoTrack, inspect_mp4_atoms, mux_raw_h264_to_mp4};
use arb_os::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcStream, read_frame_line, write_frame_line};
use arb_os::paths::ArbPaths;
use arb_storage::bundle_io::{MediaPayload, V2BundleDraft, V2BundleWriter};
use arbd::error::RPC_ERR_CONFLICT;
use arbd::{DaemonConfig, DaemonServer};
use rusty_aac::{AacEncoder, AacEncoderConfig};
use serde_json::{Value, json};
use tempfile::TempDir;

const SEC_NS: i64 = 1_000_000_000;
const BASE_BT_NS: i64 = 100 * SEC_NS;
const FRAME_STEP_NS: i64 = 33_333_333;

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

fn build_real_video_mp4(path: &Path, width: u32, height: u32, frame_count: u64) {
    let w = usize::try_from(width).expect("w fits usize");
    let h = usize::try_from(height).expect("h fits usize");
    let frames: Vec<DecodedFrame> = (0..frame_count)
        .map(|i| {
            let luma = u8::try_from(30 + (i * 7) % 200).expect("luma fits u8");
            let mut yuv = vec![luma; w * h];
            yuv.resize(w * h * 3 / 2, 128);
            DecodedFrame {
                frame_index: i,
                width,
                height,
                format: PixelFormat::Yuv420p,
                data: yuv,
            }
        })
        .collect();
    let packets = OpenH264Backend::new()
        .encode_yuv420p_sequence(&frames, 15, FRAME_STEP_NS, EncodeParams::default())
        .expect("encode H.264 frames");
    let w16 = u16::try_from(width).expect("w fits u16");
    let h16 = u16::try_from(height).expect("h fits u16");
    mux_raw_h264_to_mp4(path, w16, h16, 90_000, 3_000, &packets).expect("mux video.mp4");
}

fn build_real_audio_m4a(path: &Path, sample_rate_hz: u32, channels: u16, pcm_frames: usize) {
    let ch = usize::from(channels);
    let mut pcm = Vec::with_capacity(pcm_frames * ch);
    for i in 0..pcm_frames {
        let t = f64::from(u32::try_from(i).expect("idx fits u32")) / f64::from(sample_rate_hz);
        let s = (2.0 * PI * 440.0 * t).sin() * 0.4;
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "test sine wave f64 -> f32"
        )]
        for _ in 0..ch {
            pcm.push(s as f32);
        }
    }
    let mut enc = AacEncoder::new(AacEncoderConfig::default());
    enc.push_pcm(&pcm, channels, sample_rate_hz)
        .expect("encode AAC-LC");
    enc.finish();
    let mut packets = Vec::new();
    let mut idx = 0_i64;
    while let Ok(pkt) = enc.next_packet() {
        packets.push(AacPacket {
            pts_us: idx * 1024 * 1_000_000 / i64::from(sample_rate_hz),
            data: pkt.data,
        });
        idx += 1;
    }
    let asc = build_aac_lc_audio_specific_config(sample_rate_hz, channels).expect("ASC");
    mux_aac_to_m4a(
        path,
        &AacMuxInput {
            audio_specific_config: asc.to_vec(),
            sample_rate_hz,
            channels,
            packets,
        },
    )
    .expect("mux audio.m4a");
}

/// Seeds a rich WORM v2 bundle directly inside `dest_dir` with real H.264 video, optional real
/// 48 kHz stereo AAC-LC audio, screen transitions, taps, crash markers, error logs, and gaps.
fn seed_rich_v2_bundle(
    work_tmp: &Path,
    dest_dir: &Path,
    bundle_id: &str,
    with_audio: bool,
) -> PathBuf {
    let video_src = work_tmp.join(format!("{bundle_id}-src.mp4"));
    build_real_video_mp4(&video_src, 128, 128, 30);
    let track = Mp4VideoTrack::open(&video_src).expect("open video track");

    let audio_src = if with_audio {
        let p = work_tmp.join(format!("{bundle_id}-src.m4a"));
        // 1.2s of 48 kHz stereo AAC-LC to comfortably cover 30 frames (1.0s).
        build_real_audio_m4a(&p, 48_000, 2, 57_600);
        Some(p)
    } else {
        None
    };

    let frames: Vec<FrameRecord> = track
        .frames()
        .iter()
        .map(|f| {
            let bt = BASE_BT_NS + i64::try_from(f.frame_index).expect("idx") * FRAME_STEP_NS;
            FrameRecord {
                frame_index: f.frame_index,
                boot_id: "boot-t2b".to_owned(),
                pts_device_mono_ns: bt,
                pc_ingest_cont_ns: bt,
                alignment: TimeAlignment::Anchored {
                    boot_id: "boot-t2b".to_owned(),
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

    let end_bt = BASE_BT_NS + 29 * FRAME_STEP_NS;

    let logs = vec![
        LogRecord {
            seq: 1,
            boot_id: Some("boot-t2b".to_owned()),
            header_epoch_ns: None,
            device_read_boottime_ns: Some(BASE_BT_NS + 2 * FRAME_STEP_NS),
            pc_ingest_cont_ns: BASE_BT_NS + 2 * FRAME_STEP_NS,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-t2b".to_owned(),
                boottime_ns: BASE_BT_NS + 2 * FRAME_STEP_NS,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::LogcatDeviceReadBoottime,
            },
            bucket: RecordBucket::Main,
            level: Some("I".to_owned()),
            tag: Some("wm_resumed_activity".to_owned()),
            pid: Some(1000),
            tid: Some(1000),
            uid: None,
            package_name: None,
            message: "[0,12345,com.example.shop/.CheckoutActivity]".to_owned(),
        },
        LogRecord {
            seq: 2,
            boot_id: Some("boot-t2b".to_owned()),
            header_epoch_ns: None,
            device_read_boottime_ns: Some(BASE_BT_NS + 10 * FRAME_STEP_NS),
            pc_ingest_cont_ns: BASE_BT_NS + 10 * FRAME_STEP_NS,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-t2b".to_owned(),
                boottime_ns: BASE_BT_NS + 10 * FRAME_STEP_NS,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::LogcatDeviceReadBoottime,
            },
            bucket: RecordBucket::Main,
            level: Some("E".to_owned()),
            tag: Some("PaymentRepo".to_owned()),
            pid: Some(4200),
            tid: Some(4201),
            uid: Some(10242),
            package_name: Some("com.example.shop".to_owned()),
            message: "HTTP 500 timeout charging card".to_owned(),
        },
        LogRecord {
            seq: 3,
            boot_id: Some("boot-t2b".to_owned()),
            header_epoch_ns: None,
            device_read_boottime_ns: Some(BASE_BT_NS + 20 * FRAME_STEP_NS),
            pc_ingest_cont_ns: BASE_BT_NS + 20 * FRAME_STEP_NS,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-t2b".to_owned(),
                boottime_ns: BASE_BT_NS + 20 * FRAME_STEP_NS,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::LogcatDeviceReadBoottime,
            },
            bucket: RecordBucket::Main,
            level: Some("F".to_owned()),
            tag: Some("am_crash".to_owned()),
            pid: Some(1000),
            tid: Some(1000),
            uid: None,
            package_name: None,
            message: "[0,4200,com.example.shop,8912896,java.lang.IllegalStateException,Cart empty,Checkout.kt,88]".to_owned(),
        },
    ];

    let taps = vec![TapRecord {
        seq: 1,
        boot_id: Some("boot-t2b".to_owned()),
        event_mono_ns: Some(BASE_BT_NS + 10 * FRAME_STEP_NS),
        pc_ingest_cont_ns: BASE_BT_NS + 10 * FRAME_STEP_NS,
        alignment: TimeAlignment::Anchored {
            boot_id: "boot-t2b".to_owned(),
            boottime_ns: BASE_BT_NS + 10 * FRAME_STEP_NS,
            half_width_ns: 500_000,
            semantics: TimestampSemantics::InputEventTime,
        },
        bucket: RecordBucket::Main,
        action: "TAP".to_owned(),
        x: Some(64.0),
        y: Some(80.0),
        pointer_id: Some(0),
        gesture_kind: GestureKind::Tap,
        end_x: None,
        end_y: None,
        end_t_offset_ns: Some(15_000_000),
        path: Vec::new(),
    }];

    let discontinuities = vec![DiscontinuityEvent {
        kind: DiscontinuityKind::StreamGap,
        boot_id: Some("boot-t2b".to_owned()),
        start_boottime_ns: Some(BASE_BT_NS + 14 * FRAME_STEP_NS),
        end_boottime_ns: Some(BASE_BT_NS + 15 * FRAME_STEP_NS),
        stream: None,
        pc_cont_start_ns: BASE_BT_NS + 14 * FRAME_STEP_NS,
        pc_cont_end_ns: Some(BASE_BT_NS + 15 * FRAME_STEP_NS),
        delta_ns: None,
        detail: Some("synthetic gap".to_owned()),
    }];

    let audio_meta = if with_audio {
        Some(AudioStreamMeta {
            status: StreamStatus::Present,
            av_offset_status: AvOffsetStatus::Calibrated,
            av_offset_ms: -20,
            audio_lead_in_offset_ns: -20_000_000,
            tail_may_be_incomplete: false,
        })
    } else {
        Some(AudioStreamMeta {
            status: StreamStatus::Absent,
            av_offset_status: AvOffsetStatus::Uncalibrated,
            av_offset_ms: 0,
            audio_lead_in_offset_ns: 0,
            tail_may_be_incomplete: false,
        })
    };

    let draft = V2BundleDraft {
        bundle_kind: BundleKind::Full,
        bundle_id: bundle_id.to_owned(),
        created_at_utc: "2026-11-23T09:00:00Z".to_owned(),
        provenance: Provenance::Original,
        device: DeviceInfo {
            serial: "emu-5554".to_owned(),
            model: "Pixel_8".to_owned(),
            api_level: 35,
            is_emulator: true,
        },
        requested_window: RequestedWindow::RealElapsedSec {
            duration_sec: 10,
            trigger_pc_wall_utc: "2026-11-23T09:00:00Z".to_owned(),
        },
        epochs: vec![EpochSpan {
            boot_id: "boot-t2b".to_owned(),
            start_boottime_ns: BASE_BT_NS,
            end_boottime_ns: end_bt,
        }],
        streams: StreamsSummary {
            video: Some(VideoStreamMeta {
                status: StreamStatus::Present,
                frame_count: 30,
                tail_may_be_incomplete: false,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
                display_width: None,
                display_height: None,
            }),
            audio: audio_meta,
            logcat: Some(LogcatStreamMeta {
                status: StreamStatus::Present,
                completeness: LogCompleteness::VerifiedNoOverflow,
                overflow_events: 0,
                tail_may_be_incomplete: false,
            }),
            input: Some(InputStreamMeta {
                status: StreamStatus::Present,
                tail_may_be_incomplete: false,
            }),
        },
        unanchored_summary: UnanchoredSummary {
            included_count: 0,
            truncated_count: 0,
        },
        video_mp4: Some(MediaPayload::File(video_src)),
        audio_m4a: audio_src.map(MediaPayload::File),
        frames,
        discontinuities,
        logs,
        taps,
        clock_anchors: Vec::new(),
    };

    let bundle_path = dest_dir.join(bundle_id);
    V2BundleWriter::write(&bundle_path, &format!("seed-{bundle_id}"), &draft)
        .expect("write seeded v2 bundle");
    bundle_path
}

/// Seeds a minimal legacy schema-4 read-only folder containing a valid video.mp4 + replay.json.
fn seed_legacy_ro_bundle(work_tmp: &Path, dir: &Path) {
    fs::create_dir_all(dir).expect("create legacy dir");
    let video_path = dir.join("video.mp4");
    build_real_video_mp4(&video_path, 64, 64, 10);
    let _ = work_tmp;
    let manifest = json!({
        "schema": 4,
        "capture_mode": "VIDEO_AND_LOGS",
        "complete": true,
        "save_id": "s4-t2b-legacy-01",
        "folder_name": "legacy-ro",
        "trigger": "manual",
        "files": {
            "readme": null,
            "record": "replay.json",
            "video": "video.mp4",
            "video_frames": null,
            "video_source": null,
            "logcat_device": null,
            "logcat_app": null,
            "touches": null
        },
        "counts": {"device_log_rows": 0, "app_log_rows": 0, "video_frames": 0},
        "save_request": {"content": "VIDEO_AND_LOGS", "origin": "CLI", "video_excluded": false},
        "timebase": {
            "domain": "device_boottime",
            "known": true,
            "window_start_elapsed_ns": "1000000000",
            "window_duration_ns": "333000000",
            "window_end_uncertainty_ns": "0"
        },
        "parts": [],
        "files_sha256": {}
    });
    fs::write(
        dir.join("replay.json"),
        serde_json::to_string_pretty(&manifest).expect("serialize legacy manifest"),
    )
    .expect("write legacy replay.json");
}

#[test]
fn t2b_decode_audio_pcm_decodes_real_aac_caches_and_handles_no_audio_cleanly() {
    let tmp = TempDir::new().expect("temp dir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut rpc = RpcClient::connect(server.endpoint());

    let managed_bundles = paths.data_dir.join("bundles");
    let with_audio_dir = seed_rich_v2_bundle(tmp.path(), &managed_bundles, "b-audio", true);
    let no_audio_dir = seed_rich_v2_bundle(tmp.path(), &managed_bundles, "b-silent", false);

    // Register both bundles via `bundle.open` (simulating active viewer lease).
    let open1 = rpc.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": with_audio_dir,
            "holder_kind": "ide_viewer",
            "holder_pid": std::process::id(),
        }),
    );
    let b_audio_id = open1["bundle_id"].as_str().expect("bundle_id").to_owned();

    let open2 = rpc.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": no_audio_dir,
            "holder_kind": "ide_viewer",
            "holder_pid": std::process::id(),
        }),
    );
    let b_silent_id = open2["bundle_id"].as_str().expect("bundle_id").to_owned();

    // First decode: decodes AAC-LC -> cached WAV.
    let dec1 = rpc.call(
        "media.decode_audio_pcm",
        &json!({ "bundle_id": b_audio_id }),
    );
    assert_eq!(dec1["available"], true, "audio bundle must be available");
    assert_eq!(dec1["cached"], false, "first call must decode fresh");
    assert_eq!(dec1["sample_rate_hz"], 48_000, "sample rate must be 48 kHz");
    assert_eq!(dec1["channels"], 2, "stereo channels");
    assert_eq!(dec1["av_offset_ms"], -20, "av_offset_ms preserved");
    assert!(
        dec1["duration_ns"].as_u64().unwrap_or(0) >= 1_000_000_000,
        "duration_ns >= 1s: {dec1}"
    );
    let wav_path = PathBuf::from(dec1["wav_path"].as_str().expect("wav_path"));
    assert!(
        wav_path.starts_with(paths.cache_dir.join("audio-pcm")),
        "WAV must be cached under <cache_dir>/audio-pcm/: {}",
        wav_path.display()
    );
    let hdr = inspect_wav_header(&wav_path).expect("WAV header must be valid RIFF/PCM");
    assert_eq!(hdr.sample_rate_hz, 48_000, "WAV header sample rate");
    assert_eq!(hdr.channels, 2, "WAV header channels");

    // Second call: cache hit!
    let dec2 = rpc.call(
        "media.decode_audio_pcm",
        &json!({ "bundle_id": b_audio_id }),
    );
    assert_eq!(dec2["available"], true, "cached audio available");
    assert_eq!(dec2["cached"], true, "second call must be a cache hit");
    assert_eq!(dec2["wav_path"], dec1["wav_path"], "same wav_path returned");

    // Bundle without audio returns `{ available: false }` cleanly without error.
    let dec_none = rpc.call(
        "media.decode_audio_pcm",
        &json!({ "bundle_id": b_silent_id }),
    );
    assert_eq!(
        dec_none["available"], false,
        "no-audio bundle must report available=false"
    );
    assert!(
        dec_none["wav_path"].is_null(),
        "no-audio wav_path must be null"
    );
    assert_eq!(dec_none["sample_rate_hz"], 0, "no-audio sample_rate_hz=0");

    server.shutdown();
}

#[test]
fn t2b_export_share_mp4_full_trimmed_trash_original_and_legacy_refusal() {
    let tmp = TempDir::new().expect("temp dir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut rpc = RpcClient::connect(server.endpoint());

    let managed_bundles = paths.data_dir.join("bundles");
    let src_dir = seed_rich_v2_bundle(tmp.path(), &managed_bundles, "b-share-src", true);
    let open = rpc.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": src_dir,
            "holder_kind": "cli",
            "holder_pid": std::process::id(),
        }),
    );
    let src_id = open["bundle_id"].as_str().expect("src_id").to_owned();
    let lease_id = open["lease_id"].as_str().expect("lease_id").to_owned();
    let _ = rpc.call("bundle.close", &json!({ "lease_id": lease_id }));

    // 1. Full bundle export (no trim).
    let full_mp4 = tmp.path().join("full-share.mp4");
    let full_resp = rpc.call(
        "export.share_mp4",
        &json!({
            "bundle_id": src_id,
            "output_path": full_mp4,
        }),
    );
    assert_eq!(
        full_resp["audio_included"], true,
        "full share must include audio"
    );
    assert_eq!(
        full_resp["video_frame_count"], 30,
        "full share has all 30 frames"
    );
    assert_eq!(full_resp["edts_box_count"], 0, "zero edts");
    assert_eq!(full_resp["elst_box_count"], 0, "zero elst");
    assert_eq!(full_resp["original_trashed"], false, "original not trashed");
    assert!(
        full_resp.get("trimmed_bundle_id").is_none(),
        "no trimmed bundle for full export"
    );
    let atoms = inspect_mp4_atoms(&full_mp4).expect("inspect full share.mp4");
    assert_eq!(atoms.edts_count, 0, "on-disk edts=0");
    assert_eq!(atoms.elst_count, 0, "on-disk elst=0");
    assert_eq!(
        Mp4VideoTrack::open(&full_mp4).expect("video").frame_count(),
        30,
        "on-disk video frame count"
    );
    assert!(
        Mp4AudioTrack::open(&full_mp4)
            .expect("audio")
            .packet_count()
            > 0,
        "on-disk audio packets present"
    );

    // 2. Failing export (`AlreadyExists`) with `trash_original: true` MUST NOT trash source!
    let (_code, _msg) = rpc.call_err(
        "export.share_mp4",
        &json!({
            "bundle_id": src_id,
            "output_path": full_mp4,
            "trash_original": true,
        }),
    );
    let list_after_err = rpc.call("bundle.list", &json!({ "state": "active" }));
    assert!(
        list_after_err["bundles"]
            .as_array()
            .expect("bundles array")
            .iter()
            .any(|b| b["bundle_id"] == src_id),
        "failed export MUST NEVER trash the source bundle"
    );

    // 3. Mid-GOP trimmed export (frames 4..=22 = 19 frames) with `trash_original: true`!
    let trimmed_mp4 = tmp.path().join("trimmed-share.mp4");
    let trim_resp = rpc.call(
        "export.share_mp4",
        &json!({
            "bundle_id": src_id,
            "in_frame_index": 4,
            "out_frame_index": 22,
            "output_path": trimmed_mp4,
            "trash_original": true,
        }),
    );
    assert_eq!(
        trim_resp["video_frame_count"], 19,
        "trimmed share has 19 frames"
    );
    assert_eq!(
        trim_resp["audio_included"], true,
        "trimmed share includes trimmed audio"
    );
    assert_eq!(trim_resp["edts_box_count"], 0, "trimmed share zero edts");
    assert_eq!(trim_resp["elst_box_count"], 0, "trimmed share zero elst");
    assert_eq!(
        trim_resp["original_trashed"], true,
        "original moved to non-destructive Trash"
    );
    let trimmed_id = trim_resp["trimmed_bundle_id"]
        .as_str()
        .expect("trimmed_bundle_id must be present");
    assert!(!trimmed_id.is_empty(), "trimmed_bundle_id non-empty");
    assert!(
        src_dir.join("manifest.json").is_file(),
        "source bundle files MUST remain intact on disk (non-destructive Trash!)"
    );
    let trashed_list = rpc.call("bundle.list", &json!({ "state": "trashed" }));
    assert!(
        trashed_list["bundles"]
            .as_array()
            .expect("trashed array")
            .iter()
            .any(|b| b["bundle_id"] == src_id),
        "source bundle must be listed in trashed state"
    );
    // Restore proves non-destructiveness!
    let restored = rpc.call("bundle.restore", &json!({ "bundle_id": src_id }));
    assert_eq!(
        restored["state"], "active",
        "source bundle must be cleanly restorable"
    );

    // 4. Legacy (RO) bundle MUST refuse `trash_original: true` cleanly!
    let legacy_dir = tmp.path().join("legacy-ro");
    seed_legacy_ro_bundle(tmp.path(), &legacy_dir);
    let open_legacy = rpc.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": legacy_dir,
            "holder_kind": "cli",
            "holder_pid": std::process::id(),
        }),
    );
    let legacy_id = open_legacy["bundle_id"].as_str().expect("legacy id");
    let (legacy_code, legacy_msg) = rpc.call_err(
        "export.share_mp4",
        &json!({
            "bundle_id": legacy_id,
            "output_path": tmp.path().join("legacy-share.mp4"),
            "trash_original": true,
        }),
    );
    assert_eq!(
        legacy_code,
        i64::from(RPC_ERR_CONFLICT),
        "trash_original on LegacyRo must fail with CONFLICT: {legacy_msg}"
    );
    assert!(
        legacy_dir.join("video.mp4").is_file(),
        "legacy bundle files untouched"
    );

    server.shutdown();
}

#[test]
fn t2b_timeline_overview_inspect_frames_and_query_logs_filters() {
    let tmp = TempDir::new().expect("temp dir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut rpc = RpcClient::connect(server.endpoint());

    let managed_bundles = paths.data_dir.join("bundles");
    let bundle_dir = seed_rich_v2_bundle(tmp.path(), &managed_bundles, "b-overview", true);
    let open = rpc.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": bundle_dir,
            "holder_kind": "cli",
            "holder_pid": std::process::id(),
        }),
    );
    let bundle_id = open["bundle_id"].as_str().expect("bundle_id").to_owned();

    // 1. `timeline.overview` returns complete structured triage payload in one RPC.
    let ov = rpc.call("timeline.overview", &json!({ "bundle_id": bundle_id }));
    assert_eq!(ov["bundle_id"], bundle_id, "overview bundle_id");
    assert_eq!(ov["kind"], "full", "bundle kind");
    assert_eq!(ov["legacy_read_only"], false, "not legacy");
    assert_eq!(ov["frame_count"], 30, "30 frames");
    assert_eq!(ov["width"], 128, "width 128");
    assert_eq!(ov["height"], 128, "height 128");
    assert_eq!(ov["audio_status"], "present", "audio present");
    assert_eq!(ov["av_offset_ms"], -20, "av_offset_ms");
    assert_eq!(
        ov["logcat_completeness"], "verified_no_overflow",
        "logcat completeness"
    );
    assert_eq!(
        ov["screens"].as_array().expect("screens").len(),
        1,
        "1 Activity screen span"
    );
    assert_eq!(
        ov["screens"][0]["short_name"], "CheckoutActivity",
        "screen Activity short name"
    );
    assert_eq!(ov["taps"].as_array().expect("taps").len(), 1, "1 tap item");
    assert_eq!(ov["taps"][0]["tap_index"], 0, "tap_index 0");
    assert_eq!(ov["taps"][0]["frame_index"], 10, "tap aligned to frame 10");
    assert_eq!(ov["taps"][0]["kind"], "TAP", "tap action");
    assert_eq!(ov["taps"][0]["gesture_kind"], "tap", "tap gesture_kind");
    assert!(
        ov["taps"][0]["end_x"].is_null(),
        "tap has no end_x (T-6H item 2)"
    );
    assert_eq!(
        ov["crashes"].as_array().expect("crashes").len(),
        1,
        "1 crash marker"
    );
    assert_eq!(
        ov["crashes"][0]["exception_class"], "java.lang.IllegalStateException",
        "crash exception class"
    );
    assert_eq!(
        ov["discontinuities"].as_array().expect("discs").len(),
        1,
        "1 discontinuity"
    );
    assert_eq!(
        ov["error_log_highlights"].as_array().expect("errors").len(),
        2,
        "2 E/F error log highlights"
    );

    // 2. `timeline.query_logs` with `level`, `tag`, `keyword`, `start_frame`/`end_frame`.
    let filtered = rpc.call(
        "timeline.query_logs",
        &json!({
            "bundle_id": bundle_id,
            "bucket": "main",
            "start_frame": 5,
            "end_frame": 15,
            "level": "E",
            "tag": "payment",
            "keyword": "500"
        }),
    );
    let logs = filtered["logs"].as_array().expect("logs array");
    assert_eq!(logs.len(), 1, "exact 1 filtered error log in frames 5..=15");
    assert_eq!(logs[0]["tag"], "PaymentRepo", "matched tag");

    // 3. `media.inspect_frames` — `contact_sheet` mode.
    let sheet = rpc.call(
        "media.inspect_frames",
        &json!({
            "bundle_id": bundle_id,
            "mode": "contact_sheet",
            "max_tiles": 6,
            "include_tap_marker": true
        }),
    );
    assert_eq!(sheet["mode"], "contact_sheet", "contact_sheet mode");
    let rendered = sheet["frame_indices_rendered"]
        .as_array()
        .expect("frame_indices_rendered");
    assert_eq!(rendered.len(), 6, "6 tiles rendered");
    assert_eq!(rendered[0], 0, "starts at frame 0");
    assert_eq!(rendered[5], 29, "ends at frame 29");
    let sheet_path = PathBuf::from(sheet["png_path"].as_str().expect("png_path"));
    let sheet_bytes = fs::read(&sheet_path).expect("read cached sheet PNG");
    assert_eq!(
        &sheet_bytes[0..8],
        &PNG_SIGNATURE,
        "valid PNG signature on sheet"
    );
    assert!(
        sheet["png_base64"]
            .as_str()
            .is_some_and(|s| s.starts_with("iVBORw0KGgo")),
        "inline png_base64 must carry PNG header"
    );

    // 4. `media.inspect_frames` — `single` mode.
    let single = rpc.call(
        "media.inspect_frames",
        &json!({
            "bundle_id": bundle_id,
            "mode": "single",
            "frame_index": 10,
            "include_tap_marker": true
        }),
    );
    assert_eq!(single["mode"], "single", "single mode");
    assert_eq!(single["width"], 128, "single frame width");
    assert_eq!(single["height"], 128, "single frame height");
    assert_eq!(
        single["frame_indices_rendered"],
        json!([10]),
        "single frame 10"
    );
    let single_bytes = fs::read(single["png_path"].as_str().expect("single path")).expect("read");
    assert_eq!(
        &single_bytes[0..8],
        &PNG_SIGNATURE,
        "valid PNG signature on single"
    );

    // 5. `media.inspect_frames` — `tap_crop` mode via `tap_index: 0`.
    let crop = rpc.call(
        "media.inspect_frames",
        &json!({
            "bundle_id": bundle_id,
            "mode": "tap_crop",
            "tap_index": 0
        }),
    );
    assert_eq!(crop["mode"], "tap_crop", "tap_crop mode");
    assert_eq!(
        crop["frame_indices_rendered"],
        json!([10]),
        "tap 0 resolved to frame 10"
    );
    assert_eq!(crop["tap_crop"]["tap_x"], 64, "tap_x");
    assert_eq!(crop["tap_crop"]["tap_y"], 80, "tap_y");
    assert_eq!(
        crop["width"], crop["tap_crop"]["crop_width"],
        "crop width matches PNG"
    );
    assert_eq!(
        crop["height"], crop["tap_crop"]["crop_height"],
        "crop height matches PNG"
    );
    let crop_bytes = fs::read(crop["png_path"].as_str().expect("crop path")).expect("read");
    assert_eq!(
        &crop_bytes[0..8],
        &PNG_SIGNATURE,
        "valid PNG signature on tap_crop"
    );

    server.shutdown();
}

#[test]
fn t6a_bundle_display_size_scales_taps_and_exposes_epoch_frame_ranges() {
    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths)).expect("start arbd");
    let mut rpc = RpcClient::connect(server.endpoint());

    let video_src = tmp.path().join("t6a-scaled-src.mp4");
    build_real_video_mp4(&video_src, 128, 128, 30);
    let track = Mp4VideoTrack::open(&video_src).expect("open video track");
    let bundle_dir = tmp.path().join("ext-bundle-scaled");

    let base_bt = 100_000_000_000_i64;
    let step = 33_333_333_i64;
    let mut frame_records = Vec::with_capacity(track.frames().len());
    for (idx, f) in track.frames().iter().enumerate() {
        let i = i64::try_from(idx).expect("idx");
        // Split into 2 epochs across a reboot at frame 15!
        let (boot_id, bt) = if idx < 15 {
            ("boot-epoch-0", base_bt + i * step)
        } else {
            ("boot-epoch-1", 5_000_000_000_i64 + (i - 15) * step)
        };
        frame_records.push(FrameRecord {
            frame_index: f.frame_index,
            boot_id: boot_id.to_owned(),
            pts_device_mono_ns: bt,
            pc_ingest_cont_ns: 200_000_000_000 + i * step,
            alignment: TimeAlignment::Anchored {
                boot_id: boot_id.to_owned(),
                boottime_ns: bt,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: f.is_keyframe,
            gop_id: f.gop_id,
            mp4_sample_offset: f.offset,
            mp4_sample_size: f.size,
        });
    }

    // Tap in physical 1080x2400 display coordinates -> center (540, 1200) must map to (64, 64) on 128x128 video!
    let taps = vec![TapRecord {
        seq: 1,
        boot_id: Some("boot-epoch-1".to_owned()),
        event_mono_ns: Some(5_000_000_000 + 3 * step),
        pc_ingest_cont_ns: 200_000_000_000 + 18 * step,
        alignment: TimeAlignment::Anchored {
            boot_id: "boot-epoch-1".to_owned(),
            boottime_ns: 5_000_000_000 + 3 * step,
            half_width_ns: 1_000_000,
            semantics: TimestampSemantics::InputEventTime,
        },
        bucket: RecordBucket::Main,
        action: "TAP".to_owned(),
        x: Some(540.0),
        y: Some(1200.0),
        pointer_id: Some(0),
        gesture_kind: GestureKind::Tap,
        end_x: None,
        end_y: None,
        end_t_offset_ns: Some(10_000_000),
        path: Vec::new(),
    }];

    let draft = V2BundleDraft {
        bundle_kind: BundleKind::Full,
        bundle_id: "01JT6ASCALETEST00000000001".to_owned(),
        created_at_utc: "2026-11-23T12:00:00Z".to_owned(),
        provenance: Provenance::Original,
        device: DeviceInfo {
            serial: "emulator-5554".to_owned(),
            model: "Pixel 9".to_owned(),
            api_level: 37,
            is_emulator: true,
        },
        requested_window: RequestedWindow::RealElapsedSec {
            duration_sec: 10,
            trigger_pc_wall_utc: "2026-11-23T12:00:00Z".to_owned(),
        },
        epochs: vec![
            EpochSpan {
                boot_id: "boot-epoch-0".to_owned(),
                start_boottime_ns: base_bt,
                end_boottime_ns: base_bt + 14 * step,
            },
            EpochSpan {
                boot_id: "boot-epoch-1".to_owned(),
                start_boottime_ns: 5_000_000_000,
                end_boottime_ns: 5_000_000_000 + 14 * step,
            },
        ],
        streams: StreamsSummary {
            video: Some(VideoStreamMeta {
                status: StreamStatus::Present,
                frame_count: 30,
                tail_may_be_incomplete: false,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
                display_width: Some(1080),
                display_height: Some(2400),
            }),
            audio: None,
            logcat: Some(LogcatStreamMeta {
                status: StreamStatus::Absent,
                completeness: LogCompleteness::VerifiedNoOverflow,
                overflow_events: 0,
                tail_may_be_incomplete: false,
            }),
            input: Some(InputStreamMeta {
                status: StreamStatus::Present,
                tail_may_be_incomplete: false,
            }),
        },
        unanchored_summary: UnanchoredSummary {
            included_count: 0,
            truncated_count: 0,
        },
        video_mp4: Some(MediaPayload::File(video_src)),
        audio_m4a: None,
        frames: frame_records,
        discontinuities: Vec::new(),
        logs: Vec::new(),
        taps,
        clock_anchors: Vec::new(),
    };
    V2BundleWriter::write(&bundle_dir, "job-t6a-scale", &draft).expect("write bundle");

    let opened = rpc.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": bundle_dir,
            "holder_kind": "cli",
            "holder_pid": std::process::id()
        }),
    );
    let bid = opened["bundle_id"].as_str().expect("bundle_id").to_owned();
    assert_eq!(opened["summary"]["streams"]["video"]["display_width"], 1080);
    assert_eq!(
        opened["summary"]["streams"]["video"]["display_height"],
        2400
    );
    let ranges = opened["summary"]["epoch_frame_ranges"]
        .as_array()
        .expect("epoch_frame_ranges");
    assert_eq!(ranges.len(), 2, "two reboot epochs expected");
    assert_eq!(ranges[0]["boot_id"], "boot-epoch-0");
    assert_eq!(ranges[0]["start_frame_index"], 0);
    assert_eq!(ranges[0]["end_frame_index"], 14);
    assert_eq!(ranges[1]["boot_id"], "boot-epoch-1");
    assert_eq!(ranges[1]["start_frame_index"], 15);
    assert_eq!(ranges[1]["end_frame_index"], 29);

    let overview = rpc.call("timeline.overview", &json!({ "bundle_id": bid }));
    assert_eq!(overview["display_width"], 1080);
    assert_eq!(overview["display_height"], 2400);
    assert_eq!(
        overview["epoch_frame_ranges"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        overview["taps"][0]["frame_index"], 18,
        "epoch-1 tap must map to frame 18 inside epoch 1"
    );

    // Tap crop WITHOUT caller-supplied display_width/display_height must automatically scale
    // (540, 1200) from bundle metadata 1080x2400 onto 128x128 video center (64, 64)!
    let crop = rpc.call(
        "media.inspect_frames",
        &json!({
            "bundle_id": bid,
            "mode": "tap_crop",
            "tap_index": 0
        }),
    );
    assert_eq!(crop["frame_indices_rendered"], json!([18]));
    assert_eq!(crop["tap_crop"]["tap_x"], 64);
    assert_eq!(crop["tap_crop"]["tap_y"], 64);

    // Contact sheet with include_tap_marker: true also automatically scales without error.
    let sheet = rpc.call(
        "media.inspect_frames",
        &json!({
            "bundle_id": bid,
            "mode": "contact_sheet",
            "frame_indices": [0, 18],
            "include_tap_marker": true
        }),
    );
    assert_eq!(sheet["frame_indices_rendered"], json!([0, 18]));

    server.shutdown();
}
