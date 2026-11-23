//! Integration tests for pure-Rust AAC-LC (`audio.m4a`) -> PCM RIFF/WAV decoding.

use std::f64::consts::PI;
use std::fs;
use std::io::ErrorKind;

use arb_media::audio::{
    AacMuxInput, AacPacket, build_aac_lc_audio_specific_config, mux_aac_to_m4a,
};
use arb_media::audio_decode::{decode_audio_m4a_to_wav, inspect_wav_header};
use arb_media::error::MediaError;
use rusty_aac::{AacEncoder, AacEncoderConfig};
use tempfile::TempDir;

/// Encodes a real 48 kHz stereo 440 Hz sine wave into AAC-LC packets and muxes them into `m4a_path`.
fn write_real_aac_m4a(
    m4a_path: &std::path::Path,
    sample_rate_hz: u32,
    channels: u16,
    frames: usize,
) {
    let ch = usize::from(channels);
    let mut pcm = Vec::with_capacity(frames * ch);
    for i in 0..frames {
        let t = f64::from(u32::try_from(i).expect("frame fits u32")) / f64::from(sample_rate_hz);
        let left = (2.0 * PI * 440.0 * t).sin() * 0.45;
        let right = (2.0 * PI * 660.0 * t).sin() * 0.45;
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "test sine synthesis converts f64 in [-0.45, 0.45] to f32"
        )]
        {
            pcm.push(left as f32);
            if channels == 2 {
                pcm.push(right as f32);
            }
        }
    }

    let mut enc = AacEncoder::new(AacEncoderConfig::default());
    enc.push_pcm(&pcm, channels, sample_rate_hz)
        .expect("encode PCM into AAC-LC");
    enc.finish();

    let mut packets = Vec::new();
    let mut idx = 0_i64;
    while let Ok(pkt) = enc.next_packet() {
        let pts_us = idx * 1024 * 1_000_000 / i64::from(sample_rate_hz);
        packets.push(AacPacket {
            pts_us,
            data: pkt.data,
        });
        idx += 1;
    }
    assert!(
        !packets.is_empty(),
        "AacEncoder must emit at least one AAC-LC access unit"
    );

    let asc =
        build_aac_lc_audio_specific_config(sample_rate_hz, channels).expect("build standard ASC");
    let input = AacMuxInput {
        audio_specific_config: asc.to_vec(),
        sample_rate_hz,
        channels,
        packets,
    };
    mux_aac_to_m4a(m4a_path, &input).expect("mux real AAC-LC into audio.m4a");
}

#[test]
fn real_aac_lc_m4a_decodes_to_valid_48khz_stereo_s16le_wav_with_non_silent_energy() {
    let tmp = TempDir::new().expect("temp dir");
    let m4a_path = tmp.path().join("audio.m4a");
    let wav_path = tmp.path().join("audio.wav");

    // 0.5 seconds of 48 kHz stereo sine wave (24,000 frames).
    write_real_aac_m4a(&m4a_path, 48_000, 2, 24_000);

    let report = decode_audio_m4a_to_wav(&m4a_path, &wav_path).expect("decode audio.m4a -> WAV");
    assert_eq!(report.sample_rate_hz, 48_000, "sample rate must be 48 kHz");
    assert_eq!(report.channels, 2, "channel count must be stereo (2)");
    assert_eq!(report.bits_per_sample, 16, "bit depth must be 16-bit s16le");
    assert!(
        report.packet_count >= 20,
        "expected >= 20 decoded AAC packets, got {}",
        report.packet_count
    );
    assert!(
        report.pcm_frame_count >= 24_000,
        "decoded PCM frames ({}) must cover encoded duration",
        report.pcm_frame_count
    );
    assert!(
        report.duration_ns >= 450_000_000,
        "decoded duration must be >= 450ms, got {} ns",
        report.duration_ns
    );

    let wav_bytes = fs::read(&wav_path).expect("read output WAV");
    assert_eq!(
        u64::try_from(wav_bytes.len()).expect("len fits u64"),
        report.output_bytes,
        "report.output_bytes must match file length"
    );
    assert_eq!(&wav_bytes[0..4], b"RIFF", "WAV must start with RIFF");
    assert_eq!(&wav_bytes[8..12], b"WAVE", "WAV must carry WAVE marker");
    assert_eq!(&wav_bytes[12..16], b"fmt ", "WAV must carry fmt chunk");
    assert_eq!(&wav_bytes[36..40], b"data", "WAV must carry data chunk");

    let header = inspect_wav_header(&wav_path).expect("inspect_wav_header must parse output");
    assert_eq!(header.sample_rate_hz, 48_000, "header sample rate");
    assert_eq!(header.channels, 2, "header channels");
    assert_eq!(header.bits_per_sample, 16, "header bits_per_sample");
    assert_eq!(
        header.pcm_frame_count, report.pcm_frame_count,
        "frame count match"
    );
    assert_eq!(header.duration_ns, report.duration_ns, "duration match");

    // Verify decoded PCM carries real audible energy (not all zeros / NaN).
    let pcm_slice = &wav_bytes[44..];
    assert_eq!(
        pcm_slice.len() % 4,
        0,
        "stereo s16le bytes must align to 4-byte frames"
    );
    let peak = pcm_slice
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
        .max()
        .unwrap_or(0);
    assert!(
        peak > 4_000,
        "decoded 440/660 Hz tone must have strong PCM amplitude, got peak {peak}"
    );
}

#[test]
fn decode_audio_refuses_to_overwrite_existing_output_or_source_and_cleans_temps() {
    let tmp = TempDir::new().expect("temp dir");
    let m4a_path = tmp.path().join("audio.m4a");
    let wav_path = tmp.path().join("audio.wav");
    write_real_aac_m4a(&m4a_path, 48_000, 2, 4_096);

    // Refuses targeting source input file.
    let err = decode_audio_m4a_to_wav(&m4a_path, &m4a_path)
        .expect_err("must refuse overwriting source audio.m4a");
    assert!(
        matches!(err, MediaError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}"
    );

    // First decode succeeds.
    decode_audio_m4a_to_wav(&m4a_path, &wav_path).expect("initial decode");
    // Second decode to same path refuses to replace.
    let err2 = decode_audio_m4a_to_wav(&m4a_path, &wav_path)
        .expect_err("must refuse replacing existing WAV");
    assert!(
        matches!(err2, MediaError::Io(ref io_err) if io_err.kind() == ErrorKind::AlreadyExists),
        "expected AlreadyExists, got {err2:?}"
    );

    // No `.tmp-*` leftovers.
    let leftovers: Vec<_> = fs::read_dir(tmp.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp files may remain: {leftovers:?}"
    );
}
