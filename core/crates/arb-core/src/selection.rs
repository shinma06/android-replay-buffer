//! Pure three-valued selection engine shared identically by save and trim.
//!
//! Every stream (video, audio, logcat, input) passes through the exact same
//! [`classify_alignment`] and [`select_records`] rules so that:
//! - Delayed-arrival records whose true device time lies outside `[W_start, W_end]`
//!   are strictly excluded (`OutOfRange`), preventing delayed-arrival bloat.
//! - Unanchored or cross-boot records never pollute the main timeline and are
//!   quarantined strictly within PC ingestion proximity and capped by record
//!   count and byte budget.
//! - Trim reuses the identical classifier and carries forward existing
//!   unanchored sidecar records transparently without silently dropping them.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bundle::{FrameRecord, LogRecord, TapRecord};
use crate::time::TimeAlignment;

/// Default PC ingestion proximity margin (2 seconds) for unanchored quarantine.
pub const DEFAULT_UNANCHORED_INGEST_MARGIN_NS: u64 = 2_000_000_000;

/// Default maximum number of records retained in the `unanchored` sidecar.
pub const DEFAULT_MAX_UNANCHORED_RECORDS: usize = 50_000;

/// Default maximum byte budget (16 MiB) for the `unanchored` sidecar.
pub const DEFAULT_MAX_UNANCHORED_BYTES: usize = 16 * 1024 * 1024;

/// Default boundary drain timeout (500 ms) after a save trigger.
pub const DEFAULT_DRAIN_TIMEOUT_MS: u64 = 500;

/// Target window specification shared by save (real elapsed) and trim (frame range).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WindowSpec {
    /// Save window spanning `[start_boottime_ns, end_boottime_ns]` in real elapsed time.
    RealElapsed {
        /// Reference device boot epoch for the window.
        boot_id: String,
        /// Inclusive window start on `DeviceBoottime` in nanoseconds.
        start_boottime_ns: i64,
        /// Inclusive window end on `DeviceBoottime` in nanoseconds.
        end_boottime_ns: i64,
        /// Host `PcContinuousMono` timestamp corresponding to window start.
        ingest_start_cont_ns: i64,
        /// Host `PcContinuousMono` timestamp corresponding to window end (save trigger).
        ingest_end_cont_ns: i64,
    },
    /// Trim window derived from exact integer `[in_frame_index, out_frame_index]` bounds.
    FrameRange {
        /// Reference device boot epoch of the trimmed frame span.
        boot_id: String,
        /// Inclusive starting frame index in the source bundle.
        in_frame_index: u64,
        /// Inclusive ending frame index in the source bundle.
        out_frame_index: u64,
        /// Exact `DeviceBoottime` start derived from `in_frame_index`.
        start_boottime_ns: i64,
        /// Exact `DeviceBoottime` end derived from `out_frame_index`.
        end_boottime_ns: i64,
        /// Host `PcContinuousMono` ingestion start corresponding to `in_frame_index`.
        ingest_start_cont_ns: i64,
        /// Host `PcContinuousMono` ingestion end corresponding to `out_frame_index`.
        ingest_end_cont_ns: i64,
    },
}

/// Errors returned when constructing a [`WindowSpec`] from frame indices.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SelectionError {
    /// Requested `in_frame_index` was greater than `out_frame_index`.
    #[error(
        "invalid trim frame range: in_frame_index ({in_frame_index}) > out_frame_index ({out_frame_index})"
    )]
    InvertedFrameRange {
        /// Requested start frame index.
        in_frame_index: u64,
        /// Requested end frame index.
        out_frame_index: u64,
    },
    /// One of the boundary frame indices did not exist in the provided frame table.
    #[error("frame index {frame_index} not found in source frame table")]
    FrameNotFound {
        /// Missing frame index.
        frame_index: u64,
    },
    /// Start and end boundary frames belong to different device boot epochs.
    #[error("boundary frames span different boot epochs ('{in_boot_id}' vs '{out_boot_id}')")]
    CrossBootFrameRange {
        /// Boot ID of `in_frame_index`.
        in_boot_id: String,
        /// Boot ID of `out_frame_index`.
        out_boot_id: String,
    },
    /// Boundary frame has no `DeviceBoottime` alignment to anchor the trim window.
    #[error("boundary frame {frame_index} is unanchored and cannot define a boottime trim window")]
    UnanchoredBoundaryFrame {
        /// Frame index lacking boottime alignment.
        frame_index: u64,
    },
}

