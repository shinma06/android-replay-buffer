//! Integration tests for the single-file H.264 + AAC-LC `share.mp4` muxer ([`mux_share_mp4`]).
//!
//! Video is real `OpenH264` output; audio is a synthetic AAC-LC `audio.m4a` whose access units
//! carry unique tags, so byte-for-byte stream-copy identity is verifiable without `ffmpeg`.
//! Real `ffprobe`/`ffmpeg` verification is documented in `VER-14-av-mux-contact-sheet.md` and
//! driven by the `#[ignore]`d [`emit_share_mp4_for_external_probe`] test.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use arb_media::MediaError;
use arb_media::audio::{
    AacMuxInput, AacPacket, Mp4AudioTrack, build_aac_lc_audio_specific_config, mux_aac_to_m4a,
};
use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
use arb_media::mp4::{Mp4AtomReport, Mp4VideoTrack, inspect_mp4_atoms, mux_raw_h264_to_mp4};
use arb_media::mux_av::mux_share_mp4;
use tempfile::TempDir;

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;
const TIMESCALE: u32 = 90_000;
const FRAME_TICKS: u32 = 3_000; // 30 fps
const FRAME_NS: i64 = 33_333_333;
const FRAME_COUNT: usize = 60; // exactly 2.0 s
const GOP: u32 = 30;
const AUDIO_PACKETS: usize = 100; // 2.133 s > video
const NS_PER_PACKET: i64 = 21_333_333; // 1024 / 48 kHz (floor)

fn yuv_frame(idx: u64) -> DecodedFrame {
    let w = usize::try_from(WIDTH).expect("width");
    let h = usize::try_from(HEIGHT).expect("height");
    let mut data = vec![128u8; w * h + 2 * (w / 2) * (h / 2)];
    let bar = usize::try_from((idx * 3) % 100).expect("bar");
    for row in 0..h {
        for col in 0..w {
            data[row * w + col] = if col >= bar && col < bar + 20 {
                220
            } else {
                40
            };
        }
    }
    DecodedFrame {
        frame_index: idx,
        width: WIDTH,
        height: HEIGHT,
        format: PixelFormat::Yuv420p,
        data,
    }
}

fn build_video(path: &Path) {
    let frames: Vec<DecodedFrame> = (0..FRAME_COUNT)
        .map(|i| yuv_frame(u64::try_from(i).expect("index")))
        .collect();
    let packets = OpenH264Backend::new()
        .encode_yuv420p_sequence(
            &frames,
            GOP,
            FRAME_NS,
            EncodeParams {
                bitrate_bps: 1_000_000,
                max_fps: 30,
            },
        )
        .expect("encode synthetic video");
    mux_raw_h264_to_mp4(
        path,
        u16::try_from(WIDTH).expect("w"),
        u16::try_from(HEIGHT).expect("h"),
        TIMESCALE,
        FRAME_TICKS,
        &packets,
    )
    .expect("mux video.mp4");
}

fn aac_payload(seq: usize) -> Vec<u8> {
    let mut au = vec![
        0x21,
        0x10,
        0x04,
        0x60,
        0x8C,
        0x1C,
        u8::try_from(seq & 0xFF).expect("low"),
        u8::try_from((seq >> 8) & 0xFF).expect("high"),
    ];
    for extra in 0..(seq % 7) {
        au.push(u8::try_from((seq + extra) & 0xFF).expect("pad"));
    }
    au
}

fn build_audio(path: &Path) -> Vec<Vec<u8>> {
    let asc = build_aac_lc_audio_specific_config(48_000, 2).expect("asc");
    let payloads: Vec<Vec<u8>> = (0..AUDIO_PACKETS).map(aac_payload).collect();
    let packets = payloads
        .iter()
        .enumerate()
        .map(|(i, data)| AacPacket {
            pts_us: i64::try_from(i).expect("idx") * 1024 * 1_000_000 / 48_000,
            data: data.clone(),
        })
        .collect();
    mux_aac_to_m4a(
        path,
        &AacMuxInput {
            audio_specific_config: asc.to_vec(),
            sample_rate_hz: 48_000,
            channels: 2,
            packets,
        },
    )
    .expect("mux audio.m4a");
    payloads
}

