//! Deterministic PC-side `Tap` vs `Swipe` gesture classification (zero extra Android device
//! load: every classification below runs entirely on recorded host-side touch samples).
//!
//! Owner's real Android Studio trial feedback (T-6H item 2): a swipe used to be recorded as a
//! raw sequence of `DOWN`/`MOVE`/`UP` touch samples and rendered as a flood of individual taps.
//! [`classify_stroke`] takes one fully-captured touch stroke (`DOWN -> MOVE* -> UP`) and collapses
//! it into exactly one [`GestureKind::Tap`] or [`GestureKind::Swipe`] classification.

use serde::{Deserialize, Serialize};

/// Maximum number of decimated trajectory points retained in a classified [`GestureKind::Swipe`]
/// path (`<= 24` keeps saved bundles and wire payloads compact regardless of sampling rate).
pub const MAX_PATH_POINTS: usize = 24;

/// Fallback touch slop (in pixels / raw touch-panel units) used when the display's physical
/// width/height are unknown at classification time.
pub const FALLBACK_TOUCH_SLOP: f64 = 24.0;

/// Minimum touch slop floor (in pixels), even on very small or unknown displays.
pub const MIN_TOUCH_SLOP: f64 = 20.0;

/// Fraction of `min(display_width, display_height)` used to derive a resolution-aware touch slop.
pub const TOUCH_SLOP_DISPLAY_FRACTION: f64 = 0.025;

/// High-level classification of one fully-captured touch stroke.
///
/// `#[serde(default)]`-friendly: [`Default`] resolves to [`GestureKind::Tap`] so every legacy
/// saved bundle (and every legacy schema 3/4 import) that predates this field deserializes
/// cleanly as a `Tap`, which matches that legacy data's single-point-sample shape exactly.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureKind {
    /// A short touch with negligible travel distance; recorded as a single point.
    #[default]
    Tap,
    /// A touch that traveled at least [`touch_slop`] from its `DOWN` point; recorded with an
    /// end point and a decimated trajectory [`TouchPoint`] path.
    Swipe,
}

/// One sample along a classified [`GestureKind::Swipe`] trajectory.
///
/// `t_offset_ns` is nanoseconds elapsed since the stroke's `DOWN` sample (always `>= 0`), kept
/// relative so the path travels cleanly with the tap's existing device-boottime alignment without
/// needing a second independent time-alignment computation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TouchPoint {
    /// X coordinate (same unit/space as the owning tap's `x`/`y`, typically display pixels).
    pub x: f64,
    /// Y coordinate (same unit/space as the owning tap's `x`/`y`).
    pub y: f64,
    /// Nanoseconds elapsed since the stroke's `DOWN` sample.
    pub t_offset_ns: i64,
}

/// Result of classifying one complete `DOWN -> MOVE* -> UP` touch stroke.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifiedStroke {
    /// `Tap` or `Swipe`.
    pub kind: GestureKind,
    /// Final sample's X coordinate.
    pub end_x: f64,
    /// Final sample's Y coordinate.
    pub end_y: f64,
    /// Final sample's `t_offset_ns` (nanoseconds since `DOWN`; i.e. the stroke's total duration).
    pub end_t_offset_ns: i64,
    /// Decimated trajectory (at most [`MAX_PATH_POINTS`] points), empty for `Tap`.
    pub path: Vec<TouchPoint>,
}

/// Resolution-aware touch slop threshold in the same unit as [`TouchPoint::x`]/`y`
/// (`max(MIN_TOUCH_SLOP, TOUCH_SLOP_DISPLAY_FRACTION * min(display_width, display_height))`
/// when both display dimensions are known and positive; otherwise [`FALLBACK_TOUCH_SLOP`]).
#[must_use]
pub fn touch_slop(display_width: Option<f64>, display_height: Option<f64>) -> f64 {
    match (display_width, display_height) {
        (Some(w), Some(h)) if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 => {
            (TOUCH_SLOP_DISPLAY_FRACTION * w.min(h)).max(MIN_TOUCH_SLOP)
        }
        _ => FALLBACK_TOUCH_SLOP,
    }
}

/// Classifies one complete touch stroke's ordered samples (first = `DOWN`, last = `UP`/`CANCEL`)
/// against [`slop`](touch_slop). `samples` MUST be non-empty; a single-sample stroke (no `MOVE`
/// at all) is always a [`GestureKind::Tap`].
///
/// Distance uses both the direct `DOWN -> UP` displacement AND the maximum excursion from `DOWN`
/// across every sample, so a swipe-and-return gesture (large excursion, small net displacement)
/// is still correctly classified as a [`GestureKind::Swipe`].
///
/// # Panics
///
/// Panics if `samples` is empty.
#[must_use]
pub fn classify_stroke(samples: &[TouchPoint], slop: f64) -> ClassifiedStroke {
    assert!(!samples.is_empty(), "classify_stroke requires >= 1 sample");
    let start = samples[0];
    let end = *samples.last().expect("non-empty samples");

    let net_dist = hypot(end.x - start.x, end.y - start.y);
    let max_excursion = samples
        .iter()
        .map(|p| hypot(p.x - start.x, p.y - start.y))
        .fold(0.0_f64, f64::max);

    let kind = if net_dist < slop && max_excursion < slop {
        GestureKind::Tap
    } else {
        GestureKind::Swipe
    };

    let path = match kind {
        GestureKind::Tap => Vec::new(),
        GestureKind::Swipe => decimate(samples, MAX_PATH_POINTS),
    };

    ClassifiedStroke {
        kind,
        end_x: end.x,
        end_y: end.y,
        end_t_offset_ns: end.t_offset_ns,
        path,
    }
}

