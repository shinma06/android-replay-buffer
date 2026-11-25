//! Layer-1 PDD (K9a): bounded model checking of the MP4 box/table parsers.
//!
//! Inputs are symbolic byte arrays of bounded length; unwinding assertions stay enabled.
//!
//! Parser error paths build messages with `format!`; rendering a symbolic integer as decimal
//! text is a loop of 64-bit divisions that CBMC could not finish, so the parser harnesses stub
//! `std::fmt::format` (run with `-Z stubbing`). Only the message text is replaced: every
//! `Err`/`Ok` decision and every bound check still runs on production code.
//!
//! Harnesses gated by `cfg_attr(feature = "kani-extended", kani::proof, ...)` did not verify
//! within five minutes locally (Kani 0.68 / CBMC 6.11). They run only with
//! `cargo kani -p <crate> --features kani-extended` and in the non-blocking `kani-extended`
//! job of `.github/workflows/pdd-kani.yml`; `docs/proofs/tcb-axioms.md` §3 lists them.
#![cfg_attr(not(feature = "kani-extended"), allow(dead_code))]

use crate::mp4::{
    AvccConfig, ParamSetMode, avcc_sample_to_annexb, checked_entry_count, iter_boxes, parse_stsc,
    parse_stsz, parse_stts, ticks_to_ns_signed, ticks_to_ns_unsigned,
};

const N: usize = 24;

/// Replaces `format!` rendering in parser error paths (see the module docs).
fn format_stub(_args: std::fmt::Arguments<'_>) -> String {
    String::new()
}

fn any_slice(buf: &[u8; N]) -> &[u8] {
    let len: usize = kani::any();
    kani::assume(len <= N);
    &buf[..len]
}

#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
fn k9_checked_entry_count_bounds_allocation() {
    let buf: [u8; N] = kani::any();
    let body = any_slice(&buf);
    let header_len = usize::from(kani::any::<u8>() % 17);
    let entry_size = usize::from(kani::any::<u8>() % 12) + 1;
    let count: u32 = kani::any();
    if let Ok(c) = checked_entry_count(body, count, header_len, entry_size, "k") {
        assert_eq!(c as u64, u64::from(count));
        assert!(
            c == 0 || header_len + c * entry_size <= body.len(),
            "entries fit in box"
        );
    }
}

#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(5)]
fn k9_iter_boxes_children_stay_in_parent() {
    let buf: [u8; N] = kani::any();
    let data = any_slice(&buf);
    if let Ok(boxes) = iter_boxes(data) {
        let mut total = 0usize;
        for (_, payload) in &boxes {
            total += payload.len() + 8;
        }
        assert!(
            total <= data.len(),
            "child payloads + headers never exceed parent"
        );
    }
}

/// Shapes: truncated header, header only, and header + one entry (the symbolic-length
/// variant over a 24-byte buffer did not finish within 15 minutes).
#[cfg_attr(
    feature = "kani-extended",
    kani::proof,
    kani::stub(std::fmt::format, format_stub),
    kani::unwind(6)
)]
fn k9_parse_stts_bounded() {
    let buf: [u8; 16] = kani::any();
    let data = &buf[..[4, 8, 16][usize::from(kani::any::<u8>() % 3)]];
    let max_samples = usize::from(kani::any::<u8>() % 4);
    if let Ok(d) = parse_stts(data, max_samples) {
        assert!(d.len() <= max_samples, "expansion capped by stsz count");
    }
}

/// Shapes: header only, one entry, and two entries so the ordering check is reachable (a
/// symbolic length over 32 bytes did not finish within 15 minutes; partial entries are
/// rejected by `checked_entry_count`, proven separately).
#[cfg_attr(
    feature = "kani-extended",
    kani::proof,
    kani::stub(std::fmt::format, format_stub),
    kani::unwind(4)
)]
fn k9_parse_stsc_strictly_increasing() {
    let buf: [u8; 32] = kani::any();
    let data = &buf[..[8, 20, 32][usize::from(kani::any::<u8>() % 3)]];
    if let Ok(e) = parse_stsc(data) {
        assert!(!e.is_empty() && e[0].0 == 1);
        let mut i = 1;
        while i < e.len() {
            assert!(e[i - 1].0 < e[i].0 && e[i].1 > 0);
            i += 1;
        }
    }
}

/// Shapes: truncated, header only, and one or two explicit sizes (a symbolic length over
/// 24 bytes did not finish within 15 minutes).
#[cfg_attr(
    feature = "kani-extended",
    kani::proof,
    kani::stub(std::fmt::format, format_stub),
    kani::unwind(10)
)]
fn k9_parse_stsz_bounded_by_file() {
    let buf: [u8; 20] = kani::any();
    let data = &buf[..[8, 12, 16, 20][usize::from(kani::any::<u8>() % 4)]];
    let file_len = u64::from(kani::any::<u8>() % 8);
    if let Ok(sizes) = parse_stsz(data, file_len) {
        let default_size = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        if default_size != 0 {
            assert!(sizes.len() as u64 * u64::from(default_size) <= file_len);
        } else {
            assert!(12 + sizes.len() * 4 <= data.len());
        }
    }
}

