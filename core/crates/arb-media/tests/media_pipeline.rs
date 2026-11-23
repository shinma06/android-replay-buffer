//! End-to-end integration tests for `arb-media` using pure-Rust OpenH264-synthesized
//! barcode-stamped H.264 fixtures (zero `ffmpeg` runtime dependency).

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use arb_media::MediaError;
#[cfg(target_os = "macos")]
use arb_media::codec::VideoToolboxBackend;
use arb_media::codec::{
    DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat, VideoCodecBackend,
    default_codec_backend,
};
use arb_media::mp4::{Mp4VideoTrack, inspect_mp4_atoms, mux_raw_h264_to_mp4};
use arb_media::playback::{
    AudioClockSource, AvOffsetStatus, CacheLookup, ClockDriveMode, FilmstripCache, FilmstripConfig,
    FilmstripOrigin, GopLruCache, MonotonicTimeSource, PresentationClock, ScrubController,
};
use arb_media::trim::{
    PngExportMetadata, export_frame_png, export_frame_png_with_metadata, trim_video,
};
use tempfile::TempDir;

const WIDTH: u32 = 448;
const HEIGHT: u32 = 256;
const TIMESCALE: u32 = 90_000;
const FRAME_DURATION_TICKS: u32 = 3_000; // 30 fps
const FRAME_DURATION_NS: i64 = 33_333_333;
const FRAME_COUNT: usize = 90;
const GOP_SIZE: u32 = 30;

/// Sync prefix `1010` in the top 4 bits of the 16-bit frame-index barcode.
const BARCODE_SYNC_PREFIX: u16 = 0b1010;
const BARCODE_ORIGIN_X: usize = 16;
const BARCODE_ORIGIN_Y: usize = 16;
const BARCODE_CELL_PX: usize = 24;

