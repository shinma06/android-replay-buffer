//! Resolve wire bounds once on the exact media timeline used by the trimmer.
use crate::error::DaemonError;
use arb_core::bundle::FrameRecord;
use arb_core::selection::WindowSpec;
use arb_media::mp4::Mp4VideoTrack;

#[derive(Clone, Copy, Debug)]
pub(super) struct TrimRange {
    pub first: u64,
    pub last: u64,
    pub start: u64,
    pub end: u64,
    pub reaches_end: bool,
    pub realtime: bool,
    pub head_offset: i64,
    tail_offset: i64,
}

pub(super) fn time_pair(
    start: Option<u64>,
    end: Option<u64>,
) -> Result<Option<(u64, u64)>, DaemonError> {
    match (start, end) {
        (None, None) => Ok(None),
        (Some(a), Some(b)) if a < b => Ok(Some((a, b))),
        _ => Err(DaemonError::InvalidParams(
            "開始・終了時刻には開始 < 終了を満たすペアを指定してください".into(),
        )),
    }
}

impl TrimRange {
    pub(super) fn resolve(
        track: &Mp4VideoTrack,
        frames: (Option<u64>, Option<u64>),
        times: Option<(u64, u64)>,
    ) -> Result<Self, DaemonError> {
        let (pts, duration) = arb_media::trim::trim_timeline(track)?;
        let invalid = || DaemonError::InvalidParams("選択区間が動画の範囲外です".into());
        if pts.last().is_none_or(|&last| last >= duration) {
            return Err(invalid());
        }
        let (first, last, start, end) = if let Some((start, end)) = times {
            let plan = arb_media::timeline::plan_time_range_trim(&pts, duration, start, end)
                .map_err(|e| DaemonError::InvalidParams(e.into()))?;
            (plan.in_frame_index, plan.out_frame_index, start, end)
        } else {
            let (first, last) = match frames {
                (None, None) => (0, pts.len() - 1),
                (Some(a), Some(b)) if a <= b && b < track.frame_count() => (
                    usize::try_from(a).map_err(|_| invalid())?,
                    usize::try_from(b).map_err(|_| invalid())?,
                ),
                _ => return Err(invalid()),
            };
            (
                first,
                last,
                pts[first],
                pts.get(last + 1).copied().unwrap_or(duration),
            )
        };
        Ok(Self {
            first: u64::try_from(first).map_err(|_| invalid())?,
            last: u64::try_from(last).map_err(|_| invalid())?,
            start,
            end,
            reaches_end: end == duration,
            realtime: times.is_some(),
            head_offset: i64::try_from(start - pts[first]).map_err(|_| invalid())?,
            tail_offset: i64::try_from(end - pts[last] - 1).map_err(|_| invalid())?,
        })
    }

    pub(super) fn window(self, frames: &[FrameRecord]) -> Result<WindowSpec, DaemonError> {
        let mut window = WindowSpec::from_frame_indices(frames, self.first, self.last)?;
        // SelectionPolicy uses closed integer intervals. OUT - 1 is the last selected ns.
        if self.realtime
            && let WindowSpec::FrameRange {
                start_boottime_ns,
                end_boottime_ns,
                ingest_start_cont_ns,
                ingest_end_cont_ns,
                ..
            } = &mut window
        {
            *start_boottime_ns = start_boottime_ns.saturating_add(self.head_offset);
            *end_boottime_ns = end_boottime_ns.saturating_add(self.tail_offset);
            *ingest_start_cont_ns = ingest_start_cont_ns.saturating_add(self.head_offset);
            *ingest_end_cont_ns = ingest_end_cont_ns.saturating_add(self.tail_offset);
        }
        Ok(window)
    }

    pub(super) fn trim_video(
        self,
        source: &std::path::Path,
        output: &std::path::Path,
        backend: &dyn arb_media::codec::VideoCodecBackend,
    ) -> Result<arb_media::trim::TrimReport, DaemonError> {
        Ok(if self.realtime {
            arb_media::trim::trim_video_time_range(source, output, self.start, self.end, backend)?
        } else {
            arb_media::trim::trim_video(source, output, self.first, self.last, backend)?
        })
    }
}

