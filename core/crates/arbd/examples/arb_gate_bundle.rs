//! Deterministic release-gate fixture generator (G-M3 / G-M4).
//!
//! Writes one standard WORM bundle of synthetic FHD30 video (`OpenH264`, configurable GOP), a stereo
//! AAC-LC `audio.m4a`, logcat rows and a fixed set of Tap / Swipe gestures, plus a
//! `<bundle-id>.gate.json` description of every expected gesture position. It needs no device and
//! no `ffmpeg`; `core/scripts/verify_release_bundle.py` drives the *shipped* `arb-cli` / `arbd`
//! over the result, so the fixture is only an input, never the code under test.
//!
//! ```text
//! cargo run --release -p arbd --example arb_gate_bundle -- --out-dir DIR [--gop-seconds 1|5] [--frames 720]
//! ```

#![expect(
    clippy::print_stdout,
    reason = "fixture generator CLI prints the generated paths for the calling script"
)]

use std::f64::consts::PI;
use std::fs;
use std::path::{Path, PathBuf};

use arb_core::bundle::{
    AudioStreamMeta, AvOffsetStatus, BundleKind, DeviceInfo, EpochSpan, FrameRecord,
    InputStreamMeta, LogCompleteness, LogRecord, LogcatStreamMeta, Provenance, RecordBucket,
    RequestedWindow, StreamStatus, StreamsSummary, TapRecord, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::gesture::{GestureKind, TouchPoint};
use arb_core::time::{TimeAlignment, TimestampSemantics};
use arb_media::audio::{
    AacMuxInput, AacPacket, build_aac_lc_audio_specific_config, mux_aac_to_m4a,
};
use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
use arb_media::mp4::{Mp4VideoTrack, RawH264Packet, mux_raw_h264_to_mp4};
use arb_storage::bundle_io::{MediaPayload, V2BundleDraft, V2BundleWriter};
use rusty_aac::{AacEncoder, AacEncoderConfig};
use serde_json::{Value, json};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: i64 = 30;
const FRAME_STEP_NS: i64 = 33_333_333;
const BASE_BT_NS: i64 = 200 * 1_000_000_000;
const BOOT_ID: &str = "boot-gate";
const BARCODE_BITS: u32 = 16;
const BARCODE_CELL: u32 = 32;
const TRIM_IN: u64 = 47;
const TRIM_OUT: u64 = 647;
const SAMPLE_RATE: u32 = 48_000;

/// One scripted gesture, expressed in video (== display) pixels.
struct Gesture {
    kind: GestureKind,
    start_frame: u64,
    x: f64,
    y: f64,
    end: Option<(f64, f64)>,
    duration_ns: i64,
}

fn gestures() -> Vec<Gesture> {
    vec![
        // Before the trim window: must never be burned into a trimmed export.
        Gesture {
            kind: GestureKind::Tap,
            start_frame: 20,
            x: 1500.0,
            y: 900.0,
            end: None,
            duration_ns: 20_000_000,
        },
        Gesture {
            kind: GestureKind::Tap,
            start_frame: 120,
            x: 960.0,
            y: 540.0,
            end: None,
            duration_ns: 20_000_000,
        },
        Gesture {
            kind: GestureKind::Tap,
            start_frame: 300,
            x: 480.0,
            y: 800.0,
            end: None,
            duration_ns: 20_000_000,
        },
        Gesture {
            kind: GestureKind::Swipe,
            start_frame: 400,
            x: 300.0,
            y: 300.0,
            end: Some((1500.0, 300.0)),
            duration_ns: 300_000_000,
        },
    ]
}

fn barcode_bit(frame_index: u64, cell: u32) -> bool {
    (frame_index >> (BARCODE_BITS - 1 - cell)) & 1 == 1
}

/// Static luma gradient, a 16-cell machine barcode of the frame number (top-left) and a moving
/// square near the bottom, so every frame differs while the tap / swipe areas stay plain.
fn synthesize_frame(frame_index: u64) -> DecodedFrame {
    let width = usize::try_from(WIDTH).expect("width fits usize");
    let height = usize::try_from(HEIGHT).expect("height fits usize");
    let mut data = vec![128_u8; width * height * 3 / 2];
    for y in 0..height {
        let row = &mut data[y * width..(y + 1) * width];
        let base = 90 + u8::try_from(y * 40 / height).expect("gradient fits u8");
        for (x, px) in row.iter_mut().enumerate() {
            *px = base + u8::try_from(x * 20 / width).expect("gradient fits u8");
        }
    }
    let cell = usize::try_from(BARCODE_CELL).expect("cell fits usize");
    for bit in 0..BARCODE_BITS {
        let luma = if barcode_bit(frame_index, bit) {
            235
        } else {
            16
        };
        let x0 = usize::try_from(bit).expect("bit fits usize") * cell;
        for y in 0..cell {
            data[y * width + x0..y * width + x0 + cell].fill(luma);
        }
    }
    let sq_x = usize::try_from((frame_index * 2) % 1_800).expect("square x fits usize");
    for y in 980..1044 {
        data[y * width + sq_x..y * width + sq_x + 64].fill(220);
    }
    DecodedFrame {
        frame_index,
        width: WIDTH,
        height: HEIGHT,
        format: PixelFormat::Yuv420p,
        data,
    }
}

fn encode_video(
    path: &Path,
    frames: u64,
    gop_frames: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = OpenH264Backend::new();
    let mut packets: Vec<RawH264Packet> = Vec::new();
    let mut start = 0_u64;
    while start < frames {
        let end = (start + u64::from(gop_frames)).min(frames);
        let chunk: Vec<DecodedFrame> = (start..end).map(synthesize_frame).collect();
        let chunk_len = u32::try_from(chunk.len())?;
        let mut chunk_packets = backend.encode_yuv420p_sequence(
            &chunk,
            chunk_len,
            FRAME_STEP_NS,
            EncodeParams::default(),
        )?;
        let offset = i64::try_from(start)?.saturating_mul(FRAME_STEP_NS);
        for pkt in &mut chunk_packets {
            pkt.pts_ns = pkt.pts_ns.saturating_add(offset);
        }
        packets.extend(chunk_packets);
        start = end;
    }
    mux_raw_h264_to_mp4(
        path,
        u16::try_from(WIDTH)?,
        u16::try_from(HEIGHT)?,
        90_000,
        3_000,
        &packets,
    )?;
    Ok(())
}

fn encode_audio(path: &Path, seconds: u32) -> Result<(), Box<dyn std::error::Error>> {
    let total = u64::from(seconds) * u64::from(SAMPLE_RATE);
    let mut pcm: Vec<f32> = Vec::with_capacity(usize::try_from(total * 2)?);
    for i in 0..total {
        // Exact integer-to-float conversion is fine for a test tone below 2^52.
        let t = f64::from(u32::try_from(i % u64::from(SAMPLE_RATE))?) / f64::from(SAMPLE_RATE);
        let tone = f64::sin(2.0 * PI * 440.0 * t) * 0.3;
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "synthetic sine sample f64 -> f32"
        )]
        let sample = tone as f32;
        pcm.push(sample);
        pcm.push(sample);
    }
    let mut enc = AacEncoder::new(AacEncoderConfig::default());
    enc.push_pcm(&pcm, 2, SAMPLE_RATE)?;
    enc.finish();
    let mut packets = Vec::new();
    let mut idx = 0_i64;
    while let Ok(pkt) = enc.next_packet() {
        packets.push(AacPacket {
            pts_us: idx * 1024 * 1_000_000 / i64::from(SAMPLE_RATE),
            data: pkt.data,
        });
        idx += 1;
    }
    let asc = build_aac_lc_audio_specific_config(SAMPLE_RATE, 2)?;
    mux_aac_to_m4a(
        path,
        &AacMuxInput {
            audio_specific_config: asc.to_vec(),
            sample_rate_hz: SAMPLE_RATE,
            channels: 2,
            packets,
        },
    )?;
    Ok(())
}

