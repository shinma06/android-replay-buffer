//! Layer-1 PDD: Kani bounded model-checking harnesses for `arb-core` (K1–K7).
//!
//! Compiled only under `cargo kani` (`cfg(kani)`); unwinding assertions stay enabled.
//!
//! Harnesses gated by `cfg_attr(feature = "kani-extended", kani::proof, ...)` did not verify
//! within five minutes locally (Kani 0.68 / CBMC 6.11). They run only with
//! `cargo kani -p <crate> --features kani-extended` and in the non-blocking `kani-extended`
//! job of `.github/workflows/pdd-kani.yml`; `docs/proofs/tcb-axioms.md` §3 lists them.
#![cfg_attr(not(feature = "kani-extended"), allow(dead_code))]

use crate::gesture::{TouchPoint, classify_stroke, decimate, map_touch_to_frame, touch_slop};
use crate::selection::{
    AlignedRecord, Membership, SelectionPolicy, TimestampedRecord, UncertainPlacement, WindowSpec,
    classify_alignment, classify_record, select_for_trim_with_truncated, select_records,
};
use crate::state::{
    BundleLifecycleState, PinStatus, RingBudgetPolicy, RingChunkMeta, RingChunkStatus,
    RingEvictionPlanner, RingWriteDecision, SaveJob, SaveJobEvent, SaveJobState,
};
use crate::time::{
    ClockTimeline, ClockTimelineConfig, DeviceClockTriple, PcSyncProbe, TimeAlignment,
    TimestampSemantics, UncertaintyReason,
};

fn any_boot() -> String {
    if kani::any() {
        "a".to_owned()
    } else {
        "b".to_owned()
    }
}

fn any_alignment() -> TimeAlignment {
    alignment_of(kani::any::<u8>() % 3)
}

fn alignment_of(variant: u8) -> TimeAlignment {
    match variant {
        0 => TimeAlignment::Anchored {
            boot_id: any_boot(),
            boottime_ns: kani::any(),
            half_width_ns: kani::any(),
            semantics: TimestampSemantics::LogcatHeaderWall,
        },
        1 => TimeAlignment::Bounded {
            boot_id: any_boot(),
            min_boottime_ns: kani::any(),
            max_boottime_ns: kani::any(),
            reason: UncertaintyReason::DeviceSleepSpanned,
        },
        _ => TimeAlignment::Unanchored {
            pc_ingest_cont_ns: kani::any(),
            reason: UncertaintyReason::NoClockAnchor,
        },
    }
}

fn real_window(boot_id: &str, start: i64, end: i64) -> WindowSpec {
    WindowSpec::RealElapsed {
        boot_id: boot_id.to_owned(),
        start_boottime_ns: start,
        end_boottime_ns: end,
        ingest_start_cont_ns: kani::any(),
        ingest_end_cont_ns: kani::any(),
    }
}

/// Independent i128 reference oracle for [`classify_alignment`].
fn oracle(alignment: &TimeAlignment, w_boot: &str, ws: i128, we: i128) -> Membership {
    let (ws, we) = (ws.min(we), ws.max(we));
    match alignment {
        TimeAlignment::Anchored {
            boot_id,
            boottime_ns,
            half_width_ns,
            ..
        } if boot_id == w_boot => {
            let lo = i128::from(*boottime_ns) - i128::from(*half_width_ns);
            let hi = i128::from(*boottime_ns) + i128::from(*half_width_ns);
            if hi < ws || lo > we {
                Membership::OutOfRange
            } else {
                Membership::InRange
            }
        }
        TimeAlignment::Bounded {
            boot_id,
            min_boottime_ns,
            max_boottime_ns,
            ..
        } if boot_id == w_boot => {
            let lo = i128::from((*min_boottime_ns).min(*max_boottime_ns));
            let hi = i128::from((*min_boottime_ns).max(*max_boottime_ns));
            if ws <= lo && hi <= we {
                Membership::InRange
            } else if hi < ws || lo > we {
                Membership::OutOfRange
            } else {
                Membership::UncertainTime(UncertainPlacement::PartialOverlap)
            }
        }
        _ => Membership::UncertainTime(UncertainPlacement::UnanchoredProximity),
    }
}

// ---------------------------------------------------------------- K1
fn k1_oracle_and_swap(variant: u8) {
    let alignment = alignment_of(variant);
    let w_boot = any_boot();
    let (s, e): (i64, i64) = (kani::any(), kani::any());
    let m = classify_alignment(&alignment, &real_window(&w_boot, s, e));
    assert_eq!(m, oracle(&alignment, &w_boot, i128::from(s), i128::from(e)));
    let swapped = classify_alignment(&alignment, &real_window(&w_boot, e, s));
    assert_eq!(m, swapped, "endpoint swap must not change membership");
    if variant == 2 {
        assert!(matches!(
            m,
            Membership::UncertainTime(UncertainPlacement::UnanchoredProximity)
        ));
    }
}

#[kani::proof]
fn k1_anchored_matches_oracle_and_swap_invariant() {
    k1_oracle_and_swap(0);
}

#[kani::proof]
fn k1_bounded_matches_oracle_and_swap_invariant() {
    k1_oracle_and_swap(1);
}

#[kani::proof]
fn k1_unanchored_always_uncertain() {
    k1_oracle_and_swap(2);
}

fn k1_expansion(variant: u8) {
    let alignment = alignment_of(variant);
    let (s, e): (i64, i64) = (kani::any(), kani::any());
    kani::assume(s <= e);
    let (s2, e2): (i64, i64) = (kani::any(), kani::any());
    kani::assume(s2 <= s && e <= e2);
    let narrow = classify_alignment(&alignment, &real_window("a", s, e));
    let wide = classify_alignment(&alignment, &real_window("a", s2, e2));
    if narrow == Membership::InRange {
        assert_eq!(wide, Membership::InRange, "W ⊆ W' keeps InRange");
    }
    if wide == Membership::OutOfRange {
        assert_eq!(
            narrow,
            Membership::OutOfRange,
            "disjoint from W' ⇒ disjoint from W"
        );
    }
}

#[kani::proof]
fn k1_anchored_window_expansion_never_demotes() {
    k1_expansion(0);
}