/// Exact saturating conversion over the full `i64`/`u64` tick domain for one timescale:
/// the truncated quotient clamped to the target range, with no intermediate overflow.
///
/// The oracle checks the quotient-remainder identity with multiplications only, so the
/// production division is the only one CBMC bit-blasts. Every product is bounded by
/// `2^63 * 10^9` or `2^64 * 2^32`, far below `2^127`, so the wrapping operations are exact
/// and the oracle adds no overflow checks of its own.
///
/// Only `ts = 1` verifies in the required set. For any other constant the production
/// 128-bit divider stays the bottleneck: with `ts = 90_000` the unsigned half alone did not
/// finish within 10 minutes (Kani 0.68 / CBMC 6.11 / CaDiCaL, Linux), also with Kissat,
/// MiniSat or Z3, with ticks limited to 32 bits, and with an oracle that avoids every
/// multiplication of the result; ticks limited to 16 bits took 100 seconds. A fully
/// symbolic `u32` timescale did not finish within 15 minutes.
fn ticks_exact(ts: u32) {
    let d = i128::from(ts);
    let a: i64 = kani::any();
    let n = i128::from(a).wrapping_mul(1_000_000_000);
    let na = ticks_to_ns_signed(a, ts);
    let qd = i128::from(na).wrapping_mul(d);
    if na == i64::MAX {
        assert!(n >= qd, "positive overflow saturates");
    } else if na == i64::MIN {
        assert!(n <= qd, "negative overflow saturates");
    } else if n >= 0 {
        assert!(
            qd <= n && n < qd.wrapping_add(d),
            "in-range value is the truncated quotient"
        );
    } else {
        assert!(
            qd.wrapping_sub(d) < n && n <= qd,
            "in-range value is the truncated quotient"
        );
    }
    let d = u128::from(ts);
    let ua: u64 = kani::any();
    let n = u128::from(ua).wrapping_mul(1_000_000_000);
    let nu = ticks_to_ns_unsigned(ua, ts);
    let qd = u128::from(nu).wrapping_mul(d);
    if nu == u64::MAX {
        assert!(n >= qd, "unsigned overflow saturates");
    } else {
        assert!(
            qd <= n && n < qd.wrapping_add(d),
            "in-range value is the floor quotient"
        );
    }
}

/// Adjacent-tick monotonicity for one timescale (all pairs follow by induction).
///
/// All three timescales stay `kani-extended`: comparing two production results makes CBMC
/// relate two separate 128-bit products (`a * 10^9` and `(a + 1) * 10^9`), which did not
/// finish within 10 minutes even for `ts = 1` with the unsigned half alone, or for
/// unsigned ticks limited to 32 bits.
fn ticks_monotone(ts: u32) {
    let a: i64 = kani::any();
    kani::assume(a < i64::MAX);
    assert!(ticks_to_ns_signed(a, ts) <= ticks_to_ns_signed(a + 1, ts));
    let ua: u64 = kani::any();
    kani::assume(ua < u64::MAX);
    assert!(ticks_to_ns_unsigned(ua, ts) <= ticks_to_ns_unsigned(ua + 1, ts));
}

#[kani::proof]
fn k9_ticks_to_ns_exact_ts_1() {
    ticks_exact(1);
}

#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k9_ticks_to_ns_exact_ts_90000() {
    ticks_exact(90_000);
}

#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k9_ticks_to_ns_exact_ts_u32_max() {
    ticks_exact(u32::MAX);
}

#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k9_ticks_to_ns_monotone_ts_1() {
    ticks_monotone(1);
}

#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k9_ticks_to_ns_monotone_ts_90000() {
    ticks_monotone(90_000);
}

#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k9_ticks_to_ns_monotone_ts_u32_max() {
    ticks_monotone(u32::MAX);
}

/// Samples up to 10 bytes (up to five NAL units); the 16-byte variant did not finish within
/// 15 minutes because every NAL copy has a symbolic length.
#[cfg_attr(feature = "kani-extended", kani::proof, kani::unwind(14))]
fn k9_avcc_sample_to_annexb_bounded() {
    let buf: [u8; 10] = kani::any();
    let len: usize = kani::any();
    kani::assume(len <= 10);
    let sample = &buf[..len];
    let nls = usize::from(kani::any::<u8>() % 6);
    let cfg = AvccConfig {
        raw_avcc: Vec::new(),
        nal_length_size: nls,
        sps: vec![vec![0x67]],
        pps: vec![vec![0x68]],
    };
    let mode = match kani::any::<u8>() % 3 {
        0 => ParamSetMode::Omit,
        1 => ParamSetMode::Prepend,
        _ => ParamSetMode::PrependIfMissing,
    };
    let mut out = Vec::new();
    avcc_sample_to_annexb(sample, &cfg, mode, &mut out);
    if nls == 4 && matches!(mode, ParamSetMode::Omit) {
        assert!(
            out.len() <= sample.len(),
            "4-byte prefixes become 4-byte start codes"
        );
    }
    if nls == 0 || nls > 4 {
        assert!(
            out.len() <= 10,
            "unsupported NAL length size emits no sample NALs"
        );
    }
}