/// Generates a deterministic `YUV420p` frame carrying a high-contrast 16-bit machine-readable
/// barcode (`1010` sync + 12-bit `frame_index`) plus a moving bar to exercise inter-frame motion.
fn synthesize_barcode_yuv420p_frame(frame_index: u64) -> DecodedFrame {
    let w = usize::try_from(WIDTH).expect("WIDTH fits usize");
    let h = usize::try_from(HEIGHT).expect("HEIGHT fits usize");
    let y_len = w * h;
    let uv_len = (w / 2) * (h / 2);
    let mut data = vec![128u8; y_len + uv_len * 2];

    // Fill luma with a gentle moving pattern so P-frames carry real motion vectors.
    let bar_x = usize::try_from((frame_index * 9) % u64::from(WIDTH.saturating_sub(32)))
        .expect("bar_x fits usize");
    for row in 0..h {
        let base = row * w;
        for col in 0..w {
            let luma = if row >= 80 && col >= bar_x && col < bar_x + 28 {
                210u8
            } else {
                48u8
            };
            data[base + col] = luma;
        }
    }

    // Stamp 16-bit barcode: 4-bit sync prefix `1010` + 12-bit frame_index.
    let low12 = u16::try_from(frame_index & 0x0FFF).expect("12-bit value fits u16");
    let code: u16 = (BARCODE_SYNC_PREFIX << 12) | low12;
    for bit_idx in 0..16usize {
        let shift = 15 - bit_idx;
        let is_one = ((code >> shift) & 1) == 1;
        let val = if is_one { 235u8 } else { 20u8 };
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

/// Decodes the 16-bit visual barcode from a tightly packed Y-plane.
fn read_barcode_from_y(y_plane: &[u8], width: usize) -> Option<u64> {
    let mut code: u16 = 0;
    for bit_idx in 0..16usize {
        let bx = BARCODE_ORIGIN_X + bit_idx * BARCODE_CELL_PX;
        let mut sum: u32 = 0;
        for dy in 6..18 {
            let row = (BARCODE_ORIGIN_Y + dy) * width;
            for dx in 6..18 {
                sum += u32::from(y_plane[row + bx + dx]);
            }
        }
        let avg = sum / 144;
        let bit = u16::from(avg >= 128);
        code = (code << 1) | bit;
    }
    if (code >> 12) == BARCODE_SYNC_PREFIX {
        Some(u64::from(code & 0x0FFF))
    } else {
        None
    }
}

/// Decodes the 16-bit visual barcode from an interleaved RGB24 image.
fn read_barcode_from_rgb24(rgb: &[u8], width: usize) -> Option<u64> {
    let mut code: u16 = 0;
    for bit_idx in 0..16usize {
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
        let avg = sum / 144;
        let bit = u16::from(avg >= 128);
        code = (code << 1) | bit;
    }
    if (code >> 12) == BARCODE_SYNC_PREFIX {
        Some(u64::from(code & 0x0FFF))
    } else {
        None
    }
}

fn read_barcode_from_frame(frame: &DecodedFrame) -> Option<u64> {
    let w = usize::try_from(frame.width).expect("frame width fits usize");
    match frame.format {
        PixelFormat::Yuv420p => read_barcode_from_y(frame.y_plane()?, w),
        PixelFormat::Rgb24 => read_barcode_from_rgb24(&frame.data, w),
        PixelFormat::Rgba8 | PixelFormat::Bgra8 => {
            let rgb = frame.to_rgb24().ok()?;
            read_barcode_from_rgb24(&rgb.data, w)
        }
        _ => None,
    }
}

/// Builds a synthetic 90-frame (3 GOPs of 30 frames) barcode-stamped H.264 MP4 using `OpenH264`.
fn build_synthetic_fixture_mp4(path: &Path) {
    let backend = OpenH264Backend::new();
    let frames: Vec<DecodedFrame> = (0..FRAME_COUNT)
        .map(|i| synthesize_barcode_yuv420p_frame(u64::try_from(i).expect("index fits u64")))
        .collect();
    let packets = backend
        .encode_yuv420p_sequence(
            &frames,
            GOP_SIZE,
            FRAME_DURATION_NS,
            EncodeParams {
                bitrate_bps: 4_000_000,
                max_fps: 30,
            },
        )
        .expect("OpenH264 synthetic encoding must succeed");

    let width_u16 = u16::try_from(WIDTH).expect("WIDTH fits u16");
    let height_u16 = u16::try_from(HEIGHT).expect("HEIGHT fits u16");
    let report = mux_raw_h264_to_mp4(
        path,
        width_u16,
        height_u16,
        TIMESCALE,
        FRAME_DURATION_TICKS,
        &packets,
    )
    .expect("mux_raw_h264_to_mp4 must succeed");
    assert_eq!(
        report.frame_count,
        u64::try_from(FRAME_COUNT).expect("FRAME_COUNT fits u64"),
        "muxed frame count must equal synthetic frame count"
    );
    assert_eq!(
        report.keyframe_count, 3,
        "90 frames with 30-frame GOP must yield 3 IDR keyframes"
    );
}

#[test]
fn mux_parse_and_index_exact_frames_and_gops() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let mp4_path = tmp.path().join("video.mp4");
    build_synthetic_fixture_mp4(&mp4_path);

    let atoms = inspect_mp4_atoms(&mp4_path).expect("atom inspection must succeed");
    assert!(
        atoms.faststart,
        "muxed MP4 must be faststart (moov before mdat)"
    );
    assert_eq!(
        atoms.edts_count, 0,
        "muxed MP4 must contain zero edts boxes"
    );
    assert_eq!(
        atoms.elst_count, 0,
        "muxed MP4 must contain zero elst boxes"
    );
    assert_eq!(
        atoms.avc1_count, 1,
        "muxed MP4 must contain a single avc1 entry"
    );
    assert_eq!(
        atoms.stsd_entry_count, 1,
        "muxed MP4 must have stsd entry_count == 1"
    );

    let track = Mp4VideoTrack::open(&mp4_path).expect("Mp4VideoTrack::open must succeed");
    assert_eq!(
        track.frame_count(),
        u64::try_from(FRAME_COUNT).expect("FRAME_COUNT fits u64"),
        "indexed frame count must match"
    );
    assert_eq!(
        track.keyframes(),
        &[0, 30, 60],
        "keyframes must sit at exact GOP starts 0, 30, 60"
    );
    assert!(
        !track.has_b_frames(),
        "synthetic baseline stream must not report B-frames"
    );

    for (idx, frame) in track.frames().iter().enumerate() {
        let idx_u64 = u64::try_from(idx).expect("idx fits u64");
        assert_eq!(frame.frame_index, idx_u64, "frame_index must be sequential");
        assert_eq!(
            frame.gop_id,
            idx_u64 / u64::from(GOP_SIZE),
            "gop_id must increment at each keyframe boundary"
        );
        assert_eq!(
            frame.is_keyframe,
            idx_u64 % u64::from(GOP_SIZE) == 0,
            "is_keyframe must match stss positions"
        );
        assert_eq!(
            frame.pts_ns, frame.dts_ns,
            "no-B-frame stream must have pts_ns == dts_ns"
        );
    }
}

#[test]
fn mid_gop_trim_openh264_exact_barcodes_and_zero_elst() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let src_path = tmp.path().join("source.mp4");
    let out_path = tmp.path().join("trimmed_openh264.mp4");
    build_synthetic_fixture_mp4(&src_path);

    let openh264 = OpenH264Backend::new();
    let report = trim_video(&src_path, &out_path, 17, 49, &openh264)
        .expect("mid-GOP trim [17..=49] with OpenH264 must succeed");

    assert_eq!(
        report.output_frame_count, 33,
        "trimmed frame count [17..=49] must be 33"
    );
    assert_eq!(
        report.reencoded_frames, 13,
        "head GOP remainder [17..30) must re-encode 13 frames"
    );
    assert_eq!(
        report.stream_copied_frames, 20,
        "tail [30..=49] must stream-copy 20 frames"
    );
    assert_eq!(
        report.edts_box_count, 0,
        "report must confirm zero edts boxes"
    );
    assert_eq!(
        report.elst_box_count, 0,
        "report must confirm zero elst boxes"
    );

    let atoms = inspect_mp4_atoms(&out_path).expect("trimmed atom inspection must succeed");
    assert_eq!(atoms.edts_count, 0, "trimmed MP4 must have 0 edts boxes");
    assert_eq!(atoms.elst_count, 0, "trimmed MP4 must have 0 elst boxes");
    assert_eq!(atoms.avc1_count, 1, "trimmed MP4 must use single-stsd avc1");
    assert_eq!(
        atoms.stsd_entry_count, 1,
        "trimmed MP4 stsd entry_count must be 1"
    );
    assert!(
        atoms.faststart,
        "trimmed MP4 must have faststart moov before mdat"
    );

    let trimmed = Mp4VideoTrack::open(&out_path).expect("trimmed MP4 must parse cleanly");
    assert_eq!(
        trimmed.frame_count(),
        33,
        "trimmed track must contain exactly 33 frames"
    );
    assert_eq!(
        trimmed.keyframes(),
        &[0, 13],
        "trimmed keyframes must be frame 0 (fresh IDR) and frame 13 (splice IDR)"
    );

    // Decode all 33 frames with OpenH264 and verify exact visual barcode 17 + i on every frame.
    let gop0 = openh264
        .decode_gop(&trimmed, 0, PixelFormat::Yuv420p)
        .expect("decoding trimmed GOP 0 with OpenH264 must succeed");
    let gop1 = openh264
        .decode_gop(&trimmed, 1, PixelFormat::Yuv420p)
        .expect("decoding trimmed GOP 1 with OpenH264 must succeed");
    let all_frames: Vec<DecodedFrame> = gop0.into_iter().chain(gop1).collect();
    assert_eq!(
        all_frames.len(),
        33,
        "decoded trimmed stream must emit all 33 frames"
    );
    for (i, frame) in all_frames.iter().enumerate() {
        let expected_barcode = 17 + u64::try_from(i).expect("i fits u64");
        let got = read_barcode_from_frame(frame);
        assert_eq!(
            got,
            Some(expected_barcode),
            "OpenH264 trimmed frame {i} must carry exact barcode {expected_barcode}"
        );
    }

    // On macOS, cross-verify OpenH264's spliced output decodes cleanly on Apple VideoToolbox too!
    #[cfg(target_os = "macos")]
    {
        let vt = VideoToolboxBackend::try_new().expect("VideoToolbox must initialize on macOS");
        let f0 = vt
            .decode_frame(&trimmed, 0, PixelFormat::Yuv420p)
            .expect("VideoToolbox must decode frame 0 of OpenH264-trimmed MP4");
        let f13 = vt
            .decode_frame(&trimmed, 13, PixelFormat::Yuv420p)
            .expect("VideoToolbox must decode splice IDR frame 13 of OpenH264-trimmed MP4");
        let f32 = vt
            .decode_frame(&trimmed, 32, PixelFormat::Yuv420p)
            .expect("VideoToolbox must decode last frame 32 of OpenH264-trimmed MP4");
        assert_eq!(
            read_barcode_from_frame(&f0),
            Some(17),
            "VideoToolbox cross-decode frame 0 must match barcode 17"
        );
        assert_eq!(
            read_barcode_from_frame(&f13),
            Some(30),
            "VideoToolbox cross-decode splice frame 13 must match barcode 30"
        );
        assert_eq!(
            read_barcode_from_frame(&f32),
            Some(49),
            "VideoToolbox cross-decode last frame 32 must match barcode 49"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn mid_gop_trim_videotoolbox_exact_barcodes_and_zero_elst() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let src_path = tmp.path().join("source.mp4");
    let out_path = tmp.path().join("trimmed_vt.mp4");
    build_synthetic_fixture_mp4(&src_path);

    let vt = VideoToolboxBackend::try_new().expect("VideoToolbox must initialize on macOS");
    let report = trim_video(&src_path, &out_path, 17, 49, &vt)
        .expect("mid-GOP trim [17..=49] with VideoToolbox must succeed");

    assert_eq!(
        report.backend_name, "videotoolbox",
        "report must identify VideoToolbox backend"
    );
    assert_eq!(
        report.output_frame_count, 33,
        "VideoToolbox trimmed frame count must be 33"
    );
    assert_eq!(
        report.reencoded_frames, 13,
        "VideoToolbox head re-encode must be 13 frames"
    );
    assert_eq!(
        report.stream_copied_frames, 20,
        "VideoToolbox stream copy must be 20 frames"
    );

    let atoms =
        inspect_mp4_atoms(&out_path).expect("VideoToolbox trimmed atom inspection must succeed");
    assert_eq!(
        atoms.edts_count, 0,
        "VideoToolbox trimmed MP4 must have 0 edts boxes"
    );
    assert_eq!(
        atoms.elst_count, 0,
        "VideoToolbox trimmed MP4 must have 0 elst boxes"
    );
    assert_eq!(
        atoms.avc1_count, 1,
        "VideoToolbox trimmed MP4 must have single avc1"
    );
    assert_eq!(
        atoms.stsd_entry_count, 1,
        "VideoToolbox trimmed MP4 stsd entry_count must be 1"
    );
    assert!(
        atoms.faststart,
        "VideoToolbox trimmed MP4 must be faststart"
    );

    let trimmed = Mp4VideoTrack::open(&out_path).expect("VideoToolbox trimmed MP4 must parse");
    assert_eq!(
        trimmed.frame_count(),
        33,
        "VideoToolbox trimmed track must contain 33 frames"
    );

    // Verify all 33 frames with VideoToolbox itself.
    let gop0 = vt
        .decode_gop(&trimmed, 0, PixelFormat::Yuv420p)
        .expect("VideoToolbox decode GOP 0 must succeed");
    let gop1 = vt
        .decode_gop(&trimmed, 1, PixelFormat::Yuv420p)
        .expect("VideoToolbox decode GOP 1 must succeed");
    let all_frames: Vec<DecodedFrame> = gop0.into_iter().chain(gop1).collect();
    assert_eq!(
        all_frames.len(),
        33,
        "VideoToolbox must decode all 33 trimmed frames"
    );
    for (i, frame) in all_frames.iter().enumerate() {
        let expected_barcode = 17 + u64::try_from(i).expect("i fits u64");
        assert_eq!(
            read_barcode_from_frame(frame),
            Some(expected_barcode),
            "VideoToolbox trimmed frame {i} must carry exact barcode {expected_barcode}"
        );
    }

    // Cross-verify VideoToolbox-trimmed output with OpenH264 too!
    let openh264 = OpenH264Backend::new();
    let f0 = openh264
        .decode_frame(&trimmed, 0, PixelFormat::Yuv420p)
        .expect("OpenH264 must decode VideoToolbox-trimmed frame 0");
    let f32 = openh264
        .decode_frame(&trimmed, 32, PixelFormat::Yuv420p)
        .expect("OpenH264 must decode VideoToolbox-trimmed frame 32");
    assert_eq!(
        read_barcode_from_frame(&f0),
        Some(17),
        "OpenH264 cross-decode of VideoToolbox trim frame 0 must match barcode 17"
    );
    assert_eq!(
        read_barcode_from_frame(&f32),
        Some(49),
        "OpenH264 cross-decode of VideoToolbox trim frame 32 must match barcode 49"
    );
}

#[test]
fn idr_start_trim_is_lossless_stream_copy_with_exact_barcodes() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let src_path = tmp.path().join("source.mp4");
    let out_path = tmp.path().join("trimmed_idr.mp4");
    build_synthetic_fixture_mp4(&src_path);

    let backend = default_codec_backend();
    let report = trim_video(&src_path, &out_path, 30, 59, backend.as_ref())
        .expect("IDR-start trim [30..=59] must succeed");

    assert_eq!(
        report.reencoded_frames, 0,
        "IDR-start trim must perform 0 re-encodes"
    );
    assert_eq!(
        report.head_decoded_frames, 0,
        "IDR-start trim must decode 0 head frames"
    );
    assert_eq!(
        report.stream_copied_frames, 30,
        "IDR-start trim must stream-copy all 30 frames"
    );
    assert_eq!(
        report.output_frame_count, 30,
        "IDR-start trim output must contain 30 frames"
    );

    let atoms = inspect_mp4_atoms(&out_path).expect("IDR trim atom inspection must succeed");
    assert_eq!(
        atoms.edts_count, 0,
        "IDR-trimmed MP4 must have 0 edts boxes"
    );
    assert_eq!(
        atoms.elst_count, 0,
        "IDR-trimmed MP4 must have 0 elst boxes"
    );

    let trimmed = Mp4VideoTrack::open(&out_path).expect("IDR-trimmed MP4 must parse");
    let frames = backend
        .decode_gop(&trimmed, 0, PixelFormat::Yuv420p)
        .expect("decoding IDR-trimmed GOP 0 must succeed");
    assert_eq!(
        frames.len(),
        30,
        "decoded IDR-trimmed GOP must contain 30 frames"
    );
    for (i, frame) in frames.iter().enumerate() {
        let expected = 30 + u64::try_from(i).expect("i fits u64");
        assert_eq!(
            read_barcode_from_frame(frame),
            Some(expected),
            "IDR-trimmed frame {i} must carry exact barcode {expected}"
        );
    }
}

#[test]
fn export_frame_png_mid_gop_verifies_barcode_and_text_chunks() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let src_path = tmp.path().join("source.mp4");
    let png_path = tmp.path().join("frame_43.png");
    let plain_png_path = tmp.path().join("frame_17.png");
    build_synthetic_fixture_mp4(&src_path);

    let backend = default_codec_backend();
    let meta = PngExportMetadata {
        boottime_ns: Some(123_456_789_000),
        source_bundle_id: Some("bundle-01JABC".to_owned()),
    };
    let report = export_frame_png_with_metadata(&src_path, 43, &png_path, &meta, backend.as_ref())
        .expect("export_frame_png_with_metadata must succeed");
    assert_eq!(
        report.frame_index, 43,
        "PNG report must record exact frame_index 43"
    );
    assert_eq!(report.width, WIDTH, "PNG report width must match source");
    assert_eq!(report.height, HEIGHT, "PNG report height must match source");
    assert!(report.output_bytes > 0, "PNG output file must be non-empty");

    // Decode PNG back and verify both exact visual barcode and `tEXt` chunks.
    let file = File::open(&png_path).expect("exported PNG must be readable");
    let decoder = png::Decoder::new(BufReader::new(file));
    let mut reader = decoder.read_info().expect("PNG header must decode cleanly");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .expect("PNG pixel data must decode cleanly");
    let pixels = &buf[..info.buffer_size()];
    let barcode =
        read_barcode_from_rgb24(pixels, usize::try_from(info.width).expect("w fits usize"));
    assert_eq!(
        barcode,
        Some(43),
        "decoded PNG pixels must carry exact visual barcode 43"
    );

    let text_chunks = &reader.info().uncompressed_latin1_text;
    assert!(
        text_chunks
            .iter()
            .any(|c| c.keyword == "arb:frame_index" && c.text == "43"),
        "PNG must contain arb:frame_index=43 tEXt chunk"
    );
    assert!(
        text_chunks
            .iter()
            .any(|c| c.keyword == "arb:boottime_ns" && c.text == "123456789000"),
        "PNG must contain arb:boottime_ns tEXt chunk"
    );
    assert!(
        text_chunks
            .iter()
            .any(|c| c.keyword == "arb:source_bundle_id" && c.text == "bundle-01JABC"),
        "PNG must contain arb:source_bundle_id tEXt chunk"
    );

    // Also test the default `export_frame_png` helper.
    let plain_report = export_frame_png(&src_path, 17, &plain_png_path, backend.as_ref())
        .expect("export_frame_png must succeed");
    assert_eq!(
        plain_report.frame_index, 17,
        "plain PNG report must record frame 17"
    );
}

