//! Pure-Rust ISO-BMFF AAC-LC (`audio.m4a`) muxer, parser/indexer, and time-range trimmer.
//!
//! Designed for Android `scrcpy-server-v5.0.1` raw AAC-LC audio streams (typically 48 kHz stereo,
//! 1024 samples per access unit + 2-byte `AudioSpecificConfig`), with zero external dependencies
//! and zero GPL/AGPL code:
//! - Parses and constructs ISO/IEC 14496-3 `AudioSpecificConfig` records (AAC-LC object type 2).
//! - Builds and parses ISO/IEC 14496-12 / 14496-14 `mp4a` + `esds` sample entries.
//! - Muxes faststart (`ftyp` `M4A ` + `moov` + `mdat`) `audio.m4a` files with **strictly zero
//!   `edts`/`elst` boxes**, transparent 7/9-byte ADTS header stripping, and atomic no-replace
//!   publishing (R2 S6).
//! - Indexes `audio.m4a` tracks with full R2 S8 sample-table allocation bounds checking and
//!   performs lossless streaming time-range trimming (`trim_audio_m4a`) rebased cleanly to `pts = 0`.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::MediaError;
use crate::mp4::{
    AUDIO_SAMPLE_ENTRY_HEADER_LEN, AUDIO_SAMPLE_ENTRY_V1_HEADER_LEN, ChunkOffsetMode,
    MAX_TRACK_SAMPLES, box_wrap, build_mdhd, build_mvhd, find_box, full_box_wrap, iter_boxes,
    map_sample_offsets, parse_chunk_offsets, parse_mdhd, parse_stsc, parse_stsz, parse_stts,
    read_exact_at, read_moov_box, read_u16_be, sample_size_u32, ticks_to_ns_signed,
    ticks_to_ns_unsigned,
};
mod realtime;
pub use realtime::trim_audio_time_range;

use crate::output::{Publish, ensure_output_is_not_input, write_atomically};

/// MPEG-4 Audio Object Type for AAC-LC (ISO/IEC 14496-3 Table 1.17).
pub const AAC_LC_OBJECT_TYPE: u8 = 2;

/// Standard PCM samples decoded from one AAC-LC access unit (`frameLengthFlag == 0`).
pub const AAC_LC_SAMPLES_PER_FRAME: u32 = 1024;

/// Short-frame PCM samples decoded from one AAC-LC access unit (`frameLengthFlag == 1`).
pub const AAC_LC_SHORT_SAMPLES_PER_FRAME: u32 = 960;

/// Microseconds per second used when converting packet `pts_us` deltas into audio ticks.
const MICROS_PER_SEC: i128 = 1_000_000;

/// Minimum sane explicit sampling rate accepted in `AudioSpecificConfig` (ISO/IEC 14496-3 lowest
/// standard rate is 7350 Hz).
const MIN_EXPLICIT_SAMPLE_RATE_HZ: u32 = 7_350;

/// Maximum sane explicit sampling rate accepted in `AudioSpecificConfig`.
const MAX_EXPLICIT_SAMPLE_RATE_HZ: u32 = 192_000;

/// Maximum `AudioSpecificConfig` byte length accepted from untrusted `esds` boxes.
const MAX_AUDIO_SPECIFIC_CONFIG_LEN: usize = 1024;

/// Maximum single-packet duration cap in seconds when computing `stts` deltas from `pts_us`.
const MAX_PACKET_DURATION_SECS: u64 = 60;

/// ISO/IEC 14496-1 `ES_DescrTag` (`0x03`).
const ES_DESCR_TAG: u8 = 0x03;

/// ISO/IEC 14496-1 `DecoderConfigDescrTag` (`0x04`).
const DECODER_CONFIG_DESCR_TAG: u8 = 0x04;

/// ISO/IEC 14496-1 `DecSpecificInfoTag` (`0x05`).
const DEC_SPECIFIC_INFO_TAG: u8 = 0x05;

/// ISO/IEC 14496-1 `SLConfigDescrTag` (`0x06`).
const SL_CONFIG_DESCR_TAG: u8 = 0x06;

/// ISO/IEC 14496-1 `objectTypeIndication` for Audio ISO/IEC 14496-3 (MPEG-4 Audio).
const OBJECT_TYPE_MPEG4_AUDIO: u8 = 0x40;

/// ISO/IEC 14496-1 `DecoderConfigDescriptor` byte for `streamType = 0x05` (`AudioStream`),
/// `upStream = 0`, `reserved = 1` (`(0x05 << 2) | 1 == 0x15`).
const STREAM_TYPE_AUDIO_BYTE: u8 = 0x15;

/// ISO/IEC 14496-14 predefined `SLConfigDescriptor` value (`0x02`, reserved for MP4 files).
const SL_CONFIG_PREDEFINED_MP4: u8 = 0x02;

/// ISO/IEC 14496-3 Table 1.18 standard MPEG-4 sampling frequencies (indices `0..=12`).
const MPEG4_SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// Parsed ISO/IEC 14496-3 `AudioSpecificConfig` for an AAC-LC elementary stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSpecificConfig {
    /// MPEG-4 Audio Object Type (validated to be [`AAC_LC_OBJECT_TYPE`] = `2`).
    pub audio_object_type: u8,
    /// Decoded sampling frequency in Hz (e.g., `48_000`).
    pub sample_rate_hz: u32,
    /// Decoded channel count (e.g., `2` for stereo).
    pub channel_count: u16,
    /// Decoded PCM samples per AAC-LC access unit (`1024` or `960`).
    pub samples_per_frame: u32,
    /// Raw `AudioSpecificConfig` bytes preserved verbatim.
    pub raw_bytes: Vec<u8>,
}

/// MSB-first bit reader for ISO/IEC 14496-3 `AudioSpecificConfig` parsing.
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bits(&mut self, count: u8) -> Result<u32, MediaError> {
        let mut value = 0u32;
        for _ in 0..count {
            let byte_idx = self.bit_pos / 8;
            let bit_in_byte = 7 - (self.bit_pos % 8);
            let byte = *self.data.get(byte_idx).ok_or_else(|| {
                MediaError::InvalidContainer("truncated AudioSpecificConfig bitstream".to_owned())
            })?;
            let bit = u32::from((byte >> bit_in_byte) & 1);
            value = (value << 1) | bit;
            self.bit_pos += 1;
        }
        Ok(value)
    }
}

/// Parses and validates a 2+ byte ISO/IEC 14496-3 `AudioSpecificConfig` for AAC-LC.
///
/// Supports both the standard 4-bit `samplingFrequencyIndex` table (`0..=12`) and explicit
/// 24-bit sampling frequencies (`samplingFrequencyIndex == 15`), and validates that
/// `audio_object_type == 2` (AAC-LC) with a sane sample rate and channel configuration.
///
/// # Errors
///
/// Returns [`MediaError::InvalidContainer`] if `data` is shorter than 2 bytes, exceeds the
/// safety size bound, or encodes an invalid sample rate or channel configuration; returns
/// [`MediaError::UnsupportedFeature`] if `audio_object_type != 2`.
pub fn parse_audio_specific_config(data: &[u8]) -> Result<AudioSpecificConfig, MediaError> {
    if data.len() < 2 {
        return Err(MediaError::InvalidContainer(format!(
            "AudioSpecificConfig must be at least 2 bytes, got {}",
            data.len()
        )));
    }
    if data.len() > MAX_AUDIO_SPECIFIC_CONFIG_LEN {
        return Err(MediaError::InvalidContainer(format!(
            "AudioSpecificConfig length {} exceeds {MAX_AUDIO_SPECIFIC_CONFIG_LEN}-byte limit",
            data.len()
        )));
    }

    let mut reader = BitReader::new(data);
    let aot_raw = reader.read_bits(5)?;
    let aot = if aot_raw == 31 {
        32 + reader.read_bits(6)?
    } else {
        aot_raw
    };
    if aot == 0 {
        return Err(MediaError::InvalidContainer(
            "AudioSpecificConfig audioObjectType 0 is reserved".to_owned(),
        ));
    }
    if aot != u32::from(AAC_LC_OBJECT_TYPE) {
        return Err(MediaError::UnsupportedFeature(format!(
            "unsupported AudioSpecificConfig audioObjectType {aot} (only AAC-LC object_type=2 is supported)"
        )));
    }

    let freq_index = reader.read_bits(4)?;
    let sample_rate_hz = if freq_index == 15 {
        let explicit = reader.read_bits(24)?;
        if !(MIN_EXPLICIT_SAMPLE_RATE_HZ..=MAX_EXPLICIT_SAMPLE_RATE_HZ).contains(&explicit) {
            return Err(MediaError::InvalidContainer(format!(
                "explicit AudioSpecificConfig sample rate {explicit} Hz is outside \
                 {MIN_EXPLICIT_SAMPLE_RATE_HZ}..={MAX_EXPLICIT_SAMPLE_RATE_HZ} Hz"
            )));
        }
        explicit
    } else {
        let idx = usize::try_from(freq_index).map_err(|_| {
            MediaError::InvalidContainer("samplingFrequencyIndex overflow".to_owned())
        })?;
        *MPEG4_SAMPLE_RATES.get(idx).ok_or_else(|| {
            MediaError::InvalidContainer(format!(
                "reserved AudioSpecificConfig samplingFrequencyIndex {freq_index}"
            ))
        })?
    };

    let channel_cfg = reader.read_bits(4)?;
    let channel_count = match channel_cfg {
        1..=6 => u16::try_from(channel_cfg).map_err(|_| {
            MediaError::InvalidContainer("channelConfiguration overflow".to_owned())
        })?,
        7 => 8,
        0 => {
            return Err(MediaError::InvalidContainer(
                "AudioSpecificConfig channelConfiguration 0 (PCE) is not supported".to_owned(),
            ));
        }
        other => {
            return Err(MediaError::InvalidContainer(format!(
                "reserved AudioSpecificConfig channelConfiguration {other}"
            )));
        }
    };

    let frame_length_flag = reader.read_bits(1)?;
    let samples_per_frame = if frame_length_flag == 1 {
        AAC_LC_SHORT_SAMPLES_PER_FRAME
    } else {
        AAC_LC_SAMPLES_PER_FRAME
    };

    Ok(AudioSpecificConfig {
        audio_object_type: AAC_LC_OBJECT_TYPE,
        sample_rate_hz,
        channel_count,
        samples_per_frame,
        raw_bytes: data.to_vec(),
    })
}

