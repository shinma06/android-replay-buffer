//! GPL-free `VideoCodecBackend` trait, pixel-format conversion helpers, and platform backends.
//!
//! - [`OpenH264Backend`]: Universal cross-OS baseline (`BSD-2-Clause` Cisco `OpenH264`).
//! - `VideoToolboxBackend` (`macOS` only): Direct C FFI to Apple hardware `VideoToolbox`
//!   (`CoreFoundation` / `CoreVideo` / `CoreMedia` / `VideoToolbox`, zero external crates).
//! - [`default_codec_backend`]: Picks `VideoToolboxBackend` on macOS (falling back to
//!   [`OpenH264Backend`] if hardware initialization fails) and [`OpenH264Backend`] elsewhere.

mod openh264_backend;
#[cfg(target_os = "macos")]
mod videotoolbox_backend;

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use crate::error::MediaError;
use crate::mp4::Mp4VideoTrack;

pub use self::openh264_backend::OpenH264Backend;
#[cfg(target_os = "macos")]
pub use self::videotoolbox_backend::VideoToolboxBackend;

/// Default re-encoding bitrate (10 Mbps) used for head-GOP trim re-encoding.
pub const DEFAULT_REENCODE_BITRATE_BPS: u32 = 10_000_000;

/// Default maximum frame rate hint for head-GOP re-encoding.
pub const DEFAULT_REENCODE_MAX_FPS: u32 = 30;

/// Pixel layout of a [`DecodedFrame`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    /// Tightly packed planar 4:2:0 (`Y` followed by `U` followed by `V`).
    Yuv420p,
    /// Interleaved 24-bit RGB (`R, G, B`).
    Rgb24,
    /// Interleaved 32-bit RGBA (`R, G, B, A`).
    Rgba8,
    /// Interleaved 32-bit BGRA (`B, G, R, A`).
    Bgra8,
}

/// Decoded video frame paired with its exact zero-based `frame_index`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// Zero-based frame index in the source video track.
    pub frame_index: u64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel format describing [`Self::data`].
    pub format: PixelFormat,
    /// Tightly packed pixel payload in [`Self::format`].
    pub data: Vec<u8>,
}

impl DecodedFrame {
    /// Returns the payload byte length of this decoded frame.
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    /// Returns the tightly packed Y (luma) plane if `self.format == PixelFormat::Yuv420p`.
    pub fn y_plane(&self) -> Option<&[u8]> {
        if self.format != PixelFormat::Yuv420p {
            return None;
        }
        let y_len = usize::try_from(self.width)
            .ok()?
            .checked_mul(usize::try_from(self.height).ok()?)?;
        self.data.get(..y_len)
    }