#[test]
fn gop_lru_cache_hits_on_backward_and_forward_step_and_evicts_bounded() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let src_path = tmp.path().join("source.mp4");
    build_synthetic_fixture_mp4(&src_path);
    let track = Mp4VideoTrack::open(&src_path).expect("track must open");
    let backend = OpenH264Backend::new();

    let mut cache = GopLruCache::new(2, 64 * 1024 * 1024);

    // Cold miss on frame 17 decodes GOP 0.
    let (f17, lookup17) = cache
        .fetch_frame(&track, 17, PixelFormat::Yuv420p, &backend)
        .expect("fetch frame 17 must succeed");
    assert_eq!(
        lookup17,
        CacheLookup::Miss,
        "initial fetch must be a cache miss"
    );
    assert_eq!(
        read_barcode_from_frame(f17),
        Some(17),
        "frame 17 barcode must match"
    );

    // Forward +1 step to frame 18 hits GOP 0 cache.
    let (f18, lookup18) = cache
        .fetch_frame(&track, 18, PixelFormat::Yuv420p, &backend)
        .expect("fetch frame 18 must succeed");
    assert_eq!(
        lookup18,
        CacheLookup::Hit,
        "forward +1 step inside GOP 0 must hit cache"
    );
    assert_eq!(
        read_barcode_from_frame(f18),
        Some(18),
        "frame 18 barcode must match"
    );

    // Backward -1 step back to frame 17 and 16 hits GOP 0 cache.
    let (f17_again, lookup17_again) = cache
        .fetch_frame(&track, 17, PixelFormat::Yuv420p, &backend)
        .expect("backward step to frame 17 must succeed");
    assert_eq!(
        lookup17_again,
        CacheLookup::Hit,
        "backward -1 step to 17 must hit cache"
    );
    assert_eq!(
        read_barcode_from_frame(f17_again),
        Some(17),
        "cached frame 17 barcode must match"
    );

    let (f16, lookup16) = cache
        .fetch_frame(&track, 16, PixelFormat::Yuv420p, &backend)
        .expect("backward step to frame 16 must succeed");
    assert_eq!(
        lookup16,
        CacheLookup::Hit,
        "backward -1 step to 16 must hit cache"
    );
    assert_eq!(
        read_barcode_from_frame(f16),
        Some(16),
        "cached frame 16 barcode must match"
    );

    let stats_before = cache.stats();
    assert!(
        stats_before.cache_hits >= 3,
        "cache_hits must be >= 3 after forward and backward steps"
    );
    assert_eq!(
        stats_before.cache_misses, 1,
        "only 1 GOP decode should have occurred"
    );

    // Load GOP 1 and GOP 2 with max_gops = 2 -> triggers eviction of GOP 0.
    let _ = cache
        .fetch_frame(&track, 35, PixelFormat::Yuv420p, &backend)
        .expect("fetch GOP 1 must succeed");
    let _ = cache
        .fetch_frame(&track, 65, PixelFormat::Yuv420p, &backend)
        .expect("fetch GOP 2 must succeed");
    let stats_after = cache.stats();
    assert_eq!(
        stats_after.cached_gops, 2,
        "cached_gops must stay bounded by max_gops=2"
    );
    assert_eq!(
        stats_after.evictions, 1,
        "loading 3rd GOP with max_gops=2 must evict 1 GOP"
    );
}