struct Fixture {
    _tmp: TempDir,
    dir: PathBuf,
    video: PathBuf,
    audio: PathBuf,
    audio_payloads: Vec<Vec<u8>>,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    let video = dir.join("video.mp4");
    let audio = dir.join("audio.m4a");
    build_video(&video);
    let audio_payloads = build_audio(&audio);
    Fixture {
        _tmp: tmp,
        dir,
        video,
        audio,
        audio_payloads,
    }
}

fn count_boxes(report: &Mp4AtomReport, fourcc: [u8; 4]) -> usize {
    report.boxes.iter().filter(|b| b.fourcc == fourcc).count()
}

/// Track IDs (`tkhd`) in file order.
fn track_ids(path: &Path, report: &Mp4AtomReport) -> Vec<u32> {
    let bytes = fs::read(path).expect("read share.mp4");
    report
        .boxes
        .iter()
        .filter(|b| &b.fourcc == b"tkhd")
        .map(|b| {
            // 8-byte header + version/flags(4) + creation(4) + modification(4) => track_ID.
            let at = usize::try_from(b.offset).expect("offset") + 20;
            u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
        })
        .collect()
}

fn assert_no_temp_leftovers(dir: &Path) {
    let leftovers: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

fn read_all_video(track: &Mp4VideoTrack) -> Vec<Vec<u8>> {
    (0..track.frame_count())
        .map(|i| {
            let mut buf = Vec::new();
            track.read_sample_into(i, &mut buf).expect("read sample");
            buf
        })
        .collect()
}

fn read_all_audio(track: &Mp4AudioTrack) -> Vec<Vec<u8>> {
    (0..track.packet_count())
        .map(|i| {
            let mut buf = Vec::new();
            track.read_packet_into(i, &mut buf).expect("read packet");
            buf
        })
        .collect()
}

#[test]
fn mux_h264_and_aac_into_faststart_zero_elst_share_mp4() {
    let fx = fixture();
    let out = fx.dir.join("share.mp4");
    let report = mux_share_mp4(&fx.video, Some(&fx.audio), 0, &out).expect("mux share.mp4");

    // Container structure.
    let atoms = inspect_mp4_atoms(&out).expect("inspect share.mp4");
    assert!(atoms.faststart, "moov must precede mdat");
    assert_eq!(atoms.edts_count, 0, "zero edts");
    assert_eq!(atoms.elst_count, 0, "zero elst");
    assert_eq!(atoms.avc1_count, 1, "single avc1 sample entry");
    assert_eq!(atoms.stsd_entry_count, 1, "single-entry video stsd");
    assert_eq!(count_boxes(&atoms, *b"trak"), 2, "video + audio trak");
    assert_eq!(count_boxes(&atoms, *b"mp4a"), 1, "one mp4a entry");
    assert_eq!(count_boxes(&atoms, *b"esds"), 1, "one esds");
    assert_eq!(count_boxes(&atoms, *b"mdat"), 1, "one mdat");
    assert_eq!(track_ids(&out, &atoms), vec![1, 2], "video=1, audio=2");
    let top: Vec<[u8; 4]> = atoms
        .boxes
        .iter()
        .filter(|b| b.depth == 0)
        .map(|b| b.fourcc)
        .collect();
    assert_eq!(top, vec![*b"ftyp", *b"moov", *b"mdat"], "top-level order");

    // ftyp brands.
    let bytes = fs::read(&out).expect("read share.mp4");
    let ftyp_len = usize::try_from(u32::from_be_bytes(bytes[0..4].try_into().expect("size")))
        .expect("ftyp len");
    let ftyp = &bytes[8..ftyp_len];
    assert_eq!(&ftyp[..4], b"isom", "major brand");
    let brands: Vec<&[u8]> = ftyp[8..].chunks(4).collect();
    for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
        assert!(
            brands.iter().any(|b| b == brand),
            "compatible brand {}",
            String::from_utf8_lossy(brand)
        );
    }

    // Video: every sample is copied bit-for-bit (the source already has in-band SPS/PPS).
    let src_video = Mp4VideoTrack::open(&fx.video).expect("open source video");
    let out_video = Mp4VideoTrack::open(&out).expect("open share video");
    assert_eq!(out_video.frame_count(), src_video.frame_count());
    assert_eq!(out_video.keyframes(), src_video.keyframes());
    assert_eq!(out_video.timescale(), src_video.timescale());
    assert_eq!(
        (out_video.width(), out_video.height()),
        (src_video.width(), src_video.height())
    );
    assert_eq!(out_video.frames()[0].pts_ns, 0, "video starts at 0");
    assert_eq!(
        read_all_video(&out_video),
        read_all_video(&src_video),
        "video samples bit-for-bit"
    );

    // Audio: first 94 packets (those starting before the 2.0 s video end), bit-for-bit.
    let out_audio = Mp4AudioTrack::open(&out).expect("open share audio");
    assert_eq!(out_audio.sample_rate_hz(), 48_000);
    assert_eq!(out_audio.channels(), 2);
    assert_eq!(out_audio.packets()[0].pts_ns, 0, "audio starts at 0");
    let kept = usize::try_from(out_audio.packet_count()).expect("count");
    assert_eq!(kept, 94, "packets starting at/after 2.0 s are dropped");
    assert_eq!(
        read_all_audio(&out_audio),
        fx.audio_payloads[..kept].to_vec()
    );

    // Interleaving: audio payload begins before the last video sample (not video-then-audio).
    let first_audio_off = out_audio.packets()[0].offset;
    let last_video_off = out_video.frames().last().expect("frames").offset;
    assert!(
        first_audio_off < last_video_off,
        "audio must be interleaved with video, not appended"
    );

    // Report.
    assert!(report.audio_included);
    assert_eq!(report.video_frame_count, 60);
    assert_eq!(report.video_keyframe_count, 2);
    assert_eq!(report.video_duration_ns, 2_000_000_000);
    assert_eq!(report.audio_packet_count, 94);
    assert_eq!(report.audio_packets_skipped_leading, 0);
    assert_eq!(report.audio_packets_dropped_trailing, 6);
    assert_eq!(report.sample_rate_hz, 48_000);
    assert_eq!(report.channels, 2);
    assert_eq!(report.audio_start_ns, 0);
    assert_eq!(report.audio_residual_offset_ns, 0);
    assert_eq!((report.edts_box_count, report.elst_box_count), (0, 0));
    assert!(report.faststart);
    assert_eq!(
        report.output_bytes,
        u64::try_from(bytes.len()).expect("len")
    );
    assert_no_temp_leftovers(&fx.dir);
}

