//! Frame-accurate integer `frame_index` video trimming and PNG frame export.
//!
//! - [`trim_video`] addresses frames strictly by zero-based `frame_index` (never rounded seconds),
//!   performs lossless stream copy when starting on an IDR keyframe, re-encodes only the head GOP
//!   remainder when starting mid-GOP, and outputs a faststart single-`stsd` `avc1` + in-band
//!   SPS/PPS MP4 with **zero `edts`/`elst` boxes** (0 pre-start frames).
//! - [`export_frame_png`] / [`export_frame_png_with_metadata`] decodes the exact `frame_index`
//!   and writes a standard PNG with `tEXt` provenance chunks.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::codec::{EncodeParams, PixelFormat, VideoCodecBackend};
use crate::error::MediaError;
use crate::mp4::{Mp4VideoTrack, MuxSample, MuxSegment, write_trim_mp4};
use crate::output::{Publish, ensure_output_is_not_input, write_atomically};

/// Summary returned after completing a frame-accurate [`trim_video`] operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimReport {
    /// Inclusive starting zero-based frame index from the input MP4.
    pub in_frame_index: u64,
    /// Inclusive ending zero-based frame index from the input MP4.
    pub out_frame_index: u64,
    /// Requested inclusive start for a time range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_time_ns: Option<u64>,
    /// Requested exclusive end for a time range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_time_ns: Option<u64>,
    /// Actual quantized duration for a time range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_duration_ns: Option<u64>,
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
/// The output is written to a temp sibling, `fsync`ed and published **without replacing** an
/// existing file; `output_mp4` resolving to `input_mp4` is refused. The stream-copied remainder
/// is streamed from the source file sample by sample (never held in memory), and every output
/// sample uses 4-byte NAL lengths.
///
/// # Errors
///
/// Returns:
/// - [`MediaError::InvalidFrameRange`] if `in_frame_index > out_frame_index` or `out_frame_index >= frame_count`.
/// - [`MediaError::UnsupportedFeature`] if the input track contains B-frame reordering (`has_b_frames == true`),
///   preventing silent frame corruption, or if `in_frame_index` precedes the first sync sample
///   (no IDR to start the output from).
/// - [`MediaError::InvalidArgument`] if `output_mp4` is the input file, or [`MediaError::Io`]
///   (`AlreadyExists`) if `output_mp4` already exists.
/// - [`MediaError::Codec`], [`MediaError::InvalidContainer`], or [`MediaError::Io`] on decode, encode,
///   or container I/O failure.
pub fn trim_video(
    input_mp4: impl AsRef<Path>,
    output_mp4: impl AsRef<Path>,
    in_frame_index: u64,
    out_frame_index: u64,
    backend: &dyn VideoCodecBackend,
) -> Result<TrimReport, MediaError> {
    let input_path = input_mp4.as_ref();
    let output_path = output_mp4.as_ref();
    ensure_output_is_not_input(input_path, output_path)?;
    let track = Mp4VideoTrack::open(input_path)?;
    trim_track(
        &track,
        output_path,
        in_frame_index,
        out_frame_index,
        backend,
        None,
    )
}

/// Writes the frames displayed in the half-open media time range, shortening the boundary samples.
///
/// # Errors
/// Rejects invalid ranges, reordered video and unsupported sample timing. Uses the same codec,
/// container and atomic publication checks as [`trim_video`].
pub fn trim_video_time_range(
    input_mp4: impl AsRef<Path>,
    output_mp4: impl AsRef<Path>,
    in_time_ns: u64,
    out_time_ns: u64,
    backend: &dyn VideoCodecBackend,
) -> Result<TrimReport, MediaError> {
    let output = output_mp4.as_ref();
    ensure_output_is_not_input(input_mp4.as_ref(), output)?;
    let track = Mp4VideoTrack::open(input_mp4)?;
    track.require_no_b_frames("trim_video_time_range")?;
    let frames = track.frames();
    let pts = crate::timeline::normalized_timestamps(frames.iter().map(|frame| frame.pts_ns));
    // Accept both the viewport's end (truncated PTS plus truncated duration) and the end
    // converted once from PTS ticks; either can exceed the other by one ns.
    let tail = frames
        .last()
        .filter(|f| f.duration_ticks > 0)
        .map(|f| i64::try_from(f.duration_ns.max(1)).unwrap_or(i64::MAX));
    let mut end = crate::timeline::end_ns(&pts, tail, 1);
    if let (Some(first), Some(last)) = (
        frames.first(),
        frames.last().filter(|f| f.duration_ticks > 0),
    ) {
        let ticks = last
            .pts_ticks
            .checked_add(i64::from(last.duration_ticks))
            .ok_or_else(|| MediaError::InvalidContainer("track duration overflow".into()))?;
        let exact =
            crate::mp4::ticks_to_ns_signed(ticks, track.timescale()).saturating_sub(first.pts_ns);
        end = end.max(exact);
    }
    let end = end.unsigned_abs();
    let pts: Vec<_> = pts.into_iter().map(i64::unsigned_abs).collect();
    let invalid = |msg: &str| MediaError::InvalidArgument(msg.to_owned());
    let plan = crate::timeline::plan_time_range_trim(&pts, end, in_time_ns, out_time_ns)
        .map_err(invalid)?;
    // A cut straddling a frame boundary can be shorter than two source ticks.
    // Raise precision only when the source clock cannot give each frame one tick.
    let span = u128::from(out_time_ns - in_time_ns);
    let count =
        u128::try_from(plan.durations_ns.len()).map_err(|_| invalid("sample count overflow"))?;
    let mut scale = track.timescale();
    if count > 1 && (span * u128::from(scale) + 500_000_000) / 1_000_000_000 < count {
        scale = u32::try_from((count * 1_000_000_000).div_ceil(span))
            .map_err(|_| invalid("timescale overflow"))?;
    }
    let ticks = crate::timeline::quantize_durations_ns_to_ticks(&plan.durations_ns, scale)
        .map_err(invalid)?;
    // Clocks finer than 1 ns can truncate a positive length to zero.
    let duration =
        crate::mp4::ticks_to_ns_unsigned(ticks.iter().map(|&n| u64::from(n)).sum(), scale).max(1);
    let mut report = trim_track(
        &track,
        output,
        u64::try_from(plan.in_frame_index).map_err(|_| invalid("frame index overflow"))?,
        u64::try_from(plan.out_frame_index).map_err(|_| invalid("frame index overflow"))?,
        backend,
        Some((scale, &ticks)),
    )?;
    report.in_time_ns = Some(in_time_ns);
    report.out_time_ns = Some(out_time_ns);
    report.output_duration_ns = Some(duration);
    Ok(report)
}