/// Builds a canonical 2-byte ISO/IEC 14496-3 AAC-LC `AudioSpecificConfig`.
///
/// For example, `build_aac_lc_audio_specific_config(48_000, 2)` returns `Ok([0x11, 0x90])`.
///
/// # Errors
///
/// Returns [`MediaError::InvalidArgument`] if `sample_rate_hz` is not in the standard MPEG-4
/// frequency table (`7350..=96000` Hz) or `channels` is not one of `1, 2, 3, 4, 5, 6, 8`.
pub fn build_aac_lc_audio_specific_config(
    sample_rate_hz: u32,
    channels: u16,
) -> Result<[u8; 2], MediaError> {
    let freq_index = MPEG4_SAMPLE_RATES
        .iter()
        .position(|&rate| rate == sample_rate_hz)
        .ok_or_else(|| {
            MediaError::InvalidArgument(format!(
                "unsupported AAC-LC sample rate {sample_rate_hz} Hz for 2-byte AudioSpecificConfig"
            ))
        })?;
    let freq_u16 = u16::try_from(freq_index)
        .map_err(|_| MediaError::InvalidArgument("sample rate index overflow".to_owned()))?;

    let channel_cfg: u16 = match channels {
        1..=6 => channels,
        8 => 7,
        _ => {
            return Err(MediaError::InvalidArgument(format!(
                "unsupported AAC-LC channel count {channels} (expected 1..=6 or 8)"
            )));
        }
    };

    // 5 bits audioObjectType (2) | 4 bits samplingFrequencyIndex | 4 bits channelConfiguration | 3 bits GA config (000)
    let bits: u16 = (u16::from(AAC_LC_OBJECT_TYPE) << 11) | (freq_u16 << 7) | (channel_cfg << 3);
    Ok(bits.to_be_bytes())
}

/// Strips a leading 7-byte (no-CRC) or 9-byte (CRC) ADTS frame header when present.
///
/// To avoid corrupting raw AAC access units whose payload bytes coincidentally begin with
/// `0xFF 0xF*`, this helper requires the 12-bit ADTS syncword (`0xFFF`), MPEG layer `0`,
/// an exact 13-bit `aac_frame_length == packet.len()` match, and a non-empty payload after
/// the header. Returns `Some(raw_au)` if an ADTS header was stripped, or `None` if `packet`
/// is already a raw AAC access unit.
pub fn strip_adts_header(packet: &[u8]) -> Option<&[u8]> {
    if packet.len() < 7 {
        return None;
    }
    if packet[0] != 0xFF || (packet[1] & 0xF6) != 0xF0 {
        return None;
    }
    let protection_absent = (packet[1] & 0x01) != 0;
    let header_len = if protection_absent { 7usize } else { 9usize };
    if packet.len() <= header_len {
        return None;
    }
    let frame_len = (usize::from(packet[3] & 0x03) << 11)
        | (usize::from(packet[4]) << 3)
        | (usize::from(packet[5]) >> 5);
    if frame_len != packet.len() {
        return None;
    }
    // Only strip single raw-data-block ADTS frames (`number_of_raw_data_blocks_in_frame == 0`).
    if (packet[6] & 0x03) != 0 {
        return None;
    }
    packet.get(header_len..)
}

/// Encodes an ISO/IEC 14496-1 `ExpandableClass` descriptor length (1..=4 bytes).
fn push_descriptor_length(len: usize, out: &mut Vec<u8>) -> Result<(), MediaError> {
    let len_u32 = u32::try_from(len)
        .map_err(|_| MediaError::InvalidContainer("esds descriptor length overflow".to_owned()))?;
    if len_u32 >= (1u32 << 28) {
        return Err(MediaError::InvalidContainer(
            "esds descriptor length exceeds 28-bit ISO/IEC 14496-1 limit".to_owned(),
        ));
    }
    if len_u32 < 0x80 {
        out.push(u8::try_from(len_u32).unwrap_or(0));
    } else if len_u32 < 0x4000 {
        out.push(u8::try_from(((len_u32 >> 7) & 0x7F) | 0x80).unwrap_or(0));
        out.push(u8::try_from(len_u32 & 0x7F).unwrap_or(0));
    } else if len_u32 < 0x0020_0000 {
        out.push(u8::try_from(((len_u32 >> 14) & 0x7F) | 0x80).unwrap_or(0));
        out.push(u8::try_from(((len_u32 >> 7) & 0x7F) | 0x80).unwrap_or(0));
        out.push(u8::try_from(len_u32 & 0x7F).unwrap_or(0));
    } else {
        out.push(u8::try_from(((len_u32 >> 21) & 0x7F) | 0x80).unwrap_or(0));
        out.push(u8::try_from(((len_u32 >> 14) & 0x7F) | 0x80).unwrap_or(0));
        out.push(u8::try_from(((len_u32 >> 7) & 0x7F) | 0x80).unwrap_or(0));
        out.push(u8::try_from(len_u32 & 0x7F).unwrap_or(0));
    }
    Ok(())
}

/// Reads one ISO/IEC 14496-1 descriptor (`tag`, `length`, `payload`) starting at `*pos`.
fn read_descriptor<'a>(data: &'a [u8], pos: &mut usize) -> Result<(u8, &'a [u8]), MediaError> {
    let tag = *data
        .get(*pos)
        .ok_or_else(|| MediaError::InvalidContainer("truncated esds descriptor tag".to_owned()))?;
    *pos += 1;

    let mut length = 0usize;
    let mut byte_count = 0usize;
    loop {
        if byte_count >= 4 {
            return Err(MediaError::InvalidContainer(
                "esds descriptor length exceeds 4 bytes".to_owned(),
            ));
        }
        let byte = *data.get(*pos).ok_or_else(|| {
            MediaError::InvalidContainer("truncated esds descriptor length".to_owned())
        })?;
        *pos += 1;
        byte_count += 1;
        length = (length << 7) | usize::from(byte & 0x7F);
        if (byte & 0x80) == 0 {
            break;
        }
    }

    let end = pos.checked_add(length).ok_or_else(|| {
        MediaError::InvalidContainer("esds descriptor length overflow".to_owned())
    })?;
    let payload = data.get(*pos..end).ok_or_else(|| {
        MediaError::InvalidContainer("esds descriptor payload exceeds parent box".to_owned())
    })?;
    *pos = end;
    Ok((tag, payload))
}

/// Builds an ISO/IEC 14496-14 `esds` `FullBox` carrying `audio_specific_config`.
fn build_esds_box(
    audio_specific_config: &[u8],
    max_bitrate: u32,
    avg_bitrate: u32,
) -> Result<Vec<u8>, MediaError> {
    // DecoderSpecificInfo (tag 0x05)
    let mut dec_spec = Vec::with_capacity(5 + audio_specific_config.len());
    dec_spec.push(DEC_SPECIFIC_INFO_TAG);
    push_descriptor_length(audio_specific_config.len(), &mut dec_spec)?;
    dec_spec.extend_from_slice(audio_specific_config);

    // DecoderConfigDescriptor (tag 0x04)
    let mut dec_cfg_body = Vec::with_capacity(13 + dec_spec.len());
    dec_cfg_body.push(OBJECT_TYPE_MPEG4_AUDIO);
    dec_cfg_body.push(STREAM_TYPE_AUDIO_BYTE);
    dec_cfg_body.extend_from_slice(&[0u8; 3]); // bufferSizeDB
    dec_cfg_body.extend_from_slice(&max_bitrate.to_be_bytes());
    dec_cfg_body.extend_from_slice(&avg_bitrate.to_be_bytes());
    dec_cfg_body.extend_from_slice(&dec_spec);

    let mut dec_cfg = Vec::with_capacity(5 + dec_cfg_body.len());
    dec_cfg.push(DECODER_CONFIG_DESCR_TAG);
    push_descriptor_length(dec_cfg_body.len(), &mut dec_cfg)?;
    dec_cfg.extend_from_slice(&dec_cfg_body);

    // SLConfigDescriptor (tag 0x06, predefined = 0x02)
    let sl_cfg = [SL_CONFIG_DESCR_TAG, 0x01, SL_CONFIG_PREDEFINED_MP4];

    // ES_Descriptor (tag 0x03)
    let mut es_body = Vec::with_capacity(3 + dec_cfg.len() + sl_cfg.len());
    es_body.extend_from_slice(&1u16.to_be_bytes()); // ES_ID = 1
    es_body.push(0x00); // flags = 0 (no streamDependence, URL, or OCR)
    es_body.extend_from_slice(&dec_cfg);
    es_body.extend_from_slice(&sl_cfg);

    let mut es_descr = Vec::with_capacity(5 + es_body.len());
    es_descr.push(ES_DESCR_TAG);
    push_descriptor_length(es_body.len(), &mut es_descr)?;
    es_descr.extend_from_slice(&es_body);

    full_box_wrap(*b"esds", 0, 0, &es_descr)
}

