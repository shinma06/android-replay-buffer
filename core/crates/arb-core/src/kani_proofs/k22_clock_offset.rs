//! K22 (#591): `device_clock_offset` arithmetic in `crate::time`.
//!
//! An estimate is two stages: an `i128` stage (`sync_probe_parts`, `trigger_parts`) that
//! combines the clocks, and `finish_estimate`, which rounds to milliseconds and widens the
//! uncertainty. `sync_probe_offset` and `estimate_device_clock_offset_at_trigger` are exactly
//! `finish_estimate ∘ stage`. Every input is an arbitrary `i64`; Kani's overflow checks prove
//! that no intermediate overflows, and the oracles here restate each stage independently.

use crate::time::{
    CLOCK_OFFSET_SAMPLE_SKEW_NS, ClockOffsetSource, HostClockSample,
    MAX_CLOCK_OFFSET_EXTRAPOLATION_NS, OffsetParts, SyncProbeClocks,
    TRIGGER_ESTIMATE_BASE_UNCERTAINTY_NS, finish_estimate, sync_probe_parts, trigger_parts,
};

#[kani::proof]
pub fn k22_device_clock_offset_no_overflow_and_nonneg_uncertainty() {
    let probe = SyncProbeClocks {
        pc_send_cont_ns: kani::any(),
        pc_recv_cont_ns: kani::any(),
        dev_boottime_ns: kani::any(),
    };
    let triple_boottime_ns: i64 = kani::any();
    let triple_realtime_ns: i64 = kani::any();
    let host = HostClockSample {
        continuous_mono_ns: kani::any(),
        wall_epoch_ns: kani::any(),
    };
    let parts = sync_probe_parts(probe, triple_boottime_ns, triple_realtime_ns, host);

    let send = i128::from(probe.pc_send_cont_ns);
    let rtt = i128::from(probe.pc_recv_cont_ns) - send;
    assert_eq!(
        parts.is_none(),
        rtt < 0,
        "only a negative round trip is rejected"
    );
    if let Some(p) = parts {
        // midpoint = send + floor(rtt / 2); both host-side terms are relative to it.
        let mid2 = 2 * send + rtt - (rtt & 1);
        let host_wall2 = mid2
            + 2 * (i128::from(host.wall_epoch_ns) - i128::from(host.continuous_mono_ns)
                + i128::from(triple_boottime_ns)
                - i128::from(probe.dev_boottime_ns));
        assert!(2 * p.host_wall_ns == host_wall2);
        assert!(p.offset_ns == i128::from(triple_realtime_ns) - p.host_wall_ns);
        assert!(p.span_ns >= 0, "spans are absolute values");
        // The rounded-up half round trip: 2 × half ∈ {rtt, rtt + 1}.
        let half = i128::from(p.base_uncertainty_ns) - 2 * i128::from(CLOCK_OFFSET_SAMPLE_SKEW_NS);
        assert!(
            2 * half == rtt || 2 * half == rtt + 1,
            "base = ⌈rtt / 2⌉ + both skews"
        );
    }
    kani::cover!(parts.is_some_and(|p| p.offset_ns > 0), "device clock ahead");
    kani::cover!(
        parts.is_some_and(|p| p.offset_ns < 0),
        "device clock behind"
    );
}

#[kani::proof]
pub fn k22_trigger_parts_no_overflow() {
    let triple_boottime_ns: i64 = kani::any();
    let triple_realtime_ns: i64 = kani::any();
    let trigger_boottime_ns: i64 = kani::any();
    let trigger_wall_unix_ms: i64 = kani::any();
    let precision_ms: u64 = if kani::any() { 1 } else { 1_000 };
    let p = trigger_parts(
        triple_boottime_ns,
        triple_realtime_ns,
        trigger_boottime_ns,
        trigger_wall_unix_ms,
        precision_ms,
    )
    .expect("one or 1,000 ms of precision always fits");
    assert!(p.host_wall_ns == i128::from(trigger_wall_unix_ms) * 1_000_000);
    assert!(
        p.offset_ns
            == i128::from(trigger_boottime_ns) + i128::from(triple_realtime_ns)
                - i128::from(triple_boottime_ns)
                - p.host_wall_ns
    );
    assert!(p.span_ns >= 0);
    assert!(
        p.base_uncertainty_ns
            == TRIGGER_ESTIMATE_BASE_UNCERTAINTY_NS
                + precision_ms * 1_000_000
                + CLOCK_OFFSET_SAMPLE_SKEW_NS
    );
    kani::cover!(p.offset_ns < 0, "device clock behind at the trigger");
}

/// The actual production rounding and uncertainty calculation, including all out-of-range
/// inputs from either first stage. No arithmetic operation is stubbed.
#[kani::proof]
pub fn k22_finish_estimate_preserves_sign_and_nonnegative_uncertainty() {
    let p = OffsetParts {
        offset_ns: kani::any(),
        span_ns: kani::any(),
        host_wall_ns: kani::any(),
        base_uncertainty_ns: kani::any(),
    };
    let source = if kani::any() {
        ClockOffsetSource::SyncProbe
    } else {
        ClockOffsetSource::TriggerEstimate
    };
    let result = finish_estimate(p, source);
    if let Some(e) = result {
        assert_eq!(e.source, source);
        assert!(p.span_ns >= 0 && p.span_ns <= i128::from(MAX_CLOCK_OFFSET_EXTRAPOLATION_NS));
        assert!(e.offset_ms <= 0 || p.offset_ns > 0);
        assert!(e.offset_ms >= 0 || p.offset_ns < 0);
        assert!(i128::from(e.uncertainty_ms) >= 0);
        assert!(e.uncertainty_ms > 0 || p.base_uncertainty_ns == 0);
    }
    kani::cover!(
        result.is_some_and(|e| e.offset_ms < 0 && e.uncertainty_ms > 0),
        "negative with uncertainty"
    );
    kani::cover!(result.is_some_and(|e| e.offset_ms > 0), "positive offset");
    kani::cover!(result.is_some_and(|e| e.offset_ms == 0), "zero offset");
    kani::cover!(result.is_none(), "out of range");
}
