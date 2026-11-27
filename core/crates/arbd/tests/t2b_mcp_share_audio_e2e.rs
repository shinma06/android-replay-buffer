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

use arb_media::codec::VideoCodecBackend;
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
    let draft = rich_draft(work_tmp, bundle_id, with_audio);
    let bundle_path = dest_dir.join(bundle_id);
    V2BundleWriter::write(&bundle_path, &format!("seed-{bundle_id}"), &draft)
        .expect("write seeded bundle");
    bundle_path
}

fn rich_draft(work_tmp: &Path, bundle_id: &str, with_audio: bool) -> V2BundleDraft {
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

    V2BundleDraft {
        memo: None,
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
                display_width: Some(128),
                display_height: Some(128),
                carried_in: None,
            }),
            audio: audio_meta,
            logcat: Some(LogcatStreamMeta {
                dropped_count: None,
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
        share_mp4: None,
        frames,
        discontinuities,
        logs,
        taps,
        clock_anchors: Vec::new(),
    }
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
    let digest = arb_storage::sha256_hex(&fs::read(with_audio_dir.join("audio.m4a")).unwrap());
    assert_eq!(
        wav_path.file_stem().unwrap().to_str(),
        Some(digest.as_str())
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

    // A restored external recording can keep the original ID while containing different audio.
    let external = tmp.path().join("external");
    fs::create_dir(&external).unwrap();
    let copy = seed_rich_v2_bundle(&external, &external, "b-audio", false);
    build_real_audio_m4a(&copy.join("audio.m4a"), 48_000, 2, 24_000);
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(with_audio_dir.join("manifest.json")).unwrap()).unwrap();
    let audio = fs::read(copy.join("audio.m4a")).unwrap();
    let entry = manifest["files"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["path"] == "audio.m4a")
        .unwrap();
    entry["sha256"] = json!(arb_storage::sha256_hex(&audio));
    entry["bytes"] = json!(audio.len());
    fs::write(
        copy.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let changed = rpc.call("media.decode_audio_pcm", &json!({"bundle_id":copy}));
    assert_eq!(changed["cached"], false);
    assert_ne!(changed["wav_path"], dec1["wav_path"]);
    assert!(changed["duration_ns"].as_u64().unwrap() < dec1["duration_ns"].as_u64().unwrap());

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
    let dest = tmp.path().join("requested-exports");
    let standalone = rpc.call(
        "export.share_mp4",
        &json!({"bundle_id":src_id,"in_frame_index":4,"out_frame_index":22,"dest_dir":dest}),
    );
    let path = PathBuf::from(standalone["share_mp4_path"].as_str().unwrap());
    assert_eq!(path.parent(), Some(dest.as_path()));
    assert_eq!(fs::read_dir(&dest).unwrap().count(), 1);
    assert_eq!(Mp4VideoTrack::open(path).unwrap().frame_count(), 19);
    let trimmed_mp4 = tmp.path().join("trimmed-share.mp4");
    let trim_resp = rpc.call(
        "export.share_mp4",
        &json!({
            "bundle_id": src_id,
            "in_frame_index": 4,
            "out_frame_index": 22,
            "dest_dir": dest,
            "output_path": trimmed_mp4,
            "keep_bundle": true,
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
    assert_eq!(
        Path::new(trim_resp["trimmed_bundle_path"].as_str().unwrap()).parent(),
        Some(dest.as_path())
    );
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

    let highlights = rpc.call(
        "timeline.overview",
        &json!({"bundle_id":bundle_id,"tag":"PAY","package":"com.example.shop","max_error_logs":0}),
    );
    assert_eq!(highlights["total_error_logs"], 1);
    assert_eq!(highlights["error_logs_truncated"], true);
    let excluded = rpc.call(
        "timeline.overview",
        &json!({"bundle_id":bundle_id,"exclude_tags":["paymentrepo"]}),
    );
    assert_eq!(excluded["total_error_logs"], 1);
    assert_eq!(excluded["error_logs_truncated"], false);
    let page = rpc.call("bundle.query_logs", &json!({"bundle_id":bundle_id}));
    for highlight in ov["error_log_highlights"].as_array().unwrap() {
        let log = page["logs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|log| log["seq"] == highlight["seq"])
            .unwrap();
        assert_eq!(log["frame_index"], highlight["frame_index_hint"]);
    }

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
    let mut taps = vec![TapRecord {
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

    let mut overrange = taps[0].clone();
    overrange.seq = 2;
    overrange.x = Some(364_077.0);
    overrange.y = Some(204_793.0);
    taps.push(overrange);
    let draft = V2BundleDraft {
        memo: None,
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
                carried_in: None,
            }),
            audio: None,
            logcat: Some(LogcatStreamMeta {
                dropped_count: None,
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
        share_mp4: None,
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

    assert_eq!(overview["taps"][1]["x"], 1079.0);
    assert_eq!(overview["taps"][1]["y"], 2399.0);
    let crop = rpc.call(
        "media.inspect_frames",
        &json!({"bundle_id":bid,"mode":"tap_crop","tap_index":1}),
    );
    assert_eq!(crop["tap_crop"]["tap_x"], 127);
    assert_eq!(crop["tap_crop"]["tap_y"], 127);

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

#[test]
fn t6i_complete_trimmed_bundle_output_dir_burn_taps_and_non_colliding_share_mp4() {
    use arb_media::codec::VideoCodecBackend;

    let tmp = TempDir::new().expect("tempdir");
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).expect("start arbd");
    let mut rpc = RpcClient::connect(server.endpoint());

    // Configure user output_dir_override so omitted dest_dir routes there!
    let custom_out = tmp.path().join("user-custom-out");
    fs::create_dir_all(&custom_out).expect("create custom out dir");
    let _ = rpc.call(
        "config.update",
        &json!({ "output_dir_override": custom_out }),
    );

    let managed = paths.data_dir.join("bundles");
    let src_dir = seed_rich_v2_bundle(tmp.path(), &managed, "t6i-source-01", true);
    let opened = rpc.call(
        "bundle.open",
        &json!({
            "bundle_id_or_path": src_dir,
            "holder_kind": "cli",
            "holder_pid": std::process::id()
        }),
    );
    let src_id = opened["bundle_id"].as_str().expect("bundle_id").to_owned();
    let src_lease = opened["lease_id"].as_str().expect("lease_id").to_owned();
    let _ = rpc.call("bundle.close", &json!({ "lease_id": src_lease }));

    let one_log = rpc.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": src_id, "in_frame_index": 9, "out_frame_index": 11
        }),
    );
    let dir = PathBuf::from(one_log["bundle_path"].as_str().unwrap());
    let app_log = fs::read_to_string(dir.join("app-logs/com.example.shop.log")).unwrap();
    assert!(app_log.contains("HTTP 500"));
    assert!(!app_log.contains("CheckoutActivity"));

    // 1. Complete Trimmed Bundle Directory with include_share_mp4=true & burn_taps=true
    //    (omitted dest_dir -> MUST land inside output_dir_override!).
    let trim_burned = rpc.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": src_id,
            "in_frame_index": 5,
            "out_frame_index": 22,
            "include_share_mp4": true,
            "burn_taps": true,
            "holder_pid": std::process::id()
        }),
    );
    let trimmed_dir = PathBuf::from(trim_burned["bundle_path"].as_str().expect("bundle_path"));
    assert!(
        trimmed_dir.starts_with(&custom_out),
        "trimmed bundle must respect output_dir_override: {trimmed_dir:?}"
    );
    assert_eq!(
        trim_burned["burn_taps_applied"], true,
        "burn_taps must be applied on tap-containing trim"
    );
    assert!(
        trim_burned["app_logs_count"].as_u64().unwrap_or(0) >= 1,
        "trimmed bundle must extract per-app foreground logs"
    );
    let share_taps_path = PathBuf::from(
        trim_burned["share_mp4_path"]
            .as_str()
            .expect("share_mp4_path"),
    );
    assert_eq!(
        share_taps_path.file_name().and_then(|s| s.to_str()),
        Some("share-taps.mp4"),
        "burn_taps=true must emit share-taps.mp4 inside trimmed bundle"
    );
    assert!(share_taps_path.is_file(), "share-taps.mp4 must exist");
    assert!(
        trimmed_dir.join("video.mp4").is_file(),
        "lossless video.mp4"
    );
    assert!(trimmed_dir.join("audio.m4a").is_file(), "trimmed audio.m4a");
    assert!(
        trimmed_dir.join("timeline.sqlite").is_file(),
        "timeline.sqlite"
    );
    assert!(trimmed_dir.join("manifest.json").is_file(), "manifest.json");
    assert!(
        trimmed_dir.join("logcat.log").is_file(),
        "human-readable logcat.log"
    );
    assert!(
        trimmed_dir.join("events.jsonl").is_file(),
        "human-readable events.jsonl"
    );
    assert!(
        trimmed_dir
            .join("app-logs")
            .join("com.example.shop.log")
            .is_file(),
        "per-app logcat must be human-readable .log"
    );
    assert!(
        !trimmed_dir.join("unanchored.log").exists(),
        "zero unanchored logs -> no empty unanchored.log clutter"
    );

    // Verify share-taps.mp4 has zero edts/elst, preserves AAC audio track, AND has visibly
    // modified pixels at tap (64, 80) on frame 5 (source frame 10) compared to lossless video.mp4!
    let atoms = inspect_mp4_atoms(&share_taps_path).expect("inspect share-taps.mp4");
    assert_eq!(atoms.edts_count, 0, "share-taps.mp4 edts=0");
    assert_eq!(atoms.elst_count, 0, "share-taps.mp4 elst=0");
    let audio_in_share =
        Mp4AudioTrack::open(&share_taps_path).expect("audio preserved in share-taps.mp4");
    assert!(audio_in_share.packet_count() > 0, "AAC packets preserved");

    let backend = OpenH264Backend::new();
    let raw_track = Mp4VideoTrack::open(trimmed_dir.join("video.mp4")).expect("raw track");
    let burned_track = Mp4VideoTrack::open(&share_taps_path).expect("burned track");
    assert_eq!(raw_track.frame_count(), 18, "frames 5..=22 = 18 frames");
    assert_eq!(burned_track.frame_count(), 18, "burned frame count matches");
    let raw_f = backend
        .decode_frame(&raw_track, 5, PixelFormat::Yuv420p)
        .expect("decode raw tap frame");
    let burned_f = backend
        .decode_frame(&burned_track, 5, PixelFormat::Yuv420p)
        .expect("decode burned tap frame");
    let center_idx = 80 * 128 + 64;
    let diff = raw_f.data[center_idx].abs_diff(burned_f.data[center_idx]);
    assert!(
        diff >= 20,
        "burned share-taps.mp4 must visibly modify tap pixel at (64,80) vs raw video.mp4 (diff={diff})"
    );

    // 2. export.trim with include_share_mp4=true & burn_taps=false emits share.mp4 (no burn-in).
    let trim_plain = rpc.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": src_id,
            "in_frame_index": 5,
            "out_frame_index": 22,
            "include_share_mp4": true,
            "burn_taps": false,
            "holder_pid": std::process::id()
        }),
    );
    assert_eq!(trim_plain["burn_taps_applied"], false);
    let share_plain_path = PathBuf::from(
        trim_plain["share_mp4_path"]
            .as_str()
            .expect("share_mp4_path"),
    );
    assert_eq!(
        share_plain_path.file_name().and_then(|s| s.to_str()),
        Some("share.mp4"),
        "burn_taps=false must emit share.mp4"
    );

    // 3. export.share_mp4 default filenames do NOT collide between burn_taps=false & burn_taps=true.
    let share_off = rpc.call(
        "export.share_mp4",
        &json!({ "bundle_id": src_id, "burn_taps": false }),
    );
    let share_on = rpc.call(
        "export.share_mp4",
        &json!({ "bundle_id": src_id, "burn_taps": true }),
    );
    let off_path = PathBuf::from(share_off["share_mp4_path"].as_str().expect("off path"));
    let on_path = PathBuf::from(share_on["share_mp4_path"].as_str().expect("on path"));
    assert_ne!(
        off_path, on_path,
        "burned & unburned default filenames must not collide"
    );
    assert!(
        on_path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.ends_with("-taps.mp4")),
        "burn_taps=true default share filename must end with -taps.mp4: {on_path:?}"
    );
    assert!(off_path.starts_with(&custom_out) && on_path.starts_with(&custom_out));

    // 4. export.trim with trash_original=true refuses LegacyRo cleanly and succeeds on managed V2.
    let legacy_dir = tmp.path().join("t6i-legacy-ro");
    seed_legacy_ro_bundle(tmp.path(), &legacy_dir);
    let (legacy_code, legacy_msg) = rpc.call_err(
        "export.trim",
        &json!({
            "bundle_id_or_path": legacy_dir,
            "in_frame_index": 0,
            "out_frame_index": 5,
            "trash_original": true
        }),
    );
    assert_eq!(legacy_code, i64::from(RPC_ERR_CONFLICT));
    assert!(
        legacy_msg.contains("legacy"),
        "conflict reason: {legacy_msg}"
    );

    let trim_trash = rpc.call(
        "export.trim",
        &json!({
            "bundle_id_or_path": src_id,
            "in_frame_index": 5,
            "out_frame_index": 15,
            "include_share_mp4": true,
            "burn_taps": false,
            "trash_original": true,
            "holder_pid": std::process::id()
        }),
    );
    assert_eq!(
        trim_trash["original_trashed"], true,
        "source logically trashed"
    );
    assert!(
        src_dir.is_dir(),
        "WORM source directory remains intact on disk"
    );

    server.shutdown();
}

