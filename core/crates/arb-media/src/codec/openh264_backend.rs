//! Universal cross-OS H.264 codec backend powered by Cisco `OpenH264` (`BSD-2-Clause`).
//!
//! Explicitly compensates for `OpenH264`'s 1-frame internal DPB delay (discovered in Spike S1)
//! using `Flush::NoFlush` during GOP playback followed by `flush_remaining()`, ensuring frame
//! indices are never off-by-1.

use openh264::OpenH264API;
use openh264::decoder::{DecodeOptions, DecodedYUV, Decoder, DecoderConfig, Flush};
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, RateControlMode,
    SpsPpsStrategy, UsageType,
};
use openh264::formats::{YUVSlices, YUVSource as _};

use crate::codec::{
    DecodedFrame, EncodeParams, EncodedGop, EncodedSample, MutableYuvFrame, PixelFormat, RgbImage,
    VideoCodecBackend, convert_from_yuv420p, copy_plane_rows, fit_thumbnail_dimensions,
    split_yuv420p_planes, yuv420p_to_thumb_rgb24,
};
use crate::error::MediaError;
use crate::mp4::{
    Mp4VideoTrack, ParamSetMode, RawH264Packet, annexb_to_avcc_sample, avcc_sample_to_annexb,
    build_avcc_payload,
};

/// Universal GPL-free (`BSD-2-Clause`) H.264 codec backend using `openh264`.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenH264Backend;

impl OpenH264Backend {
    /// Creates a new [`OpenH264Backend`].
    pub const fn new() -> Self {
        Self
    }

    /// Encodes a sequence of `Yuv420p` frames into Annex B [`RawH264Packet`]s with deterministic
    /// IDR keyframes every `keyframe_interval` frames.
    ///
    /// Useful for pure-Rust H.264 fixture generation and software encoding paths.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidArgument`] if `frames` is empty, `keyframe_interval == 0`, or
    /// any frame is not `PixelFormat::Yuv420p`, or [`MediaError::Codec`] if encoding fails.
    pub fn encode_yuv420p_sequence(
        &self,
        frames: &[DecodedFrame],
        keyframe_interval: u32,
        frame_duration_ns: i64,
        params: EncodeParams,
    ) -> Result<Vec<RawH264Packet>, MediaError> {
        if frames.is_empty() || keyframe_interval == 0 || frame_duration_ns <= 0 {
            return Err(MediaError::InvalidArgument(
                "frames must be non-empty and keyframe_interval/frame_duration_ns must be positive"
                    .to_owned(),
            ));
        }

        let mut enc = make_encoder(params)?;
        let mut packets = Vec::with_capacity(frames.len());
        let mut bs_buf = Vec::new();
        let interval = usize::try_from(keyframe_interval).unwrap_or(1);

        for (idx, frame) in frames.iter().enumerate() {
            if frame.format != PixelFormat::Yuv420p {
                return Err(MediaError::InvalidArgument(
                    "encode_yuv420p_sequence requires PixelFormat::Yuv420p frames".to_owned(),
                ));
            }
            let width = usize::try_from(frame.width)
                .map_err(|_| MediaError::Codec("frame width exceeds usize".to_owned()))?;
            let height = usize::try_from(frame.height)
                .map_err(|_| MediaError::Codec("frame height exceeds usize".to_owned()))?;
            require_even_encode_dims(width, height)?;
            let planes = split_yuv420p_planes(&frame.data, width, height)?;

            let force_idr = idx % interval == 0;
            if force_idr {
                enc.force_intra_frame();
            }

            let slices = YUVSlices::new(
                (planes.y, planes.u, planes.v),
                (width, height),
                (width, width / 2, width / 2),
            );
            let bs = enc.encode(&slices).map_err(|err| {
                MediaError::Codec(format!("openh264 encode frame {idx}: {err:?}"))
            })?;
            if matches!(bs.frame_type(), FrameType::Skip) {
                return Err(MediaError::Codec(format!(
                    "openh264 unexpectedly skipped frame {idx}"
                )));
            }
            let is_idr = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
            if force_idr && !is_idr {
                return Err(MediaError::Codec(format!(
                    "expected IDR at frame {idx}, got {:?}",
                    bs.frame_type()
                )));
            }

            bs_buf.clear();
            bs.write_vec(&mut bs_buf);
            let idx_i64 = i64::try_from(idx).unwrap_or(0);
            packets.push(RawH264Packet {
                annex_b: bs_buf.clone(),
                pts_ns: idx_i64.saturating_mul(frame_duration_ns),
                is_keyframe: is_idr,
            });
        }

        Ok(packets)
    }
}