/// Builds a valid ISO-BMFF `mp4a` `AudioSampleEntry` box containing an `esds` descriptor box.
///
/// Validates `audio_specific_config` as AAC-LC (`object_type == 2`) and confirms its decoded
/// sample rate and channel count match `sample_rate_hz` and `channels`.
///
/// # Errors
///
/// Returns [`MediaError::InvalidContainer`] or [`MediaError::UnsupportedFeature`] if
/// `audio_specific_config` is invalid, or [`MediaError::InvalidArgument`] if `sample_rate_hz`
/// or `channels` disagrees with `audio_specific_config`.
pub fn build_mp4a_sample_entry(
    sample_rate_hz: u32,
    channels: u16,
    audio_specific_config: &[u8],
    max_bitrate: u32,
    avg_bitrate: u32,
) -> Result<Vec<u8>, MediaError> {
    let asc = parse_audio_specific_config(audio_specific_config)?;
    if asc.sample_rate_hz != sample_rate_hz || asc.channel_count != channels {
        return Err(MediaError::InvalidArgument(format!(
            "AudioSpecificConfig ({} Hz, {} ch) does not match requested mp4a parameters ({sample_rate_hz} Hz, {channels} ch)",
            asc.sample_rate_hz, asc.channel_count
        )));
    }

    let esds = build_esds_box(audio_specific_config, max_bitrate, avg_bitrate)?;
    let mut body = Vec::with_capacity(AUDIO_SAMPLE_ENTRY_HEADER_LEN + esds.len());
    body.extend_from_slice(&[0u8; 6]); // reserved
    body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index = 1
    body.extend_from_slice(&[0u8; 8]); // reserved[2]
    body.extend_from_slice(&channels.to_be_bytes()); // channelcount
    body.extend_from_slice(&16u16.to_be_bytes()); // samplesize = 16-bit
    body.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    body.extend_from_slice(&0u16.to_be_bytes()); // reserved
    // 16.16 fixed-point samplerate (rates > 65535 Hz encode 0 per ISO/IEC 14496-12 §12.2.3).
    let sr_fixed = u16::try_from(sample_rate_hz).map_or(0u32, |r| u32::from(r) << 16);
    body.extend_from_slice(&sr_fixed.to_be_bytes());
    body.extend_from_slice(&esds);
    box_wrap(*b"mp4a", &body)
}

/// Parses an ISO/IEC 14496-14 `esds` `FullBox` body (starting at version/flags) and extracts
/// its validated AAC-LC [`AudioSpecificConfig`].
fn parse_esds(esds_body: &[u8]) -> Result<AudioSpecificConfig, MediaError> {
    let descriptors = esds_body
        .get(4..)
        .ok_or_else(|| MediaError::InvalidContainer("short esds full-box header".to_owned()))?;
    let mut pos = 0usize;
    while pos < descriptors.len() {
        let (tag, payload) = read_descriptor(descriptors, &mut pos)?;
        if tag == ES_DESCR_TAG {
            return parse_es_descriptor(payload);
        }
    }
    Err(MediaError::InvalidContainer(
        "missing ES_Descriptor (tag 0x03) inside esds".to_owned(),
    ))
}

fn parse_es_descriptor(es_body: &[u8]) -> Result<AudioSpecificConfig, MediaError> {
    if es_body.len() < 3 {
        return Err(MediaError::InvalidContainer(
            "truncated ES_Descriptor header".to_owned(),
        ));
    }
    let flags = es_body[2];
    let mut pos = 3usize;
    if (flags & 0x80) != 0 {
        pos = pos.checked_add(2).ok_or_else(|| {
            MediaError::InvalidContainer("ES_Descriptor dependsOn_ES_ID overflow".to_owned())
        })?;
    }
    if (flags & 0x40) != 0 {
        let url_len = usize::from(*es_body.get(pos).ok_or_else(|| {
            MediaError::InvalidContainer("truncated ES_Descriptor URL length".to_owned())
        })?);
        pos = pos.checked_add(1 + url_len).ok_or_else(|| {
            MediaError::InvalidContainer("ES_Descriptor URL length overflow".to_owned())
        })?;
    }
    if (flags & 0x20) != 0 {
        pos = pos.checked_add(2).ok_or_else(|| {
            MediaError::InvalidContainer("ES_Descriptor OCR_ES_Id overflow".to_owned())
        })?;
    }
    if pos > es_body.len() {
        return Err(MediaError::InvalidContainer(
            "ES_Descriptor optional header fields exceed descriptor bounds".to_owned(),
        ));
    }

    while pos < es_body.len() {
        let (tag, payload) = read_descriptor(es_body, &mut pos)?;
        if tag == DECODER_CONFIG_DESCR_TAG {
            return parse_decoder_config_descriptor(payload);
        }
    }
    Err(MediaError::InvalidContainer(
        "missing DecoderConfigDescriptor (tag 0x04) inside ES_Descriptor".to_owned(),
    ))
}

fn parse_decoder_config_descriptor(dec_cfg: &[u8]) -> Result<AudioSpecificConfig, MediaError> {
    if dec_cfg.len() < 13 {
        return Err(MediaError::InvalidContainer(
            "truncated DecoderConfigDescriptor header".to_owned(),
        ));
    }
    let object_type_indication = dec_cfg[0];
    if object_type_indication != OBJECT_TYPE_MPEG4_AUDIO {
        return Err(MediaError::UnsupportedFeature(format!(
            "unsupported esds objectTypeIndication 0x{object_type_indication:02x} \
             (expected 0x{OBJECT_TYPE_MPEG4_AUDIO:02x} MPEG-4 Audio)"
        )));
    }
    let stream_type = dec_cfg[1] >> 2;
    if stream_type != 0x05 {
        return Err(MediaError::InvalidContainer(format!(
            "unexpected esds streamType 0x{stream_type:02x} (expected 0x05 AudioStream)"
        )));
    }

    let mut pos = 13usize;
    while pos < dec_cfg.len() {
        let (tag, payload) = read_descriptor(dec_cfg, &mut pos)?;
        if tag == DEC_SPECIFIC_INFO_TAG {
            return parse_audio_specific_config(payload);
        }
    }
    Err(MediaError::InvalidContainer(
        "missing DecoderSpecificInfo (tag 0x05) inside DecoderConfigDescriptor".to_owned(),
    ))
}

/// One AAC-LC access unit with presentation timestamp in microseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AacPacket {
    /// Presentation timestamp in microseconds.
    pub pts_us: i64,
    /// Encoded AAC-LC access unit bytes (raw payload, or ADTS-framed which is stripped cleanly).
    pub data: Vec<u8>,
}

/// Input parameters and ordered AAC-LC packets for [`mux_aac_to_m4a`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AacMuxInput {
    /// ISO/IEC 14496-3 `AudioSpecificConfig` bytes (e.g., `[0x11, 0x90]` for 48 kHz stereo).
    pub audio_specific_config: Vec<u8>,
    /// Audio sampling rate in Hz (written as `mdhd`/`mvhd` timescale; must match `audio_specific_config`).
    pub sample_rate_hz: u32,
    /// Channel count (must match `audio_specific_config`).
    pub channels: u16,
    /// Ordered sequence of AAC-LC access units to mux.
    pub packets: Vec<AacPacket>,
}

/// Summary returned after completing [`mux_aac_to_m4a`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioMuxReport {
    /// Number of AAC-LC access units muxed into the output `audio.m4a`.
    pub packet_count: u64,
    /// Total duration of the muxed audio track in nanoseconds.
    pub duration_ns: u64,
    /// Audio sampling rate / track timescale in Hz.
    pub sample_rate_hz: u32,
    /// Channel count recorded in `mp4a`/`esds`.
    pub channels: u16,
    /// Number of packets from which a leading ADTS header was stripped.
    pub adts_stripped_count: u64,
    /// Number of `edts` boxes written to the output (always `0`).
    pub edts_box_count: usize,
    /// Number of `elst` boxes written to the output (always `0`).
    pub elst_box_count: usize,
}

/// Metadata for a single muxed audio sample prior to writing `moov` and `mdat`.
#[derive(Debug, Clone, Copy)]
struct AudioSampleMeta {
    size: u32,
    duration_ticks: u32,
}

/// Track header parameters for building an `audio.m4a` `moov` box.
struct AudioTrackHeader<'a> {
    timescale: u32,
    sample_rate_hz: u32,
    channels: u16,
    audio_specific_config: &'a [u8],
    max_bitrate: u32,
    avg_bitrate: u32,
}

