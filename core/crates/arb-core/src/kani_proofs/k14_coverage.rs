//! K14 (#564 / #570): Kani harnesses for the stream coverage ledger and static-screen carry-in
//! (`crate::coverage`).
//!
//! The harnesses run the production `select_carry_in`, `coverage_duration_in_window` and
//! `StreamCoverage` on fixed-length arrays with symbolic timestamps, keys and intervals. The
//! expected values are written here independently of the production code, so a one-line
//! mutation of the selection, the containment check or the clipping is caught.

use crate::coverage::{
    CarryCandidate, CoverageInterval, StreamCoverage, coverage_duration_in_window, select_carry_in,
};

/// Bounded symbolic timestamp: small enough for fast SAT, wide enough for every ordering.
fn any_ts() -> i64 {
    let t: i16 = kani::any();
    i64::from(t)
}

fn any_key() -> u8 {
    let k: u8 = kani::any();
    kani::assume(k < 3);
    k
}

fn any_frames() -> [CarryCandidate<u8>; 3] {
    let frames = [
        CarryCandidate {
            ts_ns: any_ts(),
            key: any_key(),
        },
        CarryCandidate {
            ts_ns: any_ts(),
            key: any_key(),
        },
        CarryCandidate {
            ts_ns: any_ts(),
            key: any_key(),
        },
    ];
    // Ingest order: evidence timestamps never decrease.
    kani::assume(frames[0].ts_ns <= frames[1].ts_ns && frames[1].ts_ns <= frames[2].ts_ns);
    frames
}

fn any_interval() -> CoverageInterval<u8> {
    let start_ns = any_ts();
    let end_ns = any_ts();
    kani::assume(start_ns <= end_ns);
    CoverageInterval {
        key: any_key(),
        start_ns,
        end_ns,
    }
}

/// Independent oracle: some interval with `key` contains the whole span `[from, to]`.
fn oracle_covered(coverage: &[CoverageInterval<u8>], key: u8, from: i64, to: i64) -> bool {
    let mut covered = false;
    for c in coverage {
        if c.key == key && c.start_ns <= from && to <= c.end_ns {
            covered = true;
        }
    }
    covered
}

/// Without one same-key interval spanning `[f_prev, W0]` (disconnect, gap, not yet started),
/// carry-in is always `None`. Conversely, when it is spanned the frame is selected.
#[kani::proof]
#[kani::unwind(4)]
pub fn k14_carry_in_none_without_coverage() {
    let frames = any_frames();
    let coverage = [any_interval(), any_interval()];
    let w0 = any_ts();
    let got = select_carry_in(&frames, w0, &coverage);

    // Oracle for f_prev: the last frame with ts < W0.
    let mut f_prev: Option<usize> = None;
    for (i, f) in frames.iter().enumerate() {
        if f.ts_ns < w0 {
            f_prev = Some(i);
        }
    }
    match f_prev {
        None => assert!(got.is_none(), "no frame before W0 must never carry in"),
        Some(i) => {
            let covered = oracle_covered(&coverage, frames[i].key, frames[i].ts_ns, w0);
            if !covered {
                assert!(got.is_none(), "uncovered span must never carry in");
            } else {
                assert!(got == Some(i), "a covered f_prev must be carried in");
            }
        }
    }
    kani::cover!(got.is_some(), "carry-in selected");
    kani::cover!(
        got.is_none() && f_prev.is_some(),
        "carry-in refused for a frame before W0"
    );
}

/// When carry-in succeeds the frame is before `W0`, no later frame is before `W0`, its
/// timestamp is the maximum of all pre-window frames, and coverage spans it to `W0`.
#[kani::proof]
#[kani::unwind(4)]
pub fn k14_carry_in_selects_latest_valid_pre_window_frame() {
    let frames = any_frames();
    let coverage = [any_interval(), any_interval()];
    let w0 = any_ts();
    if let Some(i) = select_carry_in(&frames, w0, &coverage) {
        assert!(i < frames.len());
        assert!(
            frames[i].ts_ns < w0,
            "selected frame must be captured before W0"
        );
        for (j, f) in frames.iter().enumerate() {
            if j > i {
                assert!(f.ts_ns >= w0, "a later pre-window frame replaced it");
            }
            if f.ts_ns < w0 {
                assert!(
                    f.ts_ns <= frames[i].ts_ns,
                    "must be the latest pre-window frame"
                );
            }
        }
        assert!(oracle_covered(
            &coverage,
            frames[i].key,
            frames[i].ts_ns,
            w0
        ));
        kani::cover!(
            i == 1,
            "middle frame selected while a later frame is in window"
        );
    }
}

