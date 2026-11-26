//! Pure-Rust AAC-LC (`audio.m4a`) to signed 16-bit little-endian PCM RIFF/WAV decoder.
//!
//! Feeds raw AAC-LC access units indexed by [`Mp4AudioTrack::open`] (preserving the R2-S8
//! bounded ISO-BMFF sample-table parser as the single source of truth) into the pure-Rust
//! `rusty_aac` (`Apache-2.0`, zero transitive dependencies) decoder and writes a standard
//! 44-byte RIFF/WAVE PCM (`s16le`) file atomically via `output::write_atomically`.
//!
//! The output WAV format matches `WavFilePcmSource` in `ide-plugin/client-core` so synchronized
//! IDE audio playback keeps 100% of codec decoding inside Rust Core (`SPEC-01`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use rusty_aac::AacDecoder;
use serde::{Deserialize, Serialize};

use crate::audio::Mp4AudioTrack;
use crate::error::MediaError;
use crate::mp4::ticks_to_ns_unsigned;
use crate::output::{Publish, ensure_output_is_not_input, write_atomically};

/// Canonical RIFF/WAVE PCM header length in bytes (`RIFF` + `fmt ` + `data` headers).
const WAV_HEADER_BYTES: usize = 44;

/// RIFF `WAVE_FORMAT_PCM` tag (`1`).
const WAVE_FORMAT_PCM: u16 = 1;

/// Output PCM bit depth (16-bit signed little-endian).
const BITS_PER_SAMPLE_S16LE: u16 = 16;

/// Upper bound on decoded PCM payload size written to one WAV file (`512 MiB`, roughly
/// 45 minutes of 48 kHz stereo s16le) to guard against malformed duration amplification.
const MAX_DECODED_PCM_BYTES: u64 = 512 * 1024 * 1024;

/// Maximum WAV data chunk payload size that fits in a standard 32-bit RIFF header (`u32::MAX - 36`).
const MAX_RIFF_DATA_BYTES: u64 = 0xFFFF_FFFF_u64 - 36;

/// Summary returned by [`decode_audio_m4a_to_wav`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDecodeReport {
    /// Decoded PCM sampling rate in Hz (e.g., `48_000`).
    pub sample_rate_hz: u32,
    /// Decoded PCM channel count (e.g., `2` for stereo).
    pub channels: u16,
    /// Bits per PCM sample (always `16` for signed 16-bit little-endian).
    pub bits_per_sample: u16,
    /// Number of AAC-LC access units decoded from `audio.m4a`.
    pub packet_count: u64,
    /// Total decoded PCM sample frames (samples per channel).
    pub pcm_frame_count: u64,
    /// Decoded audio duration in nanoseconds.
    pub duration_ns: u64,
    /// Total RIFF/WAV file length written in bytes (header + PCM payload).
    pub output_bytes: u64,
}

/// Validated metadata parsed from an existing RIFF/WAV 16-bit signed LE PCM file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WavHeaderInfo {
    /// PCM sampling rate in Hz.
    pub sample_rate_hz: u32,
    /// PCM channel count (`1..=8`).
    pub channels: u16,
    /// Bits per sample (always `16`).
    pub bits_per_sample: u16,
    /// PCM payload byte length declared in the `data` chunk (clamped to actual file size).
    pub data_bytes: u64,
    /// Total PCM sample frames (`data_bytes / (channels * 2)`).
    pub pcm_frame_count: u64,
    /// Audio duration in nanoseconds derived from `pcm_frame_count` and `sample_rate_hz`.
    pub duration_ns: u64,
}

