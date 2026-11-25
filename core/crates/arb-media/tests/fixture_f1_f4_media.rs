//! Shared media fixtures F1 (portrait 1080x2400 @ 30fps short 2-GOP smoke with independent
//! barcode & integer-PTS oracle) and F4 (corrupt NAL length, mid-file EOF, oversized `stts`
//! count, and missing final duration fault matrix with bounded-error & post-fault recovery checks).

use std::fs;
use std::path::Path;

use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
use arb_media::mp4::{Mp4VideoTrack, inspect_mp4_atoms, mux_raw_h264_to_mp4};
use arb_media::mux_av::mux_share_mp4;
use arb_media::playback::GopLruCache;
use arb_media::trim::trim_video;
use tempfile::TempDir;

const TIMESCALE: u32 = 90_000;
const FRAME_DURATION_TICKS: u32 = 3_000; // exact 30 fps in 90 kHz timescale
const FRAME_DURATION_NS: i64 = 33_333_333;
const BARCODE_SYNC_PREFIX: u16 = 0b1010;
const BARCODE_ORIGIN_X: usize = 16;
const BARCODE_ORIGIN_Y: usize = 16;

/// Independent oracle for a synthetic barcode-stamped video sequence (never built from a reader).
struct VideoFixtureOracle {
    width: u32,
    height: u32,
    frame_count: u64,
    gop_size: u32,
    barcode_cell_px: usize,
    expected_pts_ticks: Vec<i64>,
    expected_keyframes: Vec<u64>,
}

impl VideoFixtureOracle {
    fn new(
        width: u32,
        height: u32,
        frame_count: u64,
        gop_size: u32,
        barcode_cell_px: usize,
    ) -> Self {
        let mut expected_pts_ticks =
            Vec::with_capacity(usize::try_from(frame_count).expect("usize"));
        let mut expected_keyframes = Vec::new();
        for idx in 0..frame_count {
            let pts_ticks = i64::try_from(idx).expect("i64") * i64::from(FRAME_DURATION_TICKS);
            expected_pts_ticks.push(pts_ticks);
            if idx % u64::from(gop_size) == 0 {
                expected_keyframes.push(idx);
            }
        }
        Self {
            width,
            height,
            frame_count,
            gop_size,
            barcode_cell_px,
            expected_pts_ticks,
            expected_keyframes,
        }
    }

    fn synthesize_frame(&self, frame_index: u64) -> DecodedFrame {
        let w = usize::try_from(self.width).expect("width fits usize");
        let h = usize::try_from(self.height).expect("height fits usize");
        let y_len = w * h;
        let uv_len = (w / 2) * (h / 2);
        let mut data = vec![128_u8; y_len + uv_len * 2];

        // Fill Y plane with dark background and a moving vertical stripe to exercise motion vectors.
        data[..y_len].fill(40);
        let bar_w = (w / 16).max(8);
        let bar_x = usize::try_from((frame_index * 17) % u64::try_from(w - bar_w).expect("u64"))
            .expect("bar_x fits usize");
        let stripe_top = (h / 3).min(h.saturating_sub(16));
        let stripe_bottom = (stripe_top + h / 6).min(h);
        for row in stripe_top..stripe_bottom {
            let off = row * w + bar_x;
            data[off..off + bar_w].fill(210);
        }

        // Stamp 16-bit machine barcode (`1010` sync prefix + 12-bit frame_index).
        let low12 = u16::try_from(frame_index & 0x0FFF).expect("12-bit fits u16");
        let code = (BARCODE_SYNC_PREFIX << 12) | low12;
        let cell = self.barcode_cell_px;
        for bit_idx in 0..16_usize {
            let is_one = ((code >> (15 - bit_idx)) & 1) == 1;
            let luma = if is_one { 235_u8 } else { 20_u8 };
            let bx = BARCODE_ORIGIN_X + bit_idx * cell;
            for dy in 0..cell {
                let row_off = (BARCODE_ORIGIN_Y + dy) * w + bx;
                data[row_off..row_off + cell].fill(luma);
            }
        }

        DecodedFrame {
            frame_index,
            width: self.width,
            height: self.height,
            format: PixelFormat::Yuv420p,
            data,
        }
    }

