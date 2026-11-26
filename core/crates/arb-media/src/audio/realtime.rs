//! PCM-sample aligned AAC export on the video selection clock.
use std::path::Path;

use rusty_aac::{AacDecoder, AacEncoder, AacEncoderConfig};

use super::{
    AudioSampleMeta, AudioTrackHeader, Mp4AudioTrack, build_aac_lc_audio_specific_config,
    compute_bitrates, write_faststart_m4a,
};
use crate::error::MediaError;
use crate::mp4::{MAX_TRACK_SAMPLES, sample_size_u32};
use crate::output::{Publish, ensure_output_is_not_input};

/// Selects PCM samples in a half-open range on the audio clock, padding an actual overlap
/// with silence where video leads or outlasts audio. The output starts at video IN.
///
/// # Errors
/// Returns `NoAudioInRange` when no samples overlap, or a codec, container or I/O error.
pub fn trim_audio_time_range(
    input: &Path,
    output: &Path,
    start_ns: i64,
    end_ns: i64,
) -> Result<(), MediaError> {
    ensure_output_is_not_input(input, output)?;
    let track = Mp4AudioTrack::open(input)?;
    let rate = track.sample_rate_hz();
    let channels = track.channels();
    let sample_at = |ns: i64| {
        // First PCM sample whose presentation time is >= ns, also when video leads audio.
        let n = i128::from(ns) * i128::from(rate);
        i64::try_from(-(-n).div_euclid(1_000_000_000)).unwrap_or(i64::MAX)
    };
    let start = sample_at(start_ns);
    let end = sample_at(end_ns);
    let absent = || MediaError::NoAudioInRange { start_ns, end_ns };
    if end <= start
        || end <= 0
        || start_ns >= i64::try_from(track.duration_ns()).unwrap_or(i64::MAX)
    {
        return Err(absent());
    }
    let length = u64::try_from(end - start).map_err(|_| absent())?;
    if length.div_ceil(1024) >= u64::try_from(MAX_TRACK_SAMPLES).unwrap_or(u64::MAX) {
        return Err(MediaError::InvalidArgument(
            "selection exceeds audio sample limit".into(),
        ));
    }
    let first = track.packets().partition_point(|p| {
        p.pts_ns
            .saturating_add(i64::try_from(p.duration_ns).unwrap_or(i64::MAX))
            < start_ns
    });
    let mut decoder = AacDecoder::with_config_bytes(&track.audio_specific_config().raw_bytes)
        .map_err(|e| MediaError::Codec(e.to_string()))?;
    let mut encoder = AacEncoder::new(AacEncoderConfig::default());
    encoder.set_streaming(true);
    let mut packet = Vec::new();
    let mut cursor = start;
    let mut has_audio = false;
    // One preceding AU primes the AAC-LC overlap/add window.
    for p in &track.packets()[first.saturating_sub(1)..] {
        let at = sample_at(p.pts_ns);
        if at >= end {
            break;
        }
        track.read_packet_into(p.index, &mut packet)?;
        let frame = decoder
            .decode(&packet, None)
            .map_err(|e| MediaError::Codec(e.to_string()))?;
        if frame.channels != channels || frame.sample_rate != rate {
            return Err(MediaError::UnsupportedFeature(
                "changing audio format during trim".into(),
            ));
        }
        let left = at.max(cursor);
        let samples =
            i64::try_from(frame.samples.len() / usize::from(channels)).unwrap_or(i64::MAX);
        let right = end
            .min(at.saturating_add(samples))
            .min(sample_at(p.pts_ns.saturating_add(
                i64::try_from(p.duration_ns).unwrap_or(i64::MAX),
            )));
        if right <= left {
            continue;
        }
        silence(&mut encoder, left - cursor, channels, rate)?;
        let offset = |s| usize::try_from(s - at).unwrap_or(0) * usize::from(channels);
        encoder
            .push_pcm(&frame.samples[offset(left)..offset(right)], channels, rate)
            .map_err(|e| MediaError::Codec(e.to_string()))?;
        cursor = right;
        has_audio = true;
    }
    if !has_audio {
        return Err(absent());
    }
    silence(&mut encoder, end - cursor, channels, rate)?;
    encoder.finish();
    write_selection(output, &mut encoder, length, rate, channels)
}

fn silence(
    encoder: &mut AacEncoder,
    mut frames: i64,
    channels: u16,
    rate: u32,
) -> Result<(), MediaError> {
    if frames > 0 {
        let zeros = vec![0.0; 1024 * usize::from(channels)];
        while frames > 0 {
            let count = usize::try_from(frames.min(1024)).unwrap_or(0);
            encoder
                .push_pcm(&zeros[..count * usize::from(channels)], channels, rate)
                .map_err(|e| MediaError::Codec(e.to_string()))?;
            frames -= i64::try_from(count).unwrap_or(0);
        }
    }
    Ok(())
}

fn write_selection(
    output: &Path,
    encoder: &mut AacEncoder,
    mut remaining: u64,
    rate: u32,
    channels: u16,
) -> Result<(), MediaError> {
    let mut packets = Vec::new();
    let mut metas = Vec::new();
    let priming = i64::from(encoder.priming_samples());
    while remaining > 0 {
        let packet = encoder
            .next_packet()
            .map_err(|e| MediaError::Codec(e.to_string()))?;
        // A decoder must consume the overlap-add priming AU before presenting IN.
        // Its zero presentation span keeps the first audible PCM sample at video time zero.
        let duration_ticks = if packet.pts < priming {
            0
        } else {
            packet
                .duration
                .min(u32::try_from(remaining).unwrap_or(u32::MAX))
        };
        metas.push(AudioSampleMeta {
            size: sample_size_u32(packet.data.len())?,
            duration_ticks,
        });
        packets.push(packet.data);
        remaining -= u64::from(duration_ticks);
    }
    let (max_bitrate, avg_bitrate) = compute_bitrates(&metas, rate);
    let asc = build_aac_lc_audio_specific_config(rate, channels)?;
    let header = AudioTrackHeader {
        timescale: rate,
        sample_rate_hz: rate,
        channels,
        audio_specific_config: &asc,
        max_bitrate,
        avg_bitrate,
    };
    write_faststart_m4a(output, Publish::NoReplace, &header, &metas, |idx, buf| {
        buf.clear();
        buf.extend_from_slice(&packets[idx]);
        Ok(())
    })?;
    Ok(())
}
