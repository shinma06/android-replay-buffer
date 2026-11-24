//! Pure-Rust, GPL-free tap & swipe overlay rasterizer and full-range burn-in transcoder.
//!
//! Composites high-contrast anti-aliased touch indicators directly onto decoded video frames
//! (without mutating the immutable WORM source bundle) and re-encodes via the active GPL-free
//! [`VideoCodecBackend`] (`VideoToolbox` hardware on macOS, `OpenH264` on Linux/Windows) into a
//! faststart, single-`stsd` `avc1`, zero-`edts`/zero-`elst` MP4.
//!
//! When zero mappable touch events overlap `[in_frame_index..=out_frame_index]`,
//! [`burn_in_taps_video`] automatically takes the lossless [`trim_video`] fast path with zero
//! full-video re-encoding.

use std::path::Path;

use arb_core::bundle::{RecordBucket, TapRecord};
use arb_core::gesture::{GestureKind, TouchPoint};
use arb_core::time::TimeAlignment;
use serde::{Deserialize, Serialize};

use crate::codec::{EncodeParams, MutableYuvFrame, RgbImage, VideoCodecBackend};
use crate::error::MediaError;
use crate::mp4::{Mp4VideoTrack, MuxSample, MuxSegment, write_trim_mp4};
use crate::output::ensure_output_is_not_input;
use crate::trim::trim_video;

/// Smooth fade-out duration after a `Tap` timestamp (450 ms).
pub const TAP_FADE_DURATION_NS: i64 = 450_000_000;

/// Smooth fade-out duration after a `Swipe` release timestamp (500 ms).
pub const SWIPE_FADE_DURATION_NS: i64 = 500_000_000;

/// Periodic IDR keyframe cadence for full-range burn-in re-encoding (every 30 frames).
pub const DEFAULT_BURN_IN_KEYFRAME_INTERVAL: u64 = 30;

/// Physical device display dimensions used to scale touch coordinates onto video pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BurnInDisplayGeometry {
    /// Physical display width in pixels, when known.
    pub display_width: Option<u32>,
    /// Physical display height in pixels, when known.
    pub display_height: Option<u32>,
}

/// Normalized touch gesture event ready for frame-level overlay rasterization.
#[derive(Debug, Clone, PartialEq)]
pub struct BurnInTouchEvent {
    /// Classified gesture kind (`Tap` or `Swipe`).
    pub kind: GestureKind,
    /// Gesture start timestamp (`DOWN`) in nanoseconds on the bundle's `DeviceBoottime` clock.
    pub start_t_ns: i64,
    /// Gesture end timestamp (`UP`) in nanoseconds (`>= start_t_ns`).
    pub end_t_ns: i64,
    /// Start X coordinate in display (or normalized `0..=1`) coordinates.
    pub x: f64,
    /// Start Y coordinate in display (or normalized `0..=1`) coordinates.
    pub y: f64,
    /// Optional end X coordinate for `Swipe`.
    pub end_x: Option<f64>,
    /// Optional end Y coordinate for `Swipe`.
    pub end_y: Option<f64>,
    /// Decimated trajectory points for `Swipe`.
    pub path: Vec<TouchPoint>,
}

impl BurnInTouchEvent {
    /// Converts a main-timeline [`TapRecord`] into a [`BurnInTouchEvent`], returning `None` for
    /// quarantined `unanchored` records or records without finite start coordinates.
    #[must_use]
    pub fn from_tap_record(tap: &TapRecord) -> Option<Self> {
        if tap.bucket != RecordBucket::Main {
            return None;
        }
        let x = tap.x.filter(|v| v.is_finite())?;
        let y = tap.y.filter(|v| v.is_finite())?;
        let start_t_ns = match &tap.alignment {
            TimeAlignment::Anchored { boottime_ns, .. } => *boottime_ns,
            TimeAlignment::Bounded {
                min_boottime_ns, ..
            } => *min_boottime_ns,
            _ => return None,
        };
        let duration_ns = tap.end_t_offset_ns.unwrap_or(0).max(0);
        let end_t_ns = start_t_ns.saturating_add(duration_ns);
        Some(Self {
            kind: tap.gesture_kind,
            start_t_ns,
            end_t_ns,
            x,
            y,
            end_x: tap.end_x.filter(|v| v.is_finite()),
            end_y: tap.end_y.filter(|v| v.is_finite()),
            path: tap.path.clone(),
        })
    }
}

/// Summary returned after completing [`burn_in_taps_video`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BurnInReport {
    /// Total frames written to the output MP4.
    pub output_frame_count: u64,
    /// Number of output frames onto which at least one tap or swipe overlay was rasterized.
    pub burned_frame_count: u64,
    /// `true` when full-range re-encoding ran; `false` when the 0-touch lossless fast path ran.
    pub reencoded: bool,
    /// Name of the [`VideoCodecBackend`] used.
    pub backend_name: String,
    /// Number of `edts` boxes written (always `0`).
    pub edts_box_count: usize,
    /// Number of `elst` boxes written (always `0`).
    pub elst_box_count: usize,
}

#[inline]
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "pixel coordinates fit well within f64's 52-bit exact integer mantissa"
)]
const fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

#[inline]
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "sub-second gesture elapsed durations fit cleanly in f64 precision"
)]
const fn i64_to_f64(value: i64) -> f64 {
    value as f64
}

#[inline]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "normalized [0.0, 1.0] alpha/coverage values intentionally narrow to f32"
)]
const fn f64_to_f32_unit(value: f64) -> f32 {
    value as f32
}