#[kani::proof]
fn k1_bounded_window_expansion_never_demotes() {
    k1_expansion(1);
}

#[derive(Debug)]
struct VideoRecord {
    alignment: TimeAlignment,
    frame_index: u64,
}

impl AlignedRecord for VideoRecord {
    fn time_alignment(&self) -> &TimeAlignment {
        &self.alignment
    }
    fn pc_ingest_cont_ns(&self) -> i64 {
        0
    }
    fn estimated_bytes(&self) -> usize {
        1
    }
    fn frame_index(&self) -> Option<u64> {
        Some(self.frame_index)
    }
}

fn k1_frame_range(variant: u8) {
    let w_boot = any_boot();
    let (in_f, out_f): (u64, u64) = (kani::any(), kani::any());
    let window = WindowSpec::FrameRange {
        boot_id: w_boot.clone(),
        in_frame_index: in_f,
        out_frame_index: out_f,
        start_boottime_ns: kani::any(),
        end_boottime_ns: kani::any(),
        ingest_start_cont_ns: 0,
        ingest_end_cont_ns: 0,
    };
    let rec = VideoRecord {
        alignment: alignment_of(variant),
        frame_index: kani::any(),
    };
    let m = classify_record(&rec, &window);
    let same_boot = match &rec.alignment {
        TimeAlignment::Anchored { boot_id, .. } | TimeAlignment::Bounded { boot_id, .. } => {
            *boot_id == w_boot
        }
        _ => false,
    };
    let in_idx = in_f <= rec.frame_index && rec.frame_index <= out_f;
    assert_eq!(m == Membership::InRange, same_boot && in_idx);
    assert_eq!(m == Membership::OutOfRange, same_boot && !in_idx);
    if !same_boot {
        assert!(
            matches!(m, Membership::UncertainTime(_)),
            "unanchored/cross-boot never main"
        );
    }
}

#[kani::proof]
fn k1_frame_range_anchored_by_index_only() {
    k1_frame_range(0);
}

#[kani::proof]
fn k1_frame_range_bounded_by_index_only() {
    k1_frame_range(1);
}

#[kani::proof]
fn k1_frame_range_unanchored_never_main() {
    k1_frame_range(2);
}

// ---------------------------------------------------------------- K2
/// Small-domain alignment for the K2 partition/budget harnesses. Classification over the full
/// `i64`/`u64` domain is K1's job; K2 only needs every membership outcome to be reachable, and the
/// full-domain variant ran CBMC out of memory for `n >= 2`.
fn any_small_alignment() -> TimeAlignment {
    match kani::any::<u8>() % 3 {
        0 => TimeAlignment::Anchored {
            boot_id: any_boot(),
            boottime_ns: i64::from(kani::any::<i8>()),
            half_width_ns: u64::from(kani::any::<u8>() % 8),
            semantics: TimestampSemantics::LogcatHeaderWall,
        },
        1 => TimeAlignment::Bounded {
            boot_id: any_boot(),
            min_boottime_ns: i64::from(kani::any::<i8>()),
            max_boottime_ns: i64::from(kani::any::<i8>()),
            reason: UncertaintyReason::DeviceSleepSpanned,
        },
        _ => TimeAlignment::Unanchored {
            pc_ingest_cont_ns: i64::from(kani::any::<i8>()),
            reason: UncertaintyReason::NoClockAnchor,
        },
    }
}

fn small_window(boot_id: &str) -> WindowSpec {
    WindowSpec::RealElapsed {
        boot_id: boot_id.to_owned(),
        start_boottime_ns: i64::from(kani::any::<i8>()),
        end_boottime_ns: i64::from(kani::any::<i8>()),
        ingest_start_cont_ns: i64::from(kani::any::<i8>()),
        ingest_end_cont_ns: i64::from(kani::any::<i8>()),
    }
}

fn any_record(tag: u8) -> TimestampedRecord<u8> {
    TimestampedRecord {
        payload: tag,
        alignment: any_small_alignment(),
        pc_ingest_cont_ns: i64::from(kani::any::<i8>()),
        estimated_bytes: usize::from(kani::any::<u8>() % 16),
    }
}

fn any_policy() -> SelectionPolicy {
    SelectionPolicy {
        unanchored_ingest_margin_ns: u64::from(kani::any::<u8>() % 8),
        max_unanchored_records: usize::from(kani::any::<u8>() % 4),
        max_unanchored_bytes: usize::from(kani::any::<u8>() % 48),
        drain_timeout_ms: 0,
    }
}

fn any_records(n: usize, base: u8) -> Vec<TimestampedRecord<u8>> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < n {
        v.push(any_record(base + u8::try_from(i).unwrap_or(0)));
        i += 1;
    }
    v
}

/// K2 equivalence body for a concrete record count; split per `n` so each CBMC
/// problem stays small (the symbolic-`n` variant ran out of memory).
fn k2_equivalence(n: usize) {
    let records = any_records(n, 0);
    let window = small_window(&any_boot());
    let policy = any_policy();
    let a = select_records(records.clone(), &window, &policy);
    let b = select_for_trim_with_truncated(records.clone(), [], 0, &window, &policy);
    assert_eq!(a, b, "save and trim must share one partition");
    assert_eq!(
        a.main.len() + a.unanchored.len() + a.unanchored_truncated_count + a.excluded_count,
        n,
        "conservation"
    );
    assert!(a.unanchored.len() <= policy.max_unanchored_records);
    let bytes: usize = a.unanchored.iter().map(|r| r.estimated_bytes).sum();
    assert!(bytes <= policy.max_unanchored_bytes);
    for r in &a.main {
        assert_eq!(classify_record(r, &window), Membership::InRange);
    }
    // Input order is preserved (payload tags strictly increasing).
    let mut i = 1;
    while i < a.main.len() {
        assert!(a.main[i - 1].payload < a.main[i].payload);
        i += 1;
    }
}

#[kani::proof]
#[kani::unwind(3)]
fn k2_select_records_equals_trim_with_empty_sidecar_n1() {
    k2_equivalence(1);
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(4))]
fn k2_select_records_equals_trim_with_empty_sidecar_n2() {
    k2_equivalence(2);
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(5))]
fn k2_select_records_equals_trim_with_empty_sidecar_n3() {
    k2_equivalence(3);
}