/// `OpenH264` encodes only even 4:2:0 picture sizes (its `YUVSlices` asserts on odd ones);
/// refuse cleanly instead of panicking.
fn require_even_encode_dims(width: usize, height: usize) -> Result<(), MediaError> {
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) || width == 0 || height == 0 {
        return Err(MediaError::UnsupportedFeature(format!(
            "openh264 cannot encode odd/zero picture dimensions {width}x{height}"
        )));
    }
    Ok(())
}

fn make_decoder(flush: Flush) -> Result<Decoder, MediaError> {
    let cfg = DecoderConfig::new().flush_after_decode(flush);
    Decoder::with_api_config(OpenH264API::from_source(), cfg)
        .map_err(|err| MediaError::Codec(format!("openh264 decoder init failed: {err:?}")))
}

fn make_encoder(params: EncodeParams) -> Result<Encoder, MediaError> {
    let fps_f32 = f32::from(u16::try_from(params.max_fps.clamp(1, 240)).unwrap_or(30));
    let enc_cfg = EncoderConfig::new()
        .bitrate(BitRate::from_bps(params.bitrate_bps))
        .max_frame_rate(FrameRate::from_hz(fps_f32))
        .rate_control_mode(RateControlMode::Off)
        .usage_type(UsageType::ScreenContentRealTime)
        .complexity(Complexity::Low)
        .adaptive_quantization(false)
        .background_detection(false)
        .skip_frames(false)
        .sps_pps_strategy(SpsPpsStrategy::ConstantId)
        .num_threads(0);
    Encoder::with_api_config(OpenH264API::from_source(), enc_cfg)
        .map_err(|err| MediaError::Codec(format!("openh264 encoder init failed: {err:?}")))
}