#[test]
fn filmstrip_cache_writes_externally_and_reloads_without_touching_source() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let bundle_dir = tmp.path().join("immutable-bundle");
    let cache_dir = tmp.path().join("external-cache");
    std::fs::create_dir_all(&bundle_dir).expect("bundle_dir creation must succeed");

    let src_path = bundle_dir.join("video.mp4");
    build_synthetic_fixture_mp4(&src_path);
    let original_bytes = std::fs::read(&src_path).expect("reading original MP4 must succeed");

    let track = Mp4VideoTrack::open(&src_path).expect("track must open");
    let backend = default_codec_backend();
    let cfg = FilmstripConfig {
        max_width: 224,
        max_height: 128,
        max_threads: 4,
    };

    let first = FilmstripCache::load_or_generate(
        &cache_dir,
        "bundle-test-01",
        &track,
        cfg,
        backend.as_ref(),
    )
    .expect("initial filmstrip generation must succeed");
    assert_eq!(
        first.origin,
        FilmstripOrigin::Generated,
        "first filmstrip call must generate cache"
    );
    assert_eq!(
        first.cache.len(),
        3,
        "filmstrip must contain all 3 keyframes"
    );
    assert!(
        first.cache_path.starts_with(&cache_dir),
        "filmstrip cache file must live under external cache_dir"
    );
    assert!(
        first.cache_path.exists(),
        "filmstrip.bin must exist on disk after generation"
    );

    // Ensure immutable bundle directory still contains ONLY video.mp4 and exact bytes!
    let bundle_entries: Vec<_> = std::fs::read_dir(&bundle_dir)
        .expect("reading bundle_dir must succeed")
        .collect();
    assert_eq!(
        bundle_entries.len(),
        1,
        "filmstrip generation must never write any file inside immutable bundle directory"
    );
    assert_eq!(
        std::fs::read(&src_path).expect("re-reading original MP4 must succeed"),
        original_bytes,
        "source MP4 bytes must remain completely untouched"
    );

    // Reload from external disk cache.
    let second = FilmstripCache::load_or_generate(
        &cache_dir,
        "bundle-test-01",
        &track,
        cfg,
        backend.as_ref(),
    )
    .expect("filmstrip reload from disk must succeed");
    assert_eq!(
        second.origin,
        FilmstripOrigin::DiskCache,
        "second filmstrip call must reload from disk cache"
    );
    assert_eq!(
        second.cache, first.cache,
        "reloaded filmstrip entries must match generated entries identically"
    );

    // Path traversal bundle_id must be rejected.
    let bad = FilmstripCache::cache_path(&cache_dir, "../escape");
    assert!(
        matches!(bad, Err(MediaError::InvalidArgument(_))),
        "path traversal bundle_id must return InvalidArgument"
    );
}