#[inline]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is explicitly rounded and clamped to [0.0, 255.0]"
)]
fn f32_to_u8_clamped(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[inline]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is explicitly bounded within [lo, hi] where both are valid usize pixel coords"
)]
fn floor_clamped(val: f64, lo: usize, hi: usize) -> usize {
    if !val.is_finite() || val <= usize_to_f64(lo) {
        return lo;
    }
    if val >= usize_to_f64(hi) {
        return hi;
    }
    val.floor() as usize
}

#[inline]
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is explicitly bounded within [lo, hi] where both are valid usize pixel coords"
)]
fn ceil_clamped(val: f64, lo: usize, hi: usize) -> usize {
    if !val.is_finite() || val <= usize_to_f64(lo) {
        return lo;
    }
    if val >= usize_to_f64(hi) {
        return hi;
    }
    val.ceil() as usize
}

/// Pre-scaled gesture mapped into video pixel coordinates `[0, video_w) x [0, video_h)`.
#[derive(Debug, Clone)]
struct MappedTouchEvent {
    kind: GestureKind,
    start_t_ns: i64,
    end_t_ns: i64,
    start: (f64, f64),
    end: (f64, f64),
    /// Polyline points `(vx, vy, t_offset_ns)` including start and end.
    polyline: Vec<(f64, f64, i64)>,
}

impl MappedTouchEvent {
    fn is_active_at(&self, frame_t_ns: i64) -> bool {
        if frame_t_ns < self.start_t_ns {
            return false;
        }
        let fade_window = match self.kind {
            GestureKind::Swipe => SWIPE_FADE_DURATION_NS,
            _ => TAP_FADE_DURATION_NS,
        };
        let expire_t = match self.kind {
            GestureKind::Swipe => self.end_t_ns.saturating_add(fade_window),
            _ => self.start_t_ns.saturating_add(fade_window),
        };
        frame_t_ns <= expire_t
    }
}

fn map_point_to_video(
    x: f64,
    y: f64,
    video_w: u32,
    video_h: u32,
    geom: BurnInDisplayGeometry,
) -> Option<(f64, f64)> {
    arb_core::gesture::map_touch_to_frame(
        x,
        y,
        geom.display_width,
        geom.display_height,
        video_w,
        video_h,
    )
}

fn map_events(
    events: &[BurnInTouchEvent],
    video_w: u32,
    video_h: u32,
    geom: BurnInDisplayGeometry,
) -> Vec<MappedTouchEvent> {
    let mut out = Vec::with_capacity(events.len());
    for ev in events {
        let Some(start) = map_point_to_video(ev.x, ev.y, video_w, video_h, geom) else {
            continue;
        };
        let end = match (ev.end_x, ev.end_y) {
            (Some(ex), Some(ey)) => {
                map_point_to_video(ex, ey, video_w, video_h, geom).unwrap_or(start)
            }
            _ => start,
        };
        let mut polyline: Vec<(f64, f64, i64)> = Vec::new();
        if ev.kind == GestureKind::Swipe {
            for pt in &ev.path {
                if let Some((px, py)) = map_point_to_video(pt.x, pt.y, video_w, video_h, geom) {
                    polyline.push((px, py, pt.t_offset_ns.max(0)));
                }
            }
            if polyline.len() < 2 {
                polyline.clear();
                polyline.push((start.0, start.1, 0));
                let dur = ev.end_t_ns.saturating_sub(ev.start_t_ns).max(0);
                polyline.push((end.0, end.1, dur));
            }
        }
        out.push(MappedTouchEvent {
            kind: ev.kind,
            start_t_ns: ev.start_t_ns,
            end_t_ns: ev.end_t_ns.max(ev.start_t_ns),
            start,
            end,
            polyline,
        });
    }
    out
}

/// Premultiplied-alpha RGBA bounding-box overlay canvas composited via Porter-Duff source-over.
struct OverlayBox {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    /// Premultiplied `[r, g, b, a]` in `0.0..=1.0`.
    pixels: Vec<[f32; 4]>,
}

impl OverlayBox {
    fn new(x0: usize, y0: usize, x1: usize, y1: usize) -> Self {
        let w = x1.saturating_sub(x0);
        let h = y1.saturating_sub(y0);
        Self {
            x0,
            y0,
            w,
            h,
            pixels: vec![[0.0; 4]; w.saturating_mul(h)],
        }
    }

    #[inline]
    fn blend_over(&mut self, gx: usize, gy: usize, rgb: [f32; 3], alpha: f32) {
        if alpha <= 0.001 || gx < self.x0 || gy < self.y0 {
            return;
        }
        let lx = gx - self.x0;
        let ly = gy - self.y0;
        if lx >= self.w || ly >= self.h {
            return;
        }
        let clamped_a = alpha.clamp(0.0, 1.0);
        let idx = ly * self.w + lx;
        let dst = self.pixels[idx];
        let inv = 1.0 - clamped_a;
        self.pixels[idx] = [
            rgb[0] * clamped_a + dst[0] * inv,
            rgb[1] * clamped_a + dst[1] * inv,
            rgb[2] * clamped_a + dst[2] * inv,
            clamped_a + dst[3] * inv,
        ];
    }