/// K2 prior-sidecar body for concrete counts; split per shape because the symbolic-count
/// variant exhausted CBMC memory.
fn k2_prior_sidecar(n_prior: usize, n_main: usize) {
    let prior = any_records(n_prior, 100);
    let main = any_records(n_main, 0);
    let prior_truncated = usize::from(kani::any::<u8>() % 4);
    let window = small_window("a");
    let policy = any_policy();
    let p = select_for_trim_with_truncated(main, prior, prior_truncated, &window, &policy);
    assert!(
        p.main.iter().all(|r| r.payload < 100),
        "prior sidecar never promoted to main"
    );
    assert_eq!(
        p.main.len() + p.unanchored.len() + p.unanchored_truncated_count + p.excluded_count,
        n_main + n_prior + prior_truncated,
        "conservation incl. prior sidecar and prior truncated count"
    );
    assert!(p.unanchored.len() <= policy.max_unanchored_records);
    let bytes: usize = p.unanchored.iter().map(|r| r.estimated_bytes).sum();
    assert!(bytes <= policy.max_unanchored_bytes);
    // Prior entries are admitted first: a new record can only sit before none of them.
    let mut seen_new = false;
    for r in &p.unanchored {
        if r.payload < 100 {
            seen_new = true;
        } else {
            assert!(
                !seen_new,
                "prior sidecar entries are quarantined before new records"
            );
        }
    }
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(4))]
fn k2_prior_sidecar_none_main_two() {
    k2_prior_sidecar(0, 2);
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(4))]
fn k2_prior_sidecar_one_consumes_budget_first() {
    k2_prior_sidecar(1, 1);
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(4))]
fn k2_prior_sidecar_two_consumes_budget_first() {
    k2_prior_sidecar(2, 1);
}

// ---------------------------------------------------------------- K3
fn any_probe() -> PcSyncProbe {
    PcSyncProbe {
        boot_id: "a".to_owned(),
        pc_send_cont_ns: kani::any(),
        pc_send_active_ns: kani::any(),
        dev_boottime_ns: kani::any(),
        dev_monotonic_ns: kani::any(),
        pc_recv_cont_ns: kani::any(),
        pc_recv_active_ns: kani::any(),
    }
}

#[kani::proof]
fn k3_pc_sync_probe_boundary_math() {
    let p = any_probe();
    let (send, recv) = (p.pc_send_cont_ns, p.pc_recv_cont_ns);
    let mid = p.pc_midpoint_cont_ns();
    if send > recv {
        assert!(p.rtt_ns().is_none() && p.half_width_ns().is_none());
    } else {
        let rtt = p.rtt_ns().unwrap_or(0);
        assert_eq!(i128::from(rtt), i128::from(recv) - i128::from(send));
        let hw = p.half_width_ns().unwrap_or(u64::MAX);
        assert!(hw <= rtt);
        assert!(send <= mid && mid <= recv);
        let (lo, hi) = (
            i128::from(mid) - i128::from(hw),
            i128::from(mid) + i128::from(hw),
        );
        assert!(
            lo <= i128::from(send) && i128::from(recv) <= hi,
            "no odd-RTT gap"
        );
    }
    let exact = i128::from(p.dev_boottime_ns) - i128::from(mid);
    assert_eq!(p.offset_ns().is_some(), i64::try_from(exact).is_ok());
}

#[kani::proof]
fn k3_elapsed_same_boot() {
    let (a, b) = (any_boot(), any_boot());
    let (s, e): (i64, i64) = (kani::any(), kani::any());
    let r = ClockTimeline::elapsed_same_boot(&a, s, &b, e);
    if a != b {
        assert!(r.is_err(), "cross-boot arithmetic forbidden");
    } else {
        let exact = i128::from(e) - i128::from(s);
        assert_eq!(
            r.ok().map(i128::from),
            i64::try_from(exact).ok().map(i128::from)
        );
    }
}

fn timeline_with_triples(n: usize) -> ClockTimeline {
    let mut tl = ClockTimeline::new(ClockTimelineConfig::new());
    let mut i = 0;
    while i < n {
        let t = DeviceClockTriple {
            boot_id: "a".to_owned(),
            boottime_ns: kani::any(),
            monotonic_ns: kani::any(),
            realtime_ns: kani::any(),
        };
        let _ = tl.ingest_device_triple(t, kani::any());
        i += 1;
    }
    tl
}

fn assert_well_formed(al: &TimeAlignment) {
    if let TimeAlignment::Bounded {
        min_boottime_ns,
        max_boottime_ns,
        ..
    } = al
    {
        assert!(min_boottime_ns <= max_boottime_ns, "Bounded min <= max");
    }
}

/// Split per concrete triple count: the symbolic-count variant exceeded the 15-minute CBMC
/// timebox (three fully symbolic `ingest_device_triple` calls).
fn k3_map_device_boot_isolated(n: usize) {
    let tl = timeline_with_triples(n);
    let mono: i64 = kani::any();
    let al = tl.map_device_monotonic("a", mono, TimestampSemantics::InputEventTime, 7);
    assert_well_formed(&al);
    let cross = tl.map_device_monotonic("b", mono, TimestampSemantics::InputEventTime, 7);
    assert!(
        matches!(cross, TimeAlignment::Unanchored { .. }),
        "cross-boot never anchored"
    );
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(4))]
fn k3_map_device_one_triple_no_panic_and_boot_isolated() {
    k3_map_device_boot_isolated(1);
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(4))]
fn k3_map_device_two_triples_no_panic_and_boot_isolated() {
    k3_map_device_boot_isolated(2);
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(4))]
fn k3_map_device_monotonic_monotone_on_two_triples() {
    let tl = timeline_with_triples(2);
    let (m1, m2): (i64, i64) = (kani::any(), kani::any());
    kani::assume(m1 <= m2);
    let a1 = tl.map_device_monotonic("a", m1, TimestampSemantics::InputEventTime, 0);
    let a2 = tl.map_device_monotonic("a", m2, TimestampSemantics::InputEventTime, 0);
    if let (
        TimeAlignment::Anchored {
            boottime_ns: b1, ..
        },
        TimeAlignment::Anchored {
            boottime_ns: b2, ..
        },
    ) = (&a1, &a2)
    {
        assert!(b1 <= b2, "anchored monotonic mapping is order preserving");
    }
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(5))]
fn k3_map_logcat_wall_no_panic_and_causal_ceiling() {
    let n = usize::from(kani::any::<u8>() % 3) + 1;
    let tl = timeline_with_triples(n);
    let header: i64 = kani::any();
    let read: Option<i64> = if kani::any() { Some(kani::any()) } else { None };
    let al = tl.map_logcat_wall("a", header, read, 3);
    assert_well_formed(&al);
    if let (TimeAlignment::Anchored { boottime_ns, .. }, Some(read_bt)) = (&al, read) {
        let tol = i128::from(tl.config().wall_jump_threshold_ns);
        assert!(
            i128::from(*boottime_ns) <= i128::from(read_bt) + tol,
            "never after read ceiling"
        );
    }
    let cross = tl.map_logcat_wall("b", header, read, 3);
    assert!(matches!(cross, TimeAlignment::Unanchored { .. }));
}