#[test]
fn trim_stops_before_commit_when_source_clock_anchors_cannot_be_read() {
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let source = seed_rich_v2_bundle(
        tmp.path(),
        &paths.data_dir.join("bundles"),
        "bad-clock",
        false,
    );
    let db = rusqlite::Connection::open(source.join("timeline.sqlite")).unwrap();
    db.execute(
        "INSERT INTO clock_anchors(kind, boot_id, payload_json) VALUES ('device_triple', 'boot-t2b', '{}')",
        [],
    )
    .unwrap();
    drop(db);
    let server = DaemonServer::start(DaemonConfig::new(paths)).unwrap();
    let mut rpc = RpcClient::connect(server.endpoint());
    let dest = tmp.path().join("exports");
    let (_, message) = rpc.call_err("export.trim", &json!({
        "bundle_id_or_path": source, "in_frame_index": 5, "out_frame_index": 15, "dest_dir": dest
    }));
    assert!(message.contains("parse_device_triple"), "{message}");
    assert!(!dest.exists() || fs::read_dir(dest).unwrap().next().is_none());
    server.shutdown();
}

#[test]
fn realtime_trim_round_trip_and_four_streams_inside_one_held_picture() {
    const START: i64 = 1_500_000_003;
    const END: i64 = 4_000_000_003;
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap();
    let mut rpc = RpcClient::connect(server.endpoint());
    let mut draft = rich_draft(tmp.path(), "still", false);
    let file = tmp.path().join("held.mp4");
    let frames = vec![
        DecodedFrame {
            frame_index: 0,
            width: 128,
            height: 128,
            format: PixelFormat::Yuv420p,
            data: vec![128; 128 * 128 * 3 / 2]
        };
        2
    ];
    let packets = OpenH264Backend::new()
        .encode_yuv420p_sequence(&frames, 1, 5 * SEC_NS, EncodeParams::default())
        .unwrap();
    mux_raw_h264_to_mp4(&file, 128, 128, 90_000, 3000, &packets).unwrap();
    let track = Mp4VideoTrack::open(&file).unwrap();
    draft.frames.truncate(2);
    for (record, frame) in draft.frames.iter_mut().zip(track.frames()) {
        let t = BASE_BT_NS + frame.pts_ns;
        record.pts_device_mono_ns = t;
        record.pc_ingest_cont_ns = t;
        if let TimeAlignment::Anchored { boottime_ns, .. } = &mut record.alignment {
            *boottime_ns = t;
        }
        record.mp4_sample_offset = frame.offset;
        record.mp4_sample_size = frame.size;
        record.is_keyframe = frame.is_keyframe;
        record.gop_id = frame.gop_id;
    }
    draft.video_mp4 = Some(MediaPayload::File(file));
    draft.streams.video.as_mut().unwrap().frame_count = 2;
    draft.epochs[0].end_boottime_ns = BASE_BT_NS + 5 * SEC_NS;
    let audio = tmp.path().join("long.m4a");
    build_real_audio_m4a(&audio, 48_000, 2, 240_000);
    draft.audio_m4a = Some(MediaPayload::File(audio));
    let meta = draft.streams.audio.as_mut().unwrap();
    meta.status = StreamStatus::Present;
    meta.audio_lead_in_offset_ns = 20_000_000;
    let mut foreground = draft.logs[0].clone();
    foreground.seq = 5;
    let template = draft.logs[1].clone();
    draft.logs = [START - 1, START, END - 1, END]
        .into_iter()
        .enumerate()
        .map(|(i, t)| {
            let mut row = template.clone();
            row.seq = u64::try_from(i + 1).unwrap();
            row.message = format!("log-{t}");
            if let TimeAlignment::Anchored { boottime_ns, .. } = &mut row.alignment {
                *boottime_ns = BASE_BT_NS + t;
            }
            row
        })
        .collect();
    draft.logs.push(foreground);
    let tap = draft.taps[0].clone();
    draft.taps = [
        START - 500_000_000,
        START + 700_000_000,
        START + 1_600_000_000,
        END,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, t)| {
        let mut row = tap.clone();
        row.seq = u64::try_from(i + 1).unwrap();
        if let TimeAlignment::Anchored { boottime_ns, .. } = &mut row.alignment {
            *boottime_ns = BASE_BT_NS + t;
        }
        if i == 0 {
            row.gesture_kind = GestureKind::Swipe;
            row.x = Some(20.0);
            row.end_x = Some(100.0);
            row.end_y = row.y;
            row.end_t_offset_ns = Some(1_000_000_000);
        }
        row
    })
    .collect();
    let source = paths.data_dir.join("bundles/still");
    V2BundleWriter::write(&source, "seed-still", &draft).unwrap();
    let opened = rpc.call(
        "bundle.open",
        &json!({"bundle_id_or_path":source,"holder_kind":"cli","holder_pid":std::process::id()}),
    );
    let _ = rpc.call("bundle.close", &json!({"lease_id":opened["lease_id"]}));
    let session=rpc.call("edit.update_session",&json!({"bundle_id":"still","session_id":"edit","in_time_ns":START,"out_time_ns":END,"in_frame_index":12,"out_frame_index":11}));
    assert_eq!(
        session,
        rpc.call("edit.get_session", &json!({"session_id":"edit"}))
    );
    for (key, expected) in [
        ("in_time_ns", START),
        ("out_time_ns", END),
        ("in_frame_index", 0),
        ("out_frame_index", 0),
    ] {
        assert_eq!(session["session"][key], expected);
    }
    for method in ["edit.update_session", "export.trim", "export.share_mp4"] {
        let unknown = json!({"bundle_id":"not-registered", "bundle_id_or_path":"not-registered", "session_id":"edit", "in_time_ns":START});
        assert_eq!(rpc.call_err(method, &unknown).0, -32602);
        for mut args in [
            json!({"in_time_ns":START}),
            json!({"out_time_ns":END}),
            json!({"in_time_ns":END,"out_time_ns":START}),
            json!({"in_time_ns":START,"out_time_ns":START}),
            json!({"in_time_ns":START,"out_time_ns":9*SEC_NS}),
        ] {
            args[if method == "export.trim" {
                "bundle_id_or_path"
            } else {
                "bundle_id"
            }] = json!("still");
            args["session_id"] = json!("edit");
            assert_eq!(rpc.call_err(method, &args).0, -32602);
        }
    }
    let mut count = 1;
    for (method, keep) in [
        ("export.share_mp4", false),
        ("export.share_mp4", true),
        ("export.trim", true),
    ] {
        for burn in [false, true] {
            let mut args = json!({"in_time_ns":START,"out_time_ns":END,"burn_taps":burn,"keep_bundle":keep,"include_share_mp4":true});
            args[if method == "export.trim" {
                "bundle_id_or_path"
            } else {
                "bundle_id"
            }] = json!("still");
            let r = rpc.call(method, &args);
            assert_eq!(r["in_time_ns"], START);
            assert_eq!(r["out_time_ns"], END);
            let share = Path::new(r["share_mp4_path"].as_str().unwrap());
            if method == "export.trim" {
                let dir = Path::new(r["bundle_path"].as_str().unwrap());
                let filename = if burn { "share-taps.mp4" } else { "share.mp4" };
                assert_eq!(share, dir.join(filename));
                assert_eq!(r["burn_taps_applied"], burn);
                assert!(dir.join("manifest.json").is_file());
                let catalog = rpc.call("bundle.list", &json!({}));
                let entry = catalog["bundles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["bundle_id"] == r["bundle_id"])
                    .unwrap();
                assert_eq!(entry["parent_bundle_id"], "still");
                assert_eq!(entry["path"], r["bundle_path"]);
            }
            let atoms = inspect_mp4_atoms(share).unwrap();
            assert_eq!((atoms.edts_count, atoms.elst_count), (0, 0));
            let video = Mp4VideoTrack::open(share).unwrap();
            assert_eq!(
                arb_media::trim::trim_timeline(&video).unwrap().1,
                2_500_000_000
            );
            if burn {
                assert!(video.frame_count() > 3);
                let pts = video.frames().iter().map(|f| f.pts_ns).collect::<Vec<_>>();
                for onset in [700_000_000, 1_600_000_000] {
                    assert!(pts.contains(&onset), "overlay starts exactly at {onset}");
                }
                let decoder = OpenH264Backend::new();
                for (time, x) in [(0_i64, 60_usize), (700_000_000, 64), (1_600_000_000, 64)] {
                    let index =
                        u64::try_from(pts.iter().position(|t| *t == time).unwrap()).unwrap();
                    let picture = decoder
                        .decode_frame(&video, index, PixelFormat::Yuv420p)
                        .unwrap();
                    assert!(
                        picture.data[80 * 128 + x] > 175,
                        "touch center at {time} ns"
                    );
                }
            } else {
                assert_eq!(video.frame_count(), 1);
            }
            assert_eq!(
                Mp4AudioTrack::open(share).unwrap().duration_ns(),
                2_500_000_000
            );
            let wav = tmp
                .path()
                .join(format!("decoded-{method}-{keep}-{burn}.wav"));
            let decoded = arb_media::audio_decode::decode_audio_m4a_to_wav(share, &wav).unwrap();
            assert_eq!(decoded.pcm_frame_count, 120_000);
            let input_wav = tmp.path().join("original.wav");
            if !input_wav.exists() {
                arb_media::audio_decode::decode_audio_m4a_to_wav(
                    tmp.path().join("long.m4a"),
                    &input_wav,
                )
                .unwrap();
            }
            // Compare interior PCM against the source on the measured A/V clock.
            let raw = fs::read(&input_wav).unwrap();
            let cut = fs::read(wav).unwrap();
            let sample = |data: &[u8], i: usize| {
                i32::from(i16::from_le_bytes(
                    data[44 + i * 4..46 + i * 4].try_into().unwrap(),
                ))
            };
            let source_start = 71_041; // ceil((1.500000003 - 0.020) * 48000)
            let error = (4000..6000)
                .map(|i| (sample(&raw, source_start + i) - sample(&cut, i)).unsigned_abs())
                .sum::<u32>()
                / 2000;
            assert!(
                error < 1200,
                "PCM aligned within AAC coding error, got {error}"
            );
            if keep {
                count += 1;
                let dir = Path::new(
                    r["trimmed_bundle_path"]
                        .as_str()
                        .or(r["bundle_path"].as_str())
                        .unwrap(),
                );
                assert_eq!(
                    Mp4AudioTrack::open(dir.join("audio.m4a"))
                        .unwrap()
                        .duration_ns(),
                    2_500_000_000
                );
                assert_eq!(
                    arb_media::trim::trim_timeline(
                        &Mp4VideoTrack::open(dir.join("video.mp4")).unwrap()
                    )
                    .unwrap()
                    .1,
                    2_500_000_000
                );
                let rows = |name: &str| {
                    fs::read_to_string(dir.join(name))
                        .unwrap()
                        .lines()
                        .map(|l| serde_json::from_str::<Value>(l).unwrap())
                        .collect::<Vec<_>>()
                };
                let logs = rows("logs.jsonl");
                assert_eq!(logs.len(), 2);
                assert_eq!(logs[0]["relative_time_ns"], 0);
                assert_eq!(logs[1]["relative_time_ns"], END - START - 1);
                let app = fs::read_to_string(dir.join("app-logs/com.example.shop.log")).unwrap();
                assert!(app.contains(&format!("log-{START}")));
                assert!(!app.contains(&format!("log-{END}")));
                let taps = rows("events.jsonl")
                    .into_iter()
                    .filter(|v| v["kind"] == "tap" || v["kind"] == "swipe")
                    .collect::<Vec<_>>();
                assert_eq!(taps.len(), 3);
                assert_eq!(taps[0]["x"], 60.0);
                assert_eq!(taps[0]["relative_time_ns"], 0);
                assert_eq!(taps[0]["end_t_offset_ns"], 500_000_000);
                assert_eq!(taps[1]["relative_time_ns"], 700_000_000);
            }
            assert_eq!(
                rpc.call("bundle.list", &json!({}))["bundles"]
                    .as_array()
                    .unwrap()
                    .len(),
                count
            );
            assert_eq!(fs::read_dir(paths.data_dir.join("tmp")).unwrap().count(), 0);
        }
    }
    let onset = START + 700_000_000;
    let one_ns = rpc.call(
        "export.share_mp4",
        &json!({"bundle_id":"still", "in_time_ns":onset, "out_time_ns":onset+1, "burn_taps":true}),
    );
    let video = Mp4VideoTrack::open(one_ns["share_mp4_path"].as_str().unwrap()).unwrap();
    assert_eq!(arb_media::trim::trim_timeline(&video).unwrap().1, 1);
    // A five-millisecond clip is shorter than one AAC access unit. Its very
    // first samples must survive encoder priming in the muxed share, too.
    let short = rpc.call(
        "export.share_mp4",
        &json!({"bundle_id":"still","in_time_ns":START,"out_time_ns":START+5_000_000}),
    );
    let share = Path::new(short["share_mp4_path"].as_str().unwrap());
    let wav = tmp.path().join("short.wav");
    assert_eq!(
        arb_media::audio_decode::decode_audio_m4a_to_wav(share, &wav)
            .unwrap()
            .pcm_frame_count,
        240
    );
    let mp4 = fs::read(share).unwrap();
    assert_eq!(&mp4[8..12], b"M4V ");
    assert!(mp4.windows(8).any(|bytes| bytes == b"iTunSMPB"));
    let raw = fs::read(wav).unwrap();
    let source_pcm = fs::read(tmp.path().join("original.wav")).unwrap();
    let start_sample = usize::try_from(
        (u64::try_from(START - 20_000_000).unwrap() * 48_000).div_ceil(1_000_000_000),
    )
    .unwrap();
    let left = |bytes: &[u8], i: usize| {
        i64::from(i16::from_le_bytes(
            bytes[44 + 4 * i..46 + 4 * i].try_into().unwrap(),
        ))
    };
    let error = (0..240)
        .map(|i| (left(&raw, i) - left(&source_pcm, start_sample + i)).abs())
        .sum::<i64>()
        / 240;
    assert!(
        error < 1800,
        "first 5ms must contain the selected PCM, error {error}"
    );
    let legacy = rpc.call(
        "export.trim",
        &json!({"bundle_id_or_path":"still","in_frame_index":0,"out_frame_index":0}),
    );
    assert_eq!(legacy["out_time_ns"], 5 * SEC_NS);
    server.shutdown();
}