    fn draw_filled_circle(&mut self, cx: f64, cy: f64, radius: f64, rgba: [f32; 4]) {
        if radius <= 0.0 || rgba[3] <= 0.001 {
            return;
        }
        let pad = radius + 1.5;
        let min_x = floor_clamped(cx - pad, self.x0, self.x0 + self.w);
        let max_x = ceil_clamped(cx + pad, self.x0, self.x0 + self.w);
        let min_y = floor_clamped(cy - pad, self.y0, self.y0 + self.h);
        let max_y = ceil_clamped(cy + pad, self.y0, self.y0 + self.h);
        let rgb = [rgba[0], rgba[1], rgba[2]];

        for py in min_y..max_y {
            let dy = (usize_to_f64(py) + 0.5) - cy;
            for px in min_x..max_x {
                let dx = (usize_to_f64(px) + 0.5) - cx;
                let dist = dx.hypot(dy);
                let cov = smooth_coverage(radius - dist);
                if cov > 0.0 {
                    self.blend_over(px, py, rgb, rgba[3] * cov);
                }
            }
        }
    }

    fn draw_ring(&mut self, cx: f64, cy: f64, radius: f64, thickness: f64, rgba: [f32; 4]) {
        if radius <= 0.0 || thickness <= 0.0 || rgba[3] <= 0.001 {
            return;
        }
        let half_w = thickness * 0.5;
        let pad = radius + half_w + 1.5;
        let min_x = floor_clamped(cx - pad, self.x0, self.x0 + self.w);
        let max_x = ceil_clamped(cx + pad, self.x0, self.x0 + self.w);
        let min_y = floor_clamped(cy - pad, self.y0, self.y0 + self.h);
        let max_y = ceil_clamped(cy + pad, self.y0, self.y0 + self.h);
        let rgb = [rgba[0], rgba[1], rgba[2]];

        for py in min_y..max_y {
            let dy = (usize_to_f64(py) + 0.5) - cy;
            for px in min_x..max_x {
                let dx = (usize_to_f64(px) + 0.5) - cx;
                let dist = (dx.hypot(dy) - radius).abs();
                let cov = smooth_coverage(half_w - dist);
                if cov > 0.0 {
                    self.blend_over(px, py, rgb, rgba[3] * cov);
                }
            }
        }
    }

