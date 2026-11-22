//! Frame-accurate integer `frame_index` video trimming and PNG frame export.
//!
//! - [`trim_video`] addresses frames strictly by zero-based `frame_index` (never rounded seconds),
//!   performs lossless stream copy when starting on an IDR keyframe, re-encodes only the head GOP
//!   remainder when starting mid-GOP, and outputs a faststart single-`stsd` `avc1` + in-band
//!   SPS/PPS MP4 with **zero `edts`/`elst` boxes** (0 pre-start frames).
//! - [`export_frame_png`] / [`export_frame_png_with_metadata`] decodes the exact `frame_index`
//!   and writes a standard PNG with `tEXt` provenance chunks.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::codec::{EncodeParams, PixelFormat, VideoCodecBackend};
use crate::error::MediaError;
use crate::mp4::{Mp4VideoTrack, MuxSample, MuxSegment, write_single_stsd_avc1_mp4};

/// Summary returned after completing a frame-accurate [`trim_video`] operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimReport {
    /// Inclusive starting zero-based frame index from the input MP4.
    pub in_frame_index: u64,
    /// Inclusive ending zero-based frame index from the input MP4.
    pub out_frame_index: u64,
    /// Total frames written to the output MP4 (`out_frame_index - in_frame_index + 1`).
    pub output_frame_count: u64,
    /// Number of input frames decoded from the head GOP IDR to reconstruct the trim boundary.
    pub head_decoded_frames: u64,
    /// Number of frames re-encoded (0 for lossless IDR-start trim).
    pub reencoded_frames: u64,
    /// Number of frames stream-copied losslessly without decode/re-encode.
    pub stream_copied_frames: u64,
    /// Name of the [`VideoCodecBackend`] used for any head-GOP re-encoding.
    pub backend_name: String,
    /// Number of `edts` boxes written to the output MP4 (always `0`).
    pub edts_box_count: usize,
    /// Number of `elst` boxes written to the output MP4 (always `0`).
    pub elst_box_count: usize,
}

/// Trims `input_mp4` to the exact inclusive integer frame range `[in_frame_index..=out_frame_index]`
/// and writes a new faststart zero-`elst` MP4 to `output_mp4` without modifying `input_mp4`.
///
/// # Errors
///
/// Returns:
/// - [`MediaError::InvalidFrameRange`] if `in_frame_index > out_frame_index` or `out_frame_index >= frame_count`.
/// - [`MediaError::UnsupportedFeature`] if the input track contains B-frame reordering (`has_b_frames == true`),
///   preventing silent frame corruption.
/// - [`MediaError::Codec`], [`MediaError::InvalidContainer`], or [`MediaError::Io`] on decode, encode,
///   or container I/O failure.
pub fn trim_video(
    input_mp4: impl AsRef<Path>,
    output_mp4: impl AsRef<Path>,
    in_frame_index: u64,
    out_frame_index: u64,
    backend: &dyn VideoCodecBackend,
) -> Result<TrimReport, MediaError> {
    let track = Mp4VideoTrack::open(input_mp4)?;
    let frame_count = track.frame_count();
    if frame_count == 0 || in_frame_index > out_frame_index || out_frame_index >= frame_count {
        return Err(MediaError::InvalidFrameRange {
            in_frame_index,
            out_frame_index,
            frame_count,
        });
    }

    if track.has_b_frames() {
        return Err(MediaError::UnsupportedFeature(
            "input track contains B-frame (ctts) reordering; explicit B-frame tail re-encoding is required and stream-copy trim is refused to prevent frame corruption"
                .to_owned(),
        ));
    }

    let (gop_start, next_gop_opt) = track.gop_bounds(in_frame_index)?;
    let splice_point = match next_gop_opt {
        Some(next_kf) if next_kf <= out_frame_index => next_kf,
        _ => out_frame_index + 1,
    };

    let mut segments: Vec<MuxSegment> = Vec::with_capacity(2);
    let mut head_decoded_frames = 0u64;
    let mut reencoded_frames = 0u64;

    if in_frame_index > gop_start {
        let encoded_head = backend.reencode_frame_range(
            &track,
            in_frame_index,
            splice_point,
            EncodeParams::default(),
        )?;
        head_decoded_frames = encoded_head.decoded_frames;
        reencoded_frames = u64::try_from(encoded_head.samples.len()).unwrap_or(0);
        let head_samples = encoded_head
            .samples
            .into_iter()
            .map(|sm| MuxSample {
                data: sm.avcc_data,
                duration_ticks: sm.duration_ticks,
                is_keyframe: sm.is_keyframe,
            })
            .collect();
        segments.push(MuxSegment {
            avcc_payload: encoded_head.avcc_payload,
            samples: head_samples,
        });
    }

    let copy_start = if in_frame_index == gop_start {
        in_frame_index
    } else {
        splice_point
    };

    let stream_copied_frames = if copy_start <= out_frame_index {
        let copy_count = out_frame_index - copy_start + 1;
        let copy_cap = usize::try_from(copy_count).unwrap_or(0);
        let mut copy_samples = Vec::with_capacity(copy_cap);
        for idx in copy_start..=out_frame_index {
            let mut data = Vec::new();
            track.read_sample_into(idx, &mut data)?;
            let info = track.frame(idx)?;
            copy_samples.push(MuxSample {
                data,
                duration_ticks: info.duration_ticks,
                is_keyframe: info.is_keyframe,
            });
        }
        segments.push(MuxSegment {
            avcc_payload: track.avcc().raw_avcc.clone(),
            samples: copy_samples,
        });
        copy_count
    } else {
        0
    };

    write_single_stsd_avc1_mp4(
        output_mp4,
        track.width(),
        track.height(),
        track.timescale(),
        &segments,
    )?;

    Ok(TrimReport {
        in_frame_index,
        out_frame_index,
        output_frame_count: reencoded_frames + stream_copied_frames,
        head_decoded_frames,
        reencoded_frames,
        stream_copied_frames,
        backend_name: backend.name().to_owned(),
        edts_box_count: 0,
        elst_box_count: 0,
    })
}