impl WindowSpec {
    /// Derives a [`WindowSpec::FrameRange`] strictly from integer frame indices
    /// in `frames` (never floating-point seconds).
    ///
    /// # Errors
    ///
    /// Returns [`SelectionError`] if the range is inverted, either frame is
    /// missing, the boundary frames belong to different boot epochs, or either
    /// boundary frame is unanchored.
    pub fn from_frame_indices(
        frames: &[FrameRecord],
        in_frame_index: u64,
        out_frame_index: u64,
    ) -> Result<Self, SelectionError> {
        if in_frame_index > out_frame_index {
            return Err(SelectionError::InvertedFrameRange {
                in_frame_index,
                out_frame_index,
            });
        }

        let in_frame = frames
            .iter()
            .find(|f| f.frame_index == in_frame_index)
            .ok_or(SelectionError::FrameNotFound {
                frame_index: in_frame_index,
            })?;
        let out_frame = frames
            .iter()
            .find(|f| f.frame_index == out_frame_index)
            .ok_or(SelectionError::FrameNotFound {
                frame_index: out_frame_index,
            })?;

        if in_frame.boot_id != out_frame.boot_id {
            return Err(SelectionError::CrossBootFrameRange {
                in_boot_id: in_frame.boot_id.clone(),
                out_boot_id: out_frame.boot_id.clone(),
            });
        }

        let start_boottime_ns = match &in_frame.alignment {
            TimeAlignment::Anchored { boottime_ns, .. } => *boottime_ns,
            TimeAlignment::Bounded {
                min_boottime_ns, ..
            } => *min_boottime_ns,
            TimeAlignment::Unanchored { .. } => {
                return Err(SelectionError::UnanchoredBoundaryFrame {
                    frame_index: in_frame_index,
                });
            }
        };

        let end_boottime_ns = match &out_frame.alignment {
            TimeAlignment::Anchored { boottime_ns, .. } => *boottime_ns,
            TimeAlignment::Bounded {
                max_boottime_ns, ..
            } => *max_boottime_ns,
            TimeAlignment::Unanchored { .. } => {
                return Err(SelectionError::UnanchoredBoundaryFrame {
                    frame_index: out_frame_index,
                });
            }
        };

        Ok(Self::FrameRange {
            boot_id: in_frame.boot_id.clone(),
            in_frame_index,
            out_frame_index,
            start_boottime_ns,
            end_boottime_ns,
            ingest_start_cont_ns: in_frame.pc_ingest_cont_ns,
            ingest_end_cont_ns: out_frame.pc_ingest_cont_ns,
        })
    }

    /// Reference device boot epoch of the window.
    pub fn boot_id(&self) -> &str {
        match self {
            Self::RealElapsed { boot_id, .. } | Self::FrameRange { boot_id, .. } => boot_id,
        }
    }

    /// Inclusive start timestamp on `DeviceBoottime` in nanoseconds.
    pub const fn start_boottime_ns(&self) -> i64 {
        match self {
            Self::RealElapsed {
                start_boottime_ns, ..
            }
            | Self::FrameRange {
                start_boottime_ns, ..
            } => *start_boottime_ns,
        }
    }

    /// Inclusive end timestamp on `DeviceBoottime` in nanoseconds.
    pub const fn end_boottime_ns(&self) -> i64 {
        match self {
            Self::RealElapsed {
                end_boottime_ns, ..
            }
            | Self::FrameRange {
                end_boottime_ns, ..
            } => *end_boottime_ns,
        }
    }

    /// Host `PcContinuousMono` timestamp corresponding to window start.
    pub const fn ingest_start_cont_ns(&self) -> i64 {
        match self {
            Self::RealElapsed {
                ingest_start_cont_ns,
                ..
            }
            | Self::FrameRange {
                ingest_start_cont_ns,
                ..
            } => *ingest_start_cont_ns,
        }
    }

    /// Host `PcContinuousMono` timestamp corresponding to window end.
    pub const fn ingest_end_cont_ns(&self) -> i64 {
        match self {
            Self::RealElapsed {
                ingest_end_cont_ns, ..
            }
            | Self::FrameRange {
                ingest_end_cont_ns, ..
            } => *ingest_end_cont_ns,
        }
    }
}

/// Quarantine caps and boundary drain policy for selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SelectionPolicy {
    /// Ingestion proximity margin `±margin` around `[ingest_start, ingest_end]` in nanoseconds.
    pub unanchored_ingest_margin_ns: u64,
    /// Hard cap on the number of records quarantined in `unanchored`.
    pub max_unanchored_records: usize,
    /// Hard cap on the total estimated bytes quarantined in `unanchored`.
    pub max_unanchored_bytes: usize,
    /// Maximum boundary drain wait in milliseconds after a save trigger.
    pub drain_timeout_ms: u64,
}