fn anchored(bt: i64, half_width_ns: u64, semantics: TimestampSemantics) -> TimeAlignment {
    TimeAlignment::Anchored {
        boot_id: BOOT_ID.to_owned(),
        boottime_ns: bt,
        half_width_ns,
        semantics,
    }
}

fn frame_bt(frame_index: u64) -> i64 {
    BASE_BT_NS + i64::try_from(frame_index).unwrap_or(0) * FRAME_STEP_NS
}

fn tap_records() -> Vec<TapRecord> {
    gestures()
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let bt = frame_bt(g.start_frame);
            let path = match (g.kind, g.end) {
                (GestureKind::Swipe, Some((ex, ey))) => (0..=4_i64)
                    .map(|step| {
                        let f = f64::from(i32::try_from(step).unwrap_or(0)) / 4.0;
                        TouchPoint {
                            x: g.x + (ex - g.x) * f,
                            y: g.y + (ey - g.y) * f,
                            t_offset_ns: g.duration_ns * step / 4,
                        }
                    })
                    .collect(),
                _ => Vec::new(),
            };
            TapRecord {
                seq: u64::try_from(i).unwrap_or(0) + 1,
                boot_id: Some(BOOT_ID.to_owned()),
                event_mono_ns: Some(bt),
                pc_ingest_cont_ns: bt,
                alignment: anchored(bt, 500_000, TimestampSemantics::InputEventTime),
                bucket: RecordBucket::Main,
                action: if g.kind == GestureKind::Swipe {
                    "SWIPE".to_owned()
                } else {
                    "TAP".to_owned()
                },
                x: Some(g.x),
                y: Some(g.y),
                pointer_id: Some(0),
                gesture_kind: g.kind,
                end_x: g.end.map(|e| e.0),
                end_y: g.end.map(|e| e.1),
                end_t_offset_ns: Some(g.duration_ns),
                path,
            }
        })
        .collect()
}