/// Optional provenance metadata embedded as PNG `tEXt` chunks by [`export_frame_png_with_metadata`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PngExportMetadata {
    /// Device boottime timestamp in nanoseconds (`arb:boottime_ns`).
    pub boottime_ns: Option<i64>,
    /// Source immutable bundle identifier (`arb:source_bundle_id`).
    pub source_bundle_id: Option<String>,
}

/// Summary returned after exporting a single decoded frame as PNG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PngExportReport {
    /// Exported zero-based frame index.
    pub frame_index: u64,
    /// PNG width in pixels.
    pub width: u32,
    /// PNG height in pixels.
    pub height: u32,
    /// Name of the [`VideoCodecBackend`] used to decode the frame.
    pub backend_name: String,
    /// Size of the written PNG file in bytes.
    pub output_bytes: u64,
}

/// Decodes exact `frame_index` from `input_mp4` and writes an RGB24 PNG with an `arb:frame_index`
/// `tEXt` metadata chunk to `output_png`.
///
/// # Errors
///
/// Returns [`MediaError::FrameOutOfRange`], [`MediaError::Codec`], [`MediaError::Png`], or
/// [`MediaError::Io`] on failure.
pub fn export_frame_png(
    input_mp4: impl AsRef<Path>,
    frame_index: u64,
    output_png: impl AsRef<Path>,
    backend: &dyn VideoCodecBackend,
) -> Result<PngExportReport, MediaError> {
    export_frame_png_with_metadata(
        input_mp4,
        frame_index,
        output_png,
        &PngExportMetadata::default(),
        backend,
    )
}

/// Decodes exact `frame_index` from `input_mp4` and writes an RGB24 PNG with `arb:frame_index`
/// and optional `arb:boottime_ns` / `arb:source_bundle_id` `tEXt` metadata chunks.
///
/// # Errors
///
/// Returns [`MediaError::FrameOutOfRange`], [`MediaError::Codec`], [`MediaError::Png`], or
/// [`MediaError::Io`] on failure.
pub fn export_frame_png_with_metadata(
    input_mp4: impl AsRef<Path>,
    frame_index: u64,
    output_png: impl AsRef<Path>,
    metadata: &PngExportMetadata,
    backend: &dyn VideoCodecBackend,
) -> Result<PngExportReport, MediaError> {
    let track = Mp4VideoTrack::open(input_mp4)?;
    let decoded = backend.decode_frame(&track, frame_index, PixelFormat::Rgb24)?;
    let rgb = decoded.to_rgb24()?;

    let out_path = output_png.as_ref();
    let file = File::create(out_path)?;
    let writer = BufWriter::new(file);

    let mut encoder = png::Encoder::new(writer, rgb.width, rgb.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .add_text_chunk("arb:frame_index".to_owned(), frame_index.to_string())
        .map_err(|err| MediaError::Png(format!("failed to attach arb:frame_index: {err}")))?;
    if let Some(boottime_ns) = metadata.boottime_ns {
        encoder
            .add_text_chunk("arb:boottime_ns".to_owned(), boottime_ns.to_string())
            .map_err(|err| MediaError::Png(format!("failed to attach arb:boottime_ns: {err}")))?;
    }
    if let Some(bundle_id) = &metadata.source_bundle_id {
        encoder
            .add_text_chunk("arb:source_bundle_id".to_owned(), bundle_id.clone())
            .map_err(|err| {
                MediaError::Png(format!("failed to attach arb:source_bundle_id: {err}"))
            })?;
    }

    let mut png_writer = encoder
        .write_header()
        .map_err(|err| MediaError::Png(format!("failed to write PNG header: {err}")))?;
    png_writer
        .write_image_data(&rgb.data)
        .map_err(|err| MediaError::Png(format!("failed to write PNG image data: {err}")))?;
    png_writer
        .finish()
        .map_err(|err| MediaError::Png(format!("failed to finalize PNG stream: {err}")))?;

    let output_bytes = std::fs::metadata(out_path)?.len();
    Ok(PngExportReport {
        frame_index,
        width: rgb.width,
        height: rgb.height,
        backend_name: backend.name().to_owned(),
        output_bytes,
    })
}
