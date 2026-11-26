//! Media time coordinates used by the viewport. Frame intervals are half-open.

/// Relative sample starts for the wire. An empty table selects estimated CFR on the client.
/// Reserve one nanosecond for each remaining frame and the exclusive end, even on overflow.
pub fn viewport_timestamps(pts: impl ExactSizeIterator<Item = i64>) -> Vec<i64> {
    if pts.len() > 100_000 {
        return Vec::new();
    }
    normalized_timestamps(pts)
}

pub(crate) fn normalized_timestamps(mut pts: impl ExactSizeIterator<Item = i64>) -> Vec<i64> {
    let count = pts.len();
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

/// Source frames intersecting the half-open selection, with their exact display durations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeRangeTrim {
    /// First visible source frame.
    pub in_frame_index: usize,
    /// Last visible source frame (inclusive).
    pub out_frame_index: usize,
    /// Selected portion of each included frame, in presentation order.
    pub durations_ns: Vec<u64>,
}

/// Intersects the normalized K10 display intervals with a half-open nanosecond selection.
///
/// # Errors
/// Rejects empty, non-increasing or nonzero-origin timelines and selections outside the track.
pub fn plan_time_range_trim(
    pts_ns: &[u64],
    duration_ns: u64,
    in_time_ns: u64,
    out_time_ns: u64,
) -> Result<TimeRangeTrim, &'static str> {
    let (in_frame_index, out_frame_index, durations) =
        trim_intervals(pts_ns, duration_ns, in_time_ns, out_time_ns)?;
    Ok(TimeRangeTrim {
        in_frame_index,
        out_frame_index,
        durations_ns: durations.collect(),
    })
}

// Borrowing the input lets the proof exercise the interval arithmetic without modeling the allocator.
pub(crate) fn trim_intervals(
    pts_ns: &[u64],
    duration_ns: u64,
    in_time_ns: u64,
    out_time_ns: u64,
) -> Result<(usize, usize, impl Iterator<Item = u64> + '_), &'static str> {
    if pts_ns.first() != Some(&0)
        || pts_ns.last() >= Some(&duration_ns)
        || pts_ns.windows(2).any(|w| w[0] >= w[1])
    {
        return Err("expected strictly increasing sample starts from zero before track end");
    }
    if in_time_ns >= out_time_ns || out_time_ns > duration_ns {
        return Err("expected 0 <= in_time_ns < out_time_ns <= duration_ns");
    }
    let in_frame_index = pts_ns.partition_point(|&p| p <= in_time_ns) - 1;
    let out_frame_index = pts_ns.partition_point(|&p| p < out_time_ns) - 1;
    let selected = (in_frame_index..=out_frame_index).map(move |k| {
        let end = pts_ns.get(k + 1).copied().unwrap_or(duration_ns);
        end.min(out_time_ns) - pts_ns[k].max(in_time_ns)
    });
    Ok((in_frame_index, out_frame_index, selected))
}

fn rounded_ticks(ns: u64, timescale: u32) -> u128 {
    (u128::from(ns) * u128::from(timescale) + 500_000_000) / 1_000_000_000
}

/// Rounds cumulative boundaries while reserving one tick for each remaining frame.
///
/// # Errors
/// Rejects empty or zero durations and totals or sample lengths unrepresentable at this scale.
pub fn quantize_durations_ns_to_ticks(
    durations_ns: &[u64],
    timescale: u32,
) -> Result<Vec<u32>, &'static str> {
    if timescale == 0 || durations_ns.is_empty() || durations_ns.contains(&0) {
        return Err("expected nonzero durations and timescale");
    }
    let total_ns = durations_ns
        .iter()
        .copied()
        .try_fold(0u64, u64::checked_add)
        .ok_or("duration exceeds u64 nanoseconds")?;
    let count = u128::try_from(durations_ns.len()).map_err(|_| "sample count overflow")?;
    let total = rounded_ticks(total_ns, timescale).max(1);
    if total < count || total > u128::from(u64::MAX) {
        return Err("total ticks cannot represent the selection at this timescale");
    }
    let mut elapsed = 0;
    let mut previous = 0;
    let mut ticks = Vec::with_capacity(durations_ns.len());
    let mut remaining = count;
    for &duration in durations_ns {
        elapsed += duration;
        remaining -= 1;
        let end = rounded_ticks(elapsed, timescale).clamp(previous + 1, total - remaining);
        ticks
            .push(u32::try_from(end - previous).map_err(|_| "sample duration exceeds stts limit")?);
        previous = end;
    }
    Ok(ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_subframe_limits() {
        let plan = plan_time_range_trim(&[0, 1, u64::MAX - 2], u64::MAX, 1, u64::MAX).unwrap();
        assert_eq!(plan.durations_ns, [u64::MAX - 3, 2]);
        assert!(quantize_durations_ns_to_ticks(&[u64::MAX, 1], 1).is_err());
        assert!(quantize_durations_ns_to_ticks(&[u64::MAX], u32::MAX).is_err());
        assert_eq!(
            quantize_durations_ns_to_ticks(&[1, 2998, 1], 1_000_000).unwrap(),
            [1, 1, 1]
        );
        assert_eq!(quantize_durations_ns_to_ticks(&[1], 1).unwrap(), [1]);
    }
}