    fn read_barcode_from_rgba(&self, frame: &DecodedFrame) -> Option<u64> {
        let w = usize::try_from(frame.width).expect("width fits usize");
        let cell = self.barcode_cell_px;
        let margin = (cell / 4).max(1);
        let mut code = 0_u16;
        for bit_idx in 0..16_usize {
            let bx = BARCODE_ORIGIN_X + bit_idx * cell;
            let mut sum = 0_u64;
            let mut count = 0_u64;
            for dy in margin..(cell - margin) {
                let row = (BARCODE_ORIGIN_Y + dy) * w;
                for dx in margin..(cell - margin) {
                    let px = (row + bx + dx) * 4;
                    let luma = (u64::from(frame.data[px])
                        + u64::from(frame.data[px + 1])
                        + u64::from(frame.data[px + 2]))
                        / 3;
                    sum += luma;
                    count += 1;
                }
            }
            let bit = u16::from(sum / count.max(1) >= 128);
            code = (code << 1) | bit;
        }
        ((code >> 12) == BARCODE_SYNC_PREFIX).then_some(u64::from(code & 0x0FFF))
    }

    fn write_mp4(&self, path: &Path, bitrate_bps: u32) {
        let backend = OpenH264Backend::new();
        let frames: Vec<DecodedFrame> = (0..self.frame_count)
            .map(|i| self.synthesize_frame(i))
            .collect();
        let packets = backend
            .encode_yuv420p_sequence(
                &frames,
                self.gop_size,
                FRAME_DURATION_NS,
                EncodeParams {
                    bitrate_bps,
                    max_fps: 30,
                },
            )
            .expect("OpenH264 encode must succeed");
        let report = mux_raw_h264_to_mp4(
            path,
            u16::try_from(self.width).expect("u16"),
            u16::try_from(self.height).expect("u16"),
            TIMESCALE,
            FRAME_DURATION_TICKS,
            &packets,
        )
        .expect("mux_raw_h264_to_mp4 must succeed");
        assert_eq!(
            report.frame_count, self.frame_count,
            "muxed frame count must equal oracle"
        );
    }
}

fn read_bytes(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read file bytes")
}

/// Locates the first ISO-BMFF box payload offset (`body_start..box_end`) matching `fourcc`.
fn find_box_span(bytes: &[u8], fourcc: [u8; 4]) -> Option<(usize, usize)> {
    bytes.windows(4).enumerate().find_map(|(pos, w)| {
        if w != fourcc || pos < 4 {
            return None;
        }
        let size = u32::from_be_bytes([
            bytes[pos - 4],
            bytes[pos - 3],
            bytes[pos - 2],
            bytes[pos - 1],
        ]);
        let end = (pos - 4).checked_add(usize::try_from(size).ok()?)?;
        (size >= 8 && end <= bytes.len()).then_some((pos + 4, end))
    })
}

#[test]
fn f1_portrait_1080x2400_30fps_two_gop_mux_decode_trim_matches_independent_oracle() {
    let tmp = TempDir::new().expect("tempdir");
    let src_mp4 = tmp.path().join("f1_portrait_1080x2400.mp4");
    let trimmed_mp4 = tmp.path().join("f1_portrait_trimmed.mp4");

    // 2 GOPs of 12 frames = 24 frames at full portrait 1080x2400 @ 30fps (fast CI smoke).
    let oracle = VideoFixtureOracle::new(1080, 2400, 24, 12, 48);
    oracle.write_mp4(&src_mp4, 6_000_000);

    let track = Mp4VideoTrack::open(&src_mp4).expect("open F1 portrait MP4");
    assert_eq!(track.width(), 1080, "portrait width must be 1080");
    assert_eq!(track.height(), 2400, "portrait height must be 2400");
    assert_eq!(
        track.frame_count(),
        oracle.frame_count,
        "F1 frame count must match independent oracle"
    );
    assert_eq!(
        track.keyframes(),
        oracle.expected_keyframes.as_slice(),
        "F1 keyframe positions must match 2-GOP oracle [0, 12]"
    );
    let actual_pts_ticks: Vec<i64> = track.frames().iter().map(|f| f.pts_ticks).collect();
    assert_eq!(
        actual_pts_ticks, oracle.expected_pts_ticks,
        "F1 integer PTS ticks must match independent oracle"
    );

    // Exact seek + RGBA decode at GOP starts, mid-GOPs, and final frame.
    let backend = OpenH264Backend::new();
    let mut gop_cache = GopLruCache::new(2, 256 * 1024 * 1024);
    for target_idx in [0_u64, 5, 12, 19, 23] {
        let (decoded, _) = gop_cache
            .fetch_frame(&track, target_idx, PixelFormat::Rgba8, &backend)
            .expect("decode portrait F1 frame");
        assert_eq!(decoded.width, 1080, "decoded portrait width");
        assert_eq!(decoded.height, 2400, "decoded portrait height");
        assert_eq!(
            oracle.read_barcode_from_rgba(decoded),
            Some(target_idx),
            "decoded portrait frame {target_idx} barcode must match independent oracle"
        );
    }

    // Mid-GOP trim crossing the GOP boundary [5..=19] -> 15 frames.
    let trim_in = 5_u64;
    let trim_out = 19_u64;
    let report = trim_video(&src_mp4, &trimmed_mp4, trim_in, trim_out, &backend)
        .expect("mid-GOP portrait trim must succeed");
    assert_eq!(
        report.output_frame_count,
        trim_out - trim_in + 1,
        "trimmed portrait frame count must equal inclusive range"
    );

    let trimmed_track = Mp4VideoTrack::open(&trimmed_mp4).expect("open trimmed portrait MP4");
    assert_eq!(trimmed_track.width(), 1080, "trimmed width must stay 1080");
    assert_eq!(
        trimmed_track.height(),
        2400,
        "trimmed height must stay 2400"
    );
    assert!(
        trimmed_track.frames()[0].is_keyframe,
        "trimmed output frame 0 must be a clean IDR keyframe"
    );
    let mut trimmed_cache = GopLruCache::new(2, 256 * 1024 * 1024);
    for (out_idx, expected_src_idx) in [(0_u64, 5_u64), (7, 12), (14, 19)] {
        let (decoded, _) = trimmed_cache
            .fetch_frame(&trimmed_track, out_idx, PixelFormat::Rgba8, &backend)
            .expect("decode trimmed portrait frame");
        assert_eq!(
            oracle.read_barcode_from_rgba(decoded),
            Some(expected_src_idx),
            "trimmed frame {out_idx} must carry source barcode {expected_src_idx}"
        );
    }
}