pub(super) fn shift_alignment(
    alignment: &arb_core::time::TimeAlignment,
    offset: i64,
) -> arb_core::time::TimeAlignment {
    use arb_core::time::TimeAlignment;
    let mut shifted = alignment.clone();
    match &mut shifted {
        TimeAlignment::Anchored { boottime_ns, .. } => {
            *boottime_ns = boottime_ns.saturating_add(offset);
        }
        TimeAlignment::Bounded {
            min_boottime_ns,
            max_boottime_ns,
            ..
        } => {
            *min_boottime_ns = min_boottime_ns.saturating_add(offset);
            *max_boottime_ns = max_boottime_ns.saturating_add(offset);
        }
        _ => {}
    }
    shifted
}

/// Clip known same-epoch gestures before the normal uncertainty/quarantine policy runs.
pub(super) fn clip_tap(tap: &mut arb_core::bundle::TapRecord, window: &WindowSpec) -> bool {
    use arb_core::time::TimeAlignment;
    let start = match &tap.alignment {
        TimeAlignment::Anchored {
            boot_id,
            boottime_ns,
            ..
        } if boot_id == window.boot_id() => *boottime_ns,
        _ => return true,
    };
    let duration = tap.end_t_offset_ns.unwrap_or(0).max(0);
    let first = window.start_boottime_ns().saturating_sub(start).max(0);
    let last = window
        .end_boottime_ns()
        .saturating_add(1)
        .saturating_sub(start)
        .min(duration);
    if start > window.end_boottime_ns()
        || (duration > 0 && start.saturating_add(duration) <= window.start_boottime_ns())
        || (duration == 0 && start < window.start_boottime_ns())
    {
        return false;
    }
    if first == 0 && last == duration {
        return true;
    }
    if tap.gesture_kind == arb_core::gesture::GestureKind::Swipe
        && let (Some(x), Some(y), Some(end_x), Some(end_y)) = (tap.x, tap.y, tap.end_x, tap.end_y)
    {
        use arb_core::gesture::TouchPoint;
        let mut points = vec![TouchPoint {
            x,
            y,
            t_offset_ns: 0,
        }];
        points.extend(
            tap.path
                .iter()
                .copied()
                .filter(|p| p.t_offset_ns > 0 && p.t_offset_ns < duration),
        );
        points.push(TouchPoint {
            x: end_x,
            y: end_y,
            t_offset_ns: duration,
        });
        points.sort_by_key(|p| p.t_offset_ns);
        let left = sample_path(&points, first);
        let right = sample_path(&points, last);
        tap.x = Some(left.x);
        tap.y = Some(left.y);
        tap.end_x = Some(right.x);
        tap.end_y = Some(right.y);
        tap.path = std::iter::once(left)
            .chain(
                points
                    .iter()
                    .copied()
                    .filter(|p| p.t_offset_ns > first && p.t_offset_ns < last),
            )
            .chain(std::iter::once(right))
            .map(|mut p| {
                p.t_offset_ns -= first;
                p
            })
            .collect();
    }
    tap.alignment = shift_alignment(&tap.alignment, first);
    tap.event_mono_ns = tap.event_mono_ns.map(|v| v.saturating_add(first));
    tap.end_t_offset_ns = Some(last - first);
    true
}

#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "interpolating display coordinates between adjacent gesture samples"
)]
fn sample_path(
    points: &[arb_core::gesture::TouchPoint],
    time: i64,
) -> arb_core::gesture::TouchPoint {
    let i = points
        .partition_point(|p| p.t_offset_ns < time)
        .min(points.len() - 1);
    let a = points[i.saturating_sub(1)];
    let b = points[i];
    let f = (time - a.t_offset_ns) as f64 / (b.t_offset_ns - a.t_offset_ns).max(1) as f64;
    arb_core::gesture::TouchPoint {
        x: a.x + (b.x - a.x) * f,
        y: a.y + (b.y - a.y) * f,
        t_offset_ns: time,
    }
}
