//! Stream coverage ledger and static-screen carry-in selection (Issues #564 / #570).
//!
//! A frame or log row only proves that the stream produced data at one instant. The span
//! between the first and last row is therefore not the time a stream *covered*: a healthy but
//! static screen sends no video frames, and a quiet app writes no log lines. This module keeps
//! the evidence of continuous coverage separately:
//!
//! - [`StreamCoverage`] tracks closed intervals on a single time axis (the host
//!   `PcContinuousMono` ingest axis in `arbd`). Data (a frame, a log row) opens or extends an
//!   interval; a liveness beacon (heartbeat) extends an *open* interval only while it arrives
//!   within `max_gap_ns` of the previous evidence. Any gap, disconnect, shutdown, reboot or
//!   stream reconfiguration closes the interval ([`StreamCoverage::close`]).
//! - [`select_carry_in`] picks the frame that was on screen at the window start `W0` only when a
//!   single coverage interval with the same key proves the stream stayed alive from that frame
//!   until `W0` (K14).
//! - [`coverage_duration_in_window`] measures how long the coverage intersects a window; the
//!   result never exceeds the window length (K14).
//!
//! Everything here is pure and allocation-light so the K14 Kani harnesses and Miri can run the
//! production code directly.

use serde::{Deserialize, Serialize};

/// Maximum number of closed intervals retained per stream before the oldest is dropped.
pub const MAX_CLOSED_INTERVALS: usize = 1024;

/// One continuous coverage interval `[start_ns, end_ns]` (both inclusive) for `key`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CoverageInterval<K> {
    /// Identity of the stream configuration that produced the evidence.
    pub key: K,
    /// First evidence timestamp in nanoseconds.
    pub start_ns: i64,
    /// Last evidence timestamp in nanoseconds (`>= start_ns`).
    pub end_ns: i64,
}

impl<K> CoverageInterval<K> {
    /// Whether `[from_ns, to_ns]` lies entirely inside this interval.
    #[must_use]
    pub const fn contains_span(&self, from_ns: i64, to_ns: i64) -> bool {
        self.start_ns <= from_ns && from_ns <= to_ns && to_ns <= self.end_ns
    }
}

/// Video coverage identity: a carried-in frame must match the boot and the stream dimensions.
///
/// A new video session (`VideoSession`), a resolution change or a reboot closes the open
/// interval, so the interval containing `W0` can never start before such a boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoCoverageKey {
    /// Device boot epoch.
    pub boot_id: String,
    /// Encoded stream width in pixels.
    pub width: u16,
    /// Encoded stream height in pixels.
    pub height: u16,
}

/// A frame considered for carry-in: its evidence timestamp and its stream identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarryCandidate<K> {
    /// Evidence timestamp on the coverage axis (ingest time in `arbd`).
    pub ts_ns: i64,
    /// Stream identity; `None`-like keys that never match an interval mark ineligible frames.
    pub key: K,
}

/// Coverage intervals of one stream, with at most one interval open at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCoverage<K> {
    max_gap_ns: i64,
    closed: Vec<CoverageInterval<K>>,
    open: Option<CoverageInterval<K>>,
}

impl<K: Clone + PartialEq> StreamCoverage<K> {
    /// Creates an empty tracker; beacons extend coverage only within `max_gap_ns`.
    #[must_use]
    pub const fn new(max_gap_ns: i64) -> Self {
        Self {
            max_gap_ns,
            closed: Vec::new(),
            open: None,
        }
    }

    /// Data evidence (a frame or a log row) for `key` at `t_ns`.
    ///
    /// Extends the open interval when it has the same key and the data arrives within the
    /// allowed gap; otherwise closes it and opens a new one at `t_ns`. Evidence at or before the
    /// current end of a same-key interval changes nothing.
    pub fn observe_data(&mut self, key: K, t_ns: i64) {
        if let Some(open) = self.open.as_mut()
            && open.key == key
        {
            if t_ns <= open.end_ns {
                return;
            }
            if t_ns.saturating_sub(open.end_ns) <= self.max_gap_ns {
                open.end_ns = t_ns;
                return;
            }
        }
        self.close();
        self.open = Some(CoverageInterval {
            key,
            start_ns: t_ns,
            end_ns: t_ns,
        });
    }

