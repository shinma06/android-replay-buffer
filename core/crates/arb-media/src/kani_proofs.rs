//! Layer-1 PDD (K9a): bounded model checking of the MP4 box/table parsers.
//!
//! Inputs are symbolic byte arrays of bounded length; unwinding assertions stay enabled.

use crate::mp4::{
    AvccConfig, ParamSetMode, avcc_sample_to_annexb, checked_entry_count, iter_boxes, parse_stsc,
    parse_stsz, parse_stts, ticks_to_ns_signed, ticks_to_ns_unsigned,
};

const N: usize = 24;

fn any_slice(buf: &[u8; N]) -> &[u8] {
    let len: usize = kani::any();
    kani::assume(len <= N);
    &buf[..len]
}

#[kani::proof]
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

#[kani::proof]
#[kani::unwind(6)]
fn k9_parse_stts_bounded() {
    let buf: [u8; N] = kani::any();
    let data = any_slice(&buf);
    let max_samples = usize::from(kani::any::<u8>() % 4);
    if let Ok(d) = parse_stts(data, max_samples) {
        assert!(d.len() <= max_samples, "expansion capped by stsz count");
    }
}

#[kani::proof]
#[kani::unwind(4)]
fn k9_parse_stsc_strictly_increasing() {
    let buf: [u8; N] = kani::any();
    let data = any_slice(&buf);
    if let Ok(e) = parse_stsc(data) {
        assert!(!e.is_empty() && e[0].0 == 1);
        let mut i = 1;
        while i < e.len() {
            assert!(e[i - 1].0 < e[i].0 && e[i].1 > 0);
            i += 1;
        }
    }
}

#[kani::proof]
#[kani::unwind(10)]
fn k9_parse_stsz_bounded_by_file() {
    let buf: [u8; N] = kani::any();
    let data = any_slice(&buf);
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

#[kani::proof]
fn k9_ticks_to_ns_saturating_and_monotone() {
    let ts: u32 = kani::any();
    kani::assume(ts >= 1);
    let (a, b): (i64, i64) = (kani::any(), kani::any());
    kani::assume(a <= b);
    let (na, nb) = (ticks_to_ns_signed(a, ts), ticks_to_ns_signed(b, ts));
    assert!(na <= nb, "signed conversion monotone");
    let exact = i128::from(a) * 1_000_000_000 / i128::from(ts);
    assert_eq!(
        i128::from(na),
        exact.clamp(i128::from(i64::MIN), i128::from(i64::MAX))
    );
    let (ua, ub): (u64, u64) = (kani::any(), kani::any());
    kani::assume(ua <= ub);
    assert!(ticks_to_ns_unsigned(ua, ts) <= ticks_to_ns_unsigned(ub, ts));
    let uexact = u128::from(ua) * 1_000_000_000 / u128::from(ts);
    assert_eq!(
        u128::from(ticks_to_ns_unsigned(ua, ts)),
        uexact.min(u128::from(u64::MAX))
    );
}

#[kani::proof]
#[kani::unwind(26)]
fn k9_avcc_sample_to_annexb_bounded() {
    let buf: [u8; 16] = kani::any();
    let len: usize = kani::any();
    kani::assume(len <= 16);
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