/// Computes a safely bounded `stts` sample delta in audio timescale ticks from adjacent `pts_us`.
fn compute_packet_duration_ticks(
    cur_pts_us: i64,
    next_pts_us: Option<i64>,
    sample_rate_hz: u32,
    default_samples_per_frame: u32,
) -> u32 {
    let Some(next_us) = next_pts_us else {
        return default_samples_per_frame;
    };
    let Some(delta_us) = next_us.checked_sub(cur_pts_us) else {
        return default_samples_per_frame;
    };
    if delta_us <= 0 {
        return default_samples_per_frame;
    }

    let max_ticks = u64::from(sample_rate_hz)
        .saturating_mul(MAX_PACKET_DURATION_SECS)
        .min(u64::from(u32::MAX));
    let ticks_i128 =
        (i128::from(delta_us) * i128::from(sample_rate_hz) + MICROS_PER_SEC / 2) / MICROS_PER_SEC;
    let Ok(ticks_u64) = u64::try_from(ticks_i128) else {
        return default_samples_per_frame;
    };
    let clamped = ticks_u64.clamp(u64::from(default_samples_per_frame), max_ticks);
    u32::try_from(clamped).unwrap_or(default_samples_per_frame)
}

/// Computes `(max_bitrate, avg_bitrate)` in bits per second for `DecoderConfigDescriptor`.
fn compute_bitrates(samples: &[AudioSampleMeta], timescale: u32) -> (u32, u32) {
    let mut total_bytes = 0u128;
    let mut total_ticks = 0u128;
    let mut max_bps = 0u32;
    let ts = u128::from(timescale);

    for sm in samples {
        let bytes = u128::from(sm.size);
        let ticks = u128::from(sm.duration_ticks);
        total_bytes = total_bytes.saturating_add(bytes);
        total_ticks = total_ticks.saturating_add(ticks);
        if let Some(pkt_bps) = bytes
            .saturating_mul(8)
            .saturating_mul(ts)
            .checked_div(ticks)
        {
            let pkt_bps_u32 = u32::try_from(pkt_bps).unwrap_or(u32::MAX);
            max_bps = max_bps.max(pkt_bps_u32);
        }
    }

    let avg_bps = total_bytes
        .saturating_mul(8)
        .saturating_mul(ts)
        .checked_div(total_ticks)
        .map_or(0, |avg| u32::try_from(avg).unwrap_or(u32::MAX));
    (max_bps.max(avg_bps), avg_bps)
}

/// Muxes AAC-LC packets into a faststart (`ftyp` `M4A ` + `moov` + `mdat`) `audio.m4a` file
/// with **strictly zero `edts`/`elst` boxes** and atomic no-replace publishing.
///
/// Accepts raw AAC-LC access units as well as ADTS-framed packets (7- or 9-byte ADTS headers
/// are detected and stripped cleanly). The destination file is written to a sibling temp file,
/// `fsync`ed, and published without overwriting any existing file.
///
/// # Errors
///
/// Returns:
/// - [`MediaError::InvalidArgument`] if `input.packets` is empty, exceeds [`MAX_TRACK_SAMPLES`],
///   or `input.sample_rate_hz` / `input.channels` disagrees with `input.audio_specific_config`.
/// - [`MediaError::InvalidContainer`] or [`MediaError::UnsupportedFeature`] if
///   `audio_specific_config` is malformed or not AAC-LC, or any packet payload is empty.
/// - [`MediaError::Io`] (`AlreadyExists`) if `path` already exists, or on filesystem I/O failure.
pub fn mux_aac_to_m4a(
    path: impl AsRef<Path>,
    input: &AacMuxInput,
) -> Result<AudioMuxReport, MediaError> {
    if input.packets.is_empty() {
        return Err(MediaError::InvalidArgument(
            "cannot mux empty AAC packet sequence".to_owned(),
        ));
    }
    if input.packets.len() > MAX_TRACK_SAMPLES {
        return Err(MediaError::InvalidArgument(format!(
            "AAC packet count {} exceeds the {MAX_TRACK_SAMPLES} sample limit",
            input.packets.len()
        )));
    }

    let asc = parse_audio_specific_config(&input.audio_specific_config)?;
    if asc.sample_rate_hz != input.sample_rate_hz || asc.channel_count != input.channels {
        return Err(MediaError::InvalidArgument(format!(
            "AudioSpecificConfig ({} Hz, {} ch) does not match AacMuxInput ({} Hz, {} ch)",
            asc.sample_rate_hz, asc.channel_count, input.sample_rate_hz, input.channels
        )));
    }

    let mut payloads: Vec<&[u8]> = Vec::with_capacity(input.packets.len());
    let mut metas: Vec<AudioSampleMeta> = Vec::with_capacity(input.packets.len());
    let mut adts_stripped_count = 0u64;

    for (idx, pkt) in input.packets.iter().enumerate() {
        let raw = if let Some(stripped) = strip_adts_header(&pkt.data) {
            adts_stripped_count += 1;
            stripped
        } else {
            pkt.data.as_slice()
        };
        if raw.is_empty() {
            return Err(MediaError::InvalidContainer(format!(
                "AAC packet {idx} has empty payload"
            )));
        }
        let next_pts = input.packets.get(idx + 1).map(|next| next.pts_us);
        let duration_ticks = compute_packet_duration_ticks(
            pkt.pts_us,
            next_pts,
            input.sample_rate_hz,
            asc.samples_per_frame,
        );
        metas.push(AudioSampleMeta {
            size: sample_size_u32(raw.len())?,
            duration_ticks,
        });
        payloads.push(raw);
    }

    let total_ticks: u64 = metas
        .iter()
        .try_fold(0u64, |acc, sm| {
            acc.checked_add(u64::from(sm.duration_ticks))
        })
        .ok_or_else(|| MediaError::InvalidContainer("audio track duration overflow".to_owned()))?;
    let (max_bitrate, avg_bitrate) = compute_bitrates(&metas, input.sample_rate_hz);

    let header = AudioTrackHeader {
        timescale: input.sample_rate_hz,
        sample_rate_hz: input.sample_rate_hz,
        channels: input.channels,
        audio_specific_config: &input.audio_specific_config,
        max_bitrate,
        avg_bitrate,
    };

    write_faststart_m4a(
        path.as_ref(),
        Publish::NoReplace,
        &header,
        &metas,
        |idx, buf| {
            buf.clear();
            buf.extend_from_slice(payloads[idx]);
            Ok(())
        },
    )?;

    let packet_count = u64::try_from(metas.len()).unwrap_or(0);
    Ok(AudioMuxReport {
        packet_count,
        duration_ns: ticks_to_ns_unsigned(total_ticks, input.sample_rate_hz),
        sample_rate_hz: input.sample_rate_hz,
        channels: input.channels,
        adts_stripped_count,
        edts_box_count: 0,
        elst_box_count: 0,
    })
}

/// Exact indexed metadata for a single AAC-LC access unit inside an `audio.m4a` track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedAudioPacket {
    /// Zero-based sequential packet index in presentation/decode order.
    pub index: u64,
    /// Presentation timestamp in nanoseconds.
    pub pts_ns: i64,
    /// Packet duration in nanoseconds.
    pub duration_ns: u64,
    /// Absolute byte offset of the encoded access unit inside the file.
    pub offset: u64,
    /// Encoded access unit length in bytes.
    pub size: u32,
    /// Raw presentation timestamp in `mdhd` timescale ticks.
    pub pts_ticks: u64,
    /// Raw sample duration in `mdhd` timescale ticks.
    pub duration_ticks: u32,
}

/// Indexed AAC-LC audio track backed by an open read-only `audio.m4a` file handle.
#[derive(Debug)]
pub struct Mp4AudioTrack {
    file: File,
    path: PathBuf,
    file_len: u64,
    timescale: u32,
    duration_ticks: u64,
    audio_specific_config: AudioSpecificConfig,
    packets: Vec<IndexedAudioPacket>,
}

struct ParsedAudioTrack {
    timescale: u32,
    duration_ticks: u64,
    audio_specific_config: AudioSpecificConfig,
    packets: Vec<IndexedAudioPacket>,
}

impl Mp4AudioTrack {
    /// Opens an ISO-BMFF `audio.m4a` (or MP4) file and indexes its primary AAC-LC (`mp4a`)
    /// audio track with full R2 S8 sample-table allocation bounds checking.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Io`] if the file cannot be read, [`MediaError::UnsupportedFeature`]
    /// if the audio track is not MPEG-4 AAC-LC, or [`MediaError::InvalidContainer`] if required
    /// boxes (`moov`, `trak`, `mdhd`, `stsd`/`mp4a`/`esds`, `stts`, `stsz`, `stsc`, `stco`/`co64`)
    /// are missing, truncated, or declare out-of-bounds table counts.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MediaError> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        let file_len = file.metadata()?.len();

        let moov = read_moov_box(&mut file, file_len)?;
        let parsed = parse_audio_moov(&moov, file_len)?;