    /// Liveness evidence (a heartbeat) at `t_ns`: extends the open interval only while the
    /// beacon stays within `max_gap_ns` of the previous evidence, and closes it otherwise.
    /// A beacon never opens coverage on its own.
    pub fn observe_alive(&mut self, t_ns: i64) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if t_ns <= open.end_ns {
            return;
        }
        if t_ns.saturating_sub(open.end_ns) <= self.max_gap_ns {
            open.end_ns = t_ns;
        } else {
            self.close();
        }
    }

    /// Closes the open interval at its last evidence (gap, disconnect, shutdown, reboot,
    /// reconfiguration or stop). Coverage is never extended past the last evidence.
    pub fn close(&mut self) {
        if let Some(open) = self.open.take() {
            self.closed.push(open);
            if self.closed.len() > MAX_CLOSED_INTERVALS {
                self.closed.remove(0);
            }
        }
    }

    /// Drops closed intervals that ended before `horizon_ns`.
    pub fn prune_before(&mut self, horizon_ns: i64) {
        self.closed.retain(|c| c.end_ns >= horizon_ns);
    }

    /// Returns the open interval only while its evidence is fresh on the ingest clock.
    #[must_use]
    pub fn current_at(&self, now_ns: i64) -> Option<&CoverageInterval<K>> {
        self.open.as_ref().filter(|interval| {
            (0..=self.max_gap_ns).contains(&now_ns.saturating_sub(interval.end_ns))
        })
    }

    /// Whether an interval is currently open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Intervals (closed and open) that intersect `[lo_ns, hi_ns]`, unclipped and in order.
    ///
    /// Starts are kept so the carry-in check can see where an interval reaching `W0` began.
    #[must_use]
    pub fn snapshot(&self, lo_ns: i64, hi_ns: i64) -> Vec<CoverageInterval<K>> {
        self.closed
            .iter()
            .chain(self.open.iter())
            .filter(|c| c.end_ns >= lo_ns && c.start_ns <= hi_ns)
            .cloned()
            .collect()
    }
}

/// Logcat coverage is keyed by the device boot epoch.
pub type LogcatCoverageKey = String;

/// Per-session coverage ledger for the video and logcat streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageLedger {
    /// Video coverage (frames open/extend; beacons extend a still-alive static screen).
    pub video: StreamCoverage<VideoCoverageKey>,
    /// Logcat coverage (log rows open/extend; beacons extend a running logcat stream).
    pub logcat: StreamCoverage<LogcatCoverageKey>,
}

impl CoverageLedger {
    /// Creates an empty ledger with the beacon timeout used to bridge evidence.
    #[must_use]
    pub const fn new(max_gap_ns: i64) -> Self {
        Self {
            video: StreamCoverage::new(max_gap_ns),
            logcat: StreamCoverage::new(max_gap_ns),
        }
    }

    /// Closes both streams (disconnect, shutdown, reboot, stop or a new capture generation).
    pub fn close_all(&mut self) {
        self.video.close();
        self.logcat.close();
    }

    /// Drops all history (device switch).
    pub fn clear(&mut self) {
        let gap = self.video.max_gap_ns;
        *self = Self::new(gap);
    }

    /// Bounded memory: drops closed intervals that ended before `horizon_ns`.
    pub fn prune_before(&mut self, horizon_ns: i64) {
        self.video.prune_before(horizon_ns);
        self.logcat.prune_before(horizon_ns);
    }

    /// Snapshot persisted with a save job so recovery reproduces the same decisions.
    #[must_use]
    pub fn snapshot(&self, lo_ns: i64, hi_ns: i64) -> CoverageSnapshot {
        CoverageSnapshot {
            video: self.video.snapshot(lo_ns, hi_ns),
            logcat: self.logcat.snapshot(lo_ns, hi_ns),
        }
    }
}

/// Coverage intervals persisted in the Phase-1 job parameters (host ingest axis).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageSnapshot {
    /// Video coverage intervals intersecting the save window.
    #[serde(default)]
    pub video: Vec<CoverageInterval<VideoCoverageKey>>,
    /// Logcat coverage intervals intersecting the save window.
    #[serde(default)]
    pub logcat: Vec<CoverageInterval<LogcatCoverageKey>>,
}

/// Selects the frame carried into a window that starts at `w0_ns` (K14).
///
/// `f_prev` is the last frame (by position) whose evidence time is before `w0_ns`; frames are
/// expected in ingest order. It is returned only when one coverage interval with the same key
/// contains the whole span `[f_prev.ts_ns, w0_ns]`. A key mismatch of `f_prev` never falls back
/// to an older frame: the older frame was no longer on screen at `W0`.
#[must_use]
pub fn select_carry_in<K: PartialEq>(
    frames: &[CarryCandidate<K>],
    w0_ns: i64,
    coverage: &[CoverageInterval<K>],
) -> Option<usize> {
    let idx = frames.iter().rposition(|f| f.ts_ns < w0_ns)?;
    let f_prev = &frames[idx];
    coverage
        .iter()
        .any(|c| c.key == f_prev.key && c.contains_span(f_prev.ts_ns, w0_ns))
        .then_some(idx)
}