#[test]
fn realtime_events_after_device_sleep_follow_the_video_clock() {
    let tmp = TempDir::new().unwrap();
    let paths = isolated_paths(&tmp);
    let server = DaemonServer::start(DaemonConfig::new(paths.clone())).unwrap();
    let mut rpc = RpcClient::connect(server.endpoint());
    let mut draft = rich_draft(tmp.path(), "sleep", false);
    let sleep = 5 * SEC_NS;
    for frame in &mut draft.frames[15..] {
        if let TimeAlignment::Anchored { boottime_ns, .. } = &mut frame.alignment {
            *boottime_ns += sleep;
        }
    }
    let target = 16 * FRAME_STEP_NS;
    let at = BASE_BT_NS + target + sleep;
    for alignment in [&mut draft.taps[0].alignment, &mut draft.logs[1].alignment] {
        if let TimeAlignment::Anchored { boottime_ns, .. } = alignment {
            *boottime_ns = at;
        }
    }
    draft.epochs[0].end_boottime_ns += sleep;
    let source = paths.data_dir.join("bundles/sleep");
    V2BundleWriter::write(&source, "seed-sleep", &draft).unwrap();
    let mut args =
        json!({"bundle_id":source, "in_time_ns":0, "out_time_ns":SEC_NS, "burn_taps":true});
    for keep in [false, true] {
        args["keep_bundle"] = json!(keep);
        let result = rpc.call("export.share_mp4", &args);
        let file = Mp4VideoTrack::open(result["share_mp4_path"].as_str().unwrap()).unwrap();
        let idx = file
            .frames()
            .iter()
            .position(|f| f.pts_ns == target)
            .expect("touch sample on media clock");
        let decoded = OpenH264Backend::new()
            .decode_frame(&file, u64::try_from(idx).unwrap(), PixelFormat::Yuv420p)
            .unwrap();
        assert!(decoded.data[80 * 128 + 64] > 175);
        if keep {
            let bundle = Path::new(result["trimmed_bundle_path"].as_str().unwrap());
            for file in ["events.jsonl", "logs.jsonl"] {
                let rows = fs::read_to_string(bundle.join(file)).unwrap();
                assert!(
                    rows.lines().any(
                        |line| serde_json::from_str::<Value>(line).unwrap()["relative_time_ns"]
                            == target
                    ),
                    "{file}"
                );
            }
        }
    }
    server.shutdown();
}