// K3 slice level: the real `map_*_in_epoch` bodies that `map_device_monotonic` /
// `map_logcat_wall` call once `epochs.get(boot_id)` has selected a boot epoch, run on fixed
// stack arrays instead of a `ClockTimeline` built through `ingest_device_triple`.
fn any_triple() -> DeviceClockTriple {
    DeviceClockTriple {
        boot_id: "a".to_owned(),
        boottime_ns: kani::any(),
        monotonic_ns: kani::any(),
        realtime_ns: kani::any(),
    }
}

fn any_config() -> ClockTimelineConfig {
    ClockTimelineConfig {
        sleep_threshold_ns: kani::any(),
        wall_jump_threshold_ns: kani::any(),
        pc_sleep_threshold_ns: kani::any(),
        max_half_width_ns: kani::any(),
        max_anchor_age_ns: kani::any(),
    }
}

/// Every placement carries the queried boot epoch's id and an Unanchored result keeps the
/// caller's PC ingest time.
fn assert_tagged(al: &TimeAlignment, boot_id: &str, pc_ingest_cont_ns: i64) {
    match al {
        TimeAlignment::Anchored { boot_id: b, .. } | TimeAlignment::Bounded { boot_id: b, .. } => {
            assert_eq!(b, boot_id, "placed only on the queried boot");
        }
        TimeAlignment::Unanchored {
            pc_ingest_cont_ns: pc,
            ..
        } => assert_eq!(*pc, pc_ingest_cont_ns),
    }
}

fn k3_slice_monotonic_no_panic(triples: &[DeviceClockTriple], mono: i64) -> TimeAlignment {
    let cfg = any_config();
    let al = ClockTimeline::map_monotonic_in_epoch(
        &cfg,
        triples,
        "a",
        mono,
        TimestampSemantics::InputEventTime,
        7,
    );
    assert_well_formed(&al);
    assert_tagged(&al, "a", 7);
    if let TimeAlignment::Anchored { semantics, .. } = &al {
        assert_eq!(*semantics, TimestampSemantics::InputEventTime);
    }
    al
}

#[kani::proof]
#[kani::unwind(4)]
fn k3_slice_map_monotonic_one_triple_no_panic_and_boot_tagged() {
    let t = any_triple();
    let mono: i64 = kani::any();
    let al = k3_slice_monotonic_no_panic(core::slice::from_ref(&t), mono);
    match al {
        TimeAlignment::Anchored {
            boottime_ns,
            half_width_ns,
            ..
        } => {
            assert_eq!(
                mono, t.monotonic_ns,
                "one triple anchors only its own sample"
            );
            assert_eq!(boottime_ns, t.boottime_ns);
            assert_eq!(half_width_ns, 0);
        }
        _ => assert_ne!(mono, t.monotonic_ns, "its own sample is always anchored"),
    }
}

#[kani::proof]
#[kani::unwind(4)]
fn k3_slice_map_monotonic_two_triples_no_panic_and_boot_tagged() {
    k3_slice_monotonic_no_panic(&[any_triple(), any_triple()], kani::any());
}

/// Domain: the ingest invariant. `ingest_device_triple` drops a triple whose `monotonic_ns` or
/// `boottime_ns` is below the previous one, so an epoch's triples are non-decreasing in both.
#[kani::proof]
#[kani::unwind(4)]
fn k3_slice_map_monotonic_monotone_on_two_ingest_ordered_triples() {
    let triples = [any_triple(), any_triple()];
    kani::assume(triples[0].monotonic_ns <= triples[1].monotonic_ns);
    kani::assume(triples[0].boottime_ns <= triples[1].boottime_ns);
    let cfg = any_config();
    let (m1, m2): (i64, i64) = (kani::any(), kani::any());
    kani::assume(m1 <= m2);
    let map = |m| {
        ClockTimeline::map_monotonic_in_epoch(
            &cfg,
            &triples,
            "a",
            m,
            TimestampSemantics::InputEventTime,
            0,
        )
    };
    if let (
        TimeAlignment::Anchored {
            boottime_ns: b1, ..
        },
        TimeAlignment::Anchored {
            boottime_ns: b2, ..
        },
    ) = (map(m1), map(m2))
    {
        assert!(b1 <= b2, "anchored monotonic mapping is order preserving");
    }
}

/// `wall_jump_indices` ranges over every strictly increasing index set with `idx + 1 < n`
/// (what `ingest_device_triple` can record), independent of the triples: a superset of the
/// reachable epochs.
fn k3_slice_logcat_ceiling(
    triples: &[DeviceClockTriple],
    wall_jump_indices: &[usize],
    read: Option<i64>,
    cfg: &ClockTimelineConfig,
) -> TimeAlignment {
    let header: i64 = kani::any();
    let al = ClockTimeline::map_logcat_wall_in_epoch(
        cfg,
        triples,
        wall_jump_indices,
        "a",
        header,
        read,
        3,
    );
    assert_well_formed(&al);
    assert_tagged(&al, "a", 3);
    if let Some(read_bt) = read {
        match &al {
            TimeAlignment::Anchored {
                boottime_ns,
                semantics,
                ..
            } => {
                assert_eq!(*semantics, TimestampSemantics::LogcatHeaderWall);
                assert!(
                    i128::from(*boottime_ns)
                        <= i128::from(read_bt) + i128::from(cfg.wall_jump_threshold_ns),
                    "never after read ceiling"
                );
            }
            TimeAlignment::Bounded {
                max_boottime_ns, ..
            } => assert_eq!(*max_boottime_ns, read_bt, "bounded by the read ceiling"),
            TimeAlignment::Unanchored { .. } => {}
        }
    }
    al
}