fn hypot(dx: f64, dy: f64) -> f64 {
    dx.hypot(dy)
}

/// Decimates `samples` down to at most `cap` points using an even stride, always keeping the
/// first and last sample so the trajectory's exact `DOWN`/`UP` endpoints are preserved.
fn decimate(samples: &[TouchPoint], cap: usize) -> Vec<TouchPoint> {
    if samples.len() <= cap || cap <= 1 {
        return samples.to_vec();
    }
    let last_idx = samples.len() - 1;
    (0..cap)
        .map(|i| samples[i * last_idx / (cap - 1)])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ClassifiedStroke, GestureKind, MAX_PATH_POINTS, TouchPoint, classify_stroke, touch_slop,
    };

    fn pt(x: f64, y: f64, t_offset_ns: i64) -> TouchPoint {
        TouchPoint { x, y, t_offset_ns }
    }

    #[test]
    fn gesture_kind_default_is_tap_for_backward_compatible_deserialize() {
        assert_eq!(GestureKind::default(), GestureKind::Tap);
    }

    #[test]
    fn touch_slop_scales_with_display_resolution_but_floors_at_min() {
        // 2.5% of min(1080, 2400) = 27.0, above the 20.0 floor.
        assert!((touch_slop(Some(1080.0), Some(2400.0)) - 27.0).abs() < 1e-9);
        // Tiny display: 2.5% of 100 = 2.5, floored to MIN_TOUCH_SLOP (20.0).
        assert!((touch_slop(Some(100.0), Some(100.0)) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn touch_slop_falls_back_when_display_dims_unknown_or_invalid() {
        assert!((touch_slop(None, None) - 24.0).abs() < 1e-9);
        assert!((touch_slop(Some(1080.0), None) - 24.0).abs() < 1e-9);
        assert!((touch_slop(Some(0.0), Some(2400.0)) - 24.0).abs() < 1e-9);
        assert!((touch_slop(Some(-5.0), Some(2400.0)) - 24.0).abs() < 1e-9);
    }

    #[test]
    fn single_sample_stroke_is_always_a_tap() {
        let samples = [pt(100.0, 200.0, 0)];
        let result = classify_stroke(&samples, 24.0);
        assert_eq!(result.kind, GestureKind::Tap);
        assert_eq!(result.path, Vec::new());
        assert_eq!(result.end_t_offset_ns, 0);
    }

    #[test]
    fn small_jitter_within_slop_classifies_as_tap() {
        let samples = [
            pt(100.0, 200.0, 0),
            pt(105.0, 203.0, 20_000_000),
            pt(102.0, 198.0, 45_000_000),
        ];
        let result = classify_stroke(&samples, 24.0);
        assert_eq!(result.kind, GestureKind::Tap);
        assert_eq!(result.path, Vec::new(), "Tap must carry zero path points");
    }

    #[test]
    fn long_drag_classifies_as_swipe_with_endpoint_and_path() {
        let samples = [
            pt(50.0, 50.0, 0),
            pt(150.0, 50.0, 30_000_000),
            pt(300.0, 50.0, 60_000_000),
            pt(500.0, 50.0, 90_000_000),
        ];
        let result: ClassifiedStroke = classify_stroke(&samples, 24.0);
        assert_eq!(result.kind, GestureKind::Swipe);
        assert!((result.end_x - 500.0).abs() < 1e-9);
        assert!((result.end_y - 50.0).abs() < 1e-9);
        assert_eq!(result.end_t_offset_ns, 90_000_000);
        assert_eq!(result.path.len(), 4);
        assert_eq!(result.path.first().copied(), Some(samples[0]));
        assert_eq!(result.path.last().copied(), Some(samples[3]));
    }

    #[test]
    fn swipe_and_return_with_small_net_displacement_is_still_a_swipe() {
        // Finger travels far away and comes back close to the start: net displacement is small,
        // but the maximum excursion must still trigger Swipe classification.
        let samples = [
            pt(100.0, 100.0, 0),
            pt(100.0, 500.0, 40_000_000),
            pt(105.0, 103.0, 80_000_000),
        ];
        let result = classify_stroke(&samples, 24.0);
        assert_eq!(result.kind, GestureKind::Swipe);
    }

    #[test]
    fn path_is_decimated_to_the_cap_and_always_keeps_both_endpoints() {
        let samples: Vec<TouchPoint> = (0..200)
            .map(|i| pt(f64::from(i), 0.0, i64::from(i) * 1_000_000))
            .collect();
        let result = classify_stroke(&samples, 24.0);
        assert_eq!(result.kind, GestureKind::Swipe);
        assert!(result.path.len() <= MAX_PATH_POINTS);
        assert_eq!(result.path.first(), samples.first());
        assert_eq!(result.path.last(), samples.last());
    }

    #[test]
    fn path_under_cap_is_returned_untouched() {
        let samples = [pt(0.0, 0.0, 0), pt(50.0, 0.0, 10_000_000)];
        let result = classify_stroke(&samples, 10.0);
        assert_eq!(result.kind, GestureKind::Swipe);
        assert_eq!(result.path.len(), 2);
    }
}
