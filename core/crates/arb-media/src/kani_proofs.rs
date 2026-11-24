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

/// A two-entry box (24 bytes) whose symbolic entry count covers zero, one and two entries
/// (larger counts are rejected by `checked_entry_count`, proven separately), with
/// `max_samples <= 1` so expansion stops inside an entry. The length is concrete: a symbolic
/// length (or `max_samples <= 2`) did not finish within 60 seconds.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(3)]
fn k9_parse_stts_bounded() {
    let buf: [u8; 24] = kani::any();
    let max_samples = usize::from(kani::any::<bool>());
    if let Ok(d) = parse_stts(&buf, max_samples) {
        assert!(d.len() <= max_samples, "expansion capped by stsz count");
    }
}

/// A two-entry box (32 bytes) whose symbolic entry count covers zero, one and two entries, so
/// the empty-table and ordering checks are both reachable (larger counts are rejected by
/// `checked_entry_count`, proven separately).
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(3)]
fn k9_parse_stsc_strictly_increasing() {
    let buf: [u8; 32] = kani::any();
    if let Ok(e) = parse_stsc(&buf) {
        assert!(!e.is_empty() && e[0].0 == 1);
        let mut i = 1;
        while i < e.len() {
            assert!(e[i - 1].0 < e[i].0 && e[i].1 > 0);
            i += 1;
        }
    }
}

/// A truncated header (8 bytes) and a box with room for two explicit sizes (20 bytes) whose
/// symbolic count and default size cover the explicit, constant-size and overflowing
/// (`count * size` saturating) paths; `file_len <= 2` bounds the constant-size expansion.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(3)]
fn k9_parse_stsz_bounded_by_file() {
    let buf: [u8; 20] = kani::any();
    assert!(
        parse_stsz(&buf[..8], 0).is_err(),
        "truncated header is rejected"
    );
    let file_len = u64::from(kani::any::<u8>() % 3);
    if let Ok(sizes) = parse_stsz(&buf, file_len) {
        let default_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if default_size != 0 {
            assert!(sizes.len() as u64 * u64::from(default_size) <= file_len);
        } else {
            assert!(12 + sizes.len() * 4 <= buf.len());
        }
    }
}

/// Exact saturating conversion over the full `i64`/`u64` tick domain for one timescale:
/// the floor quotient clamped to the target range, with no intermediate overflow. A fully
/// symbolic `u32` timescale (a symbolic 128-bit divisor) did not finish within 15 minutes,
/// so the harnesses below fix the timescales real tracks use, including both extremes.
///
/// For any timescale other than 1 the production 128-bit division by the constant is itself
/// the bottleneck: replacing the oracle division with the multiplication-only bound
/// `q * ts <= n < (q + 1) * ts`, restricting ticks to `|a| < 2^49` (100 years at 90 kHz), and
/// switching to Kissat or MiniSat each still ran past 5 minutes for `ts = 90000`.
fn ticks_exact(ts: u32) {
    let a: i64 = kani::any();
    let exact = i128::from(a) * 1_000_000_000 / i128::from(ts);
    let na = ticks_to_ns_signed(a, ts);
    if exact > i128::from(i64::MAX) {
        assert_eq!(na, i64::MAX, "positive overflow saturates");
    } else if exact < i128::from(i64::MIN) {
        assert_eq!(na, i64::MIN, "negative overflow saturates");
    } else {
        assert_eq!(i128::from(na), exact, "in-range value is exact");
    }
    let ua: u64 = kani::any();
    let uexact = u128::from(ua) * 1_000_000_000 / u128::from(ts);
    let nu = ticks_to_ns_unsigned(ua, ts);
    if uexact > u128::from(u64::MAX) {
        assert_eq!(nu, u64::MAX, "unsigned overflow saturates");
    } else {
        assert_eq!(u128::from(nu), uexact, "in-range value is exact");
    }
}

/// Adjacent-tick monotonicity for one timescale (all pairs follow by induction).
/// Two production divisions per check: even `ts = 1` with Kissat ran past 5 minutes.
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

/// Samples up to 10 bytes (up to five NAL units). Every NAL copy has a symbolic length, so
/// CBMC exhausts memory: even one concrete 8-byte sample with 4-byte prefixes (two NAL units,
/// `unwind(5)`) reached the 31 GB limit after 88 seconds, and the 16-byte variant did not
/// finish within 15 minutes.
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