fn any_read() -> Option<i64> {
    if kani::any() { Some(kani::any()) } else { None }
}

#[kani::proof]
#[kani::unwind(4)]
fn k3_slice_map_logcat_wall_one_triple_no_panic_and_causal_ceiling() {
    k3_slice_logcat_ceiling(&[any_triple()], &[], any_read(), &any_config());
}

/// No wall jump recorded: `map_logcat_stable_epoch` over a two-triple wall-stable epoch.
#[kani::proof]
#[kani::unwind(4)]
fn k3_slice_map_logcat_wall_two_triples_stable_epoch_causal_ceiling() {
    k3_slice_logcat_ceiling(
        &[any_triple(), any_triple()],
        &[],
        any_read(),
        &any_config(),
    );
}

#[kani::proof]
#[kani::unwind(5)]
fn k3_slice_map_logcat_wall_three_triples_stable_epoch_causal_ceiling() {
    k3_slice_logcat_ceiling(
        &[any_triple(), any_triple(), any_triple()],
        &[],
        any_read(),
        &any_config(),
    );
}

/// Read time relative to the recorded jump `[triples[0].boottime_ns, triples[1].boottime_ns]`.
/// The three regions plus the no-read harness partition every input, and `forward_jump` (wall
/// offset `realtime - boottime` non-decreasing across the jump, or decreasing: a rollback) splits
/// a region in two: split only for solve time.
#[derive(Clone, Copy)]
enum ReadRegion {
    BeforeJump,
    InsideJump,
    AfterJump,
}

/// Jump recorded between the two triples: the multi-segment candidate path. Domain: the ingest
/// invariant (`boottime_ns` non-decreasing within an epoch). Returns the mapping, the config,
/// the read time and the jump start.
fn k3_slice_logcat_jump_between(
    region: Option<ReadRegion>,
    forward_jump: Option<bool>,
) -> (TimeAlignment, ClockTimelineConfig, Option<i64>, i64) {
    let triples = [any_triple(), any_triple()];
    let [t0, t1] = &triples;
    let (jump_start, jump_end) = (t0.boottime_ns, t1.boottime_ns);
    kani::assume(jump_start <= jump_end);
    if let Some(forward) = forward_jump {
        kani::assume((t0.wall_offset_ns() <= t1.wall_offset_ns()) == forward);
    }
    let read = region.map(|r| {
        let read_bt: i64 = kani::any();
        match r {
            ReadRegion::BeforeJump => kani::assume(read_bt < jump_start),
            ReadRegion::InsideJump => kani::assume(jump_start <= read_bt && read_bt <= jump_end),
            ReadRegion::AfterJump => kani::assume(jump_end < read_bt),
        }
        read_bt
    });
    let cfg = any_config();
    let al =
        ClockTimeline::map_logcat_wall_in_epoch(&cfg, &triples, &[0], "a", kani::any(), read, 3);
    assert_well_formed(&al);
    (al, cfg, read, jump_start)
}

/// A read time outside the jump always places the line on the queried boot, never after the read
/// ceiling and never at or before a jump the read has already passed.
fn k3_slice_logcat_read_outside_jump(region: ReadRegion, forward_jump: bool) {
    let (al, cfg, read, jump_start) =
        k3_slice_logcat_jump_between(Some(region), Some(forward_jump));
    let read_bt = read.unwrap_or_default();
    let (boot, causal) = match &al {
        TimeAlignment::Anchored {
            boot_id,
            boottime_ns,
            semantics,
            ..
        } => (
            Some(boot_id.as_str()),
            *semantics == TimestampSemantics::LogcatHeaderWall
                && i128::from(*boottime_ns)
                    <= i128::from(read_bt) + i128::from(cfg.wall_jump_threshold_ns)
                && !(read_bt >= jump_start && *boottime_ns <= jump_start),
        ),
        TimeAlignment::Bounded {
            boot_id,
            max_boottime_ns,
            ..
        } => (Some(boot_id.as_str()), *max_boottime_ns == read_bt),
        TimeAlignment::Unanchored { .. } => (None, false),
    };
    assert_eq!(
        boot,
        Some("a"),
        "a read time places the line on the queried boot"
    );
    assert!(
        causal,
        "never after the read ceiling, never at or before a jump the read has passed"
    );
}

/// A read time inside the jump has no trustworthy lower bound: `(i64::MIN, read]`.
fn k3_slice_logcat_read_inside_jump(forward_jump: bool) {
    let (al, _, read, _) =
        k3_slice_logcat_jump_between(Some(ReadRegion::InsideJump), Some(forward_jump));
    assert!(
        matches!(
            &al,
            TimeAlignment::Bounded {
                boot_id,
                min_boottime_ns: i64::MIN,
                max_boottime_ns,
                ..
            } if boot_id == "a" && Some(*max_boottime_ns) == read
        ),
        "read inside a jump: bounded by (i64::MIN, read] on the queried boot"
    );
}

#[kani::proof]
#[kani::unwind(3)]
fn k3_slice_map_logcat_wall_two_triples_jump_between_read_before_jump_wall_forward() {
    k3_slice_logcat_read_outside_jump(ReadRegion::BeforeJump, true);
}

#[kani::proof]
#[kani::unwind(3)]
fn k3_slice_map_logcat_wall_two_triples_jump_between_read_before_jump_wall_rollback() {
    k3_slice_logcat_read_outside_jump(ReadRegion::BeforeJump, false);
}

#[kani::proof]
#[kani::unwind(4)]
fn k3_slice_map_logcat_wall_two_triples_jump_between_read_inside_jump_wall_forward() {
    k3_slice_logcat_read_inside_jump(true);
}

