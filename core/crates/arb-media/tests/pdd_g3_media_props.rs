//! Layer-3 PDD (G3): adversarial MP4 / filmstrip-cache fuzzing plus variable-GOP / VFR
//! roundtrips through mux → open → trim → share-mux.

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    clippy::fn_params_excessive_bools,
    clippy::trivially_copy_pass_by_ref,
    clippy::print_stdout,
    clippy::collapsible_if,
    clippy::similar_names,
    clippy::format_collect,
    clippy::as_conversions,
    clippy::clone_on_ref_ptr,
    reason = "PDD model/property code: long linearized models and generators read best unsplit"
)]

use std::path::Path;

use arb_media::audio::{
    AacMuxInput, AacPacket, build_aac_lc_audio_specific_config, mux_aac_to_m4a,
};
use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
use arb_media::mp4::{Mp4VideoTrack, RawH264Packet, inspect_mp4_atoms, mux_raw_h264_to_mp4};
use arb_media::mux_av::mux_share_mp4;
use arb_media::playback::FilmstripCache;
use arb_media::trim::trim_video;
use proptest::prelude::*;

fn cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn bx(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = u32::try_from(body.len() + 8)
        .expect("box len")
        .to_be_bytes()
        .to_vec();
    v.extend_from_slice(fourcc);
    v.extend_from_slice(body);
    v
}

fn full_box(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = vec![0, 0, 0, 0];
    b.extend_from_slice(body);
    bx(fourcc, &b)
}

/// Exercises every reader on the given file; only `Err` is acceptable, never panic/abort.
fn read_everything(path: &Path) {
    if let Ok(track) = Mp4VideoTrack::open(path) {
        let mut buf = Vec::new();
        for i in 0..track.frame_count().min(64) {
            let _ = track.read_sample_into(i, &mut buf);
            let _ = track.gop_bounds(i);
            let _ = track.decodable_gop_start(i);
        }
        let _ = track.frame(u64::MAX);
    }
    let _ = inspect_mp4_atoms(path);
}

fn yuv(idx: u64, w: u32, h: u32) -> DecodedFrame {
    let (wu, hu) = (w as usize, h as usize);
    let mut data = vec![128u8; wu * hu + 2 * (wu / 2) * (hu / 2)];
    let bar = usize::try_from(idx % 40).expect("bar");
    for (i, px) in data.iter_mut().take(wu * hu).enumerate() {
        let col = i % wu;
        *px = if (bar..bar + 8).contains(&col) {
            220
        } else {
            40
        };
    }
    DecodedFrame {
        frame_index: idx,
        width: w,
        height: h,
        format: PixelFormat::Yuv420p,
        data,
    }
}

/// Variable-GOP, VFR stream: each GOP is encoded separately so every GOP starts with an IDR.
fn encode_vfr(gops: &[u8], deltas_ms: &[u16]) -> Vec<RawH264Packet> {
    let backend = OpenH264Backend::new();
    let mut packets = Vec::new();
    let mut idx = 0u64;
    for &len in gops {
        let frames: Vec<DecodedFrame> = (0..u64::from(len)).map(|i| yuv(idx + i, 64, 48)).collect();
        let mut gop = backend
            .encode_yuv420p_sequence(
                &frames,
                10_000,
                33_333_333,
                EncodeParams {
                    bitrate_bps: 300_000,
                    max_fps: 30,
                },
            )
            .expect("encode gop");
        idx += u64::from(len);
        packets.append(&mut gop);
    }
    let mut pts = 0i64;
    for (i, p) in packets.iter_mut().enumerate() {
        p.pts_ns = pts;
        pts += i64::from(deltas_ms[i % deltas_ms.len()]) * 1_000_000;
    }
    packets
}