impl SelectionPolicy {
    /// Creates a policy with the default SPEC-02 quarantine caps and drain timeout.
    pub const fn new() -> Self {
        Self {
            unanchored_ingest_margin_ns: DEFAULT_UNANCHORED_INGEST_MARGIN_NS,
            max_unanchored_records: DEFAULT_MAX_UNANCHORED_RECORDS,
            max_unanchored_bytes: DEFAULT_MAX_UNANCHORED_BYTES,
            drain_timeout_ms: DEFAULT_DRAIN_TIMEOUT_MS,
        }
    }
}

impl Default for SelectionPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Explains why a record received [`Membership::UncertainTime`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertainPlacement {
    /// A `Bounded` interval partially overlaps the selection window.
    PartialOverlap,
    /// Record is `Unanchored` or from a different `boot_id`; subject to ingest-proximity quarantine.
    UnanchoredProximity,
}

/// Three-valued membership outcome of [`classify_alignment`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Membership {
    /// Confirmed inside the window; routed to the main timeline.
    InRange,
    /// Confirmed outside the window; excluded.
    OutOfRange,
    /// Neither confirmed inside nor confirmed outside; quarantined to `unanchored` under caps.
    UncertainTime(UncertainPlacement),
}

/// Partition produced by [`select_records`] and [`select_for_trim`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedPartition<T> {
    /// Records confirmed inside the window (`Membership::InRange`).
    pub main: Vec<T>,
    /// Quarantined uncertain records kept out of the main timeline.
    pub unanchored: Vec<T>,
    /// Number of eligible uncertain records dropped due to record/byte quarantine caps.
    pub unanchored_truncated_count: usize,
    /// Number of records excluded (`OutOfRange` or unanchored outside ingest proximity).
    pub excluded_count: usize,
}

/// Trait implemented by any record that can be classified and partitioned by the selection engine.
pub trait AlignedRecord {
    /// Derived time alignment of the record.
    fn time_alignment(&self) -> &TimeAlignment;

    /// Immutable host PC continuous ingestion timestamp in nanoseconds.
    fn pc_ingest_cont_ns(&self) -> i64;

    /// Estimated serialized payload size in bytes used for `max_unanchored_bytes` enforcement.
    fn estimated_bytes(&self) -> usize;

    /// Exact sequential video frame index, when this record is a video frame.
    ///
    /// When present and selecting against [`WindowSpec::FrameRange`], frame
    /// membership is decided strictly by `frame_index ∈ [in_frame_index, out_frame_index]`
    /// within the window's `boot_id` so timestamp half-widths never leak neighbor
    /// `in - 1` or `out + 1` frames into the trimmed output.
    fn frame_index(&self) -> Option<u64> {
        None
    }
}

/// Generic record wrapper carrying an arbitrary payload alongside alignment and byte size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimestampedRecord<T> {
    /// Arbitrary domain payload.
    pub payload: T,
    /// Derived time alignment.
    pub alignment: TimeAlignment,
    /// Immutable host PC continuous ingestion timestamp in nanoseconds.
    pub pc_ingest_cont_ns: i64,
    /// Estimated byte size of this record.
    pub estimated_bytes: usize,
}

impl<T> AlignedRecord for TimestampedRecord<T> {
    fn time_alignment(&self) -> &TimeAlignment {
        &self.alignment
    }

    fn pc_ingest_cont_ns(&self) -> i64 {
        self.pc_ingest_cont_ns
    }

    fn estimated_bytes(&self) -> usize {
        self.estimated_bytes
    }
}

impl AlignedRecord for FrameRecord {
    fn time_alignment(&self) -> &TimeAlignment {
        &self.alignment
    }

    fn pc_ingest_cont_ns(&self) -> i64 {
        self.pc_ingest_cont_ns
    }

    fn estimated_bytes(&self) -> usize {
        usize::try_from(self.mp4_sample_size).unwrap_or(usize::MAX)
    }

    fn frame_index(&self) -> Option<u64> {
        Some(self.frame_index)
    }
}

impl AlignedRecord for LogRecord {
    fn time_alignment(&self) -> &TimeAlignment {
        &self.alignment
    }

    fn pc_ingest_cont_ns(&self) -> i64 {
        self.pc_ingest_cont_ns
    }

    fn estimated_bytes(&self) -> usize {
        self.message
            .len()
            .saturating_add(self.tag.as_deref().map_or(0, str::len))
            .saturating_add(self.level.as_deref().map_or(0, str::len))
            .saturating_add(64)
    }
}

impl AlignedRecord for TapRecord {
    fn time_alignment(&self) -> &TimeAlignment {
        &self.alignment
    }

    fn pc_ingest_cont_ns(&self) -> i64 {
        self.pc_ingest_cont_ns
    }

    fn estimated_bytes(&self) -> usize {
        self.action.len().saturating_add(64)
    }
}