/// End of the coverage interval with `key` that contains `ts_ns`, if any (latest end wins).
#[must_use]
pub fn covering_end<K: PartialEq>(
    coverage: &[CoverageInterval<K>],
    key: &K,
    ts_ns: i64,
) -> Option<i64> {
    coverage
        .iter()
        .filter(|c| c.key == *key && c.start_ns <= ts_ns && ts_ns <= c.end_ns)
        .map(|c| c.end_ns)
        .max()
}

/// Nanoseconds of `[lo_ns, hi_ns]` covered by `coverage` (K14: `0 <= result <= hi - lo`).
///
/// Each interval is clipped to the window and only the part after everything already counted
/// is added, so overlapping intervals are never counted twice. For intervals sorted by start
/// this is the exact length of the union; for any order it stays within the window length.
#[must_use]
pub fn coverage_duration_in_window<K>(
    coverage: &[CoverageInterval<K>],
    lo_ns: i64,
    hi_ns: i64,
) -> u64 {
    if hi_ns <= lo_ns {
        return 0;
    }
    let mut cursor = i128::from(lo_ns);
    let hi = i128::from(hi_ns);
    let mut total: i128 = 0;
    for c in coverage {
        let start = i128::from(c.start_ns).max(cursor);
        let end = i128::from(c.end_ns).min(hi);
        if end > start {
            total += end - start;
            cursor = end;
        }
    }
    u64::try_from(total).unwrap_or(u64::MAX)
}