        Ok(Self {
            file,
            path,
            file_len,
            timescale: parsed.timescale,
            duration_ticks: parsed.duration_ticks,
            audio_specific_config: parsed.audio_specific_config,
            packets: parsed.packets,
        })
    }

    /// Returns the path this audio track was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the total file length in bytes.
    pub const fn file_len(&self) -> u64 {
        self.file_len
    }

    /// Returns the media timescale (ticks per second) from `mdhd`.
    pub const fn timescale(&self) -> u32 {
        self.timescale
    }

    /// Returns the track duration in `mdhd` timescale ticks.
    pub const fn duration_ticks(&self) -> u64 {
        self.duration_ticks
    }

    /// Returns the track duration in nanoseconds derived from indexed packet durations.
    pub fn duration_ns(&self) -> u64 {
        let sum_ticks: u64 = self.packets.iter().fold(0u64, |acc, p| {
            acc.saturating_add(u64::from(p.duration_ticks))
        });
        let ticks = if sum_ticks > 0 {
            sum_ticks
        } else {
            self.duration_ticks
        };
        ticks_to_ns_unsigned(ticks, self.timescale)
    }

    /// Returns the AAC-LC sampling rate in Hz from `AudioSpecificConfig`.
    pub const fn sample_rate_hz(&self) -> u32 {
        self.audio_specific_config.sample_rate_hz
    }

    /// Returns the channel count from `AudioSpecificConfig`.
    pub const fn channels(&self) -> u16 {
        self.audio_specific_config.channel_count
    }

    /// Returns the parsed `AudioSpecificConfig` extracted from `mp4a`/`esds`.
    pub const fn audio_specific_config(&self) -> &AudioSpecificConfig {
        &self.audio_specific_config
    }

    /// Returns all indexed AAC-LC packets in presentation order.
    pub fn packets(&self) -> &[IndexedAudioPacket] {
        &self.packets
    }

    /// Returns the total number of indexed audio packets.
    pub fn packet_count(&self) -> u64 {
        u64::try_from(self.packets.len()).unwrap_or(u64::MAX)
    }

    /// Returns indexed metadata for packet `index`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] if `index >= self.packet_count()`.
    pub fn packet(&self, index: u64) -> Result<&IndexedAudioPacket, MediaError> {
        let idx = usize::try_from(index).map_err(|_| MediaError::FrameOutOfRange {
            frame_index: index,
            frame_count: self.packet_count(),
        })?;
        self.packets.get(idx).ok_or(MediaError::FrameOutOfRange {
            frame_index: index,
            frame_count: self.packet_count(),
        })
    }

    /// Reads the raw AAC-LC access unit payload for `index` into `buf` via lock-free positional I/O.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] if `index` is out of bounds, or [`MediaError::Io`]
    /// if the underlying read fails.
    pub fn read_packet_into(&self, index: u64, buf: &mut Vec<u8>) -> Result<(), MediaError> {
        let info = self.packet(index)?;
        let len = usize::try_from(info.size).map_err(|_| {
            MediaError::InvalidContainer(format!("audio packet {index} size exceeds address space"))
        })?;
        buf.resize(len, 0);
        read_exact_at(&self.file, buf, info.offset)?;
        Ok(())
    }
}

fn parse_audio_moov(moov: &[u8], file_len: u64) -> Result<ParsedAudioTrack, MediaError> {
    for (cc, body) in iter_boxes(moov)? {
        if &cc == b"trak"
            && let Some(track) = parse_audio_trak(body, file_len)?
        {
            return Ok(track);
        }
    }
    Err(MediaError::InvalidContainer(
        "no AAC-LC audio track found in moov".to_owned(),
    ))
}

fn parse_audio_stsd(stsd: &[u8]) -> Result<AudioSpecificConfig, MediaError> {
    let entries = stsd
        .get(8..)
        .ok_or_else(|| MediaError::InvalidContainer("short audio stsd box".to_owned()))?;
    for (cc, body) in iter_boxes(entries)? {
        if &cc == b"mp4a" {
            if body.len() < AUDIO_SAMPLE_ENTRY_HEADER_LEN {
                return Err(MediaError::InvalidContainer(
                    "short mp4a sample entry".to_owned(),
                ));
            }
            let version = read_u16_be(body, 8)?;
            let hdr_len = match version {
                0 => AUDIO_SAMPLE_ENTRY_HEADER_LEN,
                1 => AUDIO_SAMPLE_ENTRY_V1_HEADER_LEN,
                other => {
                    return Err(MediaError::UnsupportedFeature(format!(
                        "unsupported mp4a AudioSampleEntry version {other}"
                    )));
                }
            };
            let children = body.get(hdr_len..).ok_or_else(|| {
                MediaError::InvalidContainer("truncated mp4a sample entry header".to_owned())
            })?;
            let esds_body = find_box(children, *b"esds")?
                .ok_or_else(|| MediaError::InvalidContainer("missing esds in mp4a".to_owned()))?;
            return parse_esds(esds_body);
        }
    }
    Err(MediaError::InvalidContainer(
        "no mp4a entry found in audio stsd".to_owned(),
    ))
}

fn parse_audio_trak(trak: &[u8], file_len: u64) -> Result<Option<ParsedAudioTrack>, MediaError> {
    let Some(mdia) = find_box(trak, *b"mdia")? else {
        return Ok(None);
    };
    let Some(hdlr) = find_box(mdia, *b"hdlr")? else {
        return Ok(None);
    };
    if hdlr.get(8..12) != Some(b"soun") {
        return Ok(None);
    }

    let mdhd = find_box(mdia, *b"mdhd")?
        .ok_or_else(|| MediaError::InvalidContainer("missing audio mdhd box".to_owned()))?;
    let (timescale, duration_ticks) = parse_mdhd(mdhd)?;

    let minf = find_box(mdia, *b"minf")?
        .ok_or_else(|| MediaError::InvalidContainer("missing audio minf box".to_owned()))?;
    let stbl = find_box(minf, *b"stbl")?
        .ok_or_else(|| MediaError::InvalidContainer("missing audio stbl box".to_owned()))?;

    let sample_desc_box = find_box(stbl, *b"stsd")?
        .ok_or_else(|| MediaError::InvalidContainer("missing audio stsd box".to_owned()))?;
    let audio_specific_config = parse_audio_stsd(sample_desc_box)?;

    // Parse `stsz` first (R2 S8): its bounded sample count caps `stts` expansion.
    let sample_size_box = find_box(stbl, *b"stsz")?
        .ok_or_else(|| MediaError::InvalidContainer("missing audio stsz box".to_owned()))?;
    let sizes = parse_stsz(sample_size_box, file_len)?;
    let sample_count = sizes.len();

    let time_to_sample_box = find_box(stbl, *b"stts")?
        .ok_or_else(|| MediaError::InvalidContainer("missing audio stts box".to_owned()))?;
    let durations = parse_stts(time_to_sample_box, sample_count)?;

    let sample_to_chunk_box = find_box(stbl, *b"stsc")?
        .ok_or_else(|| MediaError::InvalidContainer("missing audio stsc box".to_owned()))?;
    let stsc_entries = parse_stsc(sample_to_chunk_box)?;
    let chunk_offsets = parse_chunk_offsets(stbl)?;
    let offsets = map_sample_offsets(&chunk_offsets, &stsc_entries, &sizes, file_len)?;

    let mut packets = Vec::with_capacity(sample_count);
    let mut pts_ticks = 0u64;
    let default_delta = audio_specific_config.samples_per_frame;

    for i in 0..sample_count {
        let index = u64::try_from(i).unwrap_or(0);
        let duration_tick = durations.get(i).copied().unwrap_or(default_delta);
        let pts_signed = i64::try_from(pts_ticks).map_err(|_| {
            MediaError::InvalidContainer("audio presentation timestamp overflows i64".to_owned())
        })?;
        packets.push(IndexedAudioPacket {
            index,
            pts_ns: ticks_to_ns_signed(pts_signed, timescale),
            duration_ns: ticks_to_ns_unsigned(u64::from(duration_tick), timescale),
            offset: offsets[i],
            size: sizes[i],
            pts_ticks,
            duration_ticks: duration_tick,
        });
        pts_ticks = pts_ticks.saturating_add(u64::from(duration_tick));
    }

    Ok(Some(ParsedAudioTrack {
        timescale,
        duration_ticks,
        audio_specific_config,
        packets,
    }))
}

/// Summary returned after completing a time-range [`trim_audio_m4a`] operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTrimReport {
    /// Total number of AAC-LC access units in the input `audio.m4a`.
    pub input_packets: u64,
    /// Number of AAC-LC access units written to the trimmed output `audio.m4a`.
    pub output_packets: u64,
    /// Total duration of the trimmed output track in nanoseconds.
    pub duration_ns: u64,
    /// Audio sampling rate in Hz.
    pub sample_rate_hz: u32,
    /// Channel count.
    pub channels: u16,
    /// Number of `edts` boxes written to the output (always `0`).
    pub edts_box_count: usize,
    /// Number of `elst` boxes written to the output (always `0`).
    pub elst_box_count: usize,
    /// Source-track presentation timestamp (nanoseconds) of the first selected access
    /// unit — needed to re-derive the trimmed file's A/V lead-in offset.
    #[serde(default)]
    pub first_selected_pts_ns: i64,
}