    /// Converts this frame into an interleaved [`RgbImage`] (`RGB24`).
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Codec`] if buffer dimensions do not match the declared pixel format.
    pub fn to_rgb24(&self) -> Result<RgbImage, MediaError> {
        let width = usize::try_from(self.width)
            .map_err(|_| MediaError::Codec("frame width exceeds usize".to_owned()))?;
        let height = usize::try_from(self.height)
            .map_err(|_| MediaError::Codec("frame height exceeds usize".to_owned()))?;
        match self.format {
            PixelFormat::Rgb24 => {
                let expected = width.saturating_mul(height).saturating_mul(3);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated RGB24 frame buffer".to_owned()));
                }
                Ok(RgbImage {
                    width: self.width,
                    height: self.height,
                    data: self.data[..expected].to_vec(),
                })
            }
            PixelFormat::Rgba8 => {
                let expected = width.saturating_mul(height).saturating_mul(4);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated RGBA8 frame buffer".to_owned()));
                }
                let mut out = Vec::with_capacity(width.saturating_mul(height).saturating_mul(3));
                for px in self.data[..expected].as_chunks::<4>().0 {
                    out.extend_from_slice(&[px[0], px[1], px[2]]);
                }
                Ok(RgbImage {
                    width: self.width,
                    height: self.height,
                    data: out,
                })
            }
            PixelFormat::Bgra8 => {
                let expected = width.saturating_mul(height).saturating_mul(4);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated BGRA8 frame buffer".to_owned()));
                }
                let mut out = Vec::with_capacity(width.saturating_mul(height).saturating_mul(3));
                for px in self.data[..expected].as_chunks::<4>().0 {
                    out.extend_from_slice(&[px[2], px[1], px[0]]);
                }
                Ok(RgbImage {
                    width: self.width,
                    height: self.height,
                    data: out,
                })
            }
            PixelFormat::Yuv420p => {
                let planes = split_yuv420p_planes(&self.data, width, height)?;
                let mut rgb = vec![0u8; width.saturating_mul(height).saturating_mul(3)];
                yuv420p_to_rgb24(
                    planes.y,
                    planes.u,
                    planes.v,
                    width,
                    width.div_ceil(2),
                    width.div_ceil(2),
                    width,
                    height,
                    &mut rgb,
                );
                Ok(RgbImage {
                    width: self.width,
                    height: self.height,
                    data: rgb,
                })
            }
        }
    }

    /// Converts this frame into tightly packed `RGBA8` bytes (`width * height * 4`).
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Codec`] if buffer dimensions do not match the declared pixel format.
    pub fn to_rgba8(&self) -> Result<Vec<u8>, MediaError> {
        let w = usize::try_from(self.width)
            .map_err(|_| MediaError::Codec("frame width exceeds usize".to_owned()))?;
        let h = usize::try_from(self.height)
            .map_err(|_| MediaError::Codec("frame height exceeds usize".to_owned()))?;
        match self.format {
            PixelFormat::Rgba8 => {
                let expected = w.saturating_mul(h).saturating_mul(4);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated RGBA8 frame buffer".to_owned()));
                }
                Ok(self.data[..expected].to_vec())
            }
            PixelFormat::Bgra8 => {
                let expected = w.saturating_mul(h).saturating_mul(4);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated BGRA8 frame buffer".to_owned()));
                }
                let mut out = Vec::with_capacity(expected);
                for px in self.data[..expected].as_chunks::<4>().0 {
                    out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
                Ok(out)
            }
            PixelFormat::Rgb24 => {
                let expected = w.saturating_mul(h).saturating_mul(3);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated RGB24 frame buffer".to_owned()));
                }
                let mut out = Vec::with_capacity(w.saturating_mul(h).saturating_mul(4));
                for px in self.data[..expected].as_chunks::<3>().0 {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
                Ok(out)
            }
            PixelFormat::Yuv420p => {
                let rgb = self.to_rgb24()?;
                let mut out = Vec::with_capacity(w.saturating_mul(h).saturating_mul(4));
                for px in rgb.data.as_chunks::<3>().0 {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
                Ok(out)
            }
        }
    }

    /// Converts this frame into tightly packed `BGRA8` bytes (`width * height * 4`).
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Codec`] if buffer dimensions do not match the declared pixel format.
    pub fn to_bgra8(&self) -> Result<Vec<u8>, MediaError> {
        let w = usize::try_from(self.width)
            .map_err(|_| MediaError::Codec("frame width exceeds usize".to_owned()))?;
        let h = usize::try_from(self.height)
            .map_err(|_| MediaError::Codec("frame height exceeds usize".to_owned()))?;
        match self.format {
            PixelFormat::Bgra8 => {
                let expected = w.saturating_mul(h).saturating_mul(4);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated BGRA8 frame buffer".to_owned()));
                }
                Ok(self.data[..expected].to_vec())
            }
            PixelFormat::Rgba8 => {
                let expected = w.saturating_mul(h).saturating_mul(4);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated RGBA8 frame buffer".to_owned()));
                }
                let mut out = Vec::with_capacity(expected);
                for px in self.data[..expected].as_chunks::<4>().0 {
                    out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
                Ok(out)
            }
            PixelFormat::Rgb24 => {
                let expected = w.saturating_mul(h).saturating_mul(3);
                if self.data.len() < expected {
                    return Err(MediaError::Codec("truncated RGB24 frame buffer".to_owned()));
                }
                let mut out = Vec::with_capacity(w.saturating_mul(h).saturating_mul(4));
                for px in self.data[..expected].as_chunks::<3>().0 {
                    out.extend_from_slice(&[px[2], px[1], px[0], 255]);
                }
                Ok(out)
            }
            PixelFormat::Yuv420p => {
                let rgb = self.to_rgb24()?;
                let mut out = Vec::with_capacity(w.saturating_mul(h).saturating_mul(4));
                for px in rgb.data.as_chunks::<3>().0 {
                    out.extend_from_slice(&[px[2], px[1], px[0], 255]);
                }
                Ok(out)
            }
        }
    }
}

/// Tightly packed interleaved 24-bit RGB image (`width * height * 3` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Interleaved RGB24 byte buffer.
    pub data: Vec<u8>,
}

/// One re-encoded H.264 sample in AVCC (4-byte length-prefixed) format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedSample {
    /// Source zero-based frame index corresponding to this encoded sample.
    pub frame_index: u64,
    /// Length-prefixed AVCC sample bytes.
    pub avcc_data: Vec<u8>,
    /// Preserved sample duration in track timescale ticks.
    pub duration_ticks: u32,
    /// Whether this encoded sample is an IDR keyframe.
    pub is_keyframe: bool,
}

/// Re-encoded H.264 head-GOP segment starting with a fresh IDR keyframe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedGop {
    /// Raw Sequence Parameter Set (SPS) NAL unit emitted by the encoder.
    pub sps: Vec<u8>,
    /// Raw Picture Parameter Set (PPS) NAL unit emitted by the encoder.
    pub pps: Vec<u8>,
    /// `avcC` configuration record built from [`Self::sps`] and [`Self::pps`].
    pub avcc_payload: Vec<u8>,
    /// Number of source frames decoded (including dropped pre-start frames from the GOP IDR).
    pub decoded_frames: u64,
    /// Encoded samples in presentation/decode order starting with an IDR keyframe.
    pub samples: Vec<EncodedSample>,
}

/// Configuration parameters for head-GOP H.264 re-encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeParams {
    /// Target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Maximum frame rate in frames per second.
    pub max_fps: u32,
}

impl Default for EncodeParams {
    fn default() -> Self {
        Self {
            bitrate_bps: DEFAULT_REENCODE_BITRATE_BPS,
            max_fps: DEFAULT_REENCODE_MAX_FPS,
        }
    }
}

/// Mutable view over a decoded 4:2:0 YUV frame used for in-place overlay compositing without
/// full-frame RGB round-trip conversion.
#[non_exhaustive]
#[derive(Debug)]
pub enum MutableYuvFrame<'a> {
    /// Planar 4:2:0 (`Y`, `U`, `V` separate planes).
    Yuv420p {
        /// Frame width in pixels.
        width: usize,
        /// Frame height in pixels.
        height: usize,
        /// Mutable luma plane (`>= y_stride * height`).
        y: &'a mut [u8],
        /// Mutable Cb plane (`>= u_stride * ceil(height / 2)`).
        u: &'a mut [u8],
        /// Mutable Cr plane (`>= v_stride * ceil(height / 2)`).
        v: &'a mut [u8],
        /// Row stride in bytes for [`Self::Yuv420p::y`].
        y_stride: usize,
        /// Row stride in bytes for [`Self::Yuv420p::u`].
        u_stride: usize,
        /// Row stride in bytes for [`Self::Yuv420p::v`].
        v_stride: usize,
    },
    /// Bi-planar NV12 (`Y` plane + interleaved `U,V` chroma plane).
    Nv12 {
        /// Frame width in pixels.
        width: usize,
        /// Frame height in pixels.
        height: usize,
        /// Mutable luma plane (`>= y_stride * height`).
        y: &'a mut [u8],
        /// Mutable interleaved `CbCr` plane (`>= uv_stride * ceil(height / 2)`).
        uv: &'a mut [u8],
        /// Row stride in bytes for [`Self::Nv12::y`].
        y_stride: usize,
        /// Row stride in bytes for [`Self::Nv12::uv`].
        uv_stride: usize,
    },
}

impl MutableYuvFrame<'_> {
    /// Returns the frame width in pixels.
    #[must_use]
    pub const fn width(&self) -> usize {
        match self {
            Self::Yuv420p { width, .. } | Self::Nv12 { width, .. } => *width,
        }
    }

    /// Returns the frame height in pixels.
    #[must_use]
    pub const fn height(&self) -> usize {
        match self {
            Self::Yuv420p { height, .. } | Self::Nv12 { height, .. } => *height,
        }
    }

    /// Returns a mutable reference to the luma sample at `(x, y)` if in bounds.
    pub fn y_mut_at(&mut self, x: usize, y_pos: usize) -> Option<&mut u8> {
        match self {
            Self::Yuv420p {
                width,
                height,
                y,
                y_stride,
                ..
            }
            | Self::Nv12 {
                width,
                height,
                y,
                y_stride,
                ..
            } => {
                if x >= *width || y_pos >= *height {
                    return None;
                }
                y.get_mut(y_pos.checked_mul(*y_stride)?.checked_add(x)?)
            }
        }
    }

    /// Alpha-blends `(ov_u, ov_v)` into the 2x2 chroma cell `(cx, cy)` if in bounds.
    pub fn blend_uv_at(&mut self, cx: usize, cy: usize, ov_u: f32, ov_v: f32, alpha: f32) {
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "value is explicitly rounded and clamped to [0.0, 255.0] before u8 conversion"
        )]
        let blend = |dst: u8, src: f32| -> u8 {
            (src * alpha + f32::from(dst) * (1.0 - alpha))
                .round()
                .clamp(0.0, 255.0) as u8
        };
        match self {
            Self::Yuv420p {
                width,
                height,
                u,
                v,
                u_stride,
                v_stride,
                ..
            } => {
                if cx >= width.div_ceil(2) || cy >= height.div_ceil(2) {
                    return;
                }
                if let Some(u_idx) = cy.checked_mul(*u_stride).and_then(|r| r.checked_add(cx))
                    && let Some(cell) = u.get_mut(u_idx)
                {
                    *cell = blend(*cell, ov_u);
                }
                if let Some(v_idx) = cy.checked_mul(*v_stride).and_then(|r| r.checked_add(cx))
                    && let Some(cell) = v.get_mut(v_idx)
                {
                    *cell = blend(*cell, ov_v);
                }
            }
            Self::Nv12 {
                width,
                height,
                uv,
                uv_stride,
                ..
            } => {
                if cx >= width.div_ceil(2) || cy >= height.div_ceil(2) {
                    return;
                }
                let Some(base) = cy
                    .checked_mul(*uv_stride)
                    .and_then(|r| r.checked_add(cx.checked_mul(2)?))
                else {
                    return;
                };
                if let Some(pair) = uv.get_mut(base..base + 2) {
                    pair[0] = blend(pair[0], ov_u);
                    pair[1] = blend(pair[1], ov_v);
                }
            }
        }
    }
}

/// Stateful decoder for consecutive display-order frames of one non-reordered GOP.
pub trait SequentialDecoder: Send {
    /// Produces the next frame using the existing decoder reference pictures.
    ///
    /// # Errors
    /// Returns an error for an unreadable sample or missing decoder output.
    fn next(
        &mut self,
        track: &Mp4VideoTrack,
        index: u64,
        format: PixelFormat,
    ) -> Result<DecodedFrame, MediaError>;
}

impl std::fmt::Debug for dyn SequentialDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SequentialDecoder")
    }
}

/// An output sample displaying a source picture for the given number of output ticks.
#[derive(Clone, Copy, Debug)]
pub struct OverlaySample {
    /// Input picture to decode. Multiple output samples may share a picture.
    pub source: u64,
    /// Index supplied to the painter.
    pub index: u64,
    /// Positive duration on the output clock.
    pub ticks: u32,
}

/// GPL-free H.264 video decode and head-GOP re-encode backend abstraction.
pub trait VideoCodecBackend: Debug + Send + Sync {
    /// Returns a stable human-readable identifier for this codec backend.
    fn name(&self) -> &'static str;

    /// Decodes the exact frame at `frame_index` (decoding from its GOP IDR) into `output_format`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] if `frame_index` is invalid, [`MediaError::Io`] on
    /// sample read error, or [`MediaError::Codec`] if decoding fails.
    fn decode_frame(
        &self,
        track: &Mp4VideoTrack,
        frame_index: u64,
        output_format: PixelFormat,
    ) -> Result<DecodedFrame, MediaError>;

    /// Creates a forward decoder starting at the IDR of an inclusive GOP range.
    ///
    /// # Errors
    /// Returns a codec error if initialization fails.
    fn sequential_decoder(
        &self,
        track: &Mp4VideoTrack,
        start: u64,
        end: u64,
    ) -> Result<Box<dyn SequentialDecoder>, MediaError>;

    /// Decodes every frame belonging to `gop_id` in order into `output_format`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidArgument`] if `gop_id` does not exist in `track`, or
    /// [`MediaError::Codec`] / [`MediaError::Io`] on failure.
    fn decode_gop(
        &self,
        track: &Mp4VideoTrack,
        gop_id: u64,
        output_format: PixelFormat,
    ) -> Result<Vec<DecodedFrame>, MediaError>;

    /// Decodes all keyframes in `track` in parallel (up to `max_threads`) and scales them into
    /// RGB24 thumbnails bounded by `max_width x max_height`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidArgument`] if `max_width == 0 || max_height == 0`, or
    /// [`MediaError::Codec`] / [`MediaError::Io`] on failure.
    fn decode_keyframes_thumbnails(
        &self,
        track: &Mp4VideoTrack,
        max_width: u32,
        max_height: u32,
        max_threads: usize,
    ) -> Result<Vec<(u64, RgbImage)>, MediaError>;

    /// Decodes from the GOP IDR containing `start_frame` through `end_frame_exclusive - 1`,
    /// drops frames `< start_frame`, and re-encodes `[start_frame..end_frame_exclusive)` starting
    /// with a brand-new IDR keyframe.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidFrameRange`] if the range is empty or out of bounds, or
    /// [`MediaError::Codec`] / [`MediaError::Io`] if decode or re-encode fails.
    fn reencode_frame_range(
        &self,
        track: &Mp4VideoTrack,
        start_frame: u64,
        end_frame_exclusive: u64,
        params: EncodeParams,
    ) -> Result<EncodedGop, MediaError>;

    /// Decodes `[start_frame..end_frame_exclusive)` (streaming GOP-by-GOP), calls `apply_overlay`
    /// on each frame where `has_overlay(frame_index)` is `true`, and re-encodes the entire range
    /// with an initial IDR and periodic IDR keyframes every `keyframe_interval` frames.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidFrameRange`] if the range is empty or out of bounds, or
    /// [`MediaError::Codec`] / [`MediaError::Io`] if decode or re-encode fails.
    #[expect(
        clippy::too_many_arguments,
        reason = "streaming overlay transcode separates pure predicate from mutable painter for zero-copy VideoToolbox pass-through"
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
        if start_frame >= end_frame_exclusive || end_frame_exclusive > track.frame_count() {
            return Err(MediaError::InvalidFrameRange {
                in_frame_index: start_frame,
                out_frame_index: end_frame_exclusive.saturating_sub(1),
                frame_count: track.frame_count(),
            });
        }
        let samples = (start_frame..end_frame_exclusive)
            .map(|i| {
                Ok(OverlaySample {
                    source: i,
                    index: i,
                    ticks: track.frame(i)?.duration_ticks,
                })
            })
            .collect::<Result<Vec<_>, MediaError>>()?;
        self.reencode_overlay_samples(
            track,
            &samples,
            track.timescale(),
            params,
            keyframe_interval,
            has_overlay,
            apply_overlay,
        )
    }

    /// Encodes a presentation schedule, decoding each source picture once.
    ///
    /// # Errors
    /// Returns an error for an invalid schedule, unreadable samples or a codec failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "explicit schedule, clock, and overlay callbacks"
    )]
    fn reencode_overlay_samples(
        &self,
        track: &Mp4VideoTrack,
        schedule: &[OverlaySample],
        timescale: u32,
        params: EncodeParams,
        keyframe_interval: u64,
        has_overlay: &dyn Fn(u64) -> bool,
        apply_overlay: &mut dyn FnMut(u64, &mut MutableYuvFrame<'_>),
    ) -> Result<EncodedGop, MediaError>;
}