#[test]
fn scrub_controller_transitions_approximate_to_exact_and_honors_latest_wins() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let bundle_dir = tmp.path().join("bundle");
    std::fs::create_dir_all(&bundle_dir).expect("create bundle dir");
    let src_path = bundle_dir.join("source.mp4");
    let cache_dir = tmp.path().join("cache");
    build_synthetic_fixture_mp4(&src_path);

    let track = Mp4VideoTrack::open(&src_path).expect("track must open");
    let backend = OpenH264Backend::new();
    let filmstrip = FilmstripCache::load_or_generate(
        &cache_dir,
        "bundle-scrub",
        &track,
        FilmstripConfig::default(),
        &backend,
    )
    .expect("filmstrip must generate")
    .cache;

    let mut controller =
        ScrubController::new(GopLruCache::new(2, 64 * 1024 * 1024), Some(filmstrip));

    // Preview mid-GOP frame 17 before GOP 0 is cached -> instant approximate keyframe thumbnail!
    let preview = controller
        .preview_scrub(&track, 17)
        .expect("preview_scrub must succeed")
        .expect("preview_scrub with filmstrip must yield a ScrubFrame");
    assert!(
        preview.is_approximate,
        "uncached mid-GOP preview must set is_approximate = true"
    );
    assert_eq!(
        preview.requested_frame_index, 17,
        "preview must record requested_frame_index = 17"
    );
    assert_eq!(
        preview.displayed_frame_index, 30,
        "nearest keyframe to 17 among [0, 30, 60] must be 30"
    );

    // Latest-wins cancellation: bump generation before settling the stale generation.
    let stale_gen = preview.generation;
    let _newer_gen = controller.cancel_pending();
    let cancelled = controller
        .settle_exact_if_latest(stale_gen, &track, 17, PixelFormat::Yuv420p, &backend)
        .expect("settle_exact_if_latest must not error when superseded");
    assert!(
        cancelled.is_none(),
        "stale scrub generation must be cancelled by latest-wins semantics"
    );

    // Settle exact frame 17 -> transitions is_approximate: true -> false with exact barcode 17!
    let exact = controller
        .settle_exact(&track, 17, PixelFormat::Yuv420p, &backend)
        .expect("settle_exact must succeed");
    assert!(
        !exact.is_approximate,
        "settled exact frame must set is_approximate = false"
    );
    assert_eq!(
        exact.displayed_frame_index, 17,
        "settled frame must display exact requested frame 17"
    );
    let exact_frame = DecodedFrame {
        frame_index: exact.displayed_frame_index,
        width: exact.width,
        height: exact.height,
        format: exact.format,
        data: exact.data,
    };
    assert_eq!(
        read_barcode_from_frame(&exact_frame),
        Some(17),
        "settled scrub frame must carry exact visual barcode 17"
    );

    // Subsequent preview on already-cached frame 18 returns exact immediately!
    let cached_preview = controller
        .preview_scrub(&track, 18)
        .expect("cached preview_scrub must succeed")
        .expect("cached preview must return frame");
    assert!(
        !cached_preview.is_approximate,
        "preview_scrub hitting GOP-LRU cache must return exact frame (is_approximate = false)"
    );
    assert_eq!(
        cached_preview.displayed_frame_index, 18,
        "cached preview must display exact frame 18"
    );
}