fn build_audio(path: &Path, packets: usize) {
    let asc = build_aac_lc_audio_specific_config(48_000, 2).expect("asc");
    let packets = (0..packets)
        .map(|i| AacPacket {
            pts_us: i64::try_from(i).expect("i") * 1024 * 1_000_000 / 48_000,
            data: vec![
                0x21,
                0x10,
                0x04,
                0x60,
                0x8C,
                0x1C,
                u8::try_from(i & 0xFF).expect("u8"),
            ],
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
    .expect("mux audio");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(64), ..ProptestConfig::default() })]

    #[test]
    fn g3_random_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let tmp = tempfile::tempdir().expect("tmp");
        let p = tmp.path().join("x.mp4");
        std::fs::write(&p, &bytes).expect("write");
        read_everything(&p);
        let _ = FilmstripCache::load_from_file(&p, bytes.len() as u64, 4, &[0; 32]);
    }

    #[test]
    fn g3_hostile_atoms_never_panic(depth in 1usize..80, count in any::<u32>(), co64 in any::<u64>(), which in 0u8..4) {
        // Deeply nested containers (> 32) and giant entry counts must fail cleanly.
        let leaf = match which {
            0 => full_box(b"stsz", &[&0u32.to_be_bytes()[..], &count.to_be_bytes()].concat()),
            1 => full_box(b"stsc", &count.to_be_bytes()),
            2 => full_box(b"co64", &[&1u32.to_be_bytes()[..], &co64.to_be_bytes()].concat()),
            _ => full_box(b"stts", &[&count.to_be_bytes()[..], &[0u8; 8]].concat()),
        };
        let mut b = leaf;
        for i in 0..depth {
            b = bx([b"moov", b"trak", b"mdia", b"minf", b"stbl"][i % 5], &b);
        }
        let mut file = bx(b"ftyp", b"isom\0\0\0\0isom");
        file.extend_from_slice(&b);
        let tmp = tempfile::tempdir().expect("tmp");
        let p = tmp.path().join("x.mp4");
        std::fs::write(&p, &file).expect("write");
        read_everything(&p);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(4), ..ProptestConfig::default() })]

    #[test]
    fn g3_vfr_variable_gop_roundtrip(
        gops in prop::collection::vec(1u8..5, 1..4),
        deltas in prop::collection::vec(prop_oneof![10u16..70, Just(17), Just(10_000)], 1..5),
        cut in any::<(u8, u8)>(),
        lead_in_ms in -40i64..40,
        tail_ticks in prop_oneof![Just(1u32), Just(900_000), 1u32..90_000],
        flips in prop::collection::vec((any::<u16>(), any::<u8>()), 1..6),
    ) {
        let tmp = tempfile::tempdir().expect("tmp");
        let src = tmp.path().join("src.mp4");
        let packets = encode_vfr(&gops, &deltas);
        let n = packets.len() as u64;
        mux_raw_h264_to_mp4(&src, 64, 48, 90_000, tail_ticks, &packets).expect("mux");
        let track = Mp4VideoTrack::open(&src).expect("open");
        prop_assert_eq!(track.frame_count(), n);
        let idr: Vec<bool> = track.frames().iter().map(|f| f.is_keyframe).collect();
        let packet_idr: Vec<bool> = packets.iter().map(|p| p.is_keyframe).collect();
        prop_assert_eq!(&idr, &packet_idr, "variable GOP keyframes preserved by the muxer");
        let mut start = 0usize;
        for &g in &gops {
            prop_assert!(idr[start], "every encoded GOP starts with an IDR");
            start += usize::from(g);
        }
        prop_assert!(track.frames().windows(2).all(|w| w[0].pts_ns < w[1].pts_ns), "VFR pts strictly increasing");

        let (a, b) = (u64::from(cut.0) % n, u64::from(cut.1) % n);
        let (inf, outf) = (a.min(b), a.max(b));
        let trimmed = tmp.path().join("trim.mp4");
        let report = trim_video(&src, &trimmed, inf, outf, &OpenH264Backend::new()).expect("trim");
        prop_assert_eq!(report.output_frame_count, outf - inf + 1);
        let t = Mp4VideoTrack::open(&trimmed).expect("open trimmed");
        prop_assert_eq!(t.frame_count(), outf - inf + 1);
        prop_assert!(t.frames()[0].is_keyframe, "trimmed output starts at an IDR");

        let starts = arb_media::timeline::viewport_timestamps(track.frames().iter().map(|f| f.pts_ns));
        let end = arb_media::timeline::end_ns(&starts, Some(i64::try_from(track.frames().last().expect("sample").duration_ns).expect("duration")), 1);
        let left = starts[usize::try_from(inf).expect("in")];
        let right = arb_media::timeline::out_time(&starts, end, usize::try_from(outf).expect("out"));
        prop_assert_eq!(arb_media::timeline::frame_at_time(&starts, left), Some(usize::try_from(inf).expect("in")));
        prop_assert_eq!(arb_media::timeline::out_frame(&starts, right), Some(usize::try_from(outf).expect("out")));
        let trimmed_pts = arb_media::timeline::viewport_timestamps(t.frames().iter().map(|frame| frame.pts_ns));
        for (i, pts) in trimmed_pts.iter().enumerate() {
            prop_assert!((pts - (starts[usize::try_from(inf).expect("in") + i] - left)).abs() <= 1);
        }
        let trimmed_end = arb_media::timeline::end_ns(&trimmed_pts, Some(i64::try_from(t.frames().last().expect("sample").duration_ns).expect("duration")), 1);
        prop_assert!((trimmed_end - (right - left)).abs() <= 1);
        let audio = tmp.path().join("a.m4a");
        build_audio(&audio, 40);
        let share = tmp.path().join("share.mp4");
        mux_share_mp4(&trimmed, Some(&audio), lead_in_ms * 1_000_000, &share).expect("share mux");
        let atoms = inspect_mp4_atoms(&share).expect("inspect");
        prop_assert_eq!(atoms.edts_count, 0);
        prop_assert_eq!(atoms.elst_count, 0);
        prop_assert!(atoms.faststart, "moov precedes mdat");
        prop_assert_eq!(Mp4VideoTrack::open(&share).expect("open share").frame_count(), outf - inf + 1);

        // Bit-flipped copies of a real file must never panic.
        let mut bytes = std::fs::read(&share).expect("read");
        for (off, val) in flips {
            let i = usize::from(off) % bytes.len();
            bytes[i] ^= val | 1;
        }
        let bad = tmp.path().join("bad.mp4");
        std::fs::write(&bad, &bytes).expect("write");
        read_everything(&bad);
    }
}