/// Selects the default GPL-free [`VideoCodecBackend`] for the current host platform.
///
/// On macOS, selects `VideoToolboxBackend` when hardware initialization succeeds, falling back
/// cleanly to [`OpenH264Backend`] otherwise. On Linux and Windows, selects [`OpenH264Backend`].
pub fn default_codec_backend() -> Box<dyn VideoCodecBackend> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(vt) = VideoToolboxBackend::try_new() {
            return Box::new(vt);
        }
    }
    Box::new(OpenH264Backend::new())
}

/// Computes aspect-preserving thumbnail dimensions fitting within `max_width x max_height`.
pub(crate) fn fit_thumbnail_dimensions(
    src_width: u32,
    src_height: u32,
    max_width: u32,
    max_height: u32,
) -> (u32, u32) {
    if src_width == 0 || src_height == 0 || max_width == 0 || max_height == 0 {
        return (1, 1);
    }
    if src_width <= max_width && src_height <= max_height {
        return (src_width, src_height);
    }
    // Compare src_width / max_width vs src_height / max_height using exact u64 cross-multiplication.
    let lhs = u64::from(src_width) * u64::from(max_height);
    let rhs = u64::from(src_height) * u64::from(max_width);
    if lhs >= rhs {
        let tw = max_width.max(1);
        let th_u64 = (u64::from(src_height) * u64::from(tw)) / u64::from(src_width);
        let th = u32::try_from(th_u64.max(1)).unwrap_or(1);
        (tw, th)
    } else {
        let th = max_height.max(1);
        let tw_u64 = (u64::from(src_width) * u64::from(th)) / u64::from(src_height);
        let tw = u32::try_from(tw_u64.max(1)).unwrap_or(1);
        (tw, th)
    }
}