#[derive(Debug)]
struct MockMonotonicClock {
    now_ns: AtomicI64,
}

impl MockMonotonicClock {
    const fn new(initial_ns: i64) -> Self {
        Self {
            now_ns: AtomicI64::new(initial_ns),
        }
    }

    fn advance_ns(&self, delta_ns: i64) {
        self.now_ns.fetch_add(delta_ns, Ordering::SeqCst);
    }
}

impl MonotonicTimeSource for MockMonotonicClock {
    fn now_ns(&self) -> i64 {
        self.now_ns.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
struct MockAudioClock {
    active_pos_ns: AtomicI64,
}

impl MockAudioClock {
    const fn new(pos_ns: i64) -> Self {
        Self {
            active_pos_ns: AtomicI64::new(pos_ns),
        }
    }

    fn set_pos_ns(&self, pos_ns: i64) {
        self.active_pos_ns.store(pos_ns, Ordering::SeqCst);
    }
}

impl AudioClockSource for MockAudioClock {
    fn position_ns(&self) -> Option<i64> {
        let v = self.active_pos_ns.load(Ordering::SeqCst);
        if v < 0 { None } else { Some(v) }
    }
}

#[derive(Debug)]
struct SharedMono(std::sync::Arc<MockMonotonicClock>);

impl MonotonicTimeSource for SharedMono {
    fn now_ns(&self) -> i64 {
        self.0.now_ns()
    }
}

#[test]
fn presentation_clock_audio_master_and_monotonic_fallback_with_honest_av_offset() {
    let mono = std::sync::Arc::new(MockMonotonicClock::new(1_000_000_000));

    let mut clock =
        PresentationClock::with_time_source(Box::new(SharedMono(std::sync::Arc::clone(&mono))));

    // Defaults must be honest: 0 ms and Uncalibrated.
    assert_eq!(clock.av_offset_ms(), 0, "default av_offset_ms must be 0");
    assert_eq!(
        clock.av_offset_status(),
        AvOffsetStatus::Uncalibrated,
        "default av_offset_status must be Uncalibrated"
    );

    // 1. Playback with NO audio output device falls back cleanly to monotonic elapsed time.
    clock.play(100_000_000);
    mono.advance_ns(250_000_000);
    let r1 = clock.sample_position(None);
    assert_eq!(
        r1.drive_mode,
        ClockDriveMode::MonotonicFallback,
        "without audio clock, drive_mode must be MonotonicFallback"
    );
    assert_eq!(
        r1.media_position_ns, 350_000_000,
        "monotonic fallback must advance 100ms + 250ms = 350ms"
    );

    // 2. Active audio clock takes over and applies calibrated/user-overridden av_offset_ms.
    let audio = MockAudioClock::new(500_000_000);
    clock.set_av_offset(-40, AvOffsetStatus::UserOverride);
    let r2 = clock.sample_position(Some(&audio));
    assert_eq!(
        r2.drive_mode,
        ClockDriveMode::AudioMaster,
        "active audio clock must switch drive_mode to AudioMaster"
    );
    assert_eq!(
        r2.media_position_ns, 460_000_000,
        "audio master 500ms with -40ms UserOverride must yield 460ms"
    );
    assert_eq!(
        r2.av_offset_status,
        AvOffsetStatus::UserOverride,
        "reading must carry updated AvOffsetStatus"
    );

    // 3. Sudden audio device loss (returns None) falls back smoothly from 460ms without jumping!
    audio.set_pos_ns(-1);
    mono.advance_ns(100_000_000);
    let r3 = clock.sample_position(Some(&audio));
    assert_eq!(
        r3.drive_mode,
        ClockDriveMode::MonotonicFallback,
        "unavailable audio device must fall back cleanly to MonotonicFallback"
    );
    assert_eq!(
        r3.media_position_ns, 560_000_000,
        "fallback after audio loss must continue smoothly from 460ms + 100ms = 560ms"
    );

    // 4. Playback speed scaling (2.0x) and zero-speed validation.
    assert!(
        matches!(
            clock.set_speed_permille(0, None),
            Err(MediaError::InvalidArgument(_))
        ),
        "zero speed_permille must return InvalidArgument"
    );
    clock
        .set_speed_permille(2_000, None)
        .expect("setting 2.0x speed must succeed");
    mono.advance_ns(50_000_000);
    let r_fast = clock.sample_position(None);
    assert_eq!(
        r_fast.media_position_ns, 660_000_000,
        "2.0x speed over 50ms must advance 560ms + 100ms = 660ms"
    );

    // 5. Pause freezes the timeline.
    let paused_at = clock.pause(None);
    assert_eq!(
        paused_at, 660_000_000,
        "pause must return frozen position 660ms"
    );
    mono.advance_ns(500_000_000);
    let r4 = clock.sample_position(None);
    assert_eq!(
        r4.media_position_ns, 660_000_000,
        "paused clock must not advance while monotonic time elapses"
    );

    // 6. Seek and resolve frame_index_for_position against an actual indexed track.
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let src_path = tmp.path().join("clock_track.mp4");
    build_synthetic_fixture_mp4(&src_path);
    let track = Mp4VideoTrack::open(&src_path).expect("track must open");
    assert_eq!(
        PresentationClock::frame_index_for_position(&track, 0),
        Some(0),
        "position 0ns must map to frame 0"
    );
    assert_eq!(
        PresentationClock::frame_index_for_position(&track, 100_000_000),
        Some(3),
        "position 100ms at 30fps must map to frame 3"
    );
}

#[test]
fn b_frame_ctts_is_detected_and_explicitly_rejected_by_trim() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let src_path = tmp.path().join("source.mp4");
    let bframe_path = tmp.path().join("source_with_ctts.mp4");
    let out_path = tmp.path().join("trimmed_bframe.mp4");
    build_synthetic_fixture_mp4(&src_path);

    // Inject a valid `ctts` atom with non-monotonic presentation offsets into `stbl`
    // and adjust parent box sizes + `stco` chunk offset to produce a valid MP4 with B-frame CTS.
    let original = std::fs::read(&src_path).expect("reading source MP4 must succeed");
    let mutated = inject_synthetic_bframe_ctts(&original);
    std::fs::write(&bframe_path, &mutated).expect("writing synthetic ctts MP4 must succeed");

    let track = Mp4VideoTrack::open(&bframe_path).expect("MP4 with ctts must still parse");
    assert!(
        track.has_b_frames(),
        "non-monotonic ctts presentation order must set has_b_frames() == true"
    );

    let backend = OpenH264Backend::new();
    let err = trim_video(&bframe_path, &out_path, 10, 40, &backend);
    assert!(
        matches!(err, Err(MediaError::UnsupportedFeature(_))),
        "trim_video must return explicit UnsupportedFeature on B-frame stream instead of corrupting frames"
    );

    // R2 S9: every frame-index addressed entry point refuses B-frame tracks identically on all
    // OSes (VideoToolbox numbers decode order, OpenH264 presentation order).
    let openh264: Box<dyn VideoCodecBackend> = Box::new(OpenH264Backend::new());
    for backend in [openh264, default_codec_backend()] {
        assert!(
            matches!(
                backend.decode_frame(&track, 12, PixelFormat::Rgb24),
                Err(MediaError::UnsupportedFeature(_))
            ),
            "{}: decode_frame must reject B-frame tracks",
            backend.name()
        );
        assert!(
            matches!(
                backend.decode_gop(&track, 1, PixelFormat::Rgb24),
                Err(MediaError::UnsupportedFeature(_))
            ),
            "{}: decode_gop must reject B-frame tracks",
            backend.name()
        );
        let png = tmp.path().join(format!("bframe-{}.png", backend.name()));
        assert!(
            matches!(
                export_frame_png(&bframe_path, 12, &png, backend.as_ref()),
                Err(MediaError::UnsupportedFeature(_))
            ),
            "{}: export_frame_png must reject B-frame tracks",
            backend.name()
        );
        assert!(!png.exists(), "rejected export must not create a file");
        let mut cache = GopLruCache::new(4, usize::MAX);
        assert!(
            matches!(
                cache.fetch_frame(&track, 12, PixelFormat::Rgb24, backend.as_ref()),
                Err(MediaError::UnsupportedFeature(_))
            ),
            "{}: GopLruCache must reject B-frame tracks",
            backend.name()
        );
    }
    assert_eq!(
        PresentationClock::frame_index_for_position(&track, 100_000_000),
        None,
        "frame_index_for_position must refuse B-frame tracks"
    );
}

/// Inserts a valid 2-entry `ctts` box (non-monotonic composition offsets) into `stbl` of a
/// single-chunk faststart MP4 produced by `mux_raw_h264_to_mp4`, updating enclosing box sizes
/// and `stco` offset accordingly.
fn inject_synthetic_bframe_ctts(mp4: &[u8]) -> Vec<u8> {
    // Build a 32-byte `ctts` full box (version 0, 2 entries: 1 frame +6000, remaining frames 0).
    let mut ctts = Vec::with_capacity(32);
    ctts.extend_from_slice(&32u32.to_be_bytes());
    ctts.extend_from_slice(b"ctts");
    ctts.extend_from_slice(&0u32.to_be_bytes()); // version=0, flags=0
    ctts.extend_from_slice(&2u32.to_be_bytes()); // entry_count = 2
    ctts.extend_from_slice(&1u32.to_be_bytes()); // sample_count = 1
    ctts.extend_from_slice(&6_000u32.to_be_bytes()); // sample_offset = +6000 ticks (reorders behind frame 1)
    let rem = u32::try_from(FRAME_COUNT - 1).expect("remaining fits u32");
    ctts.extend_from_slice(&rem.to_be_bytes());
    ctts.extend_from_slice(&0u32.to_be_bytes());
    assert_eq!(
        ctts.len(),
        32,
        "ctts box length must match declared header size"
    );

    let delta = u32::try_from(ctts.len()).expect("ctts len fits u32");

    // Locate the `stco` box inside `mp4` to insert `ctts` right before `stco`.
    let stco_fourcc_pos = mp4
        .windows(4)
        .position(|w| w == b"stco")
        .expect("source MP4 must contain stco");
    let stco_box_start = stco_fourcc_pos - 4;

    let mut out = Vec::with_capacity(mp4.len() + ctts.len());
    out.extend_from_slice(&mp4[..stco_box_start]);
    out.extend_from_slice(&ctts);
    out.extend_from_slice(&mp4[stco_box_start..]);

    // Update the single `stco` chunk offset (now shifted by `delta` bytes).
    let new_stco_start = stco_box_start + ctts.len();
    let chunk_off_pos = new_stco_start + 16;
    let old_off = u32::from_be_bytes(
        out[chunk_off_pos..chunk_off_pos + 4]
            .try_into()
            .expect("stco offset bytes"),
    );
    out[chunk_off_pos..chunk_off_pos + 4].copy_from_slice(&(old_off + delta).to_be_bytes());

    // Increment sizes of every ancestor container box (`moov`, `trak`, `mdia`, `minf`, `stbl`).
    for tag in [b"moov", b"trak", b"mdia", b"minf", b"stbl"] {
        let pos = out
            .windows(4)
            .position(|w| w == tag)
            .expect("container box must exist");
        let size_pos = pos - 4;
        let old_size = u32::from_be_bytes(
            out[size_pos..size_pos + 4]
                .try_into()
                .expect("container size bytes"),
        );
        out[size_pos..size_pos + 4].copy_from_slice(&(old_size + delta).to_be_bytes());
    }

    out
}

#[test]
fn single_gop_mid_gop_trim_and_bounds_validation() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let src_path = tmp.path().join("source.mp4");
    let out_path = tmp.path().join("trimmed_single_gop.mp4");
    build_synthetic_fixture_mp4(&src_path);

