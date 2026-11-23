//! Independent pure-Rust integration tests for `arb-media`'s AAC-LC `audio.m4a` muxer,
//! parser/indexer (`Mp4AudioTrack`), and time-range trimmer (`trim_audio_m4a`).

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use arb_media::audio::{
    AAC_LC_OBJECT_TYPE, AAC_LC_SAMPLES_PER_FRAME, AacMuxInput, AacPacket, Mp4AudioTrack,
    build_aac_lc_audio_specific_config, mux_aac_to_m4a, trim_audio_m4a,
};
use arb_media::error::MediaError;
use arb_media::mp4::inspect_mp4_atoms;
use tempfile::TempDir;

/// Synthesizes a deterministic valid-looking raw AAC-LC stereo silence access unit tagged with
/// `seq` in a Data Stream Element (DSE) payload so byte-for-byte packet identity is verifiable.
fn synthetic_aac_payload(seq: usize) -> Vec<u8> {
    let mut au = vec![
        // ID_CPE (1) minimal silent stereo block + payload tag bytes
        0x21,
        0x10,
        0x04,
        0x60,
        0x8C,
        0x1C,
        u8::try_from(seq & 0xFF).expect("seq low byte"),
        u8::try_from((seq >> 8) & 0xFF).expect("seq high byte"),
    ];
    // Vary packet length slightly across sequence indices to exercise variable `stsz` entries.
    for extra in 0..(seq % 7) {
        au.push(u8::try_from((seq + extra) & 0xFF).expect("padding byte"));
    }
    au
}

/// Wraps `raw_au` with a valid 7-byte MPEG-4 AAC-LC ADTS header (48 kHz stereo).
fn wrap_with_adts_7byte(raw_au: &[u8]) -> Vec<u8> {
    let frame_len = 7 + raw_au.len();
    let mut out = Vec::with_capacity(frame_len);
    out.push(0xFF);
    out.push(0xF1); // MPEG-4, layer=0, protection_absent=1
    out.push(0x4C); // AAC-LC (profile=1), 48 kHz (freq_idx=3), ch_cfg=2 (high bit 0)
    out.push(0x80 | u8::try_from((frame_len >> 11) & 0x03).expect("len hi"));
    out.push(u8::try_from((frame_len >> 3) & 0xFF).expect("len mid"));
    out.push((u8::try_from(frame_len & 0x07).expect("len lo") << 5) | 0x1F);
    out.push(0xFC); // 1 raw data block
    out.extend_from_slice(raw_au);
    out
}

#[derive(Clone, Copy)]
enum PacketFraming {
    RawOnly,
    MixedAdts,
}

fn build_100_packet_input(framing: PacketFraming) -> (AacMuxInput, Vec<Vec<u8>>) {
    let asc = build_aac_lc_audio_specific_config(48_000, 2).expect("48 kHz stereo ASC");
    assert_eq!(asc, [0x11, 0x90], "48 kHz stereo ASC bytes");

    let mut raw_payloads = Vec::with_capacity(100);
    let mut packets = Vec::with_capacity(100);
    for i in 0..100usize {
        let raw = synthetic_aac_payload(i);
        let data = match framing {
            PacketFraming::MixedAdts if i % 2 == 0 => wrap_with_adts_7byte(&raw),
            PacketFraming::RawOnly | PacketFraming::MixedAdts => raw.clone(),
        };
        let pts_us = i64::try_from(i).expect("idx") * 1024 * 1_000_000 / 48_000;
        raw_payloads.push(raw);
        packets.push(AacPacket { pts_us, data });
    }

    (
        AacMuxInput {
            audio_specific_config: asc.to_vec(),
            sample_rate_hz: 48_000,
            channels: 2,
            packets,
        },
        raw_payloads,
    )
}

fn assert_no_temp_leftovers(dir: &Path) {
    let leftovers: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp files may remain in {}: {leftovers:?}",
        dir.display()
    );
}