/// Decodes an ISO-BMFF AAC-LC `audio.m4a` file into a crash-safe 16-bit signed little-endian
/// RIFF/WAV PCM file at `output_wav`.
///
/// Uses [`Mp4AudioTrack::open`] for bounded ISO-BMFF indexing and `rusty_aac` for pure-Rust
/// AAC-LC decoding. Streams decoded samples directly to a sibling temp file with O(1) memory,
/// patches the canonical 44-byte RIFF/WAVE header, `fsync`s, and publishes without replacing
/// an existing file or overwriting the source.
///
/// # Errors
///
/// Returns:
/// - [`MediaError::InvalidArgument`] if `output_wav` resolves to `audio_m4a`, or `audio_m4a`
///   contains zero AAC packets;
/// - [`MediaError::InvalidContainer`] or [`MediaError::UnsupportedFeature`] if `audio_m4a`
///   is malformed, not AAC-LC, changes audio geometry mid-stream, or exceeds WAV size bounds;
/// - [`MediaError::Codec`] if AAC-LC decoding fails on a packet;
/// - [`MediaError::Io`] (`AlreadyExists`) if `output_wav` already exists, or on I/O failure.
pub fn decode_audio_m4a_to_wav(
    audio_m4a: impl AsRef<Path>,
    output_wav: impl AsRef<Path>,
) -> Result<AudioDecodeReport, MediaError> {
    let input_path = audio_m4a.as_ref();
    let output_path = output_wav.as_ref();
    ensure_output_is_not_input(input_path, output_path)?;

    let track = Mp4AudioTrack::open(input_path)?;
    let packet_count = track.packet_count();
    if packet_count == 0 {
        return Err(MediaError::InvalidArgument(
            "cannot decode audio.m4a containing zero AAC packets".to_owned(),
        ));
    }

    let asc = track.audio_specific_config();
    let expected_sr = asc.sample_rate_hz;
    let expected_ch = asc.channel_count;
    if expected_sr == 0 || !(1..=8).contains(&expected_ch) {
        return Err(MediaError::InvalidContainer(format!(
            "unsupported AAC-LC audio parameters ({expected_sr} Hz, {expected_ch} ch)"
        )));
    }

    let mut aac_dec = AacDecoder::with_config_bytes(&asc.raw_bytes)
        .map_err(|err| MediaError::Codec(format!("failed to initialize AAC-LC decoder: {err}")))?;

    let mut acc = DecodeAccumulator {
        sample_rate_hz: expected_sr,
        channels: expected_ch,
        geometry_locked: false,
        decoded_packets: 0,
        pcm_frames: 0,
        data_bytes: 0,
    };

    let output_bytes = write_atomically(output_path, Publish::NoReplace, |writer| {
        // Reserve canonical 44-byte RIFF/WAVE header; patched after streaming all PCM frames.
        writer.write_all(&[0_u8; WAV_HEADER_BYTES])?;
        stream_decode_packets(&track, &mut aac_dec, packet_count, &mut acc, writer)?;
        if acc.data_bytes == 0 {
            return Err(MediaError::Codec(
                "AAC-LC track decoded to zero PCM samples".to_owned(),
            ));
        }
        let header = build_wav_header(acc.sample_rate_hz, acc.channels, acc.data_bytes)?;
        writer.seek(SeekFrom::Start(0))?;
        writer.write_all(&header)?;
        writer.flush()?;
        Ok(())
    })?;

    let duration_ns = ticks_to_ns_unsigned(acc.pcm_frames, acc.sample_rate_hz);
    Ok(AudioDecodeReport {
        sample_rate_hz: acc.sample_rate_hz,
        channels: acc.channels,
        bits_per_sample: BITS_PER_SAMPLE_S16LE,
        packet_count: acc.decoded_packets,
        pcm_frame_count: acc.pcm_frames,
        duration_ns,
        output_bytes,
    })
}

struct DecodeAccumulator {
    sample_rate_hz: u32,
    channels: u16,
    geometry_locked: bool,
    decoded_packets: u64,
    pcm_frames: u64,
    data_bytes: u64,
}