    fn draw_segment(&mut self, start: (f64, f64), end: (f64, f64), thickness: f64, rgba: [f32; 4]) {
        if thickness <= 0.0 || rgba[3] <= 0.001 {
            return;
        }
        let (x0, y0) = start;
        let (x1, y1) = end;
        let half_w = thickness * 0.5;
        let pad = half_w + 1.5;
        let min_x = floor_clamped(x0.min(x1) - pad, self.x0, self.x0 + self.w);
        let max_x = ceil_clamped(x0.max(x1) + pad, self.x0, self.x0 + self.w);
        let min_y = floor_clamped(y0.min(y1) - pad, self.y0, self.y0 + self.h);
        let max_y = ceil_clamped(y0.max(y1) + pad, self.y0, self.y0 + self.h);
        let rgb = [rgba[0], rgba[1], rgba[2]];

        let span_x = x1 - x0;
        let span_y = y1 - y0;
        let len_sq = span_x * span_x + span_y * span_y;

        for py in min_y..max_y {
            let fy = usize_to_f64(py) + 0.5;
            for px in min_x..max_x {
                let fx = usize_to_f64(px) + 0.5;
                let t = if len_sq > 1e-6 {
                    (((fx - x0) * span_x + (fy - y0) * span_y) / len_sq).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let proj_x = x0 + t * span_x;
                let proj_y = y0 + t * span_y;
                let dist = (fx - proj_x).hypot(fy - proj_y);
                let cov = smooth_coverage(half_w - dist);
                if cov > 0.0 {
                    self.blend_over(px, py, rgb, rgba[3] * cov);
                }
            }
        }
    }
}

#[inline]
fn smooth_coverage(signed_dist: f64) -> f32 {
    f64_to_f32_unit(((signed_dist + 0.6) / 1.2).clamp(0.0, 1.0))
}

fn interpolate_swipe_position(ev: &MappedTouchEvent, frame_t_ns: i64) -> (f64, f64) {
    if frame_t_ns <= ev.start_t_ns {
        return ev.start;
    }
    if frame_t_ns >= ev.end_t_ns || ev.polyline.len() < 2 {
        return ev.end;
    }
    let rel_ns = frame_t_ns.saturating_sub(ev.start_t_ns);
    for pair in ev.polyline.windows(2) {
        let (x0, y0, t0) = pair[0];
        let (x1, y1, t1) = pair[1];
        if rel_ns <= t1 {
            let span = i64_to_f64((t1 - t0).max(1));
            let frac = (i64_to_f64(rel_ns - t0) / span).clamp(0.0, 1.0);
            return (x0 + (x1 - x0) * frac, y0 + (y1 - y0) * frac);
        }
    }
    ev.end
}

fn swipe_end_direction(ev: &MappedTouchEvent) -> Option<(f64, f64)> {
    let &(ex, ey, _) = ev.polyline.last()?;
    for &(px, py, _) in ev.polyline.iter().rev().skip(1) {
        let dx = ex - px;
        let dy = ey - py;
        let len = dx.hypot(dy);
        if len > 1e-3 {
            return Some((dx / len, dy / len));
        }
    }
    let dx = ev.end.0 - ev.start.0;
    let dy = ev.end.1 - ev.start.1;
    let len = dx.hypot(dy);
    (len > 1e-3).then_some((dx / len, dy / len))
}

fn compute_active_bbox(
    active: &[&MappedTouchEvent],
    video_w: usize,
    video_h: usize,
    unit: f64,
) -> Option<(usize, usize, usize, usize)> {
    if active.is_empty() || video_w == 0 || video_h == 0 {
        return None;
    }
    let margin = (44.0 * unit).max(16.0);
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    let mut include = |x: f64, y: f64| {
        min_x = min_x.min(x - margin);
        min_y = min_y.min(y - margin);
        max_x = max_x.max(x + margin);
        max_y = max_y.max(y + margin);
    };

    for ev in active {
        include(ev.start.0, ev.start.1);
        include(ev.end.0, ev.end.1);
        for &(px, py, _) in &ev.polyline {
            include(px, py);
        }
    }

    let x0 = floor_clamped(min_x, 0, video_w) & !1;
    let y0 = floor_clamped(min_y, 0, video_h) & !1;
    let x1 = ceil_clamped(max_x, 0, video_w).min(video_w);
    let y1 = ceil_clamped(max_y, 0, video_h).min(video_h);
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

fn rasterize_frame_overlay(
    events: &[MappedTouchEvent],
    frame_t_ns: i64,
    video_w: usize,
    video_h: usize,
) -> Option<OverlayBox> {
    let active: Vec<&MappedTouchEvent> = events
        .iter()
        .filter(|ev| ev.is_active_at(frame_t_ns))
        .collect();
    if active.is_empty() {
        return None;
    }

    let min_dim = usize_to_f64(video_w.min(video_h));
    let unit = (min_dim / 720.0).clamp(0.45, 2.5);
    let (x0, y0, x1, y1) = compute_active_bbox(&active, video_w, video_h, unit)?;
    let mut canvas = OverlayBox::new(x0, y0, x1, y1);

    for ev in active {
        match ev.kind {
            GestureKind::Swipe => rasterize_swipe(&mut canvas, ev, frame_t_ns, unit),
            _ => rasterize_tap(&mut canvas, ev, frame_t_ns, unit),
        }
    }

    Some(canvas)
}

fn rasterize_tap(canvas: &mut OverlayBox, ev: &MappedTouchEvent, frame_t_ns: i64, unit: f64) {
    let elapsed = i64_to_f64(frame_t_ns.saturating_sub(ev.start_t_ns).max(0));
    let progress = (elapsed / i64_to_f64(TAP_FADE_DURATION_NS)).clamp(0.0, 1.0);
    let fade = f64_to_f32_unit((1.0 - progress).powf(1.3));
    if fade <= 0.01 {
        return;
    }

    let (cx, cy) = ev.start;
    let outer_r = (16.0 + progress * 22.0) * unit;
    let dot_r = 5.5 * unit;

    canvas.draw_filled_circle(cx, cy, outer_r, [1.0, 0.77, 0.18, 0.22 * fade]);
    canvas.draw_ring(cx, cy, outer_r, 6.0 * unit, [0.05, 0.07, 0.10, 0.56 * fade]);
    canvas.draw_ring(cx, cy, outer_r, 3.2 * unit, [1.0, 0.91, 0.52, 0.94 * fade]);
    canvas.draw_filled_circle(cx, cy, dot_r + 1.8 * unit, [0.05, 0.07, 0.10, 0.62 * fade]);
    canvas.draw_filled_circle(cx, cy, dot_r, [1.0, 1.0, 1.0, 0.96 * fade]);
}

fn rasterize_swipe(canvas: &mut OverlayBox, ev: &MappedTouchEvent, frame_t_ns: i64, unit: f64) {
    let fade = if frame_t_ns <= ev.end_t_ns {
        1.0_f32
    } else {
        let after = i64_to_f64(frame_t_ns.saturating_sub(ev.end_t_ns).max(0));
        let progress = (after / i64_to_f64(SWIPE_FADE_DURATION_NS)).clamp(0.0, 1.0);
        f64_to_f32_unit((1.0 - progress).powf(1.3))
    };
    if fade <= 0.01 {
        return;
    }

    let halo_rgba = [0.05, 0.07, 0.10, 0.55 * fade];
    let stroke_rgba = [1.0, 0.80, 0.25, 0.94 * fade];
    let halo_w = 6.6 * unit;
    let stroke_w = 3.4 * unit;

    for seg in ev.polyline.windows(2) {
        canvas.draw_segment(
            (seg[0].0, seg[0].1),
            (seg[1].0, seg[1].1),
            halo_w,
            halo_rgba,
        );
    }
    for seg in ev.polyline.windows(2) {
        canvas.draw_segment(
            (seg[0].0, seg[0].1),
            (seg[1].0, seg[1].1),
            stroke_w,
            stroke_rgba,
        );
    }

    if let Some((ux, uy)) = swipe_end_direction(ev) {
        let (ex, ey) = ev.end;
        let head_len = 15.0 * unit;
        let head_angle = std::f64::consts::PI / 6.0;
        let cos_a = head_angle.cos();
        let sin_a = head_angle.sin();
        let left_tip = (
            ex - head_len * (ux * cos_a - uy * sin_a),
            ey - head_len * (ux * sin_a + uy * cos_a),
        );
        let right_tip = (
            ex - head_len * (ux * cos_a + uy * sin_a),
            ey - head_len * (-ux * sin_a + uy * cos_a),
        );
        canvas.draw_segment((ex, ey), left_tip, halo_w, halo_rgba);
        canvas.draw_segment((ex, ey), right_tip, halo_w, halo_rgba);
        canvas.draw_segment((ex, ey), left_tip, stroke_w, stroke_rgba);
        canvas.draw_segment((ex, ey), right_tip, stroke_w, stroke_rgba);
    }

    canvas.draw_filled_circle(ev.start.0, ev.start.1, 5.5 * unit, halo_rgba);
    canvas.draw_filled_circle(ev.start.0, ev.start.1, 3.8 * unit, stroke_rgba);

    let (mx, my) = interpolate_swipe_position(ev, frame_t_ns);
    canvas.draw_filled_circle(mx, my, 9.0 * unit, [0.05, 0.07, 0.10, 0.62 * fade]);
    canvas.draw_filled_circle(mx, my, 7.0 * unit, [1.0, 0.85, 0.35, 0.96 * fade]);
    canvas.draw_filled_circle(mx, my, 4.0 * unit, [1.0, 1.0, 1.0, 0.98 * fade]);
}

#[inline]
fn rgb_to_bt601_studio_yuv(red: f32, green: f32, blue: f32) -> (f32, f32, f32) {
    let r255 = red.clamp(0.0, 1.0) * 255.0;
    let g255 = green.clamp(0.0, 1.0) * 255.0;
    let b255 = blue.clamp(0.0, 1.0) * 255.0;
    let luma = 16.0 + 0.257 * r255 + 0.504 * g255 + 0.098 * b255;
    let chroma_u = 128.0 - 0.148 * r255 - 0.291 * g255 + 0.439 * b255;
    let chroma_v = 128.0 + 0.439 * r255 - 0.368 * g255 - 0.071 * b255;
    (
        luma.clamp(16.0, 235.0),
        chroma_u.clamp(16.0, 240.0),
        chroma_v.clamp(16.0, 240.0),
    )
}

#[inline]
fn blend_channel(dst: u8, src: f32, alpha: f32) -> u8 {
    f32_to_u8_clamped(src * alpha + f32::from(dst) * (1.0 - alpha))
}

fn composite_overlay_onto_yuv(overlay: &OverlayBox, frame: &mut MutableYuvFrame<'_>) {
    let width = frame.width();
    let height = frame.height();
    if width == 0 || height == 0 || overlay.w == 0 || overlay.h == 0 {
        return;
    }

    // Pass 1: full-resolution luma (Y) blending.
    for ly in 0..overlay.h {
        let gy = overlay.y0 + ly;
        if gy >= height {
            break;
        }
        let row_base = ly * overlay.w;
        for lx in 0..overlay.w {
            let gx = overlay.x0 + lx;
            if gx >= width {
                break;
            }
            let px = overlay.pixels[row_base + lx];
            let alpha = px[3];
            if alpha <= 0.002 {
                continue;
            }
            let inv_a = 1.0 / alpha;
            let (ov_y, _, _) = rgb_to_bt601_studio_yuv(px[0] * inv_a, px[1] * inv_a, px[2] * inv_a);
            if let Some(y_cell) = frame.y_mut_at(gx, gy) {
                *y_cell = blend_channel(*y_cell, ov_y, alpha);
            }
        }
    }

    // Pass 2: 2x2-averaged chroma (U/V) blending (safe for odd width/height).
    let col_start = overlay.x0 / 2;
    let row_start = overlay.y0 / 2;
    let col_end = (overlay.x0 + overlay.w).div_ceil(2).min(width.div_ceil(2));
    let row_end = (overlay.y0 + overlay.h).div_ceil(2).min(height.div_ceil(2));

    for chroma_row in row_start..row_end {
        for chroma_col in col_start..col_end {
            let mut sum_r = 0.0_f32;
            let mut sum_g = 0.0_f32;
            let mut sum_b = 0.0_f32;
            let mut sum_a = 0.0_f32;
            let mut count = 0.0_f32;
            for dy in 0..2 {
                let gy = chroma_row * 2 + dy;
                if gy < overlay.y0 || gy >= overlay.y0 + overlay.h || gy >= height {
                    continue;
                }
                for dx in 0..2 {
                    let gx = chroma_col * 2 + dx;
                    if gx < overlay.x0 || gx >= overlay.x0 + overlay.w || gx >= width {
                        continue;
                    }
                    let px = overlay.pixels[(gy - overlay.y0) * overlay.w + (gx - overlay.x0)];
                    sum_r += px[0];
                    sum_g += px[1];
                    sum_b += px[2];
                    sum_a += px[3];
                    count += 1.0;
                }
            }
            if count <= 0.0 || sum_a <= 0.008 {
                continue;
            }
            let avg_alpha = (sum_a / count).clamp(0.0, 1.0);
            let inv_a = 1.0 / sum_a;
            let (_, ov_u, ov_v) =
                rgb_to_bt601_studio_yuv(sum_r * inv_a, sum_g * inv_a, sum_b * inv_a);
            frame.blend_uv_at(chroma_col, chroma_row, ov_u, ov_v, avg_alpha);
        }
    }
}

/// Composites all touch overlays active at `frame_t_ns` directly onto an [`RgbImage`] in place.
///
/// Returns `true` when at least one active touch overlay touched this frame.
pub fn composite_touch_overlays_rgb24(
    rgb: &mut RgbImage,
    events: &[BurnInTouchEvent],
    frame_t_ns: i64,
    display_geometry: BurnInDisplayGeometry,
) -> bool {
    let (Ok(width), Ok(height)) = (usize::try_from(rgb.width), usize::try_from(rgb.height)) else {
        return false;
    };
    if width.checked_mul(height).and_then(|n| n.checked_mul(3)) != Some(rgb.data.len()) {
        return false;
    }
    let mapped = map_events(events, rgb.width, rgb.height, display_geometry);
    let Some(overlay) = rasterize_frame_overlay(&mapped, frame_t_ns, width, height) else {
        return false;
    };

    for ly in 0..overlay.h {
        let gy = overlay.y0 + ly;
        if gy >= height {
            break;
        }
        let row_base = ly * overlay.w;
        for lx in 0..overlay.w {
            let gx = overlay.x0 + lx;
            if gx >= width {
                break;
            }
            let px = overlay.pixels[row_base + lx];
            let alpha = px[3];
            if alpha <= 0.002 {
                continue;
            }
            let inv_a = 1.0 / alpha;
            let dst = (gy * width + gx) * 3;
            if let Some(slot) = rgb.data.get_mut(dst..dst + 3) {
                slot[0] = blend_channel(slot[0], (px[0] * inv_a) * 255.0, alpha);
                slot[1] = blend_channel(slot[1], (px[1] * inv_a) * 255.0, alpha);
                slot[2] = blend_channel(slot[2], (px[2] * inv_a) * 255.0, alpha);
            }
        }
    }
    true
}

fn estimate_encode_params(track: &Mp4VideoTrack) -> EncodeParams {
    let mut params = EncodeParams::default();
    let frame_count = track.frame_count();
    let timescale = u64::from(track.timescale().max(1));
    let dur_ticks = track.duration_ticks();
    if frame_count > 1 && dur_ticks > 0 {
        let fps = ((u128::from(frame_count) * u128::from(timescale)) / u128::from(dur_ticks))
            .clamp(1, 120);
        if let Ok(fps_u32) = u32::try_from(fps) {
            params.max_fps = fps_u32;
        }
    }
    params
}

/// Trims `[in_frame_index..=out_frame_index]` from `input_mp4` and burns in all overlapping
/// `Tap` and `Swipe` gestures from `events` onto a brand-new faststart zero-`elst` MP4 at
/// `output_mp4`.
///
/// `frame_timestamps_ns` must contain the aligned `DeviceBoottime` timestamp for each frame
/// index `0..track.frame_count()` (or at least cover `in_frame_index..=out_frame_index`).
///
/// Fast-path optimization: if zero mappable touch events overlap `[in_frame_index..=out_frame_index]`,
/// skips full re-encoding and delegates directly to lossless [`trim_video`].
///
/// # Errors
///
/// Returns [`MediaError`] on invalid frame ranges, B-frame inputs, codec errors, or I/O errors.
#[expect(
    clippy::too_many_arguments,
    reason = "explicit inputs keep burn-in transcoding deterministic and free of hidden state"
)]
pub fn burn_in_taps_video(
    input_mp4: impl AsRef<Path>,
    output_mp4: impl AsRef<Path>,
    in_frame_index: u64,
    out_frame_index: u64,
    frame_timestamps_ns: &[i64],
    events: &[BurnInTouchEvent],
    display_geometry: BurnInDisplayGeometry,
    backend: &dyn VideoCodecBackend,
) -> Result<BurnInReport, MediaError> {
    let input_path = input_mp4.as_ref();
    let output_path = output_mp4.as_ref();
    ensure_output_is_not_input(input_path, output_path)?;

    let track = Mp4VideoTrack::open(input_path)?;
    let frame_count = track.frame_count();
    if frame_count == 0 || in_frame_index > out_frame_index || out_frame_index >= frame_count {
        return Err(MediaError::InvalidFrameRange {
            in_frame_index,
            out_frame_index,
            frame_count,
        });
    }
    track.require_no_b_frames("burn_in_taps_video")?;

    let video_w = u32::from(track.width());
    let video_h = u32::from(track.height());
    let mapped_events = map_events(events, video_w, video_h, display_geometry);

    let frame_ts = |frame_idx: u64| -> Option<i64> {
        usize::try_from(frame_idx)
            .ok()
            .and_then(|i| frame_timestamps_ns.get(i).copied())
    };

    let any_overlap = (in_frame_index..=out_frame_index).any(|idx| {
        frame_ts(idx).is_some_and(|t_ns| mapped_events.iter().any(|ev| ev.is_active_at(t_ns)))
    });

    if !any_overlap {
        let trim_report = trim_video(
            input_path,
            output_path,
            in_frame_index,
            out_frame_index,
            backend,
        )?;
        return Ok(BurnInReport {
            output_frame_count: trim_report.output_frame_count,
            burned_frame_count: 0,
            reencoded: false,
            backend_name: trim_report.backend_name,
            edts_box_count: trim_report.edts_box_count,
            elst_box_count: trim_report.elst_box_count,
        });
    }

    let encode_params = estimate_encode_params(&track);
    let mut burned_frame_count = 0u64;
    let has_overlay = |frame_idx: u64| -> bool {
        frame_ts(frame_idx).is_some_and(|t_ns| mapped_events.iter().any(|ev| ev.is_active_at(t_ns)))
    };
    let mut apply_overlay = |frame_idx: u64, frame: &mut MutableYuvFrame<'_>| {
        let Some(t_ns) = frame_ts(frame_idx) else {
            return;
        };
        if let Some(overlay) =
            rasterize_frame_overlay(&mapped_events, t_ns, frame.width(), frame.height())
        {
            composite_overlay_onto_yuv(&overlay, frame);
            burned_frame_count += 1;
        }
    };

    let encoded = backend.reencode_frame_range_with_overlay(
        &track,
        in_frame_index,
        out_frame_index + 1,
        encode_params,
        DEFAULT_BURN_IN_KEYFRAME_INTERVAL,
        &has_overlay,
        &mut apply_overlay,
    )?;

    let samples: Vec<MuxSample> = encoded
        .samples
        .into_iter()
        .map(|sm| MuxSample {
            data: sm.avcc_data,
            duration_ticks: sm.duration_ticks,
            is_keyframe: sm.is_keyframe,
        })
        .collect();
    let output_frame_count = u64::try_from(samples.len()).unwrap_or(0);
    let segment = MuxSegment {
        avcc_payload: encoded.avcc_payload,
        samples,
    };

    let copied = write_trim_mp4(output_path, &track, Some(&segment), None)?;
    debug_assert_eq!(copied, 0, "full-range burn-in writes no stream-copied tail");

    Ok(BurnInReport {
        output_frame_count,
        burned_frame_count,
        reencoded: true,
        backend_name: backend.name().to_owned(),
        edts_box_count: 0,
        elst_box_count: 0,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{
        BurnInDisplayGeometry, BurnInTouchEvent, burn_in_taps_video, composite_touch_overlays_rgb24,
    };
    use crate::codec::{
        DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat, RgbImage, default_codec_backend,
    };
    use crate::mp4::{Mp4VideoTrack, inspect_mp4_atoms, mux_raw_h264_to_mp4};
    use arb_core::gesture::{GestureKind, TouchPoint};

    fn solid_rgb(width: u32, height: u32, color: (u8, u8, u8)) -> RgbImage {
        let count = usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(height).unwrap_or(0));
        let mut data = Vec::with_capacity(count * 3);
        for _ in 0..count {
            data.extend_from_slice(&[color.0, color.1, color.2]);
        }
        RgbImage {
            width,
            height,
            data,
        }
    }

    fn pixel_rgb(img: &RgbImage, x: u32, y: u32) -> (u8, u8, u8) {
        let idx = (usize::try_from(y).unwrap_or(0) * usize::try_from(img.width).unwrap_or(0)
            + usize::try_from(x).unwrap_or(0))
            * 3;
        (img.data[idx], img.data[idx + 1], img.data[idx + 2])
    }

    fn rgb_diff(a: (u8, u8, u8), b: (u8, u8, u8)) -> i32 {
        (i32::from(a.0) - i32::from(b.0)).abs()
            + (i32::from(a.1) - i32::from(b.1)).abs()
            + (i32::from(a.2) - i32::from(b.2)).abs()
    }

    fn build_uniform_video(path: &std::path::Path, width: u32, height: u32, frame_count: u64) {
        let w_us = usize::try_from(width).expect("width usize");
        let h_us = usize::try_from(height).expect("height usize");
        let y_len = w_us * h_us;
        let uv_len = (w_us / 2) * (h_us / 2);
        let mut frames = Vec::with_capacity(usize::try_from(frame_count).unwrap_or(0));
        for i in 0..frame_count {
            let mut yuv = vec![40u8; y_len + uv_len * 2];
            yuv[y_len..].fill(128);
            frames.push(DecodedFrame {
                frame_index: i,
                width,
                height,
                format: PixelFormat::Yuv420p,
                data: yuv,
            });
        }
        let openh264 = OpenH264Backend::new();
        let packets = openh264
            .encode_yuv420p_sequence(&frames, 15, 33_333_333, EncodeParams::default())
            .expect("encode fixture");
        mux_raw_h264_to_mp4(
            path,
            u16::try_from(width).expect("u16 w"),
            u16::try_from(height).expect("u16 h"),
            90_000,
            3_000,
            &packets,
        )
        .expect("mux fixture");
    }

    #[test]
    fn openh264_emits_frames_before_reading_the_end_of_the_gop() {
        use crate::codec::VideoCodecBackend;
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("stream.mp4");
        build_uniform_video(&src, 64, 64, 15);
        let track = Mp4VideoTrack::open(&src).unwrap();
        let cut = track.frame(12).unwrap().offset;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&src)
            .unwrap()
            .set_len(cut)
            .unwrap();
        let mut painted = 0;
        let result = OpenH264Backend::new().reencode_frame_range_with_overlay(
            &track,
            0,
            15,
            EncodeParams::default(),
            15,
            &|_| true,
            &mut |_, _| painted += 1,
        );
        assert!(result.is_err());
        assert!(painted > 0);
    }

    #[test]
    fn tap_ripple_and_swipe_trajectory_composite_pixels_and_clamp_odd_dimensions() {
        let mut img = solid_rgb(129, 129, (20, 20, 20));
        let events = vec![
            BurnInTouchEvent {
                kind: GestureKind::Tap,
                start_t_ns: 1_000_000_000,
                end_t_ns: 1_040_000_000,
                x: 64.0,
                y: 64.0,
                end_x: None,
                end_y: None,
                path: Vec::new(),
            },
            BurnInTouchEvent {
                kind: GestureKind::Swipe,
                start_t_ns: 1_000_000_000,
                end_t_ns: 1_200_000_000,
                x: 20.0,
                y: 100.0,
                end_x: Some(110.0),
                end_y: Some(100.0),
                path: vec![
                    TouchPoint {
                        x: 20.0,
                        y: 100.0,
                        t_offset_ns: 0,
                    },
                    TouchPoint {
                        x: 65.0,
                        y: 100.0,
                        t_offset_ns: 100_000_000,
                    },
                    TouchPoint {
                        x: 110.0,
                        y: 100.0,
                        t_offset_ns: 200_000_000,
                    },
                ],
            },
        ];
        let mut truncated = RgbImage {
            width: 129,
            height: 129,
            data: vec![],
        };
        assert!(!composite_touch_overlays_rgb24(
            &mut truncated,
            &events,
            1_000_000_000,
            BurnInDisplayGeometry {
                display_width: Some(129),
                display_height: Some(129)
            }
        ));

        let changed = composite_touch_overlays_rgb24(
            &mut img,
            &events,
            1_100_000_000,
            BurnInDisplayGeometry {
                display_width: Some(129),
                display_height: Some(129),
            },
        );
        assert!(changed, "overlay must activate at t=1.1s");
        assert!(
            rgb_diff(pixel_rgb(&img, 64, 64), (20, 20, 20)) > 150,
            "tap center must brighten clearly on dark background"
        );
        assert!(
            rgb_diff(pixel_rgb(&img, 65, 100), (20, 20, 20)) > 150,
            "swipe midpoint must draw moving indicator/polyline"
        );
        assert_eq!(
            pixel_rgb(&img, 2, 2),
            (20, 20, 20),
            "untouched corner must stay exact"
        );

        let mut after = solid_rgb(129, 129, (240, 240, 240));
        let changed_after = composite_touch_overlays_rgb24(
            &mut after,
            &events,
            2_000_000_000,
            BurnInDisplayGeometry {
                display_width: Some(129),
                display_height: Some(129),
            },
        );
        assert!(!changed_after, "expired touch events must not render");
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "end-to-end burn-in test verifies both 0-touch fast path and tap+swipe pixel output"
    )]
    fn burn_in_taps_video_fast_path_and_real_pixel_burn_in_with_zero_elst() {
        let tmp = TempDir::new().expect("temp dir");
        let src_mp4 = tmp.path().join("src.mp4");
        build_uniform_video(&src_mp4, 160, 160, 30);

        let base_t = 10_000_000_000_i64;
        let step_ns = 33_333_333_i64;
        let frame_ts: Vec<i64> = (0..30).map(|i| base_t + i * step_ns).collect();
        let backend = default_codec_backend();

        // 1. Fast path when zero touches overlap the trimmed range.
        let fast_mp4 = tmp.path().join("fast.mp4");
        let fast_report = burn_in_taps_video(
            &src_mp4,
            &fast_mp4,
            0,
            14,
            &frame_ts,
            &[],
            BurnInDisplayGeometry::default(),
            backend.as_ref(),
        )
        .expect("0-touch burn-in fast path");
        assert!(!fast_report.reencoded, "0 touches must skip full re-encode");
        assert_eq!(fast_report.burned_frame_count, 0);
        assert_eq!(fast_report.output_frame_count, 15);
        assert_eq!(fast_report.edts_box_count, 0);
        assert_eq!(fast_report.elst_box_count, 0);

        // 2. Real burn-in with both Tap (scaled from 1080x1080 display -> center 80,80) and Swipe.
        let burned_mp4 = tmp.path().join("burned.mp4");
        let events = vec![
            BurnInTouchEvent {
                kind: GestureKind::Tap,
                start_t_ns: frame_ts[5],
                end_t_ns: frame_ts[6],
                x: 540.0,
                y: 540.0,
                end_x: None,
                end_y: None,
                path: Vec::new(),
            },
            BurnInTouchEvent {
                kind: GestureKind::Swipe,
                start_t_ns: frame_ts[10],
                end_t_ns: frame_ts[14],
                x: 135.0,
                y: 810.0,
                end_x: Some(945.0),
                end_y: Some(810.0),
                path: vec![
                    TouchPoint {
                        x: 135.0,
                        y: 810.0,
                        t_offset_ns: 0,
                    },
                    TouchPoint {
                        x: 540.0,
                        y: 810.0,
                        t_offset_ns: 2 * step_ns,
                    },
                    TouchPoint {
                        x: 945.0,
                        y: 810.0,
                        t_offset_ns: 4 * step_ns,
                    },
                ],
            },
        ];

        let report = burn_in_taps_video(
            &src_mp4,
            &burned_mp4,
            3,
            22,
            &frame_ts,
            &events,
            BurnInDisplayGeometry {
                display_width: Some(1080),
                display_height: Some(1080),
            },
            backend.as_ref(),
        )
        .expect("burn-in transcode");
        assert!(report.reencoded, "active touches must trigger burn-in");
        assert!(
            report.burned_frame_count >= 10,
            "expected >=10 burned frames"
        );
        assert_eq!(report.output_frame_count, 20, "frames 3..=22 = 20 frames");
        assert_eq!(report.edts_box_count, 0);
        assert_eq!(report.elst_box_count, 0);

        let atoms = inspect_mp4_atoms(&burned_mp4).expect("inspect burned MP4");
        assert_eq!(atoms.edts_count, 0, "burned MP4 must have edts=0");
        assert_eq!(atoms.elst_count, 0, "burned MP4 must have elst=0");

        let burned_track = Mp4VideoTrack::open(&burned_mp4).expect("open burned track");
        assert_eq!(burned_track.frame_count(), 20);

        // Frame 2 in output == source frame 5 (tap start): center (80,80) must be bright vs corner (4,4).
        let tap_frame = backend
            .decode_frame(&burned_track, 2, PixelFormat::Rgb24)
            .expect("decode tap frame")
            .to_rgb24()
            .expect("rgb24");
        let corner = pixel_rgb(&tap_frame, 4, 4);
        let tap_center = pixel_rgb(&tap_frame, 80, 80);
        assert!(
            rgb_diff(tap_center, corner) > 150,
            "burned tap center {tap_center:?} must clearly differ from dark background {corner:?}"
        );

        // Frame 9 in output == source frame 12 (swipe midpoint): (80, 120) must be bright vs corner (4,4).
        let swipe_frame = backend
            .decode_frame(&burned_track, 9, PixelFormat::Rgb24)
            .expect("decode swipe frame")
            .to_rgb24()
            .expect("rgb24");
        let swipe_mid = pixel_rgb(&swipe_frame, 80, 120);
        assert!(
            rgb_diff(swipe_mid, corner) > 150,
            "burned swipe midpoint {swipe_mid:?} must clearly differ from background {corner:?}"
        );
    }
}