/// Trims `input_m4a` to the AAC-LC access units intersecting `[start_ns, end_ns)` and writes a
/// new faststart zero-`elst` `audio.m4a` rebased cleanly to `pts = 0`.
///
/// An access unit spanning `[pkt_start_ns, pkt_end_ns)` (computed in exact `mdhd` ticks so
/// sub-nanosecond truncation never introduces a 1 ns gap between adjacent packets) is included
/// when `pkt_end_ns > start_ns && pkt_start_ns < end_ns`. Selected packets are stream-copied
/// from `input_m4a` using one reused buffer without decoding or re-encoding, and published
/// crash-safely without replacing any existing file (R2 S6).
///
/// # Errors
///
/// Returns:
/// - [`MediaError::InvalidArgument`] if `output_m4a` resolves to `input_m4a`, `start_ns < 0`,
///   or `end_ns <= start_ns`.
/// - [`MediaError::NoAudioInRange`] if no audio packets intersect `[start_ns, end_ns)`.
/// - [`MediaError::Io`] (`AlreadyExists`) if `output_m4a` already exists, or on I/O failure.
/// - [`MediaError::InvalidContainer`] or [`MediaError::UnsupportedFeature`] if `input_m4a`
///   is malformed or not an AAC-LC track.
pub fn trim_audio_m4a(
    input_m4a: impl AsRef<Path>,
    output_m4a: impl AsRef<Path>,
    start_ns: i64,
    end_ns: i64,
) -> Result<AudioTrimReport, MediaError> {
    let input_path = input_m4a.as_ref();
    let output_path = output_m4a.as_ref();
    ensure_output_is_not_input(input_path, output_path)?;

    if start_ns < 0 || end_ns <= start_ns {
        return Err(MediaError::InvalidArgument(format!(
            "invalid audio trim range [{start_ns}, {end_ns}): requires 0 <= start_ns < end_ns"
        )));
    }

    let track = Mp4AudioTrack::open(input_path)?;
    let timescale = track.timescale();
    let mut selected_indices: Vec<u64> = Vec::new();
    let mut metas: Vec<AudioSampleMeta> = Vec::new();

    for pkt in track.packets() {
        let pkt_end_ticks = pkt.pts_ticks.saturating_add(u64::from(pkt.duration_ticks));
        let pkt_end_signed = i64::try_from(pkt_end_ticks).unwrap_or(i64::MAX);
        let pkt_end_ns = ticks_to_ns_signed(pkt_end_signed, timescale);
        if (pkt_end_ns > start_ns || (start_ns == 0 && pkt.index == 0 && pkt.duration_ticks == 0))
            && pkt.pts_ns < end_ns
        {
            selected_indices.push(pkt.index);
            metas.push(AudioSampleMeta {
                size: pkt.size,
                duration_ticks: pkt.duration_ticks,
            });
        }
    }

    if metas.is_empty() {
        return Err(MediaError::NoAudioInRange { start_ns, end_ns });
    }

    let total_ticks: u64 = metas
        .iter()
        .try_fold(0u64, |acc, sm| {
            acc.checked_add(u64::from(sm.duration_ticks))
        })
        .ok_or_else(|| {
            MediaError::InvalidContainer("trimmed audio duration overflow".to_owned())
        })?;
    let (max_bitrate, avg_bitrate) = compute_bitrates(&metas, timescale);

    let header = AudioTrackHeader {
        timescale,
        sample_rate_hz: track.sample_rate_hz(),
        channels: track.channels(),
        audio_specific_config: &track.audio_specific_config().raw_bytes,
        max_bitrate,
        avg_bitrate,
    };

    write_faststart_m4a(
        output_path,
        Publish::NoReplace,
        &header,
        &metas,
        |idx, buf| track.read_packet_into(selected_indices[idx], buf),
    )?;

    let first_selected_pts_ns = selected_indices
        .first()
        .and_then(|idx| track.packets().iter().find(|pkt| pkt.index == *idx))
        .map_or(0, |pkt| pkt.pts_ns);
    let output_packets = u64::try_from(metas.len()).unwrap_or(0);
    Ok(AudioTrimReport {
        input_packets: track.packet_count(),
        output_packets,
        duration_ns: ticks_to_ns_unsigned(total_ticks, timescale),
        sample_rate_hz: track.sample_rate_hz(),
        channels: track.channels(),
        edts_box_count: 0,
        elst_box_count: 0,
        first_selected_pts_ns,
    })
}

// ---------------- Internal ISO-BMFF Audio Box Builders ----------------

fn build_audio_tkhd(total_duration: u64) -> Result<Vec<u8>, MediaError> {
    let mut payload = Vec::with_capacity(96);
    let version = if let Ok(dur32) = u32::try_from(total_duration) {
        payload.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        payload.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        payload.extend_from_slice(&1u32.to_be_bytes()); // track_ID = 1
        payload.extend_from_slice(&0u32.to_be_bytes()); // reserved
        payload.extend_from_slice(&dur32.to_be_bytes());
        0u8
    } else {
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&total_duration.to_be_bytes());
        1u8
    };
    payload.extend_from_slice(&[0u8; 8]); // reserved[2]
    payload.extend_from_slice(&0u16.to_be_bytes()); // layer
    payload.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
    payload.extend_from_slice(&0x0100u16.to_be_bytes()); // volume = 1.0 (audio track)
    payload.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        payload.extend_from_slice(&v.to_be_bytes());
    }
    payload.extend_from_slice(&0u32.to_be_bytes()); // width = 0
    payload.extend_from_slice(&0u32.to_be_bytes()); // height = 0
    full_box_wrap(*b"tkhd", version, 3, &payload)
}

fn build_audio_stbl(
    header: &AudioTrackHeader<'_>,
    samples: &[AudioSampleMeta],
    mdat_payload_offset: u64,
    offset_mode: ChunkOffsetMode,
) -> Result<Vec<u8>, MediaError> {
    let sample_count_u32 = u32::try_from(samples.len()).map_err(|_| {
        MediaError::InvalidContainer("audio sample count exceeds u32::MAX".to_owned())
    })?;

    // Single `mp4a` entry inside `stsd`.
    let mp4a = build_mp4a_sample_entry(
        header.sample_rate_hz,
        header.channels,
        header.audio_specific_config,
        header.max_bitrate,
        header.avg_bitrate,
    )?;
    let mut sample_desc_payload = Vec::with_capacity(4 + mp4a.len());
    sample_desc_payload.extend_from_slice(&1u32.to_be_bytes());
    sample_desc_payload.extend_from_slice(&mp4a);
    let sample_desc_box = full_box_wrap(*b"stsd", 0, 0, &sample_desc_payload)?;

    // Run-length encoded `stts`.
    let mut stts_runs: Vec<(u32, u32)> = Vec::new();
    for sm in samples {
        if let Some(last) = stts_runs.last_mut()
            && last.1 == sm.duration_ticks
        {
            last.0 += 1;
            continue;
        }
        stts_runs.push((1, sm.duration_ticks));
    }
    let stts_count = u32::try_from(stts_runs.len())
        .map_err(|_| MediaError::InvalidContainer("stts run count exceeds u32::MAX".to_owned()))?;
    let mut time_to_sample_payload = Vec::with_capacity(4 + stts_runs.len() * 8);
    time_to_sample_payload.extend_from_slice(&stts_count.to_be_bytes());
    for (cnt, delta) in stts_runs {
        time_to_sample_payload.extend_from_slice(&cnt.to_be_bytes());
        time_to_sample_payload.extend_from_slice(&delta.to_be_bytes());
    }
    let time_to_sample_box = full_box_wrap(*b"stts", 0, 0, &time_to_sample_payload)?;

    // `stsc`: single chunk holding all samples with `sample_description_index = 1`.
    let mut chunk_map_payload = Vec::with_capacity(16);
    chunk_map_payload.extend_from_slice(&1u32.to_be_bytes());
    chunk_map_payload.extend_from_slice(&1u32.to_be_bytes());
    chunk_map_payload.extend_from_slice(&sample_count_u32.to_be_bytes());
    chunk_map_payload.extend_from_slice(&1u32.to_be_bytes());
    let chunk_map_box = full_box_wrap(*b"stsc", 0, 0, &chunk_map_payload)?;

    // `stsz`: variable sample sizes.
    let mut size_table_payload = Vec::with_capacity(8 + samples.len() * 4);
    size_table_payload.extend_from_slice(&0u32.to_be_bytes());
    size_table_payload.extend_from_slice(&sample_count_u32.to_be_bytes());
    for sm in samples {
        size_table_payload.extend_from_slice(&sm.size.to_be_bytes());
    }
    let size_table_box = full_box_wrap(*b"stsz", 0, 0, &size_table_payload)?;

    // `stco` or `co64`.
    let chunk_offset_box = match offset_mode {
        ChunkOffsetMode::Co64 => {
            let mut co64_p = Vec::with_capacity(12);
            co64_p.extend_from_slice(&1u32.to_be_bytes());
            co64_p.extend_from_slice(&mdat_payload_offset.to_be_bytes());
            full_box_wrap(*b"co64", 0, 0, &co64_p)?
        }
        ChunkOffsetMode::Stco32 => {
            let off32 = u32::try_from(mdat_payload_offset).map_err(|_| {
                MediaError::InvalidContainer("stco offset exceeds u32::MAX".to_owned())
            })?;
            let mut stco_p = Vec::with_capacity(8);
            stco_p.extend_from_slice(&1u32.to_be_bytes());
            stco_p.extend_from_slice(&off32.to_be_bytes());
            full_box_wrap(*b"stco", 0, 0, &stco_p)?
        }
    };

    let mut stbl_p = Vec::new();
    stbl_p.extend_from_slice(&sample_desc_box);
    stbl_p.extend_from_slice(&time_to_sample_box);
    stbl_p.extend_from_slice(&chunk_map_box);
    stbl_p.extend_from_slice(&size_table_box);
    stbl_p.extend_from_slice(&chunk_offset_box);
    box_wrap(*b"stbl", &stbl_p)
}