#[test]
fn video_only_when_audio_is_none_is_clean_single_track() {
    let fx = fixture();
    let out = fx.dir.join("video_only.mp4");
    let report = mux_share_mp4(&fx.video, None, 0, &out).expect("mux video-only");
    assert!(!report.audio_included, "no audio requested");
    assert_eq!(report.audio_packet_count, 0);
    assert_eq!(report.sample_rate_hz, 0);

    let atoms = inspect_mp4_atoms(&out).expect("inspect");
    assert!(atoms.faststart);
    assert_eq!((atoms.edts_count, atoms.elst_count), (0, 0));
    assert_eq!(count_boxes(&atoms, *b"trak"), 1, "video trak only");
    assert_eq!(count_boxes(&atoms, *b"mp4a"), 0);
    assert_eq!(track_ids(&out, &atoms), vec![1]);
    let src = Mp4VideoTrack::open(&fx.video).expect("src");
    let got = Mp4VideoTrack::open(&out).expect("out");
    assert_eq!(read_all_video(&got), read_all_video(&src));
    assert!(
        Mp4AudioTrack::open(&out).is_err(),
        "video-only file has no audio track"
    );
}

#[test]
fn negative_lead_in_skips_leading_packets_and_reports_residual() {
    let fx = fixture();
    let out = fx.dir.join("neg.mp4");
    // Audio began 100 ms before the video: packets ending at/before video 0 are skipped.
    let report = mux_share_mp4(&fx.video, Some(&fx.audio), -100_000_000, &out).expect("mux");

    // Packet i ends at (i + 1) * 21.333 ms; i <= 3 ends at/before 100 ms (4 * 21.33 = 85.3 ms).
    assert_eq!(report.audio_packets_skipped_leading, 4);
    assert!(report.audio_included);
    assert_eq!(report.requested_audio_lead_in_offset_ns, -100_000_000);
    assert_eq!(report.audio_start_ns, 0, "muxed audio starts at >= 0");
    // First retained packet #4 started 85.33 ms into the audio => -14.67 ms vs video 0.
    let expected_residual = 4 * NS_PER_PACKET - 100_000_000;
    assert!(
        (report.audio_residual_offset_ns - expected_residual).abs() < 1_000,
        "residual {} vs {expected_residual}",
        report.audio_residual_offset_ns
    );
    assert!(
        report.audio_residual_offset_ns <= 0 && report.audio_residual_offset_ns > -22_000_000,
        "residual is within one packet and non-positive"
    );

    let atoms = inspect_mp4_atoms(&out).expect("inspect");
    assert_eq!((atoms.edts_count, atoms.elst_count), (0, 0));
    let audio = Mp4AudioTrack::open(&out).expect("open audio");
    assert!(
        audio.packets().iter().all(|p| p.pts_ns >= 0),
        "no negative audio pts"
    );
    assert_eq!(audio.packets()[0].pts_ns, 0);
    let got = read_all_audio(&audio);
    assert_eq!(got[0], fx.audio_payloads[4], "first retained is source #4");
    assert_eq!(got[..], fx.audio_payloads[4..4 + got.len()]);
}