/// Pure three-valued classifier shared by save and trim.
///
/// - `Anchored` (same `boot_id`): overlaps `[W_start, W_end]` with `±half_width_ns`
///   -> [`Membership::InRange`]; strictly outside -> [`Membership::OutOfRange`].
/// - `Bounded` (same `boot_id`): completely contained -> [`Membership::InRange`];
///   disjoint -> [`Membership::OutOfRange`]; partial overlap ->
///   [`Membership::UncertainTime`]`(`[`UncertainPlacement::PartialOverlap`]`)`.
/// - `Unanchored` or cross-boot ->
///   [`Membership::UncertainTime`]`(`[`UncertainPlacement::UnanchoredProximity`]`)`.
pub fn classify_alignment(alignment: &TimeAlignment, window: &WindowSpec) -> Membership {
    let w_start = i128::from(window.start_boottime_ns()).min(i128::from(window.end_boottime_ns()));
    let w_end = i128::from(window.start_boottime_ns()).max(i128::from(window.end_boottime_ns()));
    let w_boot = window.boot_id();

    match alignment {
        TimeAlignment::Anchored {
            boot_id,
            boottime_ns,
            half_width_ns,
            ..
        } => {
            if boot_id != w_boot {
                return Membership::UncertainTime(UncertainPlacement::UnanchoredProximity);
            }
            let center = i128::from(*boottime_ns);
            let hw = i128::from(*half_width_ns);
            let lo = center - hw;
            let hi = center + hw;
            if lo <= w_end && hi >= w_start {
                Membership::InRange
            } else {
                Membership::OutOfRange
            }
        }
        TimeAlignment::Bounded {
            boot_id,
            min_boottime_ns,
            max_boottime_ns,
            ..
        } => {
            if boot_id != w_boot {
                return Membership::UncertainTime(UncertainPlacement::UnanchoredProximity);
            }
            let lo = i128::from(*min_boottime_ns).min(i128::from(*max_boottime_ns));
            let hi = i128::from(*min_boottime_ns).max(i128::from(*max_boottime_ns));
            if lo >= w_start && hi <= w_end {
                Membership::InRange
            } else if hi < w_start || lo > w_end {
                Membership::OutOfRange
            } else {
                Membership::UncertainTime(UncertainPlacement::PartialOverlap)
            }
        }
        TimeAlignment::Unanchored { .. } => {
            Membership::UncertainTime(UncertainPlacement::UnanchoredProximity)
        }
    }
}

/// Classifies a single [`AlignedRecord`] against `window`.
///
/// For video frame records (`record.frame_index().is_some()`) evaluated against
/// [`WindowSpec::FrameRange`], same-boot frames are selected strictly by
/// `frame_index ∈ [in_frame_index, out_frame_index]` so timestamp half-width
/// overlap never leaks neighbor `in - 1` or `out + 1` frames into a trim. All
/// other streams and window kinds use [`classify_alignment`].
pub fn classify_record<T: AlignedRecord>(record: &T, window: &WindowSpec) -> Membership {
    if let (
        WindowSpec::FrameRange {
            boot_id: w_boot,
            in_frame_index,
            out_frame_index,
            ..
        },
        Some(frame_index),
    ) = (window, record.frame_index())
    {
        match record.time_alignment() {
            TimeAlignment::Anchored { boot_id, .. } | TimeAlignment::Bounded { boot_id, .. } => {
                if boot_id != w_boot {
                    return Membership::UncertainTime(UncertainPlacement::UnanchoredProximity);
                }
                if frame_index >= *in_frame_index && frame_index <= *out_frame_index {
                    return Membership::InRange;
                }
                return Membership::OutOfRange;
            }
            TimeAlignment::Unanchored { .. } => {
                return Membership::UncertainTime(UncertainPlacement::UnanchoredProximity);
            }
        }
    }
    classify_alignment(record.time_alignment(), window)
}

fn try_quarantine<T: AlignedRecord>(
    record: T,
    unanchored: &mut Vec<T>,
    unanchored_bytes: &mut usize,
    unanchored_truncated_count: &mut usize,
    policy: &SelectionPolicy,
) {
    let rec_bytes = record.estimated_bytes();
    let next_bytes = unanchored_bytes.saturating_add(rec_bytes);
    if unanchored.len() < policy.max_unanchored_records && next_bytes <= policy.max_unanchored_bytes
    {
        *unanchored_bytes = next_bytes;
        unanchored.push(record);
    } else {
        *unanchored_truncated_count = unanchored_truncated_count.saturating_add(1);
    }
}