fn build_audio_moov(
    header: &AudioTrackHeader<'_>,
    samples: &[AudioSampleMeta],
    mdat_payload_offset: u64,
    offset_mode: ChunkOffsetMode,
) -> Result<Vec<u8>, MediaError> {
    let total_duration: u64 = samples
        .iter()
        .try_fold(0u64, |acc, sm| {
            acc.checked_add(u64::from(sm.duration_ticks))
        })
        .ok_or_else(|| MediaError::InvalidContainer("audio duration overflow".to_owned()))?;

    let movie_header = build_mvhd(header.timescale, total_duration)?;
    let track_header = build_audio_tkhd(total_duration)?;
    let media_header = build_mdhd(header.timescale, total_duration)?;

    let mut hdlr_p = Vec::with_capacity(33);
    hdlr_p.extend_from_slice(&0u32.to_be_bytes());
    hdlr_p.extend_from_slice(b"soun");
    hdlr_p.extend_from_slice(&[0u8; 12]);
    hdlr_p.extend_from_slice(b"SoundHandler\0");
    let hdlr = full_box_wrap(*b"hdlr", 0, 0, &hdlr_p)?;

    // Sound Media Header (`smhd`: version=0, flags=0, balance=0, reserved=0).
    let smhd = full_box_wrap(*b"smhd", 0, 0, &[0u8; 4])?;

    let url_box = full_box_wrap(*b"url ", 0, 1, &[])?;
    let mut dref_p = Vec::with_capacity(4 + url_box.len());
    dref_p.extend_from_slice(&1u32.to_be_bytes());
    dref_p.extend_from_slice(&url_box);
    let dref = full_box_wrap(*b"dref", 0, 0, &dref_p)?;
    let dinf = box_wrap(*b"dinf", &dref)?;

    let stbl = build_audio_stbl(header, samples, mdat_payload_offset, offset_mode)?;

    let mut minf_p = Vec::new();
    minf_p.extend_from_slice(&smhd);
    minf_p.extend_from_slice(&dinf);
    minf_p.extend_from_slice(&stbl);
    let minf = box_wrap(*b"minf", &minf_p)?;

    let mut mdia_p = Vec::new();
    mdia_p.extend_from_slice(&media_header);
    mdia_p.extend_from_slice(&hdlr);
    mdia_p.extend_from_slice(&minf);
    let mdia = box_wrap(*b"mdia", &mdia_p)?;

    // Strictly ZERO `edts` / `elst` boxes written inside `trak`.
    let mut trak_p = Vec::new();
    trak_p.extend_from_slice(&track_header);
    trak_p.extend_from_slice(&mdia);
    let trak = box_wrap(*b"trak", &trak_p)?;

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&movie_header);
    moov_p.extend_from_slice(&trak);
    if samples
        .first()
        .is_some_and(|sample| sample.duration_ticks == 0)
    {
        moov_p.extend_from_slice(&aac_gapless_metadata(total_duration)?);
    }
    box_wrap(*b"moov", &moov_p)
}

/// Signals the decoder-only AAC-LC priming packet and the final partial packet
/// to MP4 players. The media clock itself covers just the presented PCM frames.
pub(crate) fn aac_gapless_metadata(presented_samples: u64) -> Result<Vec<u8>, MediaError> {
    let padding = (1024 - presented_samples % 1024) % 1024;
    let smpb = format!(
        " 00000000 00000400 {padding:08X} {presented_samples:016X} 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000"
    );
    let mut item = full_box_wrap(*b"mean", 0, 0, b"com.apple.iTunes")?;
    item.extend_from_slice(&full_box_wrap(*b"name", 0, 0, b"iTunSMPB")?);
    let mut value = vec![0; 4]; // locale
    value.extend_from_slice(smpb.as_bytes());
    item.extend_from_slice(&full_box_wrap(*b"data", 0, 1, &value)?);
    let list = box_wrap(*b"ilst", &box_wrap(*b"----", &item)?)?;
    let mut handler = vec![0; 4];
    handler.extend_from_slice(b"mdirappl");
    handler.extend_from_slice(&[0; 9]);
    let mut meta = full_box_wrap(*b"hdlr", 0, 0, &handler)?;
    meta.extend_from_slice(&list);
    box_wrap(*b"udta", &full_box_wrap(*b"meta", 0, 0, &meta)?)
}

fn write_faststart_m4a(
    path: &Path,
    mode: Publish,
    header: &AudioTrackHeader<'_>,
    samples: &[AudioSampleMeta],
    mut fill: impl FnMut(usize, &mut Vec<u8>) -> Result<(), MediaError>,
) -> Result<(), MediaError> {
    let mut ftyp_payload = Vec::with_capacity(20);
    ftyp_payload.extend_from_slice(b"M4A ");
    ftyp_payload.extend_from_slice(&0u32.to_be_bytes());
    ftyp_payload.extend_from_slice(b"M4A mp42isom");
    let ftyp = box_wrap(*b"ftyp", &ftyp_payload)?;

    let total_mdat_payload: u64 = samples
        .iter()
        .try_fold(0u64, |acc, sm| acc.checked_add(u64::from(sm.size)))
        .ok_or_else(|| {
            MediaError::InvalidContainer("audio mdat payload size overflow".to_owned())
        })?;

    let use_large_mdat = total_mdat_payload.saturating_add(8) > u64::from(u32::MAX);
    let mdat_header_len = if use_large_mdat { 16u64 } else { 8u64 };

    let moov_probe = build_audio_moov(header, samples, 0, ChunkOffsetMode::Stco32)?;
    let ftyp_len_u64 = u64::try_from(ftyp.len()).unwrap_or(0);
    let probe_offset = ftyp_len_u64
        .saturating_add(u64::try_from(moov_probe.len()).unwrap_or(0))
        .saturating_add(mdat_header_len);
    let offset_mode = if probe_offset > u64::from(u32::MAX) {
        ChunkOffsetMode::Co64
    } else {
        ChunkOffsetMode::Stco32
    };

    let moov_pass1 = match offset_mode {
        ChunkOffsetMode::Co64 => build_audio_moov(header, samples, 0, ChunkOffsetMode::Co64)?,
        ChunkOffsetMode::Stco32 => moov_probe,
    };
    let mdat_payload_offset = ftyp_len_u64
        .saturating_add(u64::try_from(moov_pass1.len()).unwrap_or(0))
        .saturating_add(mdat_header_len);
    let moov = build_audio_moov(header, samples, mdat_payload_offset, offset_mode)?;

    write_atomically(path, mode, |writer| {
        writer.write_all(&ftyp)?;
        writer.write_all(&moov)?;
        if use_large_mdat {
            let mdat_total = total_mdat_payload.saturating_add(16);
            writer.write_all(&1u32.to_be_bytes())?;
            writer.write_all(b"mdat")?;
            writer.write_all(&mdat_total.to_be_bytes())?;
        } else {
            let mdat_total = u32::try_from(total_mdat_payload.saturating_add(8)).map_err(|_| {
                MediaError::InvalidContainer("audio mdat 32-bit size overflow".to_owned())
            })?;
            writer.write_all(&mdat_total.to_be_bytes())?;
            writer.write_all(b"mdat")?;
        }
        let mut buf = Vec::new();
        for (idx, meta) in samples.iter().enumerate() {
            fill(idx, &mut buf)?;
            if u64::try_from(buf.len()).ok() != Some(u64::from(meta.size)) {
                return Err(MediaError::InvalidContainer(format!(
                    "audio packet {idx} produced {} bytes but {} were indexed",
                    buf.len(),
                    meta.size
                )));
            }
            writer.write_all(&buf)?;
        }
        writer.flush()?;
        Ok(())
    })?;
    Ok(())
}

/// Validates that `AudioSpecificConfig` round-trips for standard rates and rejects malformed inputs.
#[cfg(test)]
mod tests {
    use super::{
        AAC_LC_OBJECT_TYPE, AAC_LC_SAMPLES_PER_FRAME, AAC_LC_SHORT_SAMPLES_PER_FRAME,
        MPEG4_SAMPLE_RATES, build_aac_lc_audio_specific_config, build_esds_box,
        build_mp4a_sample_entry, compute_packet_duration_ticks, parse_audio_specific_config,
        parse_esds, read_descriptor, strip_adts_header,
    };
    use crate::error::MediaError;

    #[test]
    fn build_and_parse_standard_audio_specific_configs() {
        let asc_48k_stereo =
            build_aac_lc_audio_specific_config(48_000, 2).expect("48 kHz stereo ASC");
        assert_eq!(
            asc_48k_stereo,
            [0x11, 0x90],
            "48 kHz stereo AAC-LC ASC must be [0x11, 0x90]"
        );
        let asc_44k_stereo =
            build_aac_lc_audio_specific_config(44_100, 2).expect("44.1 kHz stereo ASC");
        assert_eq!(
            asc_44k_stereo,
            [0x12, 0x10],
            "44.1 kHz stereo AAC-LC ASC must be [0x12, 0x10]"
        );

        for rate in MPEG4_SAMPLE_RATES {
            for channels in [1u16, 2, 3, 4, 5, 6, 8] {
                let raw = build_aac_lc_audio_specific_config(rate, channels)
                    .expect("valid rate/channel pair");
                let parsed = parse_audio_specific_config(&raw).expect("parse built ASC");
                assert_eq!(
                    parsed.audio_object_type, AAC_LC_OBJECT_TYPE,
                    "object type for {rate}/{channels}"
                );
                assert_eq!(
                    parsed.sample_rate_hz, rate,
                    "sample rate round-trip for {rate}/{channels}"
                );
                assert_eq!(
                    parsed.channel_count, channels,
                    "channel count round-trip for {rate}/{channels}"
                );
                assert_eq!(
                    parsed.samples_per_frame, AAC_LC_SAMPLES_PER_FRAME,
                    "standard 1024 frame length for {rate}/{channels}"
                );
                assert_eq!(
                    parsed.raw_bytes,
                    raw.to_vec(),
                    "raw bytes preserved for {rate}/{channels}"
                );
            }
        }
    }

