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

/// Holds a parser result without running its drop glue: dropping a `MediaError` reaches the
/// boxed `dyn Error` inside `io::Error`, which CBMC resolves against every candidate drop
/// function. Only the harness skips the drop; the parser runs unchanged.
fn keep<T>(r: T) -> core::mem::ManuallyDrop<T> {
    core::mem::ManuallyDrop::new(r)
}

/// Header plus room for two entries; the entry count, every sample count/delta and
/// `max_samples` stay symbolic, so zero, one and two entries (with trailing bytes) are covered.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(4)]
fn k9_parse_stts_bounded() {
    let buf: [u8; 24] = kani::any();
    let max_samples = usize::from(kani::any::<u8>() % 4);
    let r = keep(parse_stts(&buf, max_samples));
    if let Ok(d) = &*r {
        assert!(d.len() <= max_samples, "expansion capped by stsz count");
        kani::cover!(
            d.len() == max_samples && max_samples > 1,
            "expansion reaches the cap"
        );
    }
}

/// A box shorter than its version/flags + entry-count header is rejected.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(3)]
fn k9_parse_stts_rejects_truncated_header() {
    let buf: [u8; 7] = kani::any();
    let max_samples = usize::from(kani::any::<u8>() % 4);
    assert!(keep(parse_stts(&buf, max_samples)).is_err());
}

/// Header plus room for two entries, so the ordering check between entries runs; the entry
/// count and every field stay symbolic (zero, one and two entries with trailing bytes).
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(3)]
fn k9_parse_stsc_strictly_increasing() {
    let buf: [u8; 32] = kani::any();
    let r = keep(parse_stsc(&buf));
    if let Ok(e) = &*r {
        assert!(!e.is_empty() && e[0].0 == 1 && e[0].1 > 0);
        assert!(8 + e.len() * 12 <= buf.len(), "entries fit in box");
        let mut i = 1;
        while i < e.len() {
            assert!(e[i - 1].0 < e[i].0 && e[i].1 > 0);
            i += 1;
        }
        kani::cover!(e.len() == 2, "two ordered entries accepted");
    }
}

/// A truncated header, or a box with room for no entry, is rejected.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(3)]
fn k9_parse_stsc_rejects_empty_or_truncated() {
    let buf: [u8; 15] = kani::any();
    let len = usize::from(kani::any::<u8>() % 3) * 4 + 7;
    assert!(
        keep(parse_stsc(&buf[..len])).is_err(),
        "7, 11 and 15 bytes hold no entry"
    );
}

/// Constant-size table (`default_size != 0`): the default size, the declared count and a file
/// length of 0..=3 bytes stay symbolic, so up to three samples can be accepted. The
/// explicit-size branch is proven by the next harness.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(5)]
fn k9_parse_stsz_bounded_by_file() {
    let buf: [u8; 12] = kani::any();
    let default_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    kani::assume(default_size != 0);
    let file_len = u64::from(kani::any::<u8>() % 4);
    let r = keep(parse_stsz(&buf, file_len));
    if let Ok(sizes) = &*r {
        let raw_count = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        assert_eq!(
            sizes.len() as u64,
            u64::from(raw_count),
            "declared count kept"
        );
        assert!(sizes.len() as u64 * u64::from(default_size) <= file_len);
        if let (Some(&first), Some(&last)) = (sizes.first(), sizes.last()) {
            assert!(first == default_size && last == default_size);
        }
        kani::cover!(sizes.len() > 1, "several constant-size samples accepted");
    }
}

/// Explicit-size table (`default_size == 0`) with room for two sizes: the count and both
/// sizes stay symbolic, and every accepted size lies inside the box.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(3)]
fn k9_parse_stsz_explicit_sizes_fit_box() {
    let mut buf: [u8; 20] = kani::any();
    buf[4..8].copy_from_slice(&[0; 4]);
    let file_len: u64 = kani::any();
    let r = keep(parse_stsz(&buf, file_len));
    if let Ok(sizes) = &*r {
        let raw_count = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        assert_eq!(
            sizes.len() as u64,
            u64::from(raw_count),
            "declared count kept"
        );
        assert!(12 + sizes.len() * 4 <= buf.len());
        let mut i = 0;
        while i < sizes.len() {
            let o = 12 + i * 4;
            assert_eq!(
                sizes[i],
                u32::from_be_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]])
            );
            i += 1;
        }
        kani::cover!(sizes.len() == 2, "two explicit sizes accepted");
    }
}

/// A box shorter than its version/flags + default-size + count header is rejected.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(3)]
fn k9_parse_stsz_rejects_truncated_header() {
    let buf: [u8; 11] = kani::any();
    let file_len: u64 = kani::any();
    assert!(keep(parse_stsz(&buf, file_len)).is_err());
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

/// Samples up to 10 bytes (up to five NAL units). Kept in `kani-extended`: CBMC exhausts
/// memory in propositional reduction as soon as a NAL slice returned by `split_avcc_nals` is
/// read back (`n[0]` in the SPS/PPS scan or the `extend_from_slice` copy). Even a fixed 4-byte
/// sample with 1-byte NAL lengths and a concrete `ParamSetMode` exceeds 7 GB, while
/// `split_avcc_nals` alone on the same input verifies in about 7 s.
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

/// K10. A symbolic three-frame table covers both continuous holds and adjacent boundaries.
#[kani::proof]
#[kani::unwind(5)]
fn k10_frame_and_trim() {
    use crate::timeline::{end_ns, frame_at_time, out_frame, out_time};
    let a = i64::from(kani::any::<u16>()) + 1;
    let b = a + i64::from(kani::any::<u16>()) + 1;
    let pts = [0, a, b];
    let end = end_ns(&pts, Some(i64::from(kani::any::<u16>()) + 1), 1);
    let t = i64::from(kani::any::<u32>());
    let u = i64::from(kani::any::<u32>());
    kani::assume(t <= u && u <= end);
    let i = frame_at_time(&pts, t).unwrap();
    let j = frame_at_time(&pts, u).unwrap();
    assert!(i <= j && j < pts.len());
    assert!(pts[i] <= t && (t < out_time(&pts, end, i) || t == end));
    assert!(pts[i] < out_time(&pts, end, j) && out_time(&pts, end, j) <= end);
    assert_eq!(out_frame(&pts, out_time(&pts, end, j)), Some(j));
}

#[kani::proof]
#[kani::unwind(5)]
fn k10_normalization() {
    use crate::timeline::{end_ns, viewport_timestamps};
    let table = viewport_timestamps(kani::any::<[i64; 3]>().into_iter());
    assert_eq!(table[0], 0);
    assert!(table[1] > 0 && table[2] > table[1]);
    assert!(end_ns(&table, Some(kani::any()), 1) > table[2]);
}

#[kani::proof]
fn k10_fraction() {
    use crate::timeline::{end_ns, fraction_to_time, time_to_fraction};
    let end = end_ns(&[0], Some(if kani::any() { 7 } else { 4 }), 1);
    let t = i64::from(kani::any::<u8>() & 7);
    let u = i64::from(kani::any::<u8>() & 7);
    kani::assume(t <= u && u <= end);
    assert!(time_to_fraction(t, end) <= time_to_fraction(u, end));
    assert_eq!(time_to_fraction(0, end), 0.0);
    assert_eq!(time_to_fraction(end, end), 1.0);
    assert_eq!(fraction_to_time(time_to_fraction(t, end), end), t);
}