#[kani::proof]
#[kani::unwind(4)]
fn k3_slice_map_logcat_wall_two_triples_jump_between_read_inside_jump_wall_rollback() {
    k3_slice_logcat_read_inside_jump(false);
}

/// Covers the `cand_bt <= jump_start` causal-ceiling rejection.
#[kani::proof]
#[kani::unwind(3)]
fn k3_slice_map_logcat_wall_two_triples_jump_between_read_after_jump_wall_forward() {
    k3_slice_logcat_read_outside_jump(ReadRegion::AfterJump, true);
}

/// Covers the `cand_bt <= jump_start` causal-ceiling rejection.
#[kani::proof]
#[kani::unwind(3)]
fn k3_slice_map_logcat_wall_two_triples_jump_between_read_after_jump_wall_rollback() {
    k3_slice_logcat_read_outside_jump(ReadRegion::AfterJump, false);
}

#[kani::proof]
#[kani::unwind(3)]
fn k3_slice_map_logcat_wall_two_triples_jump_between_without_read() {
    let (al, ..) = k3_slice_logcat_jump_between(None, None);
    assert_tagged(&al, "a", 3);
}

/// Three triples, every reachable wall-jump index set (`idx + 1 < 3`).
#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(5))]
fn k3_slice_map_logcat_wall_three_triples_with_jumps_causal_ceiling() {
    let jumps: &[usize] = match kani::any::<u8>() % 3 {
        0 => &[0],
        1 => &[1],
        _ => &[0, 1],
    };
    k3_slice_logcat_ceiling(
        &[any_triple(), any_triple(), any_triple()],
        jumps,
        any_read(),
        &any_config(),
    );
}

// ---------------------------------------------------------------- K4
const IDS: [&str; 4] = ["c0", "c1", "c2", "c3"];

fn any_chunks(n: usize, small: bool) -> Vec<RingChunkMeta> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < n {
        let bytes: u64 = if small {
            u64::from(kani::any::<u8>())
        } else {
            kani::any()
        };
        v.push(RingChunkMeta {
            chunk_id: IDS[i].to_owned(),
            created_cont_ns: i64::from(kani::any::<i8>()),
            duration_ns: if small {
                u64::from(kani::any::<u8>())
            } else {
                kani::any()
            },
            bytes,
            status: if kani::any() {
                RingChunkStatus::Open
            } else {
                RingChunkStatus::Closed
            },
            pin: if kani::any() {
                PinStatus::Pinned
            } else {
                PinStatus::Unpinned
            },
        });
        i += 1;
    }
    v
}

fn check_plan(small: bool) {
    let n = usize::from(kani::any::<u8>() % 5);
    let chunks = any_chunks(n, small);
    let policy = RingBudgetPolicy {
        max_ring_bytes: if small {
            u64::from(kani::any::<u16>())
        } else {
            kani::any()
        },
        max_ring_duration_ns: if small {
            u64::from(kani::any::<u16>())
        } else {
            kani::any()
        },
        min_free_disk_bytes: kani::any(),
    };
    let (incoming, free): (u64, u64) = (kani::any(), kani::any());
    let decision = RingEvictionPlanner::new(policy).plan(&chunks, incoming, free);
    let mut eligible: Vec<&RingChunkMeta> = chunks.iter().filter(|c| c.is_evictable()).collect();
    eligible.sort_by(|a, b| {
        a.created_cont_ns
            .cmp(&b.created_cont_ns)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    let total: u128 = chunks.iter().map(|c| u128::from(c.bytes)).sum();
    match decision {
        RingWriteDecision::Allow { evict_chunk_ids } => {
            assert!(evict_chunk_ids.len() <= eligible.len());
            let mut freed: u128 = 0;
            let mut i = 0;
            while i < evict_chunk_ids.len() {
                assert_eq!(
                    evict_chunk_ids[i], eligible[i].chunk_id,
                    "oldest-first prefix"
                );
                freed += u128::from(eligible[i].bytes);
                i += 1;
            }
            for c in &chunks {
                if c.pin == PinStatus::Pinned || c.status == RingChunkStatus::Open {
                    assert!(
                        !evict_chunk_ids.contains(&c.chunk_id),
                        "REQ-04 pin protection"
                    );
                }
            }
            assert!(total - freed + u128::from(incoming) <= u128::from(policy.max_ring_bytes));
            assert!(free.saturating_sub(incoming) >= policy.min_free_disk_bytes);
        }
        RingWriteDecision::StopStoragePressure { .. } => {
            assert!(
                free < policy.min_free_disk_bytes
                    || free.saturating_sub(incoming) < policy.min_free_disk_bytes
            );
        }
        RingWriteDecision::ChunkTooLarge => {
            assert!(incoming > policy.max_ring_bytes);
        }
        RingWriteDecision::StopRingFullPinned { .. } => {
            let protected: u128 = chunks
                .iter()
                .filter(|c| !c.is_evictable())
                .map(|c| u128::from(c.bytes))
                .sum();
            assert!(protected + u128::from(incoming) > u128::from(policy.max_ring_bytes));
            assert!(
                protected > 0,
                "only pinned/open data can block a fitting write"
            );
        }
    }
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(6))]
fn k4_ring_eviction_plan_realistic_sizes() {
    check_plan(true);
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(6))]
fn k4_ring_eviction_plan_extreme_u64() {
    check_plan(false);
}

// ---------------------------------------------------------------- K5
const STATES: [SaveJobState; 6] = [
    SaveJobState::Pinned,
    SaveJobState::Draining,
    SaveJobState::Materializing,
    SaveJobState::FailedRetryable,
    SaveJobState::Completed,
    SaveJobState::Discarded,
];

fn event(k: u8, path: &str) -> SaveJobEvent {
    match k % 7 {
        0 => SaveJobEvent::BeginDrain,
        1 => SaveJobEvent::BeginMaterialize,
        2 => SaveJobEvent::Complete,
        3 => SaveJobEvent::FailRetryable {
            error_message: "e".to_owned(),
        },
        4 => SaveJobEvent::Retry,
        5 => SaveJobEvent::Retarget {
            new_destination_path: path.to_owned(),
        },
        _ => SaveJobEvent::Discard,
    }
}