pub(crate) struct Yuv420pPlanes<'a> {
    pub(crate) y: &'a [u8],
    pub(crate) u: &'a [u8],
    pub(crate) v: &'a [u8],
}

pub(crate) fn split_yuv420p_planes(
    data: &[u8],
    width: usize,
    height: usize,
) -> Result<Yuv420pPlanes<'_>, MediaError> {
    let y_len = width.saturating_mul(height);
    // 4:2:0 chroma planes cover odd edges too: ceil(w/2) x ceil(h/2) (R2 S3).
    let uv_len = width.div_ceil(2).saturating_mul(height.div_ceil(2));
    let total = y_len.saturating_add(uv_len.saturating_mul(2));
    if data.len() < total {
        return Err(MediaError::Codec(
            "truncated YUV420p frame buffer".to_owned(),
        ));
    }
    Ok(Yuv420pPlanes {
        y: &data[..y_len],
        u: &data[y_len..y_len + uv_len],
        v: &data[y_len + uv_len..total],
    })
}

/// Bounds-checked luma sample read (black when out of range, never a panic).
#[inline]
fn luma_at(plane: &[u8], idx: usize) -> u8 {
    plane.get(idx).copied().unwrap_or(16)
}

/// Bounds-checked chroma sample read (neutral when out of range, never a panic).
#[inline]
fn chroma_at(plane: &[u8], idx: usize) -> u8 {
    plane.get(idx).copied().unwrap_or(128)
}