#[cfg(test)]
thread_local! { static PACKED_OUTPUTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

fn pack_decoded_yuv420p(yuv: &DecodedYUV<'_>) -> Result<(u32, u32, Vec<u8>), MediaError> {
    #[cfg(test)]
    PACKED_OUTPUTS.with(|count| count.set(count.get() + 1));
    let (w, h) = yuv.dimensions();
    let (ys, us, vs) = yuv.strides();
    let y_src = yuv.y();
    let u_src = yuv.u();
    let v_src = yuv.v();
    let y_len = w.saturating_mul(h);
    // ceil(w/2) x ceil(h/2) chroma for odd (cropped) dimensions; short source planes are
    // padded instead of panicking (R2 S3).
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let uv_len = cw.saturating_mul(ch);
    let mut data = vec![0u8; y_len + uv_len * 2];

    copy_plane_rows(&mut data[..y_len], y_src, ys, w, h, 16);
    let u_off = y_len;
    let v_off = y_len + uv_len;
    copy_plane_rows(&mut data[u_off..v_off], u_src, us, cw, ch, 128);
    copy_plane_rows(&mut data[v_off..], v_src, vs, cw, ch, 128);

    let w_u32 = u32::try_from(w)
        .map_err(|_| MediaError::Codec("decoded width overflows u32".to_owned()))?;
    let h_u32 = u32::try_from(h)
        .map_err(|_| MediaError::Codec("decoded height overflows u32".to_owned()))?;
    Ok((w_u32, h_u32, data))
}

struct DecodedYuvFrame {
    frame_index: u64,
    width: u32,
    height: u32,
    yuv420p: Vec<u8>,
}

fn decode_range_yuv420p(
    track: &Mp4VideoTrack,
    gop_start: u64,
    end_inclusive: u64,
) -> Result<Vec<DecodedYuvFrame>, MediaError> {
    let mut frames = Vec::new();
    visit_range_yuv420p(track, gop_start, end_inclusive, gop_start, |frame| {
        frames.push(frame);
        Ok(())
    })?;
    Ok(frames)
}

fn visit_range_yuv420p(
    track: &Mp4VideoTrack,
    gop_start: u64,
    end_inclusive: u64,
    first_output: u64,
    mut visit: impl FnMut(DecodedYuvFrame) -> Result<(), MediaError>,
) -> Result<(), MediaError> {
    let mut dec = make_decoder(Flush::NoFlush)?;
    let mut raw = Vec::new();
    let mut annexb = Vec::new();
    let mut emitted_idx = gop_start;

    for idx in gop_start..=end_inclusive {
        track.read_sample_into(idx, &mut raw)?;
        let mode = if idx == gop_start || track.frame(idx)?.is_keyframe {
            ParamSetMode::PrependIfMissing
        } else {
            ParamSetMode::Omit
        };
        avcc_sample_to_annexb(&raw, track.avcc(), mode, &mut annexb);
        let decoded = dec
            .decode_with_options(
                &annexb,
                DecodeOptions::new().flush_after_decode(Flush::NoFlush),
            )
            .map_err(|err| MediaError::Codec(format!("openh264 decode frame {idx}: {err:?}")))?;
        if let Some(yuv) = decoded {
            if emitted_idx >= first_output {
                let (width, height, yuv420p) = pack_decoded_yuv420p(&yuv)?;
                visit(DecodedYuvFrame {
                    frame_index: emitted_idx,
                    width,
                    height,
                    yuv420p,
                })?;
            }
            emitted_idx += 1;
        }
    }

    // Drain OpenH264's 1-frame internal DPB delay so the tail frame is never dropped or off-by-1.
    let remaining = dec
        .flush_remaining()
        .map_err(|err| MediaError::Codec(format!("openh264 flush_remaining: {err:?}")))?;
    for yuv in remaining {
        if emitted_idx >= first_output {
            let (width, height, yuv420p) = pack_decoded_yuv420p(&yuv)?;
            visit(DecodedYuvFrame {
                frame_index: emitted_idx,
                width,
                height,
                yuv420p,
            })?;
        }
        emitted_idx += 1;
    }

    Ok(())
}

fn decode_keyframe_thumb_chunk(
    track: &Mp4VideoTrack,
    chunk: &[u64],
    thumb_width: u32,
    thumb_height: u32,
    dst_w: usize,
    dst_h: usize,
) -> Result<Vec<(u64, RgbImage)>, MediaError> {
    let mut dec = make_decoder(Flush::Flush)?;
    let mut raw = Vec::new();
    let mut annexb = Vec::new();
    let mut local = Vec::with_capacity(chunk.len());

    for &kf_idx in chunk {
        track.read_sample_into(kf_idx, &mut raw)?;
        avcc_sample_to_annexb(
            &raw,
            track.avcc(),
            ParamSetMode::PrependIfMissing,
            &mut annexb,
        );
        let direct = dec
            .decode(&annexb)
            .map_err(|err| MediaError::Codec(format!("openh264 decode kf {kf_idx}: {err:?}")))?;
        let rgb_bytes = if let Some(yuv) = direct {
            let (src_w, src_h) = yuv.dimensions();
            let (ys, us, vs) = yuv.strides();
            yuv420p_to_thumb_rgb24(
                yuv.y(),
                yuv.u(),
                yuv.v(),
                ys,
                us,
                vs,
                src_w,
                src_h,
                dst_w,
                dst_h,
            )
        } else {
            let flushed = dec
                .flush_remaining()
                .map_err(|err| MediaError::Codec(format!("openh264 flush kf {kf_idx}: {err:?}")))?;
            let yuv = flushed.first().ok_or_else(|| {
                MediaError::Codec(format!("openh264 produced no image for keyframe {kf_idx}"))
            })?;
            let (src_w, src_h) = yuv.dimensions();
            let (ys, us, vs) = yuv.strides();
            yuv420p_to_thumb_rgb24(
                yuv.y(),
                yuv.u(),
                yuv.v(),
                ys,
                us,
                vs,
                src_w,
                src_h,
                dst_w,
                dst_h,
            )
        };
        local.push((
            kf_idx,
            RgbImage {
                width: thumb_width,
                height: thumb_height,
                data: rgb_bytes,
            },
        ));
    }
    Ok(local)
}

struct OpenGopDecoder {
    decoder: Decoder,
    start: u64,
    next_input: u64,
    end: u64,
    raw: Vec<u8>,
    annexb: Vec<u8>,
    tail: std::collections::VecDeque<(u32, u32, Vec<u8>)>,
}

impl super::SequentialDecoder for OpenGopDecoder {
    fn next(
        &mut self,
        track: &Mp4VideoTrack,
        index: u64,
        format: PixelFormat,
    ) -> Result<DecodedFrame, MediaError> {
        while self.next_input <= self.end {
            let input = self.next_input;
            track.read_sample_into(input, &mut self.raw)?;
            let mode = if input == self.start {
                ParamSetMode::PrependIfMissing
            } else {
                ParamSetMode::Omit
            };
            avcc_sample_to_annexb(&self.raw, track.avcc(), mode, &mut self.annexb);
            self.next_input += 1;
            if let Some(yuv) = self
                .decoder
                .decode_with_options(
                    &self.annexb,
                    DecodeOptions::new().flush_after_decode(Flush::NoFlush),
                )
                .map_err(|e| MediaError::Codec(format!("openh264 decode frame {input}: {e}")))?
            {
                let (w, h, data) = pack_decoded_yuv420p(&yuv)?;
                return convert_from_yuv420p(index, w, h, data, format);
            }
        }
        if self.tail.is_empty() {
            for yuv in self
                .decoder
                .flush_remaining()
                .map_err(|e| MediaError::Codec(format!("openh264 flush: {e}")))?
            {
                self.tail.push_back(pack_decoded_yuv420p(&yuv)?);
            }
        }
        let (w, h, data) = self
            .tail
            .pop_front()
            .ok_or_else(|| MediaError::Codec(format!("missing GOP frame {index}")))?;
        convert_from_yuv420p(index, w, h, data, format)
    }
}

impl VideoCodecBackend for OpenH264Backend {
    fn sequential_decoder(
        &self,
        _: &Mp4VideoTrack,
        start: u64,
        end: u64,
    ) -> Result<Box<dyn super::SequentialDecoder>, MediaError> {
        Ok(Box::new(OpenGopDecoder {
            decoder: make_decoder(Flush::NoFlush)?,
            start,
            next_input: start,
            end,
            raw: Vec::new(),
            annexb: Vec::new(),
            tail: std::collections::VecDeque::new(),
        }))
    }

    fn name(&self) -> &'static str {
        "openh264"
    }

    fn decode_frame(
        &self,
        track: &Mp4VideoTrack,
        frame_index: u64,
        output_format: PixelFormat,
    ) -> Result<DecodedFrame, MediaError> {
        let gop_start = track.decodable_gop_start(frame_index)?;
        let mut target = None;
        visit_range_yuv420p(track, gop_start, frame_index, frame_index, |frame| {
            if frame.frame_index == frame_index {
                target = Some(frame);
            }
            Ok(())
        })?;
        let target = target.ok_or_else(|| {
            MediaError::Codec(format!(
                "openh264 did not emit target frame {frame_index} from GOP start {gop_start}"
            ))
        })?;
        convert_from_yuv420p(
            frame_index,
            target.width,
            target.height,
            target.yuv420p,
            output_format,
        )
    }

    fn decode_gop(
        &self,
        track: &Mp4VideoTrack,
        gop_id: u64,
        output_format: PixelFormat,
    ) -> Result<Vec<DecodedFrame>, MediaError> {
        let (gop_start, gop_end) = track.decodable_gop_range(gop_id)?;
        let raw_frames = decode_range_yuv420p(track, gop_start, gop_end)?;
        let expected_len = usize::try_from(gop_end - gop_start + 1).unwrap_or(0);
        if raw_frames.len() != expected_len {
            return Err(MediaError::Codec(format!(
                "openh264 decoded {} frames for GOP {gop_id}, expected {expected_len}",
                raw_frames.len()
            )));
        }
        let mut out = Vec::with_capacity(raw_frames.len());
        for frame in raw_frames {
            out.push(convert_from_yuv420p(
                frame.frame_index,
                frame.width,
                frame.height,
                frame.yuv420p,
                output_format,
            )?);
        }
        Ok(out)
    }

    fn decode_keyframes_thumbnails(
        &self,
        track: &Mp4VideoTrack,
        max_width: u32,
        max_height: u32,
        max_threads: usize,
    ) -> Result<Vec<(u64, RgbImage)>, MediaError> {
        if max_width == 0 || max_height == 0 {
            return Err(MediaError::InvalidArgument(
                "thumbnail max_width and max_height must be non-zero".to_owned(),
            ));
        }
        let keyframes = track.keyframes();
        if keyframes.is_empty() {
            return Ok(Vec::new());
        }
        let (thumb_width, thumb_height) = fit_thumbnail_dimensions(
            u32::from(track.width()),
            u32::from(track.height()),
            max_width,
            max_height,
        );
        let dst_w = usize::try_from(thumb_width)
            .map_err(|_| MediaError::Codec("thumbnail width overflow".to_owned()))?;
        let dst_h = usize::try_from(thumb_height)
            .map_err(|_| MediaError::Codec("thumbnail height overflow".to_owned()))?;

        let workers = max_threads.max(1).min(keyframes.len());
        let chunk_size = keyframes.len().div_ceil(workers);

        let mut results: Vec<(u64, RgbImage)> = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for chunk in keyframes.chunks(chunk_size) {
                handles.push(scope.spawn(move || {
                    decode_keyframe_thumb_chunk(
                        track,
                        chunk,
                        thumb_width,
                        thumb_height,
                        dst_w,
                        dst_h,
                    )
                }));
            }

            let mut merged = Vec::with_capacity(keyframes.len());
            for handle in handles {
                let batch = handle.join().map_err(|_| {
                    MediaError::Codec("filmstrip worker thread panicked".to_owned())
                })??;
                merged.extend(batch);
            }
            Ok::<Vec<(u64, RgbImage)>, MediaError>(merged)
        })?;

        results.sort_unstable_by_key(|(kf_idx, _)| *kf_idx);
        Ok(results)
    }

    fn reencode_frame_range(
        &self,
        track: &Mp4VideoTrack,
        start_frame: u64,
        end_frame_exclusive: u64,
        params: EncodeParams,
    ) -> Result<EncodedGop, MediaError> {
        self.reencode_frame_range_with_overlay(
            track,
            start_frame,
            end_frame_exclusive,
            params,
            u64::MAX,
            &|_| false,
            &mut |_, _| {},
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "streaming frame-by-frame decode, overlay paint, and H.264 re-encode loop"
    )]
    fn reencode_frame_range_with_overlay(
        &self,
        track: &Mp4VideoTrack,
        start_frame: u64,
        end_frame_exclusive: u64,
        params: EncodeParams,
        keyframe_interval: u64,
        has_overlay: &dyn Fn(u64) -> bool,
        apply_overlay: &mut dyn FnMut(u64, &mut MutableYuvFrame<'_>),
    ) -> Result<EncodedGop, MediaError> {
        let frame_count = track.frame_count();
        if start_frame >= end_frame_exclusive || end_frame_exclusive > frame_count {
            return Err(MediaError::InvalidFrameRange {
                in_frame_index: start_frame,
                out_frame_index: end_frame_exclusive.saturating_sub(1),
                frame_count,
            });
        }
        let interval = keyframe_interval.max(1);
        let end_inclusive = end_frame_exclusive - 1;
        let expected_encoded = usize::try_from(end_frame_exclusive - start_frame).unwrap_or(0);

        let mut enc = make_encoder(params)?;
        let mut seq_param_set: Option<Vec<u8>> = None;
        let mut pic_param_set: Option<Vec<u8>> = None;
        let mut samples = Vec::with_capacity(expected_encoded);
        let mut bs_buf = Vec::new();
        let mut decoded_frames = 0u64;

        let gop_start = track.decodable_gop_start(start_frame)?;
        visit_range_yuv420p(track, gop_start, end_inclusive, gop_start, |mut frame| {
            decoded_frames += 1;
            let frame_idx = frame.frame_index;
            if frame_idx < start_frame {
                return Ok(());
            }
            let width = usize::try_from(frame.width)
                .map_err(|_| MediaError::Codec("frame width exceeds usize".to_owned()))?;
            let height = usize::try_from(frame.height)
                .map_err(|_| MediaError::Codec("frame height exceeds usize".to_owned()))?;
            require_even_encode_dims(width, height)?;

            if has_overlay(frame_idx) {
                let y_len = width.saturating_mul(height);
                let cw = width.div_ceil(2);
                let ch = height.div_ceil(2);
                let uv_len = cw.saturating_mul(ch);
                if frame.yuv420p.len() >= y_len + uv_len * 2 {
                    let (y_slice, uv_rest) = frame.yuv420p.split_at_mut(y_len);
                    let (u_slice, v_slice) = uv_rest.split_at_mut(uv_len);
                    let mut yuv_view = MutableYuvFrame::Yuv420p {
                        width,
                        height,
                        y: y_slice,
                        u: u_slice,
                        v: &mut v_slice[..uv_len],
                        y_stride: width,
                        u_stride: cw,
                        v_stride: cw,
                    };
                    apply_overlay(frame_idx, &mut yuv_view);
                }
            }

            let planes = split_yuv420p_planes(&frame.yuv420p, width, height)?;
            let rel_idx = frame_idx.saturating_sub(start_frame);
            if rel_idx % interval == 0 {
                enc.force_intra_frame();
            }

            let slices = YUVSlices::new(
                (planes.y, planes.u, planes.v),
                (width, height),
                (width, width / 2, width / 2),
            );
            let bs = enc.encode(&slices).map_err(|err| {
                MediaError::Codec(format!(
                    "openh264 overlay encode frame {frame_idx}: {err:?}"
                ))
            })?;
            if matches!(bs.frame_type(), FrameType::Skip) {
                return Err(MediaError::Codec(format!(
                    "openh264 unexpectedly skipped frame {frame_idx}"
                )));
            }
            let is_idr = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
            if frame_idx == start_frame && !is_idr {
                return Err(MediaError::Codec(format!(
                    "first overlay-encoded frame {frame_idx} was not IDR: {:?}",
                    bs.frame_type()
                )));
            }

            bs_buf.clear();
            bs.write_vec(&mut bs_buf);
            let avcc_data = annexb_to_avcc_sample(&bs_buf, &mut seq_param_set, &mut pic_param_set)?;
            let duration_ticks = track.frame(frame_idx)?.duration_ticks;
            samples.push(EncodedSample {
                frame_index: frame_idx,
                avcc_data,
                duration_ticks,
                is_keyframe: is_idr,
            });
            Ok(())
        })?;

        if samples.len() != expected_encoded {
            return Err(MediaError::Codec(format!(
                "openh264 overlay-encoded {} frames, expected {expected_encoded}",
                samples.len()
            )));
        }

        let sps = seq_param_set
            .ok_or_else(|| MediaError::Codec("openh264 encoder emitted no SPS".to_owned()))?;
        let pps = pic_param_set
            .ok_or_else(|| MediaError::Codec("openh264 encoder emitted no PPS".to_owned()))?;
        let avcc_payload = build_avcc_payload(&sps, &pps)?;

        Ok(EncodedGop {
            sps,
            pps,
            avcc_payload,
            decoded_frames,
            samples,
        })
    }
}

#[cfg(test)]
mod seek_tests {
    use super::*;
    #[test]
    fn sparse_seek_only_materializes_target_yuv() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("long.mp4");
        let frames = (0..60)
            .map(|i| DecodedFrame {
                width: 64,
                height: 64,
                frame_index: i,
                format: PixelFormat::Yuv420p,
                data: vec![u8::try_from(32 + i * 2).unwrap(); 6144],
            })
            .collect::<Vec<_>>();
        let codec = OpenH264Backend::new();
        let packets = codec
            .encode_yuv420p_sequence(&frames, 120, 33_333_333, EncodeParams::default())
            .unwrap();
        crate::mp4::mux_raw_h264_to_mp4(&path, 64, 64, 90_000, 3000, &packets).unwrap();
        let track = Mp4VideoTrack::open(path).unwrap();
        let expected = codec
            .decode_gop(&track, 0, PixelFormat::Yuv420p)
            .unwrap()
            .pop()
            .unwrap();
        PACKED_OUTPUTS.set(0);
        let frame = codec
            .decode_frame(&track, 59, PixelFormat::Yuv420p)
            .unwrap();
        assert_eq!(frame.data, expected.data);
        assert_eq!(PACKED_OUTPUTS.get(), 1);
    }
}