/// A frame whose boot / session / dimensions key differs from every covering interval is never
/// selected, and a mismatching latest frame never falls back to an older matching one.
#[kani::proof]
#[kani::unwind(4)]
pub fn k14_carry_in_rejects_boot_session_or_dims_mismatch() {
    let frames = any_frames();
    let coverage = [any_interval(), any_interval()];
    let w0 = any_ts();
    let got = select_carry_in(&frames, w0, &coverage);
    if let Some(i) = got {
        let mut key_matches = false;
        for c in &coverage {
            if c.key == frames[i].key && c.start_ns <= frames[i].ts_ns && w0 <= c.end_ns {
                key_matches = true;
            }
        }
        assert!(
            key_matches,
            "selected frame must share the covering interval's key"
        );
    }
    // If the latest pre-window frame's key is absent from coverage, nothing is selected.
    let mut f_prev: Option<usize> = None;
    for (i, f) in frames.iter().enumerate() {
        if f.ts_ns < w0 {
            f_prev = Some(i);
        }
    }
    if let Some(i) = f_prev
        && coverage.iter().all(|c| c.key != frames[i].key)
    {
        assert!(got.is_none(), "mismatching f_prev must not fall back");
        kani::cover!(
            i > 0 && coverage.iter().any(|c| c.key == frames[0].key),
            "older frame with a matching key exists"
        );
    }
}

/// Covered duration is bounded by the window, monotone when an interval is appended, exact for
/// one interval, and a full-window interval yields exactly the window length.
#[kani::proof]
#[kani::unwind(4)]
pub fn k14_coverage_duration_bounded_and_monotone() {
    let a = any_interval();
    let b = any_interval();
    let lo = any_ts();
    let hi = any_ts();
    let window_len = if hi > lo {
        u64::try_from(i128::from(hi) - i128::from(lo)).unwrap_or(u64::MAX)
    } else {
        0
    };
    let one = coverage_duration_in_window(core::slice::from_ref(&a), lo, hi);
    let two = coverage_duration_in_window(&[a.clone(), b], lo, hi);
    assert!(one <= window_len, "0 <= cov <= window_len");
    assert!(two <= window_len, "0 <= cov <= window_len");
    assert!(two >= one, "appending an interval never decreases coverage");
    // Oracle for one interval: length of the clipped intersection.
    let s = a.start_ns.max(lo);
    let e = a.end_ns.min(hi);
    let expected = if hi > lo && e > s {
        u64::try_from(i128::from(e) - i128::from(s)).unwrap_or(u64::MAX)
    } else {
        0
    };
    assert!(
        one == expected,
        "single interval coverage equals the clipped length"
    );
    if a.start_ns <= lo && hi <= a.end_ns {
        assert!(one == window_len, "full coverage equals the window length");
    }
    kani::cover!(two > one, "second interval adds coverage");
    kani::cover!(one == window_len && window_len > 0, "fully covered window");
}

/// The ledger never extends coverage across a beacon gap and a beacon never opens coverage:
/// after data at `t0` and a beacon at `t1`, the interval ends at `t1` only within the gap.
#[kani::proof]
#[kani::unwind(4)]
pub fn k14_ledger_beacon_extends_only_within_gap() {
    let gap: u8 = kani::any();
    let gap = i64::from(gap);
    let t0 = any_ts();
    let t1 = any_ts();
    let key = any_key();
    let mut alive_only = StreamCoverage::<u8>::new(gap);
    alive_only.observe_alive(t1);
    assert!(!alive_only.is_open(), "a beacon alone never opens coverage");

    let mut c = StreamCoverage::new(gap);
    c.observe_data(key, t0);
    c.observe_alive(t1);
    let snap = c.snapshot(i64::MIN, i64::MAX);
    assert!(snap.len() == 1);
    assert!(snap[0].start_ns == t0 && snap[0].key == key);
    if t1 > t0 && t1 - t0 <= gap {
        assert!(snap[0].end_ns == t1 && c.is_open());
    } else {
        assert!(snap[0].end_ns == t0);
        if t1 > t0 {
            assert!(!c.is_open(), "a late beacon closes coverage");
        }
    }
    kani::cover!(snap[0].end_ns == t1 && t1 > t0, "beacon extended coverage");
    kani::cover!(!c.is_open(), "beacon gap closed coverage");
}
