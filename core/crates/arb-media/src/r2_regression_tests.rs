//! Regression tests for the R2 review fixes S2–S8 and S10–S12 in `arb-media`.
//!
//! S1 (`VideoToolbox` callback-state ownership) is FFI-only; it is exercised on macOS by every
//! test here that decodes through [`default_codec_backend`] (including the in-band SPS/PPS
//! decoder rebuild when decoding a trimmed output whose head carries a new SPS/PPS).

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::MediaError;
use crate::codec::{
    DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat, VideoCodecBackend, copy_plane_rows,
    default_codec_backend, split_yuv420p_planes,
};
use crate::mp4::{
    Avc1TrackHeader, Mp4VideoTrack, RawH264Packet, SampleMeta, annexb_to_avcc_sample,
    build_avcc_payload, mux_raw_h264_to_mp4, normalize_avcc_nal_lengths, split_avcc_nals,
    write_faststart_avc1_mp4,
};
use crate::output::Publish;
use crate::playback::{
    AudioClockSource, ClockDriveMode, FilmstripCache, FilmstripConfig, GopLruCache,
    MonotonicTimeSource, PresentationClock,
};
use crate::trim::{export_frame_png, trim_video};

const W: u32 = 64;
const H: u32 = 48;
const TIMESCALE: u32 = 90_000;
const DUR_TICKS: u32 = 3_000;
const FRAME_NS: i64 = 33_333_333;

fn frame(i: u64) -> DecodedFrame {
    let (w, h) = (
        usize::try_from(W).expect("w"),
        usize::try_from(H).expect("h"),
    );
    let mut data = vec![128u8; w * h + 2 * (w / 2) * (h / 2)];
    let bar = usize::try_from(i * 3 % 48).expect("bar");
    for row in 0..h {
        for col in 0..w {
            data[row * w + col] = if col >= bar && col < bar + 12 {
                220
            } else {
                40
            };
        }
    }
    DecodedFrame {
        frame_index: i,
        width: W,
        height: H,
        format: PixelFormat::Yuv420p,
        data,
    }
}

fn encode_packets(count: u64, gop: u32) -> Vec<RawH264Packet> {
    let frames: Vec<DecodedFrame> = (0..count).map(frame).collect();
    OpenH264Backend::new()
        .encode_yuv420p_sequence(
            &frames,
            gop,
            FRAME_NS,
            EncodeParams {
                bitrate_bps: 1_000_000,
                max_fps: 30,
            },
        )
        .expect("encode fixture")
}

fn fixture(dir: &Path, name: &str, count: u64, gop: u32) -> PathBuf {
    let path = dir.join(name);
    let w = u16::try_from(W).expect("w");
    let h = u16::try_from(H).expect("h");
    mux_raw_h264_to_mp4(
        &path,
        w,
        h,
        TIMESCALE,
        DUR_TICKS,
        &encode_packets(count, gop),
    )
    .expect("mux fixture");
    path
}

fn no_temp_leftovers(dir: &Path) {
    let leftovers: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
}

// ---------------- S3: odd frame dimensions never panic ----------------