#[test]
fn positive_lead_in_is_reported_without_elst() {
    let fx = fixture();
    let out = fx.dir.join("pos.mp4");
    let report = mux_share_mp4(&fx.video, Some(&fx.audio), 500_000_000, &out).expect("mux");

    assert!(report.audio_included);
    assert_eq!(report.audio_packets_skipped_leading, 0);
    assert_eq!(
        report.audio_start_ns, 0,
        "audio cannot be delayed without elst"
    );
    assert_eq!(
        report.audio_residual_offset_ns, 500_000_000,
        "the whole 500 ms offset is reported as residual"
    );
    assert_eq!(report.requested_audio_lead_in_offset_ns, 500_000_000);
    let atoms = inspect_mp4_atoms(&out).expect("inspect");
    assert!(atoms.faststart);
    assert_eq!((atoms.edts_count, atoms.elst_count), (0, 0));
    let audio = Mp4AudioTrack::open(&out).expect("open audio");
    assert_eq!(audio.packets()[0].pts_ns, 0);
}

#[test]
fn audio_entirely_outside_video_range_falls_back_to_video_only() {
    let fx = fixture();

    // Audio ended before the video began (> 2.2 s lead-in): nothing at >= 0.
    let early = fx.dir.join("early.mp4");
    let report = mux_share_mp4(&fx.video, Some(&fx.audio), -10_000_000_000, &early).expect("mux");
    assert!(!report.audio_included, "no audio packet at >= 0");
    assert_eq!(report.audio_packets_skipped_leading, 100);
    assert_eq!(report.audio_residual_offset_ns, 0);
    let atoms = inspect_mp4_atoms(&early).expect("inspect");
    assert_eq!(count_boxes(&atoms, *b"trak"), 1);
    assert_eq!((atoms.edts_count, atoms.elst_count), (0, 0));

    // Audio starts after the video ended.
    let late = fx.dir.join("late.mp4");
    let report = mux_share_mp4(&fx.video, Some(&fx.audio), 2_500_000_000, &late).expect("mux");
    assert!(!report.audio_included, "audio begins after the video ends");
    assert_eq!(report.audio_packets_dropped_trailing, 100);
    assert_eq!(
        count_boxes(&inspect_mp4_atoms(&late).expect("inspect"), *b"trak"),
        1
    );
}