fn job_in(state: SaveJobState) -> SaveJob {
    let mut job = SaveJob::new("j", "/d", vec!["c0".to_owned()]);
    job.state = state;
    job
}

#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k5_save_job_apply_matches_transition_table() {
    let from = STATES[usize::from(kani::any::<u8>() % 6)];
    let k: u8 = kani::any();
    let path = match kani::any::<u8>() % 3 {
        0 => "",
        1 => "  ",
        _ => "/new",
    };
    let before = job_in(from);
    let mut job = before.clone();
    match job.apply(event(k, path)) {
        Ok(to) => {
            assert!(from.can_transition_to(to), "apply ⊆ can_transition_to");
            assert_eq!(job.state, to);
            assert!(!from.is_terminal(), "terminal states reject every event");
            if !matches!(to, SaveJobState::Completed | SaveJobState::Discarded) {
                assert_eq!(
                    job.pinned_chunk_ids, before.pinned_chunk_ids,
                    "pins conserved"
                );
            } else {
                assert!(job.pinned_chunk_ids.is_empty(), "terminal releases pins");
            }
            if k % 7 == 5 {
                assert!(!path.trim().is_empty(), "empty retarget rejected");
            }
        }
        Err(_) => assert_eq!(job, before, "rejected transition leaves job unchanged"),
    }
}

#[kani::proof]
#[kani::unwind(8)]
fn k5_every_table_edge_is_reachable_by_apply() {
    let from = STATES[usize::from(kani::any::<u8>() % 6)];
    let to = STATES[usize::from(kani::any::<u8>() % 6)];
    kani::assume(from.can_transition_to(to));
    let mut reached = false;
    let mut k = 0u8;
    while k < 7 {
        let mut job = job_in(from);
        if job.apply(event(k, "/new")) == Ok(to) {
            reached = true;
        }
        k += 1;
    }
    assert!(reached, "can_transition_to ⊆ apply");
}

#[kani::proof]
fn k5_bundle_lifecycle() {
    let s = match kani::any::<u8>() % 3 {
        0 => BundleLifecycleState::Active,
        1 => BundleLifecycleState::Trashed,
        _ => BundleLifecycleState::Purging,
    };
    let (leases, jobs): (usize, usize) = (kani::any(), kani::any());
    assert_eq!(
        s.begin_purge(leases, jobs).is_ok(),
        s == BundleLifecycleState::Trashed && leases == 0 && jobs == 0
    );
    if let Ok(p) = s.begin_purge(leases, jobs) {
        assert_eq!(p, BundleLifecycleState::Purging);
    }
    assert_eq!(s.restore().is_ok(), s == BundleLifecycleState::Trashed);
    assert_eq!(s.trash().is_ok(), s == BundleLifecycleState::Active);
}

// ---------------------------------------------------------------- K6 (arb-core half)
#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k6_map_touch_to_frame_bounds_and_monotonic() {
    let dw: u32 = kani::any();
    let dh: u32 = kani::any();
    let fw: u32 = kani::any();
    let fh: u32 = kani::any();
    kani::assume(dw > 0 && dh > 0 && fw > 0 && fh > 0);
    kani::assume(dw <= 1 << 14 && dh <= 1 << 14 && fw <= 1 << 14 && fh <= 1 << 14);
    let x1: f64 = kani::any();
    let x2: f64 = kani::any();
    let y: f64 = kani::any();
    let r1 = map_touch_to_frame(x1, y, Some(dw), Some(dh), fw, fh);
    let r2 = map_touch_to_frame(x2, y, Some(dw), Some(dh), fw, fh);
    if let Some((px, py)) = r1 {
        assert!(px >= 0.0 && px <= f64::from(fw - 1) && py >= 0.0 && py <= f64::from(fh - 1));
    } else {
        assert!(!(x1 >= 0.0 && x1 <= f64::from(dw) && y >= 0.0 && y <= f64::from(dh)));
    }
    if let (Some((p1, _)), Some((p2, _))) = (r1, r2) {
        if x1 <= x2 {
            assert!(p1 <= p2, "monotonic non-decreasing");
        }
    }
}

#[kani::proof]
fn k6_physical_one_pixel_never_jumps_to_bottom_right() {
    let dw: u32 = kani::any();
    let dh: u32 = kani::any();
    let fw: u32 = kani::any();
    let fh: u32 = kani::any();
    kani::assume(dw >= 4 && dh >= 4 && fw >= 2 && fh >= 2);
    kani::assume(dw <= 1 << 14 && dh <= 1 << 14 && fw <= dw && fh <= dh);
    let r = map_touch_to_frame(1.0, 1.0, Some(dw), Some(dh), fw, fh);
    let Some((px, py)) = r else {
        panic!("(1,1) is inside a known display")
    };
    assert!(px < f64::from(fw - 1) || fw <= 2);
    assert!(py <= f64::from(fh) / 2.0, "physical (1,1) stays top-left");
    assert!(px <= f64::from(fw) / 2.0);
}

/// One axis of `map_touch_to_frame` (the other axis pinned to a `1 -> 1` mapping of `0.0`):
/// one division and one multiplication per call instead of four.
fn k6_axis(axis_x: bool, coord: f64, display: u32, frame: u32) -> Option<f64> {
    if axis_x {
        map_touch_to_frame(coord, 0.0, Some(display), Some(1), frame, 1).map(|(px, _)| px)
    } else {
        map_touch_to_frame(0.0, coord, Some(1), Some(display), 1, frame).map(|(_, py)| py)
    }
}

/// Monotonic non-decreasing for two in-range coordinates on one axis. Both must map (in-range
/// input on a known display), so the order check is never skipped.
fn k6_axis_monotone(axis_x: bool, c1: f64, c2: f64, display: u32, frame: u32) {
    let (Some(p1), Some(p2)) = (
        k6_axis(axis_x, c1, display, frame),
        k6_axis(axis_x, c2, display, frame),
    ) else {
        panic!("in-range coordinate on a known display maps")
    };
    assert!(p1 <= p2, "monotonic non-decreasing");
}

/// Domain: integer pixel coordinates `0..=display` (as `f64`), concrete dimensions.
fn k6_axis_monotone_on_integer_pixels(axis_x: bool, display: u32, frame: u32) {
    let (u1, u2): (u32, u32) = (kani::any(), kani::any());
    kani::assume(u1 <= u2 && u2 <= display);
    k6_axis_monotone(axis_x, f64::from(u1), f64::from(u2), display, frame);
}

