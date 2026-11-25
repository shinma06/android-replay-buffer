//! Media time coordinates used by the viewport. Frame intervals are half-open.

/// Relative sample starts for the wire. An empty table selects estimated CFR on the client.
/// Reserve one nanosecond for each remaining frame and the exclusive end, even on overflow.
pub fn viewport_timestamps(mut pts: impl ExactSizeIterator<Item = i64>) -> Vec<i64> {
    let count = pts.len();
    if count > 100_000 {
        return Vec::new();
    }
    let Some(origin) = pts.next() else {
        return Vec::new();
    };
    let mut table = Vec::with_capacity(count);
    table.push(0);
    let mut previous = 0;
    for stamp in pts.by_ref() {
        let remaining = count - table.len();
        let ceiling = i64::MAX - i64::try_from(remaining).unwrap_or(i64::MAX);
        previous = stamp.saturating_sub(origin).clamp(previous + 1, ceiling);
        table.push(previous);
    }
    table
}

/// End of the last sample, using the preceding interval when its duration is unknown.
pub fn end_ns(pts: &[i64], last_duration: Option<i64>, fallback: i64) -> i64 {
    let Some((&last, prefix)) = pts.split_last() else {
        return 0;
    };
    let duration = last_duration.filter(|d| *d > 0).unwrap_or_else(|| {
        prefix
            .last()
            .map_or(fallback, |previous| last.saturating_sub(*previous))
    });
    last.saturating_add(duration.max(1))
}

/// Frame displayed at a media time; the exclusive endpoint displays the last frame.
pub fn frame_at_time(pts: &[i64], time: i64) -> Option<usize> {
    frame_at_time_by(pts, time, |p| *p)
}

pub(crate) fn frame_at_time_by<T>(
    samples: &[T],
    time: i64,
    pts: impl Fn(&T) -> i64,
) -> Option<usize> {
    (!samples.is_empty()).then(|| {
        samples
            .partition_point(|s| pts(s) <= time)
            .saturating_sub(1)
    })
}

/// Right boundary of an inclusive OUT frame.
pub fn out_time(pts: &[i64], end: i64, index: usize) -> i64 {
    pts.get(index.saturating_add(1)).copied().unwrap_or(end)
}

/// Inclusive OUT at a right boundary. Equality selects the frame immediately before it.
pub fn out_frame(pts: &[i64], time: i64) -> Option<usize> {
    (!pts.is_empty()).then(|| pts.partition_point(|p| *p < time).saturating_sub(1))
}

/// Normalized screen position. Quantization happens only at the drawing boundary.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "normalized display coordinate"
)]
pub fn time_to_fraction(time: i64, end: i64) -> f64 {
    if end <= 0 {
        return 0.0;
    }
    time.clamp(0, end) as f64 / end as f64
}

/// Inverse display coordinate, rounded to the nearest nanosecond.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "bounded rounded display coordinate"
)]
pub fn fraction_to_time(fraction: f64, end: i64) -> i64 {
    let end = end.max(0);
    ((fraction.clamp(0.0, 1.0) * end as f64).round() as i64).clamp(0, end)
}