#[test]
fn refuses_to_replace_outputs_or_overwrite_inputs_and_never_touches_them() {
    let fx = fixture();
    let video_before = fs::read(&fx.video).expect("read video");
    let audio_before = fs::read(&fx.audio).expect("read audio");

    // Existing output is never replaced.
    let out = fx.dir.join("share.mp4");
    fs::write(&out, b"precious").expect("seed output");
    let err = mux_share_mp4(&fx.video, Some(&fx.audio), 0, &out).expect_err("must not replace");
    assert!(
        matches!(&err, MediaError::Io(e) if e.kind() == ErrorKind::AlreadyExists),
        "AlreadyExists expected, got {err:?}"
    );
    assert_eq!(
        fs::read(&out).expect("read"),
        b"precious",
        "output untouched"
    );

    // Output == input (directly and through `.` aliasing).
    for target in [&fx.video, &fx.audio] {
        let err =
            mux_share_mp4(&fx.video, Some(&fx.audio), 0, target).expect_err("input as output");
        assert!(matches!(err, MediaError::InvalidArgument(_)), "got {err:?}");
    }
    let aliased = fx.dir.join(".").join("video.mp4");
    let err = mux_share_mp4(&fx.video, None, 0, &aliased).expect_err("aliased input as output");
    assert!(matches!(err, MediaError::InvalidArgument(_)), "got {err:?}");

    assert_eq!(fs::read(&fx.video).expect("read"), video_before);
    assert_eq!(fs::read(&fx.audio).expect("read"), audio_before);
    assert_no_temp_leftovers(&fx.dir);
}

#[cfg(unix)]
#[test]
fn refuses_symlink_to_input_as_output() {
    let fx = fixture();
    let link = fx.dir.join("share.mp4");
    std::os::unix::fs::symlink(&fx.video, &link).expect("symlink");
    let before = fs::read(&fx.video).expect("read video");
    let err = mux_share_mp4(&fx.video, None, 0, &link).expect_err("symlinked input");
    assert!(matches!(err, MediaError::InvalidArgument(_)), "got {err:?}");
    assert_eq!(
        fs::read(&fx.video).expect("read"),
        before,
        "input untouched"
    );
    assert_no_temp_leftovers(&fx.dir);
}

#[test]
fn malformed_inputs_return_errors_and_leave_no_output() {
    let fx = fixture();
    let out = fx.dir.join("share.mp4");

    let junk = fx.dir.join("junk.m4a");
    fs::write(&junk, b"not an mp4 at all").expect("junk");
    assert!(mux_share_mp4(&fx.video, Some(&junk), 0, &out).is_err());
    assert!(!out.exists(), "failed mux publishes nothing");

    let missing = fx.dir.join("missing.mp4");
    assert!(mux_share_mp4(&missing, None, 0, &out).is_err());
    assert!(!out.exists());
    assert_no_temp_leftovers(&fx.dir);
}

/// Writes `video.mp4`, `share.mp4`, `share_neg.mp4` and `share_video_only.mp4` into
/// `$ARB_VER14_OUT_DIR`, muxing the real-AAC `$ARB_VER14_AUDIO` (e.g. made with `ffmpeg`), so
/// `ffprobe`/`ffmpeg` can verify them. Run explicitly with `--ignored`.
#[test]
#[ignore = "driver for the manual ffprobe/ffmpeg verification in VER-14"]
fn emit_share_mp4_for_external_probe() {
    let out_dir = PathBuf::from(std::env::var("ARB_VER14_OUT_DIR").expect("ARB_VER14_OUT_DIR"));
    let audio = PathBuf::from(std::env::var("ARB_VER14_AUDIO").expect("ARB_VER14_AUDIO"));
    fs::create_dir_all(&out_dir).expect("out dir");
    let video = out_dir.join("video.mp4");
    build_video(&video);
    for (name, audio_path, offset) in [
        ("share.mp4", Some(audio.as_path()), 0i64),
        ("share_neg.mp4", Some(audio.as_path()), -100_000_000),
        ("share_pos.mp4", Some(audio.as_path()), 250_000_000),
        ("share_video_only.mp4", None, 0),
    ] {
        let report = mux_share_mp4(&video, audio_path, offset, out_dir.join(name))
            .expect("mux share variant");
        println!("{name}: {report:?}");
    }
}