/// Partitions a stream of records into `main`, `unanchored`, and exclusion/truncation counts.
pub fn select_records<T: AlignedRecord>(
    records: impl IntoIterator<Item = T>,
    window: &WindowSpec,
    policy: &SelectionPolicy,
) -> SelectedPartition<T> {
    let ingest_min =
        i128::from(window.ingest_start_cont_ns()).min(i128::from(window.ingest_end_cont_ns()));
    let ingest_max =
        i128::from(window.ingest_start_cont_ns()).max(i128::from(window.ingest_end_cont_ns()));
    let margin = i128::from(policy.unanchored_ingest_margin_ns);
    let prox_lo = ingest_min - margin;
    let prox_hi = ingest_max + margin;

    let mut main = Vec::new();
    let mut unanchored = Vec::new();
    let mut unanchored_bytes: usize = 0;
    let mut unanchored_truncated_count: usize = 0;
    let mut excluded_count: usize = 0;

    for record in records {
        match classify_record(&record, window) {
            Membership::InRange => main.push(record),
            Membership::OutOfRange => {
                excluded_count = excluded_count.saturating_add(1);
            }
            Membership::UncertainTime(UncertainPlacement::PartialOverlap) => {
                try_quarantine(
                    record,
                    &mut unanchored,
                    &mut unanchored_bytes,
                    &mut unanchored_truncated_count,
                    policy,
                );
            }
            Membership::UncertainTime(UncertainPlacement::UnanchoredProximity) => {
                let ingest = i128::from(record.pc_ingest_cont_ns());
                if ingest >= prox_lo && ingest <= prox_hi {
                    try_quarantine(
                        record,
                        &mut unanchored,
                        &mut unanchored_bytes,
                        &mut unanchored_truncated_count,
                        policy,
                    );
                } else {
                    excluded_count = excluded_count.saturating_add(1);
                }
            }
        }
    }

    SelectedPartition {
        main,
        unanchored,
        unanchored_truncated_count,
        excluded_count,
    }
}

/// Applies the exact same selection engine to a trim operation while carrying
/// forward existing `source_unanchored` records transparently.
///
/// Existing `source_unanchored` records are quarantined FIRST before any
/// newly-uncertain records from `source_main` consume quarantine caps, are
/// never promoted into `main`, and are never silently dropped as `OutOfRange`.
pub fn select_for_trim<T: AlignedRecord>(
    source_main: impl IntoIterator<Item = T>,
    source_unanchored: impl IntoIterator<Item = T>,
    frame_window: &WindowSpec,
    policy: &SelectionPolicy,
) -> SelectedPartition<T> {
    select_for_trim_with_truncated(source_main, source_unanchored, 0, frame_window, policy)
}

/// Variant of [`select_for_trim`] that also propagates the source bundle's
/// prior `source_unanchored_truncated_count` into the trimmed partition.
///
/// Existing `source_unanchored` records are quarantined FIRST before newly
/// uncertain records from `source_main` consume `policy` caps.
pub fn select_for_trim_with_truncated<T: AlignedRecord>(
    source_main: impl IntoIterator<Item = T>,
    source_unanchored: impl IntoIterator<Item = T>,
    source_unanchored_truncated_count: usize,
    frame_window: &WindowSpec,
    policy: &SelectionPolicy,
) -> SelectedPartition<T> {
    let ingest_min = i128::from(frame_window.ingest_start_cont_ns())
        .min(i128::from(frame_window.ingest_end_cont_ns()));
    let ingest_max = i128::from(frame_window.ingest_start_cont_ns())
        .max(i128::from(frame_window.ingest_end_cont_ns()));
    let margin = i128::from(policy.unanchored_ingest_margin_ns);
    let prox_lo = ingest_min - margin;
    let prox_hi = ingest_max + margin;

    let mut main = Vec::new();
    let mut unanchored = Vec::new();
    let mut unanchored_bytes: usize = 0;
    let mut unanchored_truncated_count: usize = source_unanchored_truncated_count;
    let mut excluded_count: usize = 0;

    // 1. Carry forward existing source unanchored sidecar records FIRST so
    //    newly-uncertain records cannot evict them under quarantine caps.
    for record in source_unanchored {
        try_quarantine(
            record,
            &mut unanchored,
            &mut unanchored_bytes,
            &mut unanchored_truncated_count,
            policy,
        );
    }

    // 2. Partition `source_main` using the exact same classifier as save.
    for record in source_main {
        match classify_record(&record, frame_window) {
            Membership::InRange => main.push(record),
            Membership::OutOfRange => {
                excluded_count = excluded_count.saturating_add(1);
            }
            Membership::UncertainTime(UncertainPlacement::PartialOverlap) => {
                try_quarantine(
                    record,
                    &mut unanchored,
                    &mut unanchored_bytes,
                    &mut unanchored_truncated_count,
                    policy,
                );
            }
            Membership::UncertainTime(UncertainPlacement::UnanchoredProximity) => {
                let ingest = i128::from(record.pc_ingest_cont_ns());
                if ingest >= prox_lo && ingest <= prox_hi {
                    try_quarantine(
                        record,
                        &mut unanchored,
                        &mut unanchored_bytes,
                        &mut unanchored_truncated_count,
                        policy,
                    );
                } else {
                    excluded_count = excluded_count.saturating_add(1);
                }
            }
        }
    }

    SelectedPartition {
        main,
        unanchored,
        unanchored_truncated_count,
        excluded_count,
    }
}