fn assert_fault_keeps_existing_output_intact(
    fault_mp4: &Path,
    existing_out: &Path,
    existing_sha: &[u8],
    fresh_out: &Path,
    label: &str,
) {
    let backend = OpenH264Backend::new();
    let _ = inspect_mp4_atoms(fault_mp4);

    // Every entry point (`open`, `seek/decode`, `trim`, `share export`) must never panic
    // and must never mutate pre-existing output files.
    let mut saw_error = false;
    match Mp4VideoTrack::open(fault_mp4) {
        Ok(track) => {
            let mut cache = GopLruCache::new(2, 32 * 1024 * 1024);
            let last = track.frame_count().saturating_sub(1);
            if cache
                .fetch_frame(&track, 0, PixelFormat::Rgba8, &backend)
                .is_err()
                || cache
                    .fetch_frame(&track, last, PixelFormat::Rgba8, &backend)
                    .is_err()
            {
                saw_error = true;
            }
        }
        Err(_) => {
            saw_error = true;
        }
    }

    let trim_existing = trim_video(fault_mp4, existing_out, 0, 4, &backend);
    assert!(
        trim_existing.is_err(),
        "{label}: trim into existing output path must return bounded Err"
    );
    assert_eq!(
        read_bytes(existing_out),
        existing_sha,
        "{label}: pre-existing output must stay byte-identical after failed trim"
    );

    let share_existing = mux_share_mp4(fault_mp4, None, 0, existing_out);
    assert!(
        share_existing.is_err(),
        "{label}: mux_share_mp4 into existing output path must return bounded Err"
    );
    assert_eq!(
        read_bytes(existing_out),
        existing_sha,
        "{label}: pre-existing output must stay byte-identical after failed share export"
    );

    let trim_fresh = trim_video(fault_mp4, fresh_out, 0, 4, &backend);
    if trim_fresh.is_err() {
        saw_error = true;
    }
    assert!(
        saw_error,
        "{label}: hostile input must fail cleanly at open, decode, or trim"
    );
}