/// Writes one RGB pixel at `dst` if it fits in `out`.
#[inline]
fn put_rgb(out: &mut [u8], dst: usize, (r, g, b): (u8, u8, u8)) {
    if let Some(px) = out.get_mut(dst..dst + 3) {
        px.copy_from_slice(&[r, g, b]);
    }
}

#[inline]
fn clamp_u8(val: i32) -> u8 {
    u8::try_from(val.clamp(0, 255)).unwrap_or(0)
}

#[inline]
fn yuv_pixel_to_rgb(y_sample: u8, u_sample: u8, v_sample: u8) -> (u8, u8, u8) {
    let luma = i32::from(y_sample) - 16;
    let cb = i32::from(u_sample) - 128;
    let cr = i32::from(v_sample) - 128;
    let scaled_y = 298 * luma;
    let red = clamp_u8((scaled_y + 409 * cr + 128) >> 8);
    let green = clamp_u8((scaled_y - 100 * cb - 208 * cr + 128) >> 8);
    let blue = clamp_u8((scaled_y + 516 * cb + 128) >> 8);
    (red, green, blue)
}

#[expect(
    clippy::too_many_arguments,
    reason = "planar YUV conversion requires explicit planes, strides, dimensions, and destination"
)]
pub(crate) fn yuv420p_to_rgb24(
    y_plane: &[u8],
    u_plane: &[u8],
    v_plane: &[u8],
    y_stride: usize,
    u_stride: usize,
    v_stride: usize,
    width: usize,
    height: usize,
    out_rgb: &mut [u8],
) {
    let mut dst = 0usize;
    for row in 0..height {
        let y_row = row * y_stride;
        let u_row = (row >> 1) * u_stride;
        let v_row = (row >> 1) * v_stride;
        for col in 0..width {
            let rgb = yuv_pixel_to_rgb(
                luma_at(y_plane, y_row + col),
                chroma_at(u_plane, u_row + (col >> 1)),
                chroma_at(v_plane, v_row + (col >> 1)),
            );
            put_rgb(out_rgb, dst, rgb);
            dst += 3;
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "planar YUV thumbnail downscale requires explicit planes, strides, and target dimensions"
)]
pub(crate) fn yuv420p_to_thumb_rgb24(
    y_plane: &[u8],
    u_plane: &[u8],
    v_plane: &[u8],
    y_stride: usize,
    u_stride: usize,
    v_stride: usize,
    src_width: usize,
    src_height: usize,
    dst_width: usize,
    dst_height: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; dst_width.saturating_mul(dst_height).saturating_mul(3)];
    let mut dst = 0usize;
    for ty in 0..dst_height {
        let sy = ((ty * src_height + src_height / 2) / dst_height).min(src_height - 1);
        let y_row = sy * y_stride;
        let u_row = (sy >> 1) * u_stride;
        let v_row = (sy >> 1) * v_stride;
        for tx in 0..dst_width {
            let sx = ((tx * src_width + src_width / 2) / dst_width).min(src_width - 1);
            let rgb = yuv_pixel_to_rgb(
                luma_at(y_plane, y_row + sx),
                chroma_at(u_plane, u_row + (sx >> 1)),
                chroma_at(v_plane, v_row + (sx >> 1)),
            );
            put_rgb(&mut out, dst, rgb);
            dst += 3;
        }
    }
    out
}