fn log_record(
    seq: u64,
    frame: u64,
    level: &str,
    tag: &str,
    pkg: Option<&str>,
    msg: &str,
) -> LogRecord {
    let bt = frame_bt(frame);
    LogRecord {
        seq,
        boot_id: Some(BOOT_ID.to_owned()),
        header_epoch_ns: None,
        device_read_boottime_ns: Some(bt),
        pc_ingest_cont_ns: bt,
        alignment: anchored(bt, 1_000_000, TimestampSemantics::LogcatDeviceReadBoottime),
        bucket: RecordBucket::Main,
        level: Some(level.to_owned()),
        tag: Some(tag.to_owned()),
        pid: Some(1234),
        tid: Some(1234),
        uid: pkg.map(|_| 10_200),
        package_name: pkg.map(str::to_owned),
        message: msg.to_owned(),
    }
}

fn parse_args() -> Result<(PathBuf, u32, u64, String), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out_dir: Option<PathBuf> = None;
    let mut gop_seconds = 1_u32;
    let mut frames = 720_u64;
    let mut bundle_id: Option<String> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--out-dir" => out_dir = Some(PathBuf::from(value()?)),
            "--gop-seconds" => gop_seconds = value()?.parse()?,
            "--frames" => frames = value()?.parse()?,
            "--bundle-id" => bundle_id = Some(value()?),
            other => return Err(format!("unknown argument `{other}`").into()),
        }
    }
    if !matches!(gop_seconds, 1 | 5) {
        return Err("--gop-seconds must be 1 or 5".into());
    }
    if frames <= TRIM_OUT {
        return Err(format!("--frames must exceed {TRIM_OUT}").into());
    }
    let out_dir = out_dir.ok_or("--out-dir is required")?;
    let bundle_id = bundle_id.unwrap_or_else(|| format!("gate-gop{gop_seconds}s"));
    if bundle_id.is_empty()
        || !bundle_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err(
            "--bundle-id must contain only ASCII letters, digits, hyphens or underscores".into(),
        );
    }
    Ok((out_dir, gop_seconds, frames, bundle_id))
}