#[test]
fn shared_studio_timeline_contract() {
    use arb_media::timeline::{
        end_ns, fraction_to_time, frame_at_time, out_frame, time_to_fraction, viewport_timestamps,
    };
    let mut lines = include_str!("data/vfr-timeline.txt")
        .lines()
        .filter(|line| !line.starts_with('#'));
    let numbers = |row: &str| {
        row.split_whitespace()
            .map(|n| n.parse::<i64>().expect("number"))
            .collect::<Vec<_>>()
    };
    let pts = viewport_timestamps(numbers(lines.next().expect("starts")).into_iter());
    let last = lines
        .next()
        .expect("last duration")
        .parse()
        .expect("number");
    let end = end_ns(&pts, Some(last), 1);
    assert_eq!(end, 11_000_000_000, "full duration including last hold");
    for line in lines {
        let row = numbers(line);
        assert_eq!(
            frame_at_time(&pts, row[0]),
            usize::try_from(row[1]).ok(),
            "displayed frame"
        );
        assert_eq!(
            out_frame(&pts, row[0]),
            usize::try_from(row[2]).ok(),
            "inclusive OUT"
        );
        assert_eq!(
            fraction_to_time(time_to_fraction(row[0], end), end),
            row[0],
            "screen roundtrip"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(8), ..ProptestConfig::default() })]
    #[test]
    fn g3_subframe_vfr_roundtrip(a in 0u64..23_000_000_000, b in 0u64..23_000_000_000) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("source.mp4");
        let output = tmp.path().join("output.mp4");
        let packets = encode_vfr(&[3, 3, 1], &[2000, 2000, 2000, 2000, 4000, 10_000]);
        mux_raw_h264_to_mp4(&src, 64, 48, 90_000, 90_000, &packets).unwrap();
        let start = a.min(b);
        let stop = a.max(b) + 1;
        let backend = OpenH264Backend::new();
        let report = arb_media::trim::trim_video_time_range(&src, &output, start, stop, &backend).unwrap();
        let track = Mp4VideoTrack::open(&output).unwrap();
        let atoms = inspect_mp4_atoms(&output).unwrap();
        prop_assert_eq!((atoms.edts_count, atoms.elst_count), (0,0));
        let mut idx = 0;
        for (i, (&begin, &end)) in [0,2,4,6,8,12,22].iter().zip(&[2,4,6,8,12,22,23]).enumerate() {
            let left = start.max(begin * 1_000_000_000);
            let right = stop.min(end * 1_000_000_000);
            if left < right {
                let frame = track.frame(idx).unwrap();
                prop_assert!(frame.duration_ns.abs_diff(right-left) <= 1_000_000_000u64.div_ceil(u64::from(track.timescale())));
                let decoded = arb_media::codec::VideoCodecBackend::decode_frame(&backend, &track, idx, PixelFormat::Yuv420p).unwrap();
                let bar = decoded.data[..64].iter().position(|&px| px > 128);
                prop_assert_eq!(bar, Some(i));
                idx += 1;
            }
        }
        prop_assert_eq!(track.frame_count(), idx);
        let measured = track.duration_ticks() * 1_000_000_000 / u64::from(track.timescale());
        prop_assert_eq!(report.output_duration_ns, Some(measured));
        prop_assert!(measured.abs_diff(stop-start) <= 1_000_000_000u64.div_ceil(u64::from(track.timescale())));
    }
}

