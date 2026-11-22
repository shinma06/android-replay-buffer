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
    DecodedFrame, EncodeParams, EncodedGop, EncodedSample, PixelFormat, RgbImage,
    VideoCodecBackend, convert_from_yuv420p, fit_thumbnail_dimensions, split_yuv420p_planes,
    yuv420p_to_thumb_rgb24,
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

fn pack_decoded_yuv420p(yuv: &DecodedYUV<'_>) -> Result<(u32, u32, Vec<u8>), MediaError> {
    let (w, h) = yuv.dimensions();
    let (ys, us, vs) = yuv.strides();
    let y_src = yuv.y();
    let u_src = yuv.u();
    let v_src = yuv.v();
    let y_len = w.saturating_mul(h);
    let cw = w / 2;
    let ch = h / 2;
    let uv_len = cw.saturating_mul(ch);
    let mut data = vec![0u8; y_len + uv_len * 2];

    for row in 0..h {
        data[row * w..(row + 1) * w].copy_from_slice(&y_src[row * ys..row * ys + w]);
    }
    let u_off = y_len;
    let v_off = y_len + uv_len;
    for row in 0..ch {
        data[u_off + row * cw..u_off + (row + 1) * cw]
            .copy_from_slice(&u_src[row * us..row * us + cw]);
        data[v_off + row * cw..v_off + (row + 1) * cw]
            .copy_from_slice(&v_src[row * vs..row * vs + cw]);
    }

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
    let mut dec = make_decoder(Flush::NoFlush)?;
    let mut raw = Vec::new();
    let mut annexb = Vec::new();
    let capacity = usize::try_from(end_inclusive.saturating_sub(gop_start) + 1).unwrap_or(0);
    let mut decoded_frames = Vec::with_capacity(capacity);
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
            let (width, height, yuv420p) = pack_decoded_yuv420p(&yuv)?;
            decoded_frames.push(DecodedYuvFrame {
                frame_index: emitted_idx,
                width,
                height,
                yuv420p,
            });
            emitted_idx += 1;
        }
    }

    // Drain OpenH264's 1-frame internal DPB delay so the tail frame is never dropped or off-by-1.
    let remaining = dec
        .flush_remaining()
        .map_err(|err| MediaError::Codec(format!("openh264 flush_remaining: {err:?}")))?;
    for yuv in remaining {
        let (width, height, yuv420p) = pack_decoded_yuv420p(&yuv)?;
        decoded_frames.push(DecodedYuvFrame {
            frame_index: emitted_idx,
            width,
            height,
            yuv420p,
        });
        emitted_idx += 1;
    }

    Ok(decoded_frames)
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

impl VideoCodecBackend for OpenH264Backend {
    fn name(&self) -> &'static str {
        "openh264"
    }

    fn decode_frame(
        &self,
        track: &Mp4VideoTrack,
        frame_index: u64,
        output_format: PixelFormat,
    ) -> Result<DecodedFrame, MediaError> {
        let (gop_start, _) = track.gop_bounds(frame_index)?;
        let frames = decode_range_yuv420p(track, gop_start, frame_index)?;
        let target = frames
            .into_iter()
            .find(|f| f.frame_index == frame_index)
            .ok_or_else(|| {
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
        let (gop_start, gop_end) = track.gop_range(gop_id).ok_or_else(|| {
            MediaError::InvalidArgument(format!("gop_id {gop_id} does not exist in track"))
        })?;
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
        let frame_count = track.frame_count();
        if start_frame >= end_frame_exclusive || end_frame_exclusive > frame_count {
            return Err(MediaError::InvalidFrameRange {
                in_frame_index: start_frame,
                out_frame_index: end_frame_exclusive.saturating_sub(1),
                frame_count,
            });
        }

        let (gop_start, _) = track.gop_bounds(start_frame)?;
        let end_inclusive = end_frame_exclusive - 1;
        let decoded = decode_range_yuv420p(track, gop_start, end_inclusive)?;
        let decoded_frames = u64::try_from(decoded.len()).unwrap_or(0);

        let mut enc = make_encoder(params)?;
        let mut seq_param_set: Option<Vec<u8>> = None;
        let mut pic_param_set: Option<Vec<u8>> = None;
        let expected_encoded = usize::try_from(end_frame_exclusive - start_frame).unwrap_or(0);
        let mut samples = Vec::with_capacity(expected_encoded);
        let mut bs_buf = Vec::new();

        for frame in decoded {
            let frame_idx = frame.frame_index;
            if frame_idx < start_frame || frame_idx >= end_frame_exclusive {
                continue;
            }
            let width = usize::try_from(frame.width)
                .map_err(|_| MediaError::Codec("frame width exceeds usize".to_owned()))?;
            let height = usize::try_from(frame.height)
                .map_err(|_| MediaError::Codec("frame height exceeds usize".to_owned()))?;
            let planes = split_yuv420p_planes(&frame.yuv420p, width, height)?;

            if frame_idx == start_frame {
                enc.force_intra_frame();
            }

            let slices = YUVSlices::new(
                (planes.y, planes.u, planes.v),
                (width, height),
                (width, width / 2, width / 2),
            );
            let bs = enc.encode(&slices).map_err(|err| {
                MediaError::Codec(format!("openh264 re-encode frame {frame_idx}: {err:?}"))
            })?;
            if matches!(bs.frame_type(), FrameType::Skip) {
                return Err(MediaError::Codec(format!(
                    "openh264 unexpectedly skipped frame {frame_idx}"
                )));
            }
            let is_idr = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
            if frame_idx == start_frame && !is_idr {
                return Err(MediaError::Codec(format!(
                    "first re-encoded frame {frame_idx} was not IDR: {:?}",
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
        }

        if samples.len() != expected_encoded {
            return Err(MediaError::Codec(format!(
                "openh264 re-encoded {} frames, expected {expected_encoded}",
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