/// [`coverage_duration_in_window`] restricted to intervals with `key`.
#[must_use]
pub fn coverage_duration_for_key<K: PartialEq + Clone>(
    coverage: &[CoverageInterval<K>],
    key: &K,
    lo_ns: i64,
    hi_ns: i64,
) -> u64 {
    let matching: Vec<CoverageInterval<K>> =
        coverage.iter().filter(|c| c.key == *key).cloned().collect();
    coverage_duration_in_window(&matching, lo_ns, hi_ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SEC: i64 = 1_000_000_000;

    fn iv(key: u8, start_ns: i64, end_ns: i64) -> CoverageInterval<u8> {
        CoverageInterval {
            key,
            start_ns,
            end_ns,
        }
    }

    #[test]
    fn heartbeats_extend_a_static_screen_until_a_gap() {
        let mut c = StreamCoverage::new(1_500_000_000);
        c.observe_alive(0); // a beacon alone never opens coverage
        assert!(!c.is_open());
        c.observe_data(1_u8, SEC);
        for t in 1..=30 {
            c.observe_alive(SEC + t * 300_000_000);
        }
        assert_eq!(c.snapshot(0, 100 * SEC), vec![iv(1, SEC, SEC + 9 * SEC)]);
        // A 2 s silence (beacon timeout 1.5 s) closes at the last evidence.
        c.observe_alive(12 * SEC);
        assert!(!c.is_open());
        assert_eq!(c.snapshot(0, 100 * SEC), vec![iv(1, SEC, 10 * SEC)]);
        // The next beacon does not reopen it; only data does.
        c.observe_alive(12 * SEC + 300_000_000);
        assert!(!c.is_open());
    }

    #[test]
    fn key_change_and_explicit_close_split_intervals() {
        let mut c = StreamCoverage::new(SEC);
        c.observe_data(1_u8, 0);
        c.observe_data(1_u8, 500_000_000);
        c.observe_data(2_u8, 600_000_000);
        c.close();
        c.observe_alive(700_000_000);
        c.observe_data(2_u8, 800_000_000);
        assert_eq!(
            c.snapshot(0, SEC),
            vec![
                iv(1, 0, 500_000_000),
                iv(2, 600_000_000, 600_000_000),
                iv(2, 800_000_000, 800_000_000)
            ]
        );
        c.prune_before(700_000_000);
        assert_eq!(c.snapshot(i64::MIN, i64::MAX).len(), 1);
    }

    #[test]
    fn closed_history_is_bounded() {
        let mut c = StreamCoverage::new(0);
        for i in 0..(i64::try_from(MAX_CLOSED_INTERVALS).unwrap() + 10) {
            c.observe_data(0_u8, i * 10);
            c.close();
        }
        let all = c.snapshot(i64::MIN, i64::MAX);
        assert_eq!(all.len(), MAX_CLOSED_INTERVALS);
        assert_eq!(all[0].start_ns, 100);
    }

    #[test]
    fn carry_in_requires_coverage_from_the_frame_to_w0() {
        let frames = [
            CarryCandidate {
                ts_ns: 0,
                key: 1_u8,
            },
            CarryCandidate {
                ts_ns: 5 * SEC,
                key: 1,
            },
            CarryCandidate {
                ts_ns: 40 * SEC,
                key: 1,
            },
        ];
        // Static from 5 s until after W0 = 30 s.
        assert_eq!(
            select_carry_in(&frames, 30 * SEC, &[iv(1, 0, 31 * SEC)]),
            Some(1)
        );
        // Coverage closed at 20 s (disconnect): never carry in.
        assert_eq!(
            select_carry_in(&frames, 30 * SEC, &[iv(1, 0, 20 * SEC)]),
            None
        );
        // Coverage resumed after the frame: the frame is not proven on screen at W0.
        assert_eq!(
            select_carry_in(
                &frames,
                30 * SEC,
                &[iv(1, 0, 6 * SEC), iv(1, 7 * SEC, 31 * SEC)]
            ),
            None
        );
        // A different key (other boot / dimensions) never matches.
        assert_eq!(
            select_carry_in(&frames, 30 * SEC, &[iv(2, 0, 31 * SEC)]),
            None
        );
        // No frame before W0.
        assert_eq!(select_carry_in(&frames, 0, &[iv(1, -SEC, SEC)]), None);
        // The latest pre-window frame mismatching never falls back to an older one.
        let mixed = [
            CarryCandidate {
                ts_ns: 0,
                key: 1_u8,
            },
            CarryCandidate { ts_ns: SEC, key: 2 },
        ];
        assert_eq!(select_carry_in(&mixed, 2 * SEC, &[iv(1, 0, 3 * SEC)]), None);
    }

    #[test]
    fn covered_duration_counts_overlaps_once() {
        let cov = [iv(0, 0, 10), iv(0, 5, 20), iv(0, 30, 40)];
        assert_eq!(coverage_duration_in_window(&cov, 0, 35), 25);
        assert_eq!(coverage_duration_in_window(&cov, 35, 35), 0);
        assert_eq!(coverage_duration_in_window(&cov, 40, 0), 0);
        assert_eq!(coverage_duration_for_key(&cov, &1, 0, 35), 0);
        assert_eq!(covering_end(&cov, &0, 7), Some(20));
        assert_eq!(covering_end(&cov, &0, 25), None);
    }

    /// No failure-persistence file (Miri isolation forbids `getcwd`); fewer cases under Miri.
    fn miri_safe_proptest_config() -> ProptestConfig {
        let mut cfg = ProptestConfig {
            failure_persistence: None,
            ..ProptestConfig::default()
        };
        if cfg!(miri) {
            cfg.cases = 8;
        }
        cfg
    }

    proptest! {
        #![proptest_config(miri_safe_proptest_config())]

        #[test]
        fn covered_duration_is_bounded_by_the_window(
            raw in proptest::collection::vec((-1_000i64..1_000, 0i64..500), 0..6),
            lo in -1_000i64..1_000,
            len in 0i64..2_000,
        ) {
            let cov: Vec<_> = raw.iter().map(|&(s, l)| iv(0, s, s + l)).collect();
            let got = coverage_duration_in_window(&cov, lo, lo + len);
            prop_assert!(got <= u64::try_from(len).unwrap());
            // Exact union length when sorted by start (reference: count covered unit cells).
            let mut sorted = cov;
            sorted.sort_by_key(|c| c.start_ns);
            let exact = coverage_duration_in_window(&sorted, lo, lo + len);
            let oracle = (lo..lo + len)
                .filter(|&t| sorted.iter().any(|c| c.start_ns <= t && t < c.end_ns))
                .count();
            prop_assert_eq!(exact, u64::try_from(oracle).unwrap());
        }

        #[test]
        fn carry_in_never_crosses_a_coverage_hole(
            ts in proptest::collection::vec(0i64..100, 1..6),
            w0 in 0i64..120,
            cov in proptest::collection::vec((0i64..120, 0i64..60, 0u8..2), 0..4),
        ) {
            let mut ts = ts;
            ts.sort_unstable();
            let frames: Vec<_> = ts.iter().map(|&t| CarryCandidate { ts_ns: t, key: 0_u8 }).collect();
            let cov: Vec<_> = cov.iter().map(|&(s, l, k)| iv(k, s, s + l)).collect();
            if let Some(i) = select_carry_in(&frames, w0, &cov) {
                prop_assert!(frames[i].ts_ns < w0);
                prop_assert!(frames[i + 1..].iter().all(|f| f.ts_ns >= w0));
                prop_assert!(cov.iter().any(|c| c.key == 0 && c.start_ns <= frames[i].ts_ns && w0 <= c.end_ns));
            }
        }
    }
}