#[cfg(target_os = "macos")]
pub(crate) fn nv12_to_rgb24(
    y_plane: &[u8],
    uv_plane: &[u8],
    y_stride: usize,
    uv_stride: usize,
    width: usize,
    height: usize,
    out_rgb: &mut [u8],
) {
    let mut dst = 0usize;
    for row in 0..height {
        let y_row = row * y_stride;
        let uv_row = (row >> 1) * uv_stride;
        for col in 0..width {
            let uv_idx = uv_row + (col & !1);
            let rgb = yuv_pixel_to_rgb(
                luma_at(y_plane, y_row + col),
                chroma_at(uv_plane, uv_idx),
                chroma_at(uv_plane, uv_idx + 1),
            );
            put_rgb(out_rgb, dst, rgb);
            dst += 3;
        }
    }
}

#[cfg(target_os = "macos")]
#[expect(
    clippy::too_many_arguments,
    reason = "bi-planar NV12 thumbnail downscale requires explicit planes, strides, and dimensions"
)]
pub(crate) fn nv12_to_thumb_rgb24(
    y_plane: &[u8],
    uv_plane: &[u8],
    y_stride: usize,
    uv_stride: usize,
    src_width: usize,
    src_height: usize,
    dst_width: usize,
    dst_height: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; dst_width.saturating_mul(dst_height).saturating_mul(3)];
    let mut dst = 0usize;
    for ty in 0..dst_height {
        let sy = ((ty * src_height + src_height / 2) / dst_height).min(src_height - 1);
        let y_row = sy * y_stride;
        let uv_row = (sy >> 1) * uv_stride;
        for tx in 0..dst_width {
            let sx = ((tx * src_width + src_width / 2) / dst_width).min(src_width - 1);
            let uv_idx = uv_row + (sx & !1);
            let rgb = yuv_pixel_to_rgb(
                luma_at(y_plane, y_row + sx),
                chroma_at(uv_plane, uv_idx),
                chroma_at(uv_plane, uv_idx + 1),
            );
            put_rgb(&mut out, dst, rgb);
            dst += 3;
        }
    }
    out
}