fn patch_box_u32(bytes: &mut [u8], fourcc: [u8; 4], body_offset: usize, value: u32) {
    let pos = bytes
        .windows(4)
        .position(|w| w == fourcc.as_slice())
        .unwrap_or_else(|| panic!("box {} not found", String::from_utf8_lossy(&fourcc)));
    let at = pos + 4 + body_offset;
    bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

#[test]
fn round_trip_mux_and_open_verifies_exact_packets_asc_faststart_and_zero_elst() {
    let tmp = TempDir::new().expect("temp dir");
    let m4a_path = tmp.path().join("audio.m4a");
    let (input, expected_raw) = build_100_packet_input(PacketFraming::MixedAdts);

    let report = mux_aac_to_m4a(&m4a_path, &input).expect("mux 100 AAC packets");
    assert_eq!(report.packet_count, 100, "muxed packet count");
    assert_eq!(report.sample_rate_hz, 48_000, "muxed sample rate");
    assert_eq!(report.channels, 2, "muxed channels");
    assert_eq!(report.adts_stripped_count, 50, "50 ADTS headers stripped");
    assert_eq!(report.edts_box_count, 0, "muxed edts count");
    assert_eq!(report.elst_box_count, 0, "muxed elst count");
    assert_eq!(
        report.duration_ns, 2_133_333_333,
        "100 * 1024 / 48000 s in ns"
    );

    // Inspect ISO-BMFF box hierarchy: faststart, M4A ftyp, mp4a + esds present, zero edts/elst.
    let atoms = inspect_mp4_atoms(&m4a_path).expect("inspect audio.m4a atoms");
    assert!(atoms.faststart, "moov must precede mdat (faststart)");
    assert_eq!(atoms.edts_count, 0, "zero edts boxes allowed");
    assert_eq!(atoms.elst_count, 0, "zero elst boxes allowed");
    assert_eq!(atoms.stsd_entry_count, 1, "single stsd mp4a entry");
    assert!(
        atoms.boxes.iter().any(|b| &b.fourcc == b"mp4a"),
        "mp4a box must exist"
    );
    assert!(
        atoms.boxes.iter().any(|b| &b.fourcc == b"esds"),
        "esds box must exist inside mp4a"
    );
    assert!(
        atoms.boxes.iter().any(|b| &b.fourcc == b"smhd"),
        "smhd box must exist for audio track"
    );

    // Verify ftyp major brand is `M4A `.
    let raw_file = fs::read(&m4a_path).expect("read m4a bytes");
    assert_eq!(&raw_file[4..12], b"ftypM4A ", "ftyp major brand M4A ");

    // Parse back with `Mp4AudioTrack::open` and verify every single packet byte-for-byte.
    let track = Mp4AudioTrack::open(&m4a_path).expect("open audio.m4a");
    assert_eq!(track.packet_count(), 100, "indexed packet count");
    assert_eq!(track.sample_rate_hz(), 48_000, "indexed sample rate");
    assert_eq!(track.timescale(), 48_000, "indexed mdhd timescale");
    assert_eq!(track.channels(), 2, "indexed channels");
    assert_eq!(
        track.audio_specific_config().audio_object_type,
        AAC_LC_OBJECT_TYPE,
        "indexed object type AAC-LC"
    );
    assert_eq!(
        track.audio_specific_config().raw_bytes,
        vec![0x11, 0x90],
        "indexed AudioSpecificConfig bytes"
    );
    assert_eq!(
        track.duration_ticks(),
        100 * u64::from(AAC_LC_SAMPLES_PER_FRAME),
        "indexed total duration ticks"
    );

    let mut buf = Vec::new();
    for (idx, expected_bytes) in expected_raw.iter().enumerate() {
        let idx_u64 = u64::try_from(idx).expect("idx");
        let meta = track.packet(idx_u64).expect("packet meta");
        assert_eq!(meta.index, idx_u64, "packet {idx} index");
        assert_eq!(
            meta.duration_ticks, AAC_LC_SAMPLES_PER_FRAME,
            "packet {idx} duration ticks"
        );
        assert_eq!(
            meta.pts_ticks,
            idx_u64 * u64::from(AAC_LC_SAMPLES_PER_FRAME),
            "packet {idx} pts_ticks"
        );
        track
            .read_packet_into(idx_u64, &mut buf)
            .expect("read packet payload");
        assert_eq!(
            &buf, expected_bytes,
            "packet {idx} payload must match byte-for-byte after ADTS stripping"
        );
    }

    // `mux_aac_to_m4a` must refuse overwriting an existing file.
    let overwrite_err = mux_aac_to_m4a(&m4a_path, &input);
    assert!(
        matches!(
            overwrite_err,
            Err(MediaError::Io(ref err)) if err.kind() == ErrorKind::AlreadyExists
        ),
        "mux_aac_to_m4a must never overwrite an existing file: {overwrite_err:?}"
    );
    assert_no_temp_leftovers(tmp.path());
}

#[test]
fn trim_audio_m4a_selects_exact_subset_rebases_to_zero_and_has_zero_elst() {
    let tmp = TempDir::new().expect("temp dir");
    let src_path = tmp.path().join("source.m4a");
    let (input, expected_raw) = build_100_packet_input(PacketFraming::RawOnly);
    mux_aac_to_m4a(&src_path, &input).expect("mux 100-packet source");

    let src_track = Mp4AudioTrack::open(&src_path).expect("open source");
    // Trim exact packets [20..65) (45 packets, ~0.96s) using exact packet boundaries.
    let start_ns = src_track.packet(20).expect("pkt 20").pts_ns;
    let end_ns = src_track.packet(65).expect("pkt 65").pts_ns;
    let trimmed_path = tmp.path().join("trimmed.m4a");

    let report = trim_audio_m4a(&src_path, &trimmed_path, start_ns, end_ns)
        .expect("trim audio packets 20..65");
    assert_eq!(report.input_packets, 100, "input packet count");
    assert_eq!(report.output_packets, 45, "output packet count (65 - 20)");
    assert_eq!(report.sample_rate_hz, 48_000, "trimmed sample rate");
    assert_eq!(report.channels, 2, "trimmed channels");
    assert_eq!(report.edts_box_count, 0, "trimmed edts count");
    assert_eq!(report.elst_box_count, 0, "trimmed elst count");
    assert_eq!(
        report.duration_ns,
        45 * 1024 * 1_000_000_000 / 48_000,
        "trimmed duration ns"
    );

    let atoms = inspect_mp4_atoms(&trimmed_path).expect("inspect trimmed atoms");
    assert!(atoms.faststart, "trimmed output must be faststart");
    assert_eq!(atoms.edts_count, 0, "trimmed output must have 0 edts");
    assert_eq!(atoms.elst_count, 0, "trimmed output must have 0 elst");

    let trimmed = Mp4AudioTrack::open(&trimmed_path).expect("open trimmed.m4a");
    assert_eq!(trimmed.packet_count(), 45, "trimmed indexed packet count");
    assert_eq!(
        trimmed.packet(0).expect("first trimmed packet").pts_ns,
        0,
        "first trimmed packet must be rebased to pts_ns == 0"
    );
    assert_eq!(
        trimmed.packet(0).expect("first trimmed packet").pts_ticks,
        0,
        "first trimmed packet must be rebased to pts_ticks == 0"
    );

    let mut buf = Vec::new();
    for out_idx in 0..45usize {
        let out_u64 = u64::try_from(out_idx).expect("out_idx");
        trimmed
            .read_packet_into(out_u64, &mut buf)
            .expect("read trimmed payload");
        assert_eq!(
            &buf,
            &expected_raw[20 + out_idx],
            "trimmed packet {out_idx} must match source packet {}",
            20 + out_idx
        );
    }

    // Mid-packet range [500ms, 1500ms) intersects packets 23..=70 (48 packets).
    let mid_path = tmp.path().join("mid.m4a");
    let mid_report = trim_audio_m4a(&src_path, &mid_path, 500_000_000, 1_500_000_000)
        .expect("trim [0.5s, 1.5s]");
    assert_eq!(
        mid_report.output_packets, 48,
        "[0.5s, 1.5s) intersects packets 23..=70 (48 packets)"
    );
    assert_no_temp_leftovers(tmp.path());
}

#[test]
fn trim_audio_m4a_refuses_overwrite_input_alias_and_invalid_ranges() {
    let tmp = TempDir::new().expect("temp dir");
    let src_path = tmp.path().join("source.m4a");
    let (input, _) = build_100_packet_input(PacketFraming::RawOnly);
    mux_aac_to_m4a(&src_path, &input).expect("mux 100-packet source");

    let original_bytes = fs::read(&src_path).expect("read original");
    let start_ns = 200_000_000i64;
    let end_ns = 800_000_000i64;

    assert!(
        matches!(
            trim_audio_m4a(&src_path, &src_path, start_ns, end_ns),
            Err(MediaError::InvalidArgument(_))
        ),
        "trim output == input must be refused"
    );
    let alias = tmp.path().join(".").join("source.m4a");
    assert!(
        matches!(
            trim_audio_m4a(&src_path, &alias, start_ns, end_ns),
            Err(MediaError::InvalidArgument(_))
        ),
        "alias resolving to input must be refused"
    );

    let existing = tmp.path().join("existing.m4a");
    fs::write(&existing, b"preserve me").expect("seed existing");
    assert!(
        matches!(
            trim_audio_m4a(&src_path, &existing, start_ns, end_ns),
            Err(MediaError::Io(ref err)) if err.kind() == ErrorKind::AlreadyExists
        ),
        "trim must refuse to overwrite an existing output file"
    );
    assert_eq!(
        fs::read(&existing).expect("read existing"),
        b"preserve me",
        "existing output must remain untouched"
    );

    assert!(
        matches!(
            trim_audio_m4a(&src_path, tmp.path().join("bad.m4a"), end_ns, start_ns),
            Err(MediaError::InvalidArgument(_))
        ),
        "inverted trim range must be refused"
    );
    assert!(
        matches!(
            trim_audio_m4a(
                &src_path,
                tmp.path().join("empty.m4a"),
                5_000_000_000,
                6_000_000_000
            ),
            Err(MediaError::InvalidArgument(_))
        ),
        "out-of-bounds trim range with 0 intersecting packets must be refused"
    );
    assert_eq!(
        fs::read(&src_path).expect("re-read source"),
        original_bytes,
        "source audio.m4a must remain untouched"
    );
    assert_no_temp_leftovers(tmp.path());
}

#[test]
fn crafted_oom_and_malformed_audio_boxes_return_err_without_allocating() {
    let tmp = TempDir::new().expect("temp dir");
    let base_path = tmp.path().join("base.m4a");
    let (input, _) = build_100_packet_input(PacketFraming::RawOnly);
    mux_aac_to_m4a(&base_path, &input).expect("mux base.m4a");
    let base = fs::read(&base_path).expect("read base.m4a");

    let open_patched = |name: &str, patch: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = base.clone();
        patch(&mut bytes);
        let path = tmp.path().join(name);
        fs::write(&path, &bytes).expect("write crafted m4a");
        Mp4AudioTrack::open(&path)
    };

    // stts entry_count OOM bomb.
    let res = open_patched("oom-stts-entries.m4a", &|b| {
        patch_box_u32(b, *b"stts", 4, 0x7FFF_FFFF);
    });
    assert!(
        matches!(res, Err(MediaError::InvalidContainer(_))),
        "huge stts entry_count must be rejected: {res:?}"
    );

    // stts repeat bomb (sample_count = u32::MAX): safely capped at stsz count (100).
    let capped = open_patched("stts-repeat-bomb.m4a", &|b| {
        patch_box_u32(b, *b"stts", 8, u32::MAX);
    })
    .expect("capped stts expansion parses safely without OOM");
    assert_eq!(capped.packet_count(), 100, "count capped by stsz");

    // stsz constant-size OOM bomb (default_size=1, sample_count=u32::MAX).
    let res = open_patched("oom-stsz-const.m4a", &|b| {
        patch_box_u32(b, *b"stsz", 4, 1);
        patch_box_u32(b, *b"stsz", 8, u32::MAX);
    });
    assert!(
        matches!(res, Err(MediaError::InvalidContainer(_))),
        "constant-size stsz OOM bomb must be rejected: {res:?}"
    );

    // stsz variable-size OOM bomb.
    let res = open_patched("oom-stsz-var.m4a", &|b| {
        patch_box_u32(b, *b"stsz", 8, 0x4000_0000);
    });
    assert!(
        matches!(res, Err(MediaError::InvalidContainer(_))),
        "variable-size stsz OOM bomb must be rejected: {res:?}"
    );

    // stsc & stco entry_count OOM bombs.
    for (name, fourcc) in [("oom-stsc.m4a", b"stsc"), ("oom-stco.m4a", b"stco")] {
        let res = open_patched(name, &|b| patch_box_u32(b, *fourcc, 4, 0x2000_0000));
        assert!(
            matches!(res, Err(MediaError::InvalidContainer(_))),
            "huge {} count must be rejected: {res:?}",
            String::from_utf8_lossy(fourcc)
        );
    }
}