/// Outcome of evaluating boundary drain completion for a single active stream.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainOutcome {
    /// Stream heartbeat passed `trigger_boottime_ns`; tail is complete.
    DrainedComplete,
    /// Drain still in progress within `drain_timeout_ms`.
    Waiting,
    /// Drain hit `drain_timeout_ms` before passing trigger; mark `tail_may_be_incomplete = true`.
    TimedOutTailIncomplete,
}

/// Evaluates whether a stream's post-trigger boundary drain has completed or timed out.
pub const fn evaluate_stream_drain(
    trigger_boottime_ns: i64,
    latest_stream_heartbeat_ns: Option<i64>,
    elapsed_drain_ms: u64,
    policy: &SelectionPolicy,
) -> DrainOutcome {
    if let Some(hb) = latest_stream_heartbeat_ns
        && hb >= trigger_boottime_ns
    {
        return DrainOutcome::DrainedComplete;
    }
    if elapsed_drain_ms >= policy.drain_timeout_ms {
        DrainOutcome::TimedOutTailIncomplete
    } else {
        DrainOutcome::Waiting
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DrainOutcome, Membership, SelectionPolicy, TimestampedRecord, UncertainPlacement,
        WindowSpec, classify_alignment, evaluate_stream_drain, select_for_trim, select_records,
    };
    use crate::time::{TimeAlignment, TimestampSemantics, UncertaintyReason};

    fn sample_window() -> WindowSpec {
        WindowSpec::RealElapsed {
            boot_id: "boot-1".into(),
            start_boottime_ns: 100_000_000_000,
            end_boottime_ns: 130_000_000_000,
            ingest_start_cont_ns: 1_100_000_000_000,
            ingest_end_cont_ns: 1_130_000_000_000,
        }
    }

    #[test]
    fn regression_delayed_arrival_49s_old_anchored_record_is_excluded() {
        let window = sample_window();
        // Record happened at 81s (19s before window start, 49s before trigger),
        // even though it arrived over ADB at ingest_end (1130s).
        let delayed = TimestampedRecord {
            payload: "old-log",
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-1".into(),
                boottime_ns: 81_000_000_000,
                half_width_ns: 10_000_000,
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            pc_ingest_cont_ns: 1_130_000_000_000,
            estimated_bytes: 128,
        };

        assert_eq!(
            classify_alignment(&delayed.alignment, &window),
            Membership::OutOfRange,
            "anchored record 49s before trigger must classify as OutOfRange despite late PC arrival"
        );

        let part = select_records([delayed], &window, &SelectionPolicy::default());
        assert!(
            part.main.is_empty(),
            "delayed old record must not enter main"
        );
        assert!(
            part.unanchored.is_empty(),
            "delayed anchored old record must not enter unanchored"
        );
        assert_eq!(
            part.excluded_count, 1,
            "delayed old record must be excluded"
        );
    }

    #[test]
    fn regression_unanchored_137s_runaway_is_bounded_by_proximity_and_caps() {
        let window = sample_window();
        let policy = SelectionPolicy {
            unanchored_ingest_margin_ns: 2_000_000_000,
            max_unanchored_records: 3,
            max_unanchored_bytes: 512,
            drain_timeout_ms: 500,
        };

        // 1 record 137s before ingest_end (way outside proximity) + 5 records inside proximity.
        let mut recs = vec![TimestampedRecord {
            payload: "ancient-unanchored-137s",
            alignment: TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 1_130_000_000_000 - 137_000_000_000,
                reason: UncertaintyReason::NoClockAnchor,
            },
            pc_ingest_cont_ns: 1_130_000_000_000 - 137_000_000_000,
            estimated_bytes: 64,
        }];
        for idx in 0..5_i64 {
            let ingest = 1_129_000_000_000 + idx * 100_000_000;
            recs.push(TimestampedRecord {
                payload: "near-unanchored",
                alignment: TimeAlignment::Unanchored {
                    pc_ingest_cont_ns: ingest,
                    reason: UncertaintyReason::PcIngestOnly,
                },
                pc_ingest_cont_ns: ingest,
                estimated_bytes: 64,
            });
        }

        let part = select_records(recs, &window, &policy);
        assert!(
            part.main.is_empty(),
            "unanchored records must never enter main timeline"
        );
        assert_eq!(
            part.unanchored.len(),
            3,
            "in-proximity unanchored records must stop at max_unanchored_records"
        );
        assert_eq!(
            part.unanchored_truncated_count, 2,
            "excess in-proximity unanchored records must be counted as truncated"
        );
        assert_eq!(
            part.excluded_count, 1,
            "137s-old unanchored record outside ingest proximity must be excluded"
        );
    }

    #[test]
    fn bounded_and_cross_boot_classification_rules() {
        let window = sample_window();

        let inside = TimeAlignment::Bounded {
            boot_id: "boot-1".into(),
            min_boottime_ns: 105_000_000_000,
            max_boottime_ns: 120_000_000_000,
            reason: UncertaintyReason::DeviceSleepSpanned,
        };
        assert_eq!(
            classify_alignment(&inside, &window),
            Membership::InRange,
            "bounded interval completely inside window must be InRange"
        );

        let partial = TimeAlignment::Bounded {
            boot_id: "boot-1".into(),
            min_boottime_ns: 95_000_000_000,
            max_boottime_ns: 105_000_000_000,
            reason: UncertaintyReason::DeviceWallJumpSpanned,
        };
        assert_eq!(
            classify_alignment(&partial, &window),
            Membership::UncertainTime(UncertainPlacement::PartialOverlap),
            "partially overlapping bounded interval must be UncertainTime(PartialOverlap)"
        );

        let cross_boot = TimeAlignment::Anchored {
            boot_id: "boot-other".into(),
            boottime_ns: 115_000_000_000,
            half_width_ns: 0,
            semantics: TimestampSemantics::InputEventTime,
        };
        assert_eq!(
            classify_alignment(&cross_boot, &window),
            Membership::UncertainTime(UncertainPlacement::UnanchoredProximity),
            "cross-boot record must never be InRange"
        );
    }

    #[test]
    fn trim_carries_forward_source_unanchored_without_dropping() {
        let window = WindowSpec::FrameRange {
            boot_id: "boot-1".into(),
            in_frame_index: 10,
            out_frame_index: 20,
            start_boottime_ns: 110_000_000_000,
            end_boottime_ns: 120_000_000_000,
            ingest_start_cont_ns: 1_110_000_000_000,
            ingest_end_cont_ns: 1_120_000_000_000,
        };
        let main_records = vec![
            TimestampedRecord {
                payload: "in",
                alignment: TimeAlignment::Anchored {
                    boot_id: "boot-1".into(),
                    boottime_ns: 115_000_000_000,
                    half_width_ns: 0,
                    semantics: TimestampSemantics::LogcatHeaderWall,
                },
                pc_ingest_cont_ns: 1_115_000_000_000,
                estimated_bytes: 64,
            },
            TimestampedRecord {
                payload: "out",
                alignment: TimeAlignment::Anchored {
                    boot_id: "boot-1".into(),
                    boottime_ns: 101_000_000_000,
                    half_width_ns: 0,
                    semantics: TimestampSemantics::LogcatHeaderWall,
                },
                pc_ingest_cont_ns: 1_101_000_000_000,
                estimated_bytes: 64,
            },
        ];
        // Unanchored record from earlier in the parent bundle (even outside the narrow trim ingest window)
        // MUST still be carried forward transparently when trimming.
        let source_unanchored = vec![TimestampedRecord {
            payload: "carried",
            alignment: TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 1_100_000_000_000,
                reason: UncertaintyReason::NoClockAnchor,
            },
            pc_ingest_cont_ns: 1_100_000_000_000,
            estimated_bytes: 64,
        }];

        let trimmed = select_for_trim(
            main_records,
            source_unanchored,
            &window,
            &SelectionPolicy::default(),
        );
        assert_eq!(
            trimmed.main.len(),
            1,
            "only in-range record enters trimmed main"
        );
        assert_eq!(trimmed.main[0].payload, "in", "wrong main payload");
        assert_eq!(
            trimmed.unanchored.len(),
            1,
            "source_unanchored must be carried forward transparently"
        );
        assert_eq!(
            trimmed.unanchored[0].payload, "carried",
            "carried unanchored payload must match"
        );
        assert_eq!(
            trimmed.excluded_count, 1,
            "out-of-range main record must be excluded"
        );
    }

    #[test]
    fn drain_boundary_evaluation() {
        let policy = SelectionPolicy::default();
        assert_eq!(
            evaluate_stream_drain(100, Some(100), 10, &policy),
            DrainOutcome::DrainedComplete,
            "heartbeat at trigger completes drain"
        );
        assert_eq!(
            evaluate_stream_drain(100, Some(90), 250, &policy),
            DrainOutcome::Waiting,
            "below timeout without heartbeat stays Waiting"
        );
        assert_eq!(
            evaluate_stream_drain(100, None, 500, &policy),
            DrainOutcome::TimedOutTailIncomplete,
            "reaching drain_timeout_ms marks tail incomplete"
        );
    }

    #[test]
    fn regression_frame_range_selects_frames_strictly_by_frame_index_ignoring_half_width_leak() {
        use crate::bundle::FrameRecord;

        // 5 frames spaced 33 ms apart, each with ±40 ms half_width_ns (> 33 ms period!).
        // Timestamp-only classification would leak frame 0 (in - 1) and frame 4 (out + 1)
        // into a trim of [1, 3].
        let frames: Vec<FrameRecord> = (0_u64..5)
            .map(|idx| {
                let bt =
                    100_000_000_000_i64 + i64::try_from(idx).expect("idx fits i64") * 33_000_000;
                FrameRecord {
                    frame_index: idx,
                    boot_id: "boot-1".into(),
                    pts_device_mono_ns: bt,
                    pc_ingest_cont_ns: bt,
                    alignment: TimeAlignment::Anchored {
                        boot_id: "boot-1".into(),
                        boottime_ns: bt,
                        half_width_ns: 40_000_000,
                        semantics: TimestampSemantics::VideoEncoderInputEstimate,
                    },
                    is_keyframe: idx == 0,
                    gop_id: 0,
                    mp4_sample_offset: idx * 1024,
                    mp4_sample_size: 1024,
                }
            })
            .collect();

        let window =
            WindowSpec::from_frame_indices(&frames, 1, 3).expect("valid trim range [1, 3]");
        let trimmed = select_for_trim(frames, Vec::new(), &window, &SelectionPolicy::default());
        let selected_indices: Vec<u64> = trimmed.main.iter().map(|f| f.frame_index).collect();
        assert_eq!(
            selected_indices,
            vec![1, 2, 3],
            "FrameRange must select FrameRecords strictly by frame_index ∈ [1, 3] without leaking neighbor frames"
        );
        assert_eq!(
            trimmed.excluded_count, 2,
            "neighbor frames 0 and 4 must be excluded"
        );
    }

    #[test]
    fn regression_trim_quarantines_source_unanchored_first_and_propagates_truncated_count() {
        let window = WindowSpec::FrameRange {
            boot_id: "boot-1".into(),
            in_frame_index: 10,
            out_frame_index: 20,
            start_boottime_ns: 110_000_000_000,
            end_boottime_ns: 120_000_000_000,
            ingest_start_cont_ns: 1_110_000_000_000,
            ingest_end_cont_ns: 1_120_000_000_000,
        };
        let policy = SelectionPolicy {
            unanchored_ingest_margin_ns: 2_000_000_000,
            max_unanchored_records: 2,
            max_unanchored_bytes: 1024,
            drain_timeout_ms: 500,
        };

        // 3 newly-uncertain (PartialOverlap) records in source_main.
        let source_main: Vec<TimestampedRecord<&str>> = ["new-1", "new-2", "new-3"]
            .into_iter()
            .map(|label| TimestampedRecord {
                payload: label,
                alignment: TimeAlignment::Bounded {
                    boot_id: "boot-1".into(),
                    min_boottime_ns: 105_000_000_000,
                    max_boottime_ns: 112_000_000_000,
                    reason: UncertaintyReason::DeviceWallJumpSpanned,
                },
                pc_ingest_cont_ns: 1_112_000_000_000,
                estimated_bytes: 64,
            })
            .collect();

        // 2 pre-existing source unanchored sidecar records (must NOT be evicted by new-1..new-3).
        let source_unanchored: Vec<TimestampedRecord<&str>> = ["src-a", "src-b"]
            .into_iter()
            .map(|label| TimestampedRecord {
                payload: label,
                alignment: TimeAlignment::Unanchored {
                    pc_ingest_cont_ns: 1_100_000_000_000,
                    reason: UncertaintyReason::NoClockAnchor,
                },
                pc_ingest_cont_ns: 1_100_000_000_000,
                estimated_bytes: 64,
            })
            .collect();

        let trimmed = super::select_for_trim_with_truncated(
            source_main,
            source_unanchored,
            7,
            &window,
            &policy,
        );
        let kept: Vec<&str> = trimmed.unanchored.iter().map(|r| r.payload).collect();
        assert_eq!(
            kept,
            vec!["src-a", "src-b"],
            "existing source_unanchored records must be quarantined before newly-uncertain records consume caps"
        );
        assert_eq!(
            trimmed.unanchored_truncated_count, 10,
            "source bundle's prior unanchored_truncated_count (7) must be propagated plus 3 newly truncated"
        );
    }
}