#[test]
fn f4_corrupt_nal_eof_and_stts_faults_return_bounded_errors_and_preserve_recovery() {
    let tmp = TempDir::new().expect("tempdir");
    let good_mp4 = tmp.path().join("good_baseline.mp4");
    let existing_out = tmp.path().join("existing_output_must_not_change.mp4");
    fs::write(&existing_out, b"SENTINEL-EXISTING-EXPORT-BYTES").expect("write sentinel");
    let existing_sha = read_bytes(&existing_out);

    let oracle = VideoFixtureOracle::new(160, 128, 12, 6, 8);
    oracle.write_mp4(&good_mp4, 400_000);
    let good_bytes = fs::read(&good_mp4).expect("read good mp4");
    let good_track = Mp4VideoTrack::open(&good_mp4).expect("open good baseline");

    // Fault 1: corrupt AVCC NAL length prefix inside sample 0 (`0x7FFF_FFFF` > sample size).
    let mut corrupt_nal = good_bytes.clone();
    let sample0_off = usize::try_from(good_track.frame(0).expect("frame 0").offset).expect("off");
    corrupt_nal[sample0_off..sample0_off + 4].copy_from_slice(&0x7FFF_FFFF_u32.to_be_bytes());
    let fault_nal = tmp.path().join("f4_corrupt_nal.mp4");
    fs::write(&fault_nal, &corrupt_nal).expect("write corrupt nal");
    assert_fault_keeps_existing_output_intact(
        &fault_nal,
        &existing_out,
        &existing_sha,
        &tmp.path().join("out_nal.mp4"),
        "corrupt_nal_len",
    );

    // Fault 2: mid-file EOF truncating `mdat` samples.
    let cut_len = good_bytes.len() / 2;
    let fault_eof = tmp.path().join("f4_truncated_eof.mp4");
    fs::write(&fault_eof, &good_bytes[..cut_len]).expect("write truncated eof");
    assert_fault_keeps_existing_output_intact(
        &fault_eof,
        &existing_out,
        &existing_sha,
        &tmp.path().join("out_eof.mp4"),
        "truncated_eof",
    );

    // Fault 3: oversized `stts` entry_count (`0x7FFF_FFFF`) far exceeding box payload.
    let (stts_body_start, stts_end) = find_box_span(&good_bytes, *b"stts").expect("locate stts");
    let mut stts_oversized = good_bytes.clone();
    stts_oversized[stts_body_start + 4..stts_body_start + 8]
        .copy_from_slice(&0x7FFF_FFFF_u32.to_be_bytes());
    let fault_stts_count = tmp.path().join("f4_stts_oversized_count.mp4");
    fs::write(&fault_stts_count, &stts_oversized).expect("write oversized stts");
    assert_fault_keeps_existing_output_intact(
        &fault_stts_count,
        &existing_out,
        &existing_sha,
        &tmp.path().join("out_stts_count.mp4"),
        "stts_oversized_entry_count",
    );

    // Fault 4a: `stts` box truncated mid-entry (physically missing final 4-byte duration field).
    let mut stts_truncated = good_bytes.clone();
    let stts_size_pos = stts_body_start - 8;
    let orig_stts_size = u32::try_from(stts_end - stts_size_pos).expect("stts size u32");
    stts_truncated[stts_size_pos..stts_size_pos + 4]
        .copy_from_slice(&(orig_stts_size - 4).to_be_bytes());
    let fault_stts_missing_dur = tmp.path().join("f4_stts_missing_duration.mp4");
    fs::write(&fault_stts_missing_dur, &stts_truncated).expect("write short stts");
    assert_fault_keeps_existing_output_intact(
        &fault_stts_missing_dur,
        &existing_out,
        &existing_sha,
        &tmp.path().join("out_stts_dur.mp4"),
        "stts_truncated_final_duration",
    );

    // Fault 4b: `stts` declaring 1 fewer sample than `stsz` (missing last sample duration entry)
    // must never panic or OOM on open/seek/trim.
    let mut stts_short_sum = good_bytes;
    stts_short_sum[stts_body_start + 8..stts_body_start + 12]
        .copy_from_slice(&(u32::try_from(oracle.frame_count - 1).expect("u32")).to_be_bytes());
    let fault_stts_short = tmp.path().join("f4_stts_short_sample_sum.mp4");
    fs::write(&fault_stts_short, &stts_short_sum).expect("write short-sum stts");
    if let Ok(short_track) = Mp4VideoTrack::open(&fault_stts_short) {
        let mut cache = GopLruCache::new(2, 32 * 1024 * 1024);
        let backend = OpenH264Backend::new();
        let last = short_track.frame_count().saturating_sub(1);
        let (decoded, _) = cache
            .fetch_frame(&short_track, last, PixelFormat::Rgba8, &backend)
            .expect("short-sum stts must still decode final frame safely without panic");
        assert_eq!(
            oracle.read_barcode_from_rgba(decoded),
            Some(last),
            "short-sum stts fallback must still decode correct final frame"
        );
    }

    // Immediate recovery: opening, seeking, decoding, and trimming the healthy material succeeds.
    let recovered = Mp4VideoTrack::open(&good_mp4).expect("reopen healthy mp4 after faults");
    let backend = OpenH264Backend::new();
    let mut cache = GopLruCache::new(2, 32 * 1024 * 1024);
    let (decoded, _) = cache
        .fetch_frame(&recovered, 6, PixelFormat::Rgba8, &backend)
        .expect("decode healthy frame 6 immediately after fault inputs");
    assert_eq!(
        oracle.read_barcode_from_rgba(decoded),
        Some(6),
        "recovered healthy decode must match independent barcode oracle"
    );
    let recovered_trim = tmp.path().join("recovered_trim.mp4");
    let trim_rep = trim_video(&good_mp4, &recovered_trim, 2, 9, &backend)
        .expect("healthy trim must succeed immediately after faults");
    assert_eq!(
        trim_rep.output_frame_count, 8,
        "recovered healthy trim must produce exact 8 frames"
    );
}