#[test]
fn odd_dimension_frames_convert_without_panicking() {
    let (w, h) = (19usize, 13usize);
    let chroma = w.div_ceil(2) * h.div_ceil(2);
    let yuv = DecodedFrame {
        frame_index: 0,
        width: 19,
        height: 13,
        format: PixelFormat::Yuv420p,
        data: vec![100u8; w * h + 2 * chroma],
    };
    let planes = split_yuv420p_planes(&yuv.data, w, h).expect("ceil-sized planes");
    assert_eq!(planes.u.len(), 10 * 7, "U plane is ceil(19/2) x ceil(13/2)");
    assert_eq!(
        yuv.to_rgb24().expect("rgb").data.len(),
        w * h * 3,
        "RGB24 of an odd frame has exactly w*h*3 bytes"
    );
    assert_eq!(yuv.to_rgba8().expect("rgba").len(), w * h * 4, "RGBA8 size");
    assert_eq!(yuv.to_bgra8().expect("bgra").len(), w * h * 4, "BGRA8 size");

    // A floor-sized (w/2*h/2) buffer is now reported as truncated instead of indexing OOB.
    let floor_sized = DecodedFrame {
        data: vec![100u8; w * h + 2 * (w / 2) * (h / 2)],
        ..yuv.clone()
    };
    assert!(
        matches!(floor_sized.to_rgb24(), Err(MediaError::Codec(_))),
        "short odd-size buffer must be an error, not a panic"
    );

    // Short source planes are padded, never sliced out of bounds.
    let mut dst = vec![0u8; 10 * 7];
    copy_plane_rows(&mut dst, &[7u8; 25], 8, 10, 7, 128);
    assert_eq!(&dst[..8], &[7u8; 8], "available bytes are copied");
    assert_eq!(dst[69], 128, "missing bytes are filled");

    // OpenH264 cannot encode odd sizes: a clean error instead of its internal assert panic.
    assert!(
        matches!(
            OpenH264Backend::new().encode_yuv420p_sequence(
                std::slice::from_ref(&yuv),
                1,
                FRAME_NS,
                EncodeParams::default()
            ),
            Err(MediaError::UnsupportedFeature(_))
        ),
        "odd-size encode must fail cleanly"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn odd_dimension_nv12_conversions_never_panic() {
    // NV12 helpers exist only for the macOS VideoToolbox backend.
    let (w, h) = (19usize, 13usize);
    let y = vec![50u8; 20 * h];
    let uv = vec![128u8; 20 * h.div_ceil(2)];
    let yuv = crate::codec::nv12_to_yuv420p(&y, &uv, 20, 20, w, h);
    assert_eq!(
        yuv.len(),
        w * h + 2 * w.div_ceil(2) * h.div_ceil(2),
        "NV12 -> YUV420p keeps ceil chroma"
    );
    let mut rgb = vec![0u8; w * h * 3];
    crate::codec::nv12_to_rgb24(&y, &uv[..uv.len() - 15], 20, 20, w, h, &mut rgb);
    let thumb = crate::codec::nv12_to_thumb_rgb24(&y, &uv, 20, 20, w, h, 7, 5);
    assert_eq!(thumb.len(), 7 * 5 * 3, "thumbnail size");
}

// ---------------- S4: no sync sample at frame 0 ----------------

#[test]
fn trim_and_decode_refuse_frames_before_the_first_sync_sample() {
    let tmp = TempDir::new().expect("tmp");
    let packets = encode_packets(30, 10);
    // Drop the leading IDR: sample 0 is a P-frame, the first sync sample is old frame 10.
    let path = tmp.path().join("no-leading-idr.mp4");
    mux_raw_h264_to_mp4(
        &path,
        u16::try_from(W).expect("w"),
        u16::try_from(H).expect("h"),
        TIMESCALE,
        DUR_TICKS,
        &packets[1..],
    )
    .expect("mux");
    let track = Mp4VideoTrack::open(&path).expect("open");
    assert!(!track.frames()[0].is_keyframe, "sample 0 must not be sync");
    assert_eq!(track.keyframes()[0], 9, "first IDR is sample 9");

    let backend = OpenH264Backend::new();
    for (i, (a, b)) in [(0u64, 15u64), (3, 15)].into_iter().enumerate() {
        let out = tmp.path().join(format!("bad-{i}.mp4"));
        assert!(
            matches!(
                trim_video(&path, &out, a, b, &backend),
                Err(MediaError::UnsupportedFeature(_))
            ),
            "trim [{a}..={b}] before the first IDR must be refused"
        );
        assert!(!out.exists(), "refused trim must not create output");
    }
    assert!(
        matches!(
            backend.decode_frame(&track, 2, PixelFormat::Rgb24),
            Err(MediaError::UnsupportedFeature(_))
        ),
        "decoding a frame with no preceding IDR must be refused"
    );
    let out = tmp.path().join("ok.mp4");
    let report = trim_video(&path, &out, 9, 15, &backend).expect("trim from first IDR");
    assert_eq!(report.output_frame_count, 7, "frames 9..=15");
    no_temp_leftovers(tmp.path());
}

// ---------------- S2 / S5 / S7: non-4-byte NAL length sources ----------------

/// Writes an MP4 whose `avcC` declares 2-byte NAL lengths and whose samples use them.
fn two_byte_nal_fixture(dir: &Path) -> PathBuf {
    let packets = encode_packets(30, 10);
    let mut sps = None;
    let mut pps = None;
    let mut samples = Vec::new();
    for pkt in &packets {
        let four = annexb_to_avcc_sample(&pkt.annex_b, &mut sps, &mut pps).expect("avcc");
        let mut two = Vec::new();
        for nal in split_avcc_nals(&four, 4) {
            two.extend_from_slice(&u16::try_from(nal.len()).expect("nal<64k").to_be_bytes());
            two.extend_from_slice(nal);
        }
        samples.push((two, pkt.is_keyframe));
    }
    let mut avcc = build_avcc_payload(&sps.expect("sps"), &pps.expect("pps")).expect("avcC");
    avcc[4] = 0xFC | 0x01; // lengthSizeMinusOne = 1 (2-byte NAL lengths)
    let metas: Vec<SampleMeta> = samples
        .iter()
        .map(|(d, k)| SampleMeta {
            size: u32::try_from(d.len()).expect("size"),
            duration_ticks: DUR_TICKS,
            is_keyframe: *k,
        })
        .collect();
    let path = dir.join("nal2.mp4");
    write_faststart_avc1_mp4(
        &path,
        Publish::NoReplace,
        &Avc1TrackHeader {
            width: u16::try_from(W).expect("w"),
            height: u16::try_from(H).expect("h"),
            timescale: TIMESCALE,
            avcc_payload: &avcc,
        },
        &metas,
        |idx, buf| {
            buf.clear();
            buf.extend_from_slice(&samples[idx].0);
            Ok(())
        },
    )
    .expect("write 2-byte NAL fixture");
    path
}

fn assert_all_samples_4byte(track: &Mp4VideoTrack) {
    assert_eq!(
        track.avcc().nal_length_size,
        4,
        "output avcC must declare 4-byte NAL lengths"
    );
    let mut buf = Vec::new();
    for idx in 0..track.frame_count() {
        track.read_sample_into(idx, &mut buf).expect("read");
        let reparsed = normalize_avcc_nal_lengths(&buf, 4)
            .unwrap_or_else(|e| panic!("sample {idx} is not a valid 4-byte AVCC sample: {e}"));
        assert_eq!(reparsed, buf, "sample {idx} must already be 4-byte framed");
    }
}

#[test]
fn two_byte_nal_length_sources_decode_and_trim_to_consistent_4byte_tracks() {
    let tmp = TempDir::new().expect("tmp");
    let src = two_byte_nal_fixture(tmp.path());
    let track = Mp4VideoTrack::open(&src).expect("open");
    assert_eq!(
        track.avcc().nal_length_size,
        2,
        "fixture uses 2-byte lengths"
    );

    // S2: the platform default backend (VideoToolbox on macOS) decodes 2-byte sources.
    let backend = default_codec_backend();
    let decoded = backend
        .decode_frame(&track, 14, PixelFormat::Rgb24)
        .expect("decode 2-byte NAL source");
    assert_eq!((decoded.width, decoded.height), (W, H), "decoded size");

    // S5/S7: IDR-start stream copy and mid-GOP splice both emit uniform 4-byte tracks.
    for (name, a, b) in [("copy.mp4", 10u64, 29u64), ("splice.mp4", 13, 27)] {
        let out = tmp.path().join(name);
        let report = trim_video(&src, &out, a, b, backend.as_ref()).expect("trim");
        assert_eq!(report.output_frame_count, b - a + 1, "{name}: frame count");
        let trimmed = Mp4VideoTrack::open(&out).expect("open trimmed");
        assert_all_samples_4byte(&trimmed);
        for gop in 0..u64::try_from(trimmed.keyframes().len()).expect("gops") {
            let frames = backend
                .decode_gop(&trimmed, gop, PixelFormat::Yuv420p)
                .expect("trimmed output must decode");
            assert!(!frames.is_empty(), "{name}: gop {gop} decodes");
        }
    }
    no_temp_leftovers(tmp.path());
}

// ---------------- S6: never overwrite the input, atomic publish ----------------

#[test]
fn trim_and_png_export_refuse_to_overwrite_input_or_existing_output() {
    let tmp = TempDir::new().expect("tmp");
    let src = fixture(tmp.path(), "video.mp4", 20, 10);
    let original = fs::read(&src).expect("read source");
    let backend = OpenH264Backend::new();

    assert!(
        matches!(
            trim_video(&src, &src, 0, 9, &backend),
            Err(MediaError::InvalidArgument(_))
        ),
        "trim output == input must be refused"
    );
    let dotted = tmp.path().join(".").join("video.mp4");
    assert!(
        matches!(
            trim_video(&src, &dotted, 0, 9, &backend),
            Err(MediaError::InvalidArgument(_))
        ),
        "an alias path resolving to the input must be refused"
    );
    assert!(
        matches!(
            export_frame_png(&src, 3, &src, &backend),
            Err(MediaError::InvalidArgument(_))
        ),
        "PNG output == input must be refused"
    );
    assert_eq!(
        fs::read(&src).expect("re-read"),
        original,
        "source bytes must be untouched"
    );

    let existing = tmp.path().join("existing.mp4");
    fs::write(&existing, b"keep me").expect("seed existing");
    assert!(
        matches!(
            trim_video(&src, &existing, 0, 9, &backend),
            Err(MediaError::Io(ref e)) if e.kind() == std::io::ErrorKind::AlreadyExists
        ),
        "trim must not replace an existing file"
    );
    assert_eq!(
        fs::read(&existing).expect("read existing"),
        b"keep me",
        "existing output untouched"
    );

    let out = tmp.path().join("ok.mp4");
    trim_video(&src, &out, 0, 9, &backend).expect("fresh trim");
    let png = tmp.path().join("f3.png");
    let report = export_frame_png(&src, 3, &png, &backend).expect("fresh png");
    assert_eq!(
        report.output_bytes,
        fs::metadata(&png).expect("png meta").len(),
        "reported size matches the published file"
    );
    no_temp_leftovers(tmp.path());
}

// ---------------- S8: untrusted sample-table counts ----------------

fn patch_u32(bytes: &mut [u8], fourcc: [u8; 4], field_offset: usize, value: u32) {
    let pos = bytes
        .windows(4)
        .position(|w| w == fourcc.as_slice())
        .unwrap_or_else(|| panic!("{} not found", String::from_utf8_lossy(&fourcc)));
    let at = pos + 4 + field_offset;
    bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

#[test]
fn crafted_huge_sample_table_counts_return_err_without_allocating() {
    let tmp = TempDir::new().expect("tmp");
    let src = fixture(tmp.path(), "base.mp4", 20, 10);
    let base = fs::read(&src).expect("read");
    let open_patched = |name: &str, patch: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = base.clone();
        patch(&mut bytes);
        let path = tmp.path().join(name);
        fs::write(&path, &bytes).expect("write patched");
        Mp4VideoTrack::open(&path)
    };

    // stts entry_count far beyond the box (would read/allocate billions of entries).
    let r = open_patched("stts-entries.mp4", &|b| {
        patch_u32(b, *b"stts", 4, 0x7FFF_FFFF);
    });
    assert!(
        matches!(r, Err(MediaError::InvalidContainer(_))),
        "bogus stts entry_count must be rejected: {r:?}"
    );
    // stts per-entry sample_count of 2^32-1 (repeat_n bomb): expansion is capped at stsz.
    let r = open_patched("stts-count.mp4", &|b| patch_u32(b, *b"stts", 8, u32::MAX));
    let track = r.expect("capped stts expansion still parses");
    assert_eq!(track.frame_count(), 20, "frame count comes from stsz");
    // stsz constant size with 2^32-1 samples (vec! bomb).
    let r = open_patched("stsz-const.mp4", &|b| {
        patch_u32(b, *b"stsz", 4, 1);
        patch_u32(b, *b"stsz", 8, u32::MAX);
    });
    assert!(
        matches!(r, Err(MediaError::InvalidContainer(_))),
        "bogus constant-size stsz must be rejected: {r:?}"
    );
    // stsz variable sizes with a count far beyond the box.
    let r = open_patched("stsz-var.mp4", &|b| patch_u32(b, *b"stsz", 8, 0x4000_0000));
    assert!(
        matches!(r, Err(MediaError::InvalidContainer(_))),
        "bogus stsz sample_count must be rejected: {r:?}"
    );
    // stsc / stco / stss entry counts beyond the box.
    for (name, cc) in [
        ("stsc.mp4", b"stsc"),
        ("stco.mp4", b"stco"),
        ("stss.mp4", b"stss"),
    ] {
        let r = open_patched(name, &|b| patch_u32(b, *cc, 4, 0x2000_0000));
        assert!(
            matches!(r, Err(MediaError::InvalidContainer(_))),
            "bogus {} count must be rejected: {r:?}",
            String::from_utf8_lossy(cc)
        );
    }
}

#[test]
fn full_hd_forward_playback_starts_one_decoder_per_gop_within_128_mib() {
    let tmp = TempDir::new().expect("tmp");
    let path = tmp.path().join("fhd.mp4");
    let frames: Vec<_> = (0..30)
        .map(|i| DecodedFrame {
            frame_index: i,
            width: 1920,
            height: 1080,
            format: PixelFormat::Yuv420p,
            data: vec![100 + u8::try_from(i).unwrap(); 1920 * 1080 * 3 / 2],
        })
        .collect();
    let packets = OpenH264Backend::new()
        .encode_yuv420p_sequence(&frames, 30, FRAME_NS, EncodeParams::default())
        .unwrap();
    drop(frames);
    mux_raw_h264_to_mp4(&path, 1920, 1080, TIMESCALE, DUR_TICKS, &packets).unwrap();
    let track = Mp4VideoTrack::open(path).unwrap();
    let backend = default_codec_backend();
    let budget = 128 * 1024 * 1024;
    let mut cache = GopLruCache::new(4, budget);
    for i in 0..30 {
        let (frame, _) = cache
            .fetch_frame(&track, i, PixelFormat::Rgba8, backend.as_ref())
            .unwrap();
        assert_eq!(frame.frame_index, i);
        assert_eq!(frame.byte_len(), 1920 * 1080 * 4);
        assert!(cache.stats().cached_bytes <= budget);
    }
    assert_eq!(
        cache.bypassed_decodes(),
        1,
        "one IDR start for 30 FHD frames"
    );
    assert_eq!(
        cache
            .fetch_frame(&track, 29, PixelFormat::Rgba8, backend.as_ref())
            .unwrap()
            .1,
        crate::playback::CacheLookup::Hit
    );
    let (rewound, _) = cache
        .fetch_frame(&track, 0, PixelFormat::Rgba8, backend.as_ref())
        .unwrap();
    assert_eq!(
        rewound.data,
        backend
            .decode_frame(&track, 0, PixelFormat::Rgba8)
            .unwrap()
            .data
    );
    assert_eq!(cache.bypassed_decodes(), 2);
}

// ---------------- S10: GOP cache memory bound before decoding ----------------

#[test]
fn gop_cache_never_decodes_a_gop_larger_than_max_bytes_into_memory() {
    let tmp = TempDir::new().expect("tmp");
    let src = fixture(tmp.path(), "sparse.mp4", 30, 30);
    let track = Mp4VideoTrack::open(&src).expect("open");
    let backend = OpenH264Backend::new();
    let frame_bytes = usize::try_from(W * H * 3).expect("bytes");
    // Room for 4 RGB frames, the GOP has 30.
    let mut cache = GopLruCache::new(8, frame_bytes * 4);

    let expected = backend
        .decode_frame(&track, 17, PixelFormat::Rgb24)
        .expect("reference decode");
    let (got, _) = cache
        .fetch_frame(&track, 17, PixelFormat::Rgb24, &backend)
        .expect("bounded fetch");
    assert_eq!(got.data, expected.data, "bypass returns the exact frame");
    let stats = cache.stats();
    assert_eq!(stats.cached_gops, 0, "oversized GOP must not be cached");
    assert_eq!(
        stats.cached_bytes,
        frame_bytes * 4,
        "bounded sliding window"
    );
    for index in 18..30 {
        let (frame, _) = cache
            .fetch_frame(&track, index, PixelFormat::Rgb24, &backend)
            .unwrap();
        assert_eq!(
            frame.data,
            backend
                .decode_frame(&track, index, PixelFormat::Rgb24)
                .unwrap()
                .data
        );
        assert!(cache.stats().cached_bytes <= frame_bytes * 4);
    }
    for index in [29, 28, 29] {
        assert_eq!(
            cache
                .fetch_frame(&track, index, PixelFormat::Rgb24, &backend)
                .unwrap()
                .1,
            crate::playback::CacheLookup::Hit
        );
    }
    assert_eq!(cache.bypassed_decodes(), 1, "one bypassed decode");

    cache
        .fetch_frame(&track, 2, PixelFormat::Rgb24, &backend)
        .unwrap();
    assert_eq!(cache.bypassed_decodes(), 2);

    // A GOP that fits is still cached normally.
    let small = fixture(tmp.path(), "dense.mp4", 30, 3);
    let small_track = Mp4VideoTrack::open(&small).expect("open dense");
    let mut cache2 = GopLruCache::new(8, frame_bytes * 4);
    cache2
        .fetch_frame(&small_track, 4, PixelFormat::Rgb24, &backend)
        .expect("fetch");
    assert_eq!(
        cache2.stats().cached_gops,
        1,
        "3-frame GOP fits and is cached"
    );
    assert!(
        cache2.stats().cached_bytes <= frame_bytes * 4,
        "cache never exceeds max_bytes"
    );
}

// ---------------- S11: filmstrip cache confinement ----------------

#[test]
fn filmstrip_bundle_id_is_whitelisted_and_cache_must_be_outside_bundle() {
    let cache_dir = Path::new("cache-root");
    for bad in [
        "C:evil", "C:", "a:b", "../x", "a/b", "a\\b", ".", "x.y", "CON", "nul", "com1", "LPT9", "",
    ] {
        assert!(
            matches!(
                FilmstripCache::cache_path(cache_dir, bad),
                Err(MediaError::InvalidArgument(_))
            ),
            "bundle_id {bad:?} must be rejected"
        );
    }
    let too_long = "a".repeat(65);
    assert!(
        FilmstripCache::cache_path(cache_dir, &too_long).is_err(),
        "65-char bundle_id must be rejected"
    );
    let ok = FilmstripCache::cache_path(cache_dir, "Bundle_01-abc").expect("valid id");
    assert!(ok.starts_with(cache_dir), "path stays under cache_dir");

    let tmp = TempDir::new().expect("tmp");
    let bundle = tmp.path().join("bundle");
    fs::create_dir_all(&bundle).expect("bundle dir");
    let src = fixture(&bundle, "video.mp4", 20, 10);
    let track = Mp4VideoTrack::open(&src).expect("open");
    let inside = bundle.join("not-yet-created").join("cache");
    let r = FilmstripCache::load_or_generate(
        &inside,
        "b1",
        &track,
        FilmstripConfig::default(),
        &OpenH264Backend::new(),
    );
    assert!(
        matches!(r, Err(MediaError::InvalidArgument(_))),
        "cache_dir inside the bundle must be refused"
    );
    assert!(
        !bundle.join("not-yet-created").exists(),
        "nothing may be created inside the bundle"
    );
    FilmstripCache::load_or_generate(
        tmp.path().join("cache"),
        "b1",
        &track,
        FilmstripConfig::default(),
        &OpenH264Backend::new(),
    )
    .expect("external cache dir works");
}

// ---------------- S12: presentation clock monotonicity and seek ----------------

#[derive(Debug)]
struct FakeMono(std::sync::Arc<std::sync::atomic::AtomicI64>);
impl MonotonicTimeSource for FakeMono {
    fn now_ns(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}
#[derive(Debug)]
struct FakeAudio(std::sync::atomic::AtomicI64);
impl AudioClockSource for FakeAudio {
    fn position_ns(&self) -> Option<i64> {
        let v = self.0.load(std::sync::atomic::Ordering::SeqCst);
        (v >= 0).then_some(v)
    }
}

#[test]
fn presentation_clock_is_monotonic_and_ignores_stale_pre_seek_audio() {
    use std::sync::atomic::Ordering::SeqCst;
    let mono = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let mut clock =
        PresentationClock::with_time_source(Box::new(FakeMono(std::sync::Arc::clone(&mono))));
    let audio = FakeAudio(std::sync::atomic::AtomicI64::new(0));
    let ms = 1_000_000i64;

    clock.play(0);
    audio.0.store(1_000 * ms, SeqCst);
    mono.store(1_000 * ms, SeqCst);
    assert_eq!(
        clock.sample_position(Some(&audio)).media_position_ns,
        1_000 * ms,
        "audio master"
    );
    // Audio device jitter backwards: the reported position never goes backwards.
    audio.0.store(990 * ms, SeqCst);
    let r = clock.sample_position(Some(&audio));
    assert_eq!(
        r.media_position_ns,
        1_000 * ms,
        "monotonic clamp while playing"
    );
    audio.0.store(1_020 * ms, SeqCst);
    assert_eq!(
        clock.sample_position(Some(&audio)).media_position_ns,
        1_020 * ms,
        "forward progress resumes"
    );

    // Backward seek: the audio sink still reports the stale pre-seek playhead.
    let generation = clock.seek_generation();
    clock.seek(200 * ms);
    assert_eq!(
        clock.seek_generation(),
        generation + 1,
        "new seek generation"
    );
    let r = clock.sample_position(Some(&audio));
    assert_eq!(
        r.drive_mode,
        ClockDriveMode::MonotonicFallback,
        "stale audio ignored after seek"
    );
    assert_eq!(
        r.media_position_ns,
        200 * ms,
        "position honours the seek target"
    );
    // Forward seek with stale audio behind the target is ignored as well.
    clock.seek(5_000 * ms);
    assert_eq!(
        clock.sample_position(Some(&audio)).media_position_ns,
        5_000 * ms,
        "stale behind-target audio ignored"
    );
    // The sink catches up: audio resumes as master.
    mono.fetch_add(10 * ms, SeqCst);
    audio.0.store(5_005 * ms, SeqCst);
    let r = clock.sample_position(Some(&audio));
    assert_eq!(r.drive_mode, ClockDriveMode::AudioMaster, "audio resynced");
    assert_eq!(r.media_position_ns, 5_005 * ms, "audio position used");

    // Pause resets the clamp; seeking back while paused reports the new position.
    clock.pause(Some(&audio));
    clock.seek(100 * ms);
    assert_eq!(
        clock.sample_position(None).media_position_ns,
        100 * ms,
        "seek backwards is allowed (clamp reset)"
    );
}