/// Domain: every `f64` in `0.0..=display` (`NaN` / out of range map to `None`, covered by
/// `k6_map_touch_to_frame_bounds_and_monotonic`'s bounds half), concrete dimensions.
fn k6_axis_monotone_on_any_f64(axis_x: bool, display: u32, frame: u32) {
    let (c1, c2): (f64, f64) = (kani::any(), kani::any());
    let d = f64::from(display);
    kani::assume(0.0 <= c1 && c1 <= c2 && c2 <= d);
    k6_axis_monotone(axis_x, c1, c2, display, frame);
}

#[kani::proof]
fn k6_monotonic_x_integer_pixels_1080_to_460() {
    k6_axis_monotone_on_integer_pixels(true, 1080, 460);
}

#[kani::proof]
fn k6_monotonic_y_integer_pixels_2400_to_1024() {
    k6_axis_monotone_on_integer_pixels(false, 2400, 1024);
}

#[kani::proof]
fn k6_monotonic_x_integer_pixels_1080_to_1080() {
    k6_axis_monotone_on_integer_pixels(true, 1080, 1080);
}

#[kani::proof]
fn k6_monotonic_y_integer_pixels_1920_to_1920() {
    k6_axis_monotone_on_integer_pixels(false, 1920, 1920);
}

#[kani::proof]
fn k6_monotonic_x_integer_pixels_3840_to_1920() {
    k6_axis_monotone_on_integer_pixels(true, 3840, 1920);
}

#[kani::proof]
fn k6_monotonic_y_integer_pixels_2160_to_1080() {
    k6_axis_monotone_on_integer_pixels(false, 2160, 1080);
}

/// Domain: integer pixels, symbolic dimensions up to 4096. CBMC ceiling: symbolic divisor
/// exceeded five minutes.
#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k6_monotonic_x_integer_pixels_symbolic_dims_le_4096() {
    let (display, frame): (u32, u32) = (kani::any(), kani::any());
    kani::assume(0 < display && display <= 4096 && 0 < frame && frame <= 4096);
    let (u1, u2): (u32, u32) = (kani::any(), kani::any());
    kani::assume(u1 <= u2 && u2 <= display);
    k6_axis_monotone(true, f64::from(u1), f64::from(u2), display, frame);
}

#[kani::proof]
fn k6_monotonic_x_any_f64_1_to_1() {
    k6_axis_monotone_on_any_f64(true, 1, 1);
}

#[kani::proof]
fn k6_monotonic_y_any_f64_1_to_1() {
    k6_axis_monotone_on_any_f64(false, 1, 1);
}

/// Power-of-two dimensions: `/ 1024` and `* 512` only shift the exponent.
#[kani::proof]
fn k6_monotonic_x_any_f64_pow2_1024_to_512() {
    k6_axis_monotone_on_any_f64(true, 1024, 512);
}

/// CBMC ceiling: symbolic `f64` division by a non-power-of-two constant (`/ 1080`) alone
/// exceeded five minutes.
#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k6_monotonic_x_any_f64_1080_to_460() {
    k6_axis_monotone_on_any_f64(true, 1080, 460);
}

#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k6_monotonic_y_any_f64_2400_to_1024() {
    k6_axis_monotone_on_any_f64(false, 2400, 1024);
}

// ---------------------------------------------------------------- K7
fn any_finite_point() -> TouchPoint {
    let x: f64 = kani::any();
    let y: f64 = kani::any();
    kani::assume(x.is_finite() && y.is_finite());
    TouchPoint {
        x,
        y,
        t_offset_ns: kani::any(),
    }
}

/// `f64::hypot` is a foreign C call Kani cannot model. The stub returns an arbitrary value,
/// a sound over-approximation: the property must hold whatever distance `hypot` reports.
fn hypot_any(_dx: f64, _dy: f64) -> f64 {
    kani::any()
}

/// Finite samples only: a NaN/infinite coordinate makes the distance subtraction produce NaN
/// by design (the `is_finite` guard then classifies the stroke as a tap), which Kani's
/// default NaN check reports. That case is pinned by the unit test
/// `gesture::tests::non_finite_samples_classify_as_taps_without_panicking`.
#[kani::proof]
#[kani::stub(crate::gesture::hypot, hypot_any)]
fn k7_empty_and_single_sample_strokes_are_taps() {
    let slop: f64 = kani::any();
    let empty = classify_stroke(&[], slop);
    assert_eq!(empty.kind, crate::gesture::GestureKind::Tap);
    assert!(empty.path.is_empty());
    let p = any_finite_point();
    let one = classify_stroke(&[p], slop);
    assert_eq!(one.kind, crate::gesture::GestureKind::Tap);
    assert!(one.path.is_empty());
    assert_eq!(one.end_t_offset_ns, p.t_offset_ns);
}

#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(6))]
fn k7_decimate_keeps_endpoints_and_cap() {
    let len = usize::from(kani::any::<u8>() % 5) + 1;
    let cap = usize::from(kani::any::<u8>() % 3) + 2;
    let mut samples = Vec::new();
    let mut i = 0;
    while i < len {
        samples.push(TouchPoint {
            x: 0.0,
            y: 0.0,
            t_offset_ns: i64::try_from(i).unwrap_or(0),
        });
        i += 1;
    }
    let out = decimate(&samples, cap);
    assert!(out.len() <= cap.max(len));
    if len > cap {
        assert_eq!(out.len(), cap);
    }
    assert_eq!(out[0].t_offset_ns, 0, "first sample kept");
    assert_eq!(
        out[out.len() - 1].t_offset_ns,
        samples[len - 1].t_offset_ns,
        "last sample kept"
    );
}

#[kani::proof]
fn k7_touch_slop_finite_positive() {
    let w: f64 = kani::any();
    let h: f64 = kani::any();
    let s = touch_slop(Some(w), Some(h));
    assert!(s.is_finite() && s > 0.0);
    let s2 = touch_slop(None, Some(h));
    assert!(s2.is_finite() && s2 > 0.0);
}