    #[test]
    fn parse_audio_specific_config_handles_explicit_rate_short_frame_and_rejects_invalid() {
        // Explicit 24-bit sample rate = 48_000 (0x00BB80), AAC-LC (2), freq_idx=15 (0xF), 2ch, 960 samples:
        // bits: 00010 | 1111 | 00000000 10111011 10000000 | 0010 | 1 | 00
        // bytes: 00010111 10000000 01011101 11000000 00010100 = [0x17, 0x80, 0x5D, 0xC0, 0x14]
        let explicit = [0x17u8, 0x80, 0x5D, 0xC0, 0x14];
        let parsed = parse_audio_specific_config(&explicit).expect("parse explicit 24-bit ASC");
        assert_eq!(parsed.sample_rate_hz, 48_000, "explicit sample rate");
        assert_eq!(parsed.channel_count, 2, "explicit channel count");
        assert_eq!(
            parsed.samples_per_frame, AAC_LC_SHORT_SAMPLES_PER_FRAME,
            "frameLengthFlag=1 means 960 samples"
        );

        // Reject short, non-AAC-LC, reserved sample rate index, and invalid channel config.
        assert!(
            matches!(
                parse_audio_specific_config(&[0x11]),
                Err(MediaError::InvalidContainer(_))
            ),
            "1-byte ASC must fail"
        );
        // Object type 5 (SBR) instead of 2 (AAC-LC): 00101 | 0011 | 0010 | 000 -> [0x29, 0x90]
        assert!(
            matches!(
                parse_audio_specific_config(&[0x29, 0x90]),
                Err(MediaError::UnsupportedFeature(_))
            ),
            "non-AAC-LC object type must be rejected"
        );
        // Reserved freq index 13: 00010 | 1101 | 0010 | 000 -> [0x16, 0x90]
        assert!(
            matches!(
                parse_audio_specific_config(&[0x16, 0x90]),
                Err(MediaError::InvalidContainer(_))
            ),
            "reserved samplingFrequencyIndex 13 must fail"
        );
        // Channel config 0 (PCE): [0x11, 0x80]
        assert!(
            matches!(
                parse_audio_specific_config(&[0x11, 0x80]),
                Err(MediaError::InvalidContainer(_))
            ),
            "channelConfiguration 0 must fail"
        );
        // Unsupported build params
        assert!(
            matches!(
                build_aac_lc_audio_specific_config(44_000, 2),
                Err(MediaError::InvalidArgument(_))
            ),
            "non-standard sample rate must fail"
        );
        assert!(
            matches!(
                build_aac_lc_audio_specific_config(48_000, 7),
                Err(MediaError::InvalidArgument(_))
            ),
            "unsupported channel count 7 must fail"
        );
    }

    #[test]
    fn esds_round_trips_both_compact_and_four_byte_padded_lengths() {
        let asc = build_aac_lc_audio_specific_config(48_000, 2).expect("asc");
        let esds_box = build_esds_box(&asc, 128_000, 128_000).expect("build esds");
        // Strip 8-byte box header to pass FullBox body to `parse_esds`.
        let parsed = parse_esds(&esds_box[8..]).expect("parse compact esds");
        assert_eq!(parsed.sample_rate_hz, 48_000, "compact esds sample rate");
        assert_eq!(parsed.channel_count, 2, "compact esds channels");
        assert_eq!(parsed.raw_bytes, asc.to_vec(), "compact esds ASC bytes");

        // Standard ffmpeg-style 4-byte padded expandable lengths (`0x80 0x80 0x80 <len>`).
        let padded_esds_body: Vec<u8> = vec![
            0, 0, 0, 0, // FullBox version + flags
            0x03, 0x80, 0x80, 0x80, 0x22, // ES_Descriptor, len=34
            0x00, 0x01, 0x00, // ES_ID=1, flags=0
            0x04, 0x80, 0x80, 0x80, 0x14, // DecoderConfigDescriptor, len=20
            0x40, 0x15, 0x00, 0x18, 0x00, // MPEG-4 Audio, AudioStream, bufSize
            0x00, 0x01, 0xF4, 0x00, // maxBitrate 128000
            0x00, 0x01, 0xF4, 0x00, // avgBitrate 128000
            0x05, 0x80, 0x80, 0x80, 0x02, // DecoderSpecificInfo, len=2
            0x11, 0x90, // 48 kHz stereo AAC-LC
            0x06, 0x80, 0x80, 0x80, 0x01, 0x02, // SLConfigDescriptor predefined=2
        ];
        let parsed_padded = parse_esds(&padded_esds_body).expect("parse 4-byte padded esds");
        assert_eq!(
            parsed_padded.sample_rate_hz, 48_000,
            "padded esds sample rate"
        );
        assert_eq!(parsed_padded.channel_count, 2, "padded esds channels");
        assert_eq!(
            parsed_padded.raw_bytes,
            vec![0x11, 0x90],
            "padded esds ASC bytes"
        );

        // Reject > 4-byte descriptor length.
        let mut pos = 0usize;
        let bad_len = [0x05u8, 0x80, 0x80, 0x80, 0x80, 0x02, 0x11, 0x90];
        assert!(
            matches!(
                read_descriptor(&bad_len, &mut pos),
                Err(MediaError::InvalidContainer(_))
            ),
            "5-byte descriptor length must fail"
        );
    }

    #[test]
    fn mp4a_builder_rejects_mismatched_asc() {
        let asc = build_aac_lc_audio_specific_config(48_000, 2).expect("asc");
        let err = build_mp4a_sample_entry(44_100, 2, &asc, 128_000, 128_000);
        assert!(
            matches!(err, Err(MediaError::InvalidArgument(_))),
            "mismatched sample rate between mp4a and ASC must fail"
        );
    }

    #[test]
    fn adts_stripper_strips_valid_7_and_9_byte_headers_and_preserves_raw_packets() {
        let payload = [0x21u8, 0x10, 0x04, 0x60, 0x8C, 0x1C];
        let total_7 = 7 + payload.len();
        let mut adts7 = vec![
            0xFF,
            0xF1, // MPEG-4, layer 0, protection_absent = 1 (7-byte header)
            0x4C, // profile=1 (AAC-LC), freq_idx=3 (48kHz), private=0, ch_high=0
            0x80 | u8::try_from((total_7 >> 11) & 0x03).expect("len hi"),
            u8::try_from((total_7 >> 3) & 0xFF).expect("len mid"),
            (u8::try_from(total_7 & 0x07).expect("len lo") << 5) | 0x1F,
            0xFC, // buffer fullness + 0 extra raw blocks
        ];
        adts7.extend_from_slice(&payload);
        assert_eq!(
            strip_adts_header(&adts7),
            Some(payload.as_slice()),
            "valid 7-byte ADTS header must be stripped"
        );

        let total_9 = 9 + payload.len();
        let mut adts9 = vec![
            0xFF,
            0xF0, // protection_absent = 0 (9-byte header with 2-byte CRC)
            0x4C,
            0x80 | u8::try_from((total_9 >> 11) & 0x03).expect("len hi"),
            u8::try_from((total_9 >> 3) & 0xFF).expect("len mid"),
            (u8::try_from(total_9 & 0x07).expect("len lo") << 5) | 0x1F,
            0xFC,
            0xAB, // CRC high
            0xCD, // CRC low
        ];
        adts9.extend_from_slice(&payload);
        assert_eq!(
            strip_adts_header(&adts9),
            Some(payload.as_slice()),
            "valid 9-byte ADTS header with CRC must be stripped"
        );

        // Raw AAC payload that happens to start with 0xFF 0xF1 but wrong frame_len must NOT be stripped.
        let mut raw_coincidence = adts7.clone();
        raw_coincidence.push(0x99);
        assert_eq!(
            strip_adts_header(&raw_coincidence),
            None,
            "raw packet with mismatched ADTS frame_len must not be stripped"
        );
        assert_eq!(
            strip_adts_header(&payload),
            None,
            "plain raw AAC AU must not be stripped"
        );
    }

    #[test]
    fn packet_duration_ticks_handles_48khz_rounding_and_bounds() {
        assert_eq!(
            compute_packet_duration_ticks(0, Some(21_333), 48_000, 1024),
            1024,
            "21_333 us at 48 kHz rounds to 1024 ticks"
        );
        assert_eq!(
            compute_packet_duration_ticks(0, Some(21_334), 48_000, 1024),
            1024,
            "21_334 us at 48 kHz rounds to 1024 ticks"
        );
        assert_eq!(
            compute_packet_duration_ticks(100_000, Some(50_000), 48_000, 1024),
            1024,
            "non-monotonic delta falls back to 1024 ticks"
        );
        assert_eq!(
            compute_packet_duration_ticks(0, Some(i64::MAX), 48_000, 1024),
            48_000 * 60,
            "huge delta is clamped to 60s max cap"
        );
    }
}