fn stream_decode_packets<W: Write>(
    track: &Mp4AudioTrack,
    aac_dec: &mut AacDecoder,
    packet_count: u64,
    acc: &mut DecodeAccumulator,
    writer: &mut W,
) -> Result<(), MediaError> {
    let max_bytes = MAX_DECODED_PCM_BYTES.min(MAX_RIFF_DATA_BYTES);
    let mut raw_au = Vec::new();
    let mut pcm_bytes = Vec::new();

    for idx in 0..packet_count {
        track.read_packet_into(idx, &mut raw_au)?;
        if raw_au.is_empty() {
            continue;
        }
        let mut audio = match aac_dec.decode(&raw_au, None) {
            Ok(frame) => frame,
            Err(rusty_aac::Error::Again) => continue,
            Err(err) => {
                return Err(MediaError::Codec(format!(
                    "AAC-LC decode failed at packet {idx}: {err}"
                )));
            }
        };
        if audio.samples.is_empty() {
            continue;
        }
        if audio.sample_rate == 0 || !(1..=8).contains(&audio.channels) {
            return Err(MediaError::InvalidContainer(format!(
                "decoded AAC packet {idx} produced unsupported format ({} Hz, {} ch)",
                audio.sample_rate, audio.channels
            )));
        }
        if !acc.geometry_locked {
            acc.sample_rate_hz = audio.sample_rate;
            acc.channels = audio.channels;
            acc.geometry_locked = true;
        } else if audio.sample_rate != acc.sample_rate_hz || audio.channels != acc.channels {
            return Err(MediaError::InvalidContainer(format!(
                "AAC stream format changed mid-file at packet {idx}: expected {} Hz / {} ch, got {} Hz / {} ch",
                acc.sample_rate_hz, acc.channels, audio.sample_rate, audio.channels
            )));
        }

        let ch = usize::from(acc.channels);
        if audio.samples.len() % ch != 0 {
            return Err(MediaError::Codec(format!(
                "AAC packet {idx} decoded {} samples, not a multiple of {ch} channels",
                audio.samples.len()
            )));
        }

        // AAC decodes a complete access unit even when the final stts entry
        // presents only the selected prefix of that unit.
        if idx + 1 == packet_count {
            let ticks = track.packet(idx)?.duration_ticks;
            let frames = (u64::from(ticks) * u64::from(acc.sample_rate_hz))
                .div_ceil(u64::from(track.timescale()));
            audio.samples.truncate(
                usize::try_from(frames)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(ch),
            );
        }
        let chunk_bytes = u64::try_from(audio.samples.len())
            .ok()
            .and_then(|n| n.checked_mul(2))
            .ok_or_else(|| MediaError::InvalidContainer("PCM byte count overflow".to_owned()))?;
        let next_total = acc
            .data_bytes
            .checked_add(chunk_bytes)
            .ok_or_else(|| MediaError::InvalidContainer("WAV data size overflow".to_owned()))?;
        if next_total > max_bytes {
            return Err(MediaError::InvalidContainer(format!(
                "decoded PCM size {next_total} bytes exceeds {max_bytes}-byte WAV bound"
            )));
        }

        pcm_bytes.clear();
        pcm_bytes.reserve(audio.samples.len() * 2);
        for &sample in &audio.samples {
            pcm_bytes.extend_from_slice(&f32_to_i16(sample).to_le_bytes());
        }
        writer.write_all(&pcm_bytes)?;

        acc.data_bytes = next_total;
        let frame_count = u64::try_from(audio.samples.len() / ch).unwrap_or(0);
        acc.pcm_frames = acc.pcm_frames.saturating_add(frame_count);
        acc.decoded_packets = acc.decoded_packets.saturating_add(1);
    }
    Ok(())
}