    let backend = default_codec_backend();
    // Both in_frame and out_frame lie strictly inside GOP 0 ([0..30)).
    let report = trim_video(&src_path, &out_path, 10, 20, backend.as_ref())
        .expect("single-GOP mid-GOP trim [10..=20] must succeed");
    assert_eq!(
        report.output_frame_count, 11,
        "single-GOP trim [10..=20] must output 11 frames"
    );
    assert_eq!(
        report.reencoded_frames, 11,
        "all 11 frames in single-GOP mid-GOP trim must be re-encoded"
    );
    assert_eq!(
        report.stream_copied_frames, 0,
        "single-GOP mid-GOP trim must have 0 stream-copied frames"
    );

    let atoms = inspect_mp4_atoms(&out_path).expect("atom audit must succeed");
    assert_eq!(atoms.edts_count, 0, "single-GOP trim must have 0 edts");
    assert_eq!(atoms.elst_count, 0, "single-GOP trim must have 0 elst");

    let trimmed = Mp4VideoTrack::open(&out_path).expect("single-GOP trimmed MP4 must parse");
    let decoded = backend
        .decode_gop(&trimmed, 0, PixelFormat::Bgra8)
        .expect("decoding single-GOP trimmed output into BGRA8 must succeed");
    assert_eq!(
        decoded.len(),
        11,
        "decoded BGRA8 GOP must contain 11 frames"
    );
    assert_eq!(
        read_barcode_from_frame(&decoded[0]),
        Some(10),
        "first frame of [10..=20] must carry barcode 10"
    );
    assert_eq!(
        read_barcode_from_frame(&decoded[10]),
        Some(20),
        "last frame of [10..=20] must carry barcode 20"
    );