#[test]
fn complete_90khz_clock_range_can_be_trimmed_again_at_reported_end() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("input.mp4");
    let first = tmp.path().join("first.mp4");
    let second = tmp.path().join("second.mp4");
    let backend = OpenH264Backend::new();
    let mut packets = encode_vfr(&[3], &[33]);
    for (packet, pts) in packets.iter_mut().zip([0, 33_333_333, 66_666_666]) {
        packet.pts_ns = pts;
    }
    mux_raw_h264_to_mp4(&src, 64, 48, 90_000, 3_000, &packets).unwrap();
    let trim =
        |src, dst, end| arb_media::trim::trim_video_time_range(src, dst, 0, end, &backend).unwrap();
    let report = trim(&src, &first, 100_000_000);
    assert_eq!(report.output_duration_ns, Some(100_000_000));
    let repeated = trim(&first, &second, report.output_duration_ns.unwrap());
    assert_eq!(repeated, report);
    assert_eq!(
        Mp4VideoTrack::open(&second).unwrap().duration_ticks(),
        9_000
    );
}

#[test]
fn sub_nanosecond_tick_trim_reports_a_positive_retrimmable_duration() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("input.mp4");
    let first = tmp.path().join("first.mp4");
    let second = tmp.path().join("second.mp4");
    let backend = OpenH264Backend::new();
    let packets = encode_vfr(&[2], &[400]);
    mux_raw_h264_to_mp4(&src, 64, 48, u32::MAX, 1, &packets).unwrap();
    let report = arb_media::trim::trim_video_time_range(&src, &first, 0, 1, &backend).unwrap();
    // One ns is 4 ticks (0.93 ns) at this clock; the track still lasts for a nonzero time.
    assert_eq!(Mp4VideoTrack::open(&first).unwrap().duration_ticks(), 4);
    assert_eq!(report.output_duration_ns, Some(1));
    let end = report.output_duration_ns.unwrap();
    arb_media::trim::trim_video_time_range(&first, &second, 0, end, &backend).unwrap();
}