/// Validates and reads the canonical 44-byte RIFF/WAVE 16-bit signed LE PCM header of `path`.
///
/// Used by `arbd` to verify cached `<cache_dir>/audio-pcm/<bundle_id>.wav` files before
/// reusing them without re-decoding.
///
/// # Errors
///
/// Returns [`MediaError::Io`] if `path` cannot be opened or read, or
/// [`MediaError::InvalidContainer`] if `path` is not a valid canonical 16-bit PCM RIFF/WAV.
pub fn inspect_wav_header(path: impl AsRef<Path>) -> Result<WavHeaderInfo, MediaError> {
    let mut file = File::open(path.as_ref())?;
    let file_len = file.metadata()?.len();
    if file_len < u64::try_from(WAV_HEADER_BYTES).unwrap_or(44) {
        return Err(MediaError::InvalidContainer(format!(
            "WAV file too small ({file_len} bytes)"
        )));
    }
    let mut hdr = [0_u8; WAV_HEADER_BYTES];
    file.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Err(MediaError::InvalidContainer(
            "invalid RIFF/WAVE signature".to_owned(),
        ));
    }
    if &hdr[12..16] != b"fmt " || &hdr[36..40] != b"data" {
        return Err(MediaError::InvalidContainer(
            "non-canonical WAV chunk layout (expected fmt + data)".to_owned(),
        ));
    }
    let fmt_size = u32::from_le_bytes([hdr[16], hdr[17], hdr[18], hdr[19]]);
    let audio_format = u16::from_le_bytes([hdr[20], hdr[21]]);
    let channels = u16::from_le_bytes([hdr[22], hdr[23]]);
    let sample_rate_hz = u32::from_le_bytes([hdr[24], hdr[25], hdr[26], hdr[27]]);
    let block_align = u16::from_le_bytes([hdr[32], hdr[33]]);
    let bits_per_sample = u16::from_le_bytes([hdr[34], hdr[35]]);
    let declared_data = u64::from(u32::from_le_bytes([hdr[40], hdr[41], hdr[42], hdr[43]]));

    if fmt_size != 16 || audio_format != WAVE_FORMAT_PCM {
        return Err(MediaError::InvalidContainer(format!(
            "unsupported WAV fmt chunk (size={fmt_size}, format={audio_format})"
        )));
    }
    if sample_rate_hz == 0
        || !(1..=8).contains(&channels)
        || bits_per_sample != BITS_PER_SAMPLE_S16LE
    {
        return Err(MediaError::InvalidContainer(format!(
            "unsupported WAV format ({sample_rate_hz} Hz, {channels} ch, {bits_per_sample}-bit)"
        )));
    }
    let expected_block_align = channels.saturating_mul(2);
    if block_align != expected_block_align {
        return Err(MediaError::InvalidContainer(format!(
            "WAV blockAlign mismatch: got {block_align}, expected {expected_block_align}"
        )));
    }

    let available_data = file_len.saturating_sub(u64::try_from(WAV_HEADER_BYTES).unwrap_or(44));
    if declared_data == 0 || available_data < declared_data {
        return Err(MediaError::InvalidContainer(format!(
            "truncated WAV data chunk (declared {declared_data} bytes, available {available_data} bytes)"
        )));
    }
    let frame_bytes = u64::from(expected_block_align);
    let aligned_data = (declared_data / frame_bytes) * frame_bytes;
    let pcm_frame_count = aligned_data / frame_bytes;
    let duration_ns = ticks_to_ns_unsigned(pcm_frame_count, sample_rate_hz);

    Ok(WavHeaderInfo {
        sample_rate_hz,
        channels,
        bits_per_sample,
        data_bytes: aligned_data,
        pcm_frame_count,
        duration_ns,
    })
}

fn build_wav_header(
    sample_rate_hz: u32,
    channels: u16,
    data_bytes: u64,
) -> Result<[u8; WAV_HEADER_BYTES], MediaError> {
    let data_u32 = u32::try_from(data_bytes).map_err(|_| {
        MediaError::InvalidContainer("WAV data size exceeds 32-bit RIFF limit".to_owned())
    })?;
    let riff_size = data_u32.checked_add(36).ok_or_else(|| {
        MediaError::InvalidContainer("WAV RIFF chunk size exceeds u32::MAX".to_owned())
    })?;
    let block_align = channels
        .checked_mul(2)
        .ok_or_else(|| MediaError::InvalidContainer("WAV blockAlign overflow".to_owned()))?;
    let byte_rate = sample_rate_hz
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| MediaError::InvalidContainer("WAV byteRate overflow".to_owned()))?;

    let mut hdr = [0_u8; WAV_HEADER_BYTES];
    hdr[0..4].copy_from_slice(b"RIFF");
    hdr[4..8].copy_from_slice(&riff_size.to_le_bytes());
    hdr[8..12].copy_from_slice(b"WAVE");
    hdr[12..16].copy_from_slice(b"fmt ");
    hdr[16..20].copy_from_slice(&16_u32.to_le_bytes());
    hdr[20..22].copy_from_slice(&WAVE_FORMAT_PCM.to_le_bytes());
    hdr[22..24].copy_from_slice(&channels.to_le_bytes());
    hdr[24..28].copy_from_slice(&sample_rate_hz.to_le_bytes());
    hdr[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    hdr[32..34].copy_from_slice(&block_align.to_le_bytes());
    hdr[34..36].copy_from_slice(&BITS_PER_SAMPLE_S16LE.to_le_bytes());
    hdr[36..40].copy_from_slice(b"data");
    hdr[40..44].copy_from_slice(&data_u32.to_le_bytes());
    Ok(hdr)
}

/// Converts a normalized `f32` PCM sample in `[-1.0, 1.0]` into signed 16-bit PCM with
/// symmetric clamping and NaN-to-silence sanitization.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "f64 -> i16 has no TryFrom; value is finite, rounded, and explicitly clamped to i16 bounds"
)]
fn f32_to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    let clamped = f64::from(sample).clamp(-1.0, 1.0);
    let scaled = (clamped * 32_768.0).round().clamp(-32_768.0, 32_767.0);
    scaled as i16
}