fn streams_summary(frames: u64) -> StreamsSummary {
    StreamsSummary {
        video: Some(VideoStreamMeta {
            status: StreamStatus::Present,
            frame_count: frames,
            tail_may_be_incomplete: false,
            semantics: TimestampSemantics::VideoEncoderInputEstimate,
            display_width: Some(WIDTH),
            display_height: Some(HEIGHT),
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
    }
}

fn frame_records_for(
    video: &Path,
    frames: u64,
) -> Result<Vec<FrameRecord>, Box<dyn std::error::Error>> {
    let track = Mp4VideoTrack::open(video)?;
    let records: Vec<FrameRecord> = track
        .frames()
        .iter()
        .map(|f| {
            let bt = frame_bt(f.frame_index);
            FrameRecord {
                frame_index: f.frame_index,
                boot_id: BOOT_ID.to_owned(),
                pts_device_mono_ns: bt,
                pc_ingest_cont_ns: bt,
                alignment: anchored(bt, 500_000, TimestampSemantics::VideoEncoderInputEstimate),
                is_keyframe: f.is_keyframe,
                gop_id: f.gop_id,
                mp4_sample_offset: f.offset,
                mp4_sample_size: f.size,
            }
        })
        .collect();
    if u64::try_from(records.len())? != frames {
        return Err(format!("muxed {} frames, expected {frames}", records.len()).into());
    }
    Ok(records)
}

fn demo_logs() -> Vec<LogRecord> {
    vec![
        log_record(
            1,
            60,
            "I",
            "ActivityTaskManager",
            None,
            "Displayed com.example.gate/.HomeActivity: +120ms",
        ),
        log_record(
            2,
            320,
            "E",
            "GateClient",
            Some("com.example.gate"),
            "Request failed: 503 (synthetic release-gate log)",
        ),
    ]
}

fn gate_metadata(
    bundle_id: &str,
    bundle_dir: &Path,
    frames: u64,
    gop_seconds: u32,
    gop_frames: u32,
) -> Value {
    let trim_in = i64::try_from(TRIM_IN).unwrap_or(0);
    let gesture_json: Vec<Value> = gestures()
        .iter()
        .map(|g| {
            let kind = if g.kind == GestureKind::Swipe {
                "swipe"
            } else {
                "tap"
            };
            json!({
                "kind": kind,
                "parent_frame": g.start_frame,
                "trim_frame": i64::try_from(g.start_frame).unwrap_or(0) - trim_in,
                "in_trim_range": (TRIM_IN..=TRIM_OUT).contains(&g.start_frame),
                "x": g.x,
                "y": g.y,
                "end_x": g.end.map(|e| e.0),
                "end_y": g.end.map(|e| e.1),
                "duration_ns": g.duration_ns,
            })
        })
        .collect();
    json!({
        "bundle_id": bundle_id,
        "bundle_dir": bundle_dir.to_string_lossy(),
        "width": WIDTH,
        "height": HEIGHT,
        "fps": FPS,
        "frame_count": frames,
        "gop_seconds": gop_seconds,
        "gop_frames": gop_frames,
        "trim_in": TRIM_IN,
        "trim_out": TRIM_OUT,
        "frame_step_ns": FRAME_STEP_NS,
        "barcode": { "bits": BARCODE_BITS, "cell_px": BARCODE_CELL, "msb_first": true },
        "gestures": gesture_json,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (out_dir, gop_seconds, frames, bundle_id) = parse_args()?;
    fs::create_dir_all(&out_dir)?;
    let work = tempfile::tempdir_in(&out_dir)?;
    let gop_frames = gop_seconds * u32::try_from(FPS)?;

    let video_src = work.path().join("video.mp4");
    encode_video(&video_src, frames, gop_frames)?;
    let audio_src = work.path().join("audio.m4a");
    let audio_seconds = u32::try_from(frames / u64::try_from(FPS)? + 1)?;
    encode_audio(&audio_src, audio_seconds)?;
    let frame_records = frame_records_for(&video_src, frames)?;

    let draft = V2BundleDraft {
        memo: None,
        bundle_kind: BundleKind::Full,
        bundle_id: bundle_id.clone(),
        created_at_utc: "2026-11-24T09:00:00Z".to_owned(),
        provenance: Provenance::Original,
        device: DeviceInfo {
            serial: "emu-gate-5554".to_owned(),
            model: "Pixel_Gate".to_owned(),
            api_level: 35,
            is_emulator: true,
        },
        requested_window: RequestedWindow::RealElapsedSec {
            post_roll_ms: 0,
            actual_post_roll_ms: 0,
            post_roll_truncated: false,
            duration_sec: frames / u64::try_from(FPS)?,
            trigger_pc_wall_utc: "2026-11-24T09:00:00Z".to_owned(),
        },
        epochs: vec![EpochSpan {
            boot_id: BOOT_ID.to_owned(),
            start_boottime_ns: BASE_BT_NS,
            end_boottime_ns: frame_bt(frames),
        }],
        streams: streams_summary(frames),
        unanchored_summary: UnanchoredSummary {
            included_count: 0,
            truncated_count: 0,
        },
        video_mp4: Some(MediaPayload::File(video_src)),
        audio_m4a: Some(MediaPayload::File(audio_src)),
        share_mp4: None,
        frames: frame_records,
        discontinuities: Vec::new(),
        logs: demo_logs(),
        taps: tap_records(),
        clock_anchors: Vec::new(),
    };
    let bundle_dir = out_dir.join(&bundle_id);
    V2BundleWriter::write(&bundle_dir, &format!("gate-{bundle_id}"), &draft)?;
    work.close()?;

    let meta = gate_metadata(&bundle_id, &bundle_dir, frames, gop_seconds, gop_frames);
    let meta_path = out_dir.join(format!("{bundle_id}.gate.json"));
    fs::write(&meta_path, serde_json::to_vec_pretty(&meta)?)?;
    println!("{}", bundle_dir.display());
    println!("{}", meta_path.display());
    Ok(())
}