#[cfg(target_os = "macos")]
pub(crate) fn nv12_to_yuv420p(
    y_plane: &[u8],
    uv_plane: &[u8],
    y_stride: usize,
    uv_stride: usize,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let y_len = width.saturating_mul(height);
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    let uv_len = cw.saturating_mul(ch);
    let mut out = vec![0u8; y_len + uv_len * 2];
    copy_plane_rows(&mut out[..y_len], y_plane, y_stride, width, height, 16);
    let u_base = y_len;
    let v_base = y_len + uv_len;
    for row in 0..ch {
        let uv_row = row * uv_stride;
        for col in 0..cw {
            out[u_base + row * cw + col] = chroma_at(uv_plane, uv_row + col * 2);
            out[v_base + row * cw + col] = chroma_at(uv_plane, uv_row + col * 2 + 1);
        }
    }
    out
}

/// Copies `rows` rows of `row_bytes` from a strided `src` plane into tightly packed `dst`,
/// filling any bytes the source cannot provide with `fill` (never panics on short planes).
pub(crate) fn copy_plane_rows(
    dst: &mut [u8],
    src: &[u8],
    src_stride: usize,
    row_bytes: usize,
    rows: usize,
    fill: u8,
) {
    for row in 0..rows {
        let Some(dst_row) = dst.get_mut(row * row_bytes..(row + 1) * row_bytes) else {
            return;
        };
        let start = row.saturating_mul(src_stride);
        let avail = src
            .get(start..)
            .map_or(&[][..], |rest| &rest[..rest.len().min(row_bytes)]);
        dst_row[..avail.len()].copy_from_slice(avail);
        dst_row[avail.len()..].fill(fill);
    }
}

pub(crate) fn convert_from_yuv420p(
    frame_index: u64,
    width: u32,
    height: u32,
    yuv420p: Vec<u8>,
    target: PixelFormat,
) -> Result<DecodedFrame, MediaError> {
    let frame = DecodedFrame {
        frame_index,
        width,
        height,
        format: PixelFormat::Yuv420p,
        data: yuv420p,
    };
    match target {
        PixelFormat::Yuv420p => Ok(frame),
        PixelFormat::Rgb24 => {
            let rgb = frame.to_rgb24()?;
            Ok(DecodedFrame {
                frame_index,
                width,
                height,
                format: PixelFormat::Rgb24,
                data: rgb.data,
            })
        }
        PixelFormat::Rgba8 => {
            let rgba = frame.to_rgba8()?;
            Ok(DecodedFrame {
                frame_index,
                width,
                height,
                format: PixelFormat::Rgba8,
                data: rgba,
            })
        }
        PixelFormat::Bgra8 => {
            let bgra = frame.to_bgra8()?;
            Ok(DecodedFrame {
                frame_index,
                width,
                height,
                format: PixelFormat::Bgra8,
                data: bgra,
            })
        }
    }
}