fn trim_track(
    track: &Mp4VideoTrack,
    output_path: &Path,
    in_frame_index: u64,
    out_frame_index: u64,
    backend: &dyn VideoCodecBackend,
    timing: Option<(u32, &[u32])>,
) -> Result<TrimReport, MediaError> {
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

    let (_, next_gop_opt) = track.gop_bounds(in_frame_index)?;
    // A stream-copy (or head decode) must start on a real sync sample (R2 S4).
    let gop_start = track.decodable_gop_start(in_frame_index)?;
    let splice_point = match next_gop_opt {
        Some(next_kf) if next_kf <= out_frame_index => next_kf,
        _ => out_frame_index + 1,
    };

    let mut head_segment: Option<MuxSegment> = None;
    let mut head_decoded_frames = 0u64;
    let mut reencoded_frames = 0u64;

    if in_frame_index > gop_start {
        let encoded_head = backend.reencode_frame_range(
            track,
            in_frame_index,
            splice_point,
            EncodeParams::default(),
        )?;
        head_decoded_frames = encoded_head.decoded_frames;
        reencoded_frames = u64::try_from(encoded_head.samples.len()).unwrap_or(0);
        if reencoded_frames != splice_point - in_frame_index {
            return Err(MediaError::InvalidContainer(
                "trim encoder returned an unexpected sample count".into(),
            ));
        }
        let head_samples = encoded_head
            .samples
            .into_iter()
            .map(|sm| MuxSample {
                data: sm.avcc_data,
                duration_ticks: sm.duration_ticks,
                is_keyframe: sm.is_keyframe,
            })
            .collect();
        head_segment = Some(MuxSegment {
            avcc_payload: encoded_head.avcc_payload,
            samples: head_samples,
        });
    }

    let copy_start = if in_frame_index == gop_start {
        in_frame_index
    } else {
        splice_point
    };

    let copy_range = (copy_start <= out_frame_index).then_some((copy_start, out_frame_index));
    let stream_copied_frames = write_trim_mp4(
        output_path,
        track,
        head_segment.as_ref(),
        copy_range,
        timing,
    )?;

    let output_frame_count = reencoded_frames + stream_copied_frames;
    if output_frame_count != out_frame_index - in_frame_index + 1 {
        return Err(MediaError::InvalidContainer(
            "trim output frame count mismatch".into(),
        ));
    }
    Ok(TrimReport {
        in_time_ns: None,
        out_time_ns: None,
        output_duration_ns: None,
        in_frame_index,
        out_frame_index,
        output_frame_count,
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
/// [`MediaError::Io`] on failure; [`MediaError::UnsupportedFeature`] for B-frame tracks;
/// [`MediaError::InvalidArgument`] if `output_png` is the input file; and `Io(AlreadyExists)`
/// if `output_png` already exists (the PNG is published crash-safely, never replacing a file).
pub fn export_frame_png_with_metadata(
    input_mp4: impl AsRef<Path>,
    frame_index: u64,
    output_png: impl AsRef<Path>,
    metadata: &PngExportMetadata,
    backend: &dyn VideoCodecBackend,
) -> Result<PngExportReport, MediaError> {
    let input_path = input_mp4.as_ref();
    let out_path = output_png.as_ref();
    ensure_output_is_not_input(input_path, out_path)?;
    let track = Mp4VideoTrack::open(input_path)?;
    track.require_no_b_frames("export_frame_png")?;
    let decoded = backend.decode_frame(&track, frame_index, PixelFormat::Rgb24)?;
    let rgb = decoded.to_rgb24()?;

    let output_bytes = write_atomically(out_path, Publish::NoReplace, |writer| {
        write_png(writer, &rgb, frame_index, metadata)
    })?;
    Ok(PngExportReport {
        frame_index,
        width: rgb.width,
        height: rgb.height,
        backend_name: backend.name().to_owned(),
        output_bytes,
    })
}

fn write_png(
    writer: &mut impl std::io::Write,
    rgb: &crate::codec::RgbImage,
    frame_index: u64,
    metadata: &PngExportMetadata,
) -> Result<(), MediaError> {
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
    Ok(())
}