    // Invalid ranges must return structured InvalidFrameRange errors.
    let bad_order = trim_video(&src_path, &out_path, 25, 20, backend.as_ref());
    assert!(
        matches!(bad_order, Err(MediaError::InvalidFrameRange { .. })),
        "in_frame > out_frame must fail with InvalidFrameRange"
    );
    let bad_oob = trim_video(&src_path, &out_path, 10, 90, backend.as_ref());
    assert!(
        matches!(bad_oob, Err(MediaError::InvalidFrameRange { .. })),
        "out_frame >= frame_count must fail with InvalidFrameRange"
    );
    let png_oob = export_frame_png(&src_path, 90, tmp.path().join("oob.png"), backend.as_ref());
    assert!(
        matches!(png_oob, Err(MediaError::FrameOutOfRange { .. })),
        "export_frame_png out of bounds must fail with FrameOutOfRange"
    );
}

#[test]
fn corrupt_filmstrip_cache_is_safely_regenerated() {
    let tmp = TempDir::new().expect("tempdir creation must succeed");
    let bundle_dir = tmp.path().join("bundle");
    std::fs::create_dir_all(&bundle_dir).expect("create bundle dir");
    let src_path = bundle_dir.join("source.mp4");
    let cache_dir = tmp.path().join("cache");
    build_synthetic_fixture_mp4(&src_path);

    let track = Mp4VideoTrack::open(&src_path).expect("track must open");
    let backend = OpenH264Backend::new();
    let cfg = FilmstripConfig {
        max_width: 160,
        max_height: 90,
        max_threads: 2,
    };

    let first =
        FilmstripCache::load_or_generate(&cache_dir, "bundle-corrupt", &track, cfg, &backend)
            .expect("initial filmstrip generation must succeed");
    assert_eq!(
        first.origin,
        FilmstripOrigin::Generated,
        "initial call must generate cache"
    );

    // Corrupt a byte inside the external cache file and verify checksum rejection + auto-regeneration.
    let mut bytes = std::fs::read(&first.cache_path).expect("reading filmstrip.bin must succeed");
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&first.cache_path, &bytes).expect("corrupting filmstrip.bin must succeed");

    let direct_err =
        FilmstripCache::load_from_file(&first.cache_path, track.file_len(), track.frame_count());
    assert!(
        matches!(direct_err, Err(MediaError::CacheCorrupt(_))),
        "corrupted filmstrip.bin must fail checksum with CacheCorrupt"
    );

    let recovered =
        FilmstripCache::load_or_generate(&cache_dir, "bundle-corrupt", &track, cfg, &backend)
            .expect("corrupted cache must transparently regenerate");
    assert_eq!(
        recovered.origin,
        FilmstripOrigin::Generated,
        "corrupted cache must trigger fresh generation"
    );
    assert_eq!(
        recovered.cache, first.cache,
        "regenerated filmstrip must match original"
    );
}
