//! Layer-1 PDD (K9a): bounded model checking of the MP4 box/table parsers.
//! K12: GOP cache byte budget, sequential forward decode window and filmstrip cache identity
//! of `playback.rs`.
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

use std::convert::Infallible;

use crate::codec::PixelFormat;
use crate::mp4::{
    AvccConfig, ParamSetMode, avcc_sample_to_annexb, checked_entry_count, iter_boxes, parse_stsc,
    parse_stsz, parse_stts, ticks_to_ns_signed, ticks_to_ns_unsigned,
};
use crate::playback::{
    FILMSTRIP_MAGIC, FilmstripCache, FrameWindow, WindowFrame, advance_forward_window,
    estimated_frame_bytes, fnv1a_64, full_gop_exceeds_budget, reuses_forward_window, window_frame,
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

// ---------------- K12: playback GOP budget, forward window, filmstrip identity ----------------

/// Production budget of the daemon GOP caches (`media_engine.rs`, `inspect_export.rs`).
const BUDGET_128_MIB: usize = 128 * 1024 * 1024;

/// Phone and desktop capture sizes (portrait and landscape).
const RESOLUTIONS: [(u16, u16); 8] = [
    (0, 1080),
    (1, 1),
    (1920, 1080),
    (1080, 2400),
    (1440, 3120),
    (3840, 2160),
    (720, 1280),
    (u16::MAX, u16::MAX),
];

const FORMATS: [PixelFormat; 4] = [
    PixelFormat::Yuv420p,
    PixelFormat::Rgb24,
    PixelFormat::Rgba8,
    PixelFormat::Bgra8,
];

fn any_format() -> PixelFormat {
    match kani::any::<u8>() % 4 {
        0 => PixelFormat::Yuv420p,
        1 => PixelFormat::Rgb24,
        2 => PixelFormat::Rgba8,
        _ => PixelFormat::Bgra8,
    }
}

/// Any cache budget an allocation can hold (`1..=isize::MAX`, the Rust allocation limit;
/// production budgets are the 128 MiB constants). At `usize::MAX` the saturated eviction test
/// `cached + bytes > max_bytes` cannot fire and the counter would overflow.
fn any_budget() -> usize {
    let max_bytes: usize = kani::any();
    kani::assume(max_bytes >= 1 && max_bytes <= isize::MAX as usize);
    max_bytes
}

/// Decoded frame stand-in: only the index and the byte size reach the window logic.
#[derive(Clone, Copy)]
struct Frame {
    index: u64,
    bytes: usize,
}

impl WindowFrame for Frame {
    fn index(&self) -> u64 {
        self.index
    }

    fn bytes(&self) -> usize {
        self.bytes
    }
}

const WINDOW_CAP: usize = 8;

/// FIFO model of the production `VecDeque` window (std container semantics are a TCB axiom):
/// `buf[head..tail]` holds the retained frames, oldest first.
struct Window {
    buf: [Frame; WINDOW_CAP],
    head: usize,
    tail: usize,
}

impl Window {
    const fn new() -> Self {
        Self {
            buf: [Frame { index: 0, bytes: 0 }; WINDOW_CAP],
            head: 0,
            tail: 0,
        }
    }

    const fn len(&self) -> usize {
        self.tail - self.head
    }

    fn back(&self) -> Option<&Frame> {
        if self.len() == 0 {
            None
        } else {
            Some(&self.buf[self.tail - 1])
        }
    }
}

impl FrameWindow for Window {
    type Frame = Frame;

    fn front(&self) -> Option<&Frame> {
        self.get(0)
    }

    fn get(&self, offset: usize) -> Option<&Frame> {
        if offset < self.len() {
            Some(&self.buf[self.head + offset])
        } else {
            None
        }
    }

    fn push_back(&mut self, frame: Frame) {
        assert!(self.tail < WINDOW_CAP, "model capacity");
        self.buf[self.tail] = frame;
        self.tail += 1;
    }

    fn pop_front(&mut self) -> Option<Frame> {
        if self.len() == 0 {
            return None;
        }
        self.head += 1;
        Some(self.buf[self.head - 1])
    }
}

fn retained_bytes(frames: &Window) -> usize {
    let mut sum = 0usize;
    let mut i = frames.head;
    while i < frames.tail {
        sum += frames.buf[i].bytes;
        i += 1;
    }
    sum
}

/// Window invariant after every production step: the budget counter equals the retained
/// payload, stays within `max_bytes`, and the window is the contiguous run ending at `next - 1`.
fn assert_window_invariant(frames: &Window, cached: usize, max_bytes: usize, next: u64) {
    assert_eq!(
        cached,
        retained_bytes(frames),
        "counter tracks retained bytes"
    );
    assert!(cached <= max_bytes, "cached_bytes <= max_bytes");
    if let Some(last) = frames.back() {
        assert_eq!(last.index + 1, next, "window ends at the decoder position");
    }
    let mut i = frames.head + 1;
    while i < frames.tail {
        assert_eq!(
            frames.buf[i - 1].index + 1,
            frames.buf[i].index,
            "window is contiguous"
        );
        i += 1;
    }
}

/// Frame byte size and the full-GOP budget decision never overflow or panic for any `u16` frame
/// size (the track header type: `0`, `1x1`, 1080p, 1080x2400, 1440x3120, 4K up to `u16::MAX`),
/// any pixel format, any GOP length and any budget. When the decision keeps the GOP in the cache
/// its full payload fits in `max_bytes`; a Full-HD RGBA 30-frame GOP (about 237 MiB) is streamed
/// instead of cached under 128 MiB.
#[kani::proof]
#[kani::unwind(9)]
fn k12_frame_and_gop_byte_budget_no_overflow_and_bounded() {
    let mut i = 0;
    while i < RESOLUTIONS.len() {
        let (w, h) = RESOLUTIONS[i];
        let px = usize::from(w) * usize::from(h);
        assert_eq!(estimated_frame_bytes(w, h, PixelFormat::Rgba8), px * 4);
        assert_eq!(estimated_frame_bytes(h, w, PixelFormat::Bgra8), px * 4);
        assert_eq!(estimated_frame_bytes(w, h, PixelFormat::Rgb24), px * 3);
        let yuv = estimated_frame_bytes(w, h, PixelFormat::Yuv420p);
        assert!(yuv >= px + px / 2 && yuv <= px * 3, "4:2:0 is 1.5 bytes/px");
        i += 1;
    }
    assert_eq!(estimated_frame_bytes(0, 1080, any_format()), 0);
    assert_eq!(estimated_frame_bytes(1920, 0, any_format()), 0);

    let frame = estimated_frame_bytes(kani::any(), kani::any(), any_format());
    assert!(
        frame <= 65_535 * 65_535 * 4,
        "frame bytes bounded by u16::MAX^2 * 4"
    );

    let gop_start: u64 = kani::any();
    let gop_end: u64 = kani::any();
    kani::assume(gop_start <= gop_end && gop_end - gop_start < u64::MAX);
    let max_bytes: usize = kani::any();
    let _ = full_gop_exceeds_budget(
        kani::any(),
        kani::any(),
        any_format(),
        (gop_start, gop_end),
        max_bytes,
    );

    // Exact decision under the production 128 MiB budget for every listed resolution and pixel
    // format: the GOP is streamed iff `frame * len > budget`, i.e. `len > budget / frame`.
    let len = gop_end - gop_start + 1;
    let (mut streamed, mut cached) = (false, false);
    let mut i = 0;
    while i < RESOLUTIONS.len() {
        let (w, h) = RESOLUTIONS[i];
        for format in FORMATS {
            let frame = estimated_frame_bytes(w, h, format);
            let exceeds =
                full_gop_exceeds_budget(w, h, format, (gop_start, gop_end), BUDGET_128_MIB);
            let fits = (BUDGET_128_MIB / frame.max(1)) as u64;
            assert_eq!(exceeds, frame > 0 && len > fits, "exact 128 MiB decision");
            streamed |= exceeds;
            cached |= !exceeds && frame > 0 && len > 1;
        }
        i += 1;
    }
    kani::cover!(streamed, "GOP streamed");
    kani::cover!(cached, "GOP cached");

    assert!(full_gop_exceeds_budget(
        1920,
        1080,
        PixelFormat::Rgba8,
        (0, 29),
        BUDGET_128_MIB
    ));
    assert!(full_gop_exceeds_budget(
        1080,
        2400,
        PixelFormat::Rgba8,
        (0, 29),
        BUDGET_128_MIB
    ));
    assert!(!full_gop_exceeds_budget(
        320,
        180,
        PixelFormat::Rgb24,
        (0, 29),
        BUDGET_128_MIB
    ));
}

/// One forward request through `advance_forward_window` from a fresh window (as after an IDR
/// restart) to any target, with symbolic per-frame sizes of any `usize` and any allocatable
/// budget: no arithmetic overflow, eviction keeps `cached_bytes == retained payload <=
/// max_bytes` with a contiguous window ending at the decoder position, and the served frame is
/// always `target` (retained, or returned uncached when it alone is larger than the budget).
#[kani::proof]
#[kani::unwind(4)]
fn k12_forward_window_eviction_keeps_cached_bytes_within_budget() {
    const N: u64 = 3;
    let sizes: [usize; N as usize] = kani::any();
    let max_bytes: usize = any_budget();
    let start: u64 = kani::any();
    kani::assume(start <= u64::MAX - N);

    let mut frames = Window::new();
    let mut next = start;
    let mut cached = 0usize;
    let target = start + u64::from(kani::any::<u8>()) % N;
    let Ok(served) = advance_forward_window(
        &mut frames,
        &mut next,
        &mut cached,
        max_bytes,
        target,
        |i| {
            Ok::<_, Infallible>(Frame {
                index: i,
                bytes: sizes[(i - start) as usize],
            })
        },
    );
    assert_window_invariant(&frames, cached, max_bytes, next);
    assert_eq!(next, target + 1, "decoder stops right after target");
    let served_uncached = served.is_some();
    if let Some(f) = served {
        assert!(f.index == target && f.bytes > max_bytes, "oversized target");
    } else {
        assert_eq!(window_frame(&frames, target).map(|f| f.index), Some(target));
    }
    kani::cover!(served_uncached, "oversized target served uncached");
    kani::cover!(
        frames.len() >= 2 && cached == max_bytes,
        "budget filled exactly"
    );
    kani::cover!(
        frames.front().is_some_and(|f| f.index > start),
        "oldest frames evicted"
    );
}

/// Production glue of `GopLruCache::fetch_forward` around the extracted helpers: reuse the
/// window when `reuses_forward_window` says so, otherwise restart a fresh window at the GOP's
/// IDR frame (`clear()` + new sequential decoder).
struct ForwardModel {
    frames: Window,
    next: u64,
    cached: usize,
    key: Option<(u64, PixelFormat)>,
    idr_restarts: u32,
    decoded: u64,
}

impl ForwardModel {
    const fn new() -> Self {
        Self {
            frames: Window::new(),
            next: 0,
            cached: 0,
            key: None,
            idr_restarts: 0,
            decoded: 0,
        }
    }

    fn reuses(&self, key: (u64, PixelFormat), target: u64) -> bool {
        self.key
            .is_some_and(|wk| reuses_forward_window(wk, &self.frames, self.next, key, target))
    }

    /// Serves `target` of the GOP `key.0` starting at IDR `idr`; returns the served index.
    fn fetch(
        &mut self,
        key: (u64, PixelFormat),
        idr: u64,
        target: u64,
        frame_bytes: usize,
        max_bytes: usize,
    ) -> u64 {
        if !self.reuses(key, target) {
            self.frames = Window::new();
            self.cached = 0;
            self.next = idr;
            self.key = Some(key);
            self.idr_restarts += 1;
        }
        if target < self.next {
            return window_frame(&self.frames, target).map_or(u64::MAX, |f| f.index);
        }
        let decoded = &mut self.decoded;
        let Ok(served) = advance_forward_window(
            &mut self.frames,
            &mut self.next,
            &mut self.cached,
            max_bytes,
            target,
            |i| {
                assert!(idr <= i && i <= target, "decodes only IDR..=target");
                *decoded += 1;
                Ok::<_, Infallible>(Frame {
                    index: i,
                    bytes: frame_bytes,
                })
            },
        );
        assert!(self.cached <= max_bytes, "cached_bytes <= max_bytes");
        match served {
            Some(f) => f.index,
            None => window_frame(&self.frames, target).map_or(u64::MAX, |f| f.index),
        }
    }
}

/// Up to `MAX_RETAINED` frames of any allocatable sizes retained by a forward window positioned
/// at `next`: the contiguous run `next - len..next` inside the GOP starting at `idr`, with
/// `cached_bytes` equal to their total within `max_bytes` (the window invariant).
const MAX_RETAINED: u8 = 3;

fn any_retained_window(idr: u64, next: u64, max_bytes: usize) -> (Window, usize) {
    let len = u64::from(kani::any::<u8>() % (MAX_RETAINED + 1));
    kani::assume(len <= next - idr);
    let mut frames = Window::new();
    let mut cached = 0usize;
    let mut i = 0;
    while i < len {
        let bytes: usize = kani::any();
        kani::assume(bytes <= max_bytes - cached);
        cached += bytes;
        frames.push_back(Frame {
            index: next - len + i,
            bytes,
        });
        i += 1;
    }
    (frames, cached)
}

/// A GOP of any length (up to `u32::MAX` frames), any decoded frame size and any budget,
/// including the streamed case where the full GOP exceeds the budget (a 30-frame Full-HD RGBA
/// GOP of about 237 MiB under 128 MiB, which `full_gop_exceeds_budget` sends to the forward
/// window). Base: a cold seek to `idr + s` restarts once at the IDR and decodes `s + 1` frames.
/// Step: from any window state the forward cursor can hold after serving `prev` (up to
/// `MAX_RETAINED` retained frames), the request `prev + 1` reuses the cursor (no IDR restart),
/// decodes exactly one frame, serves it and preserves the window invariant. By induction a run
/// of `k` `+1` steps decodes `k` frames, not `k * (k + 1) / 2`.
#[kani::proof]
#[kani::solver(minisat)]
#[kani::unwind(5)]
fn k12_sequential_forward_decode_avoids_idr_restart_even_over_128mib() {
    let key = (kani::any::<u64>(), any_format());
    let frame_bytes: usize = kani::any();
    let max_bytes: usize = if kani::any() {
        BUDGET_128_MIB
    } else {
        any_budget()
    };
    let idr: u64 = kani::any();
    let gop_len: u64 = kani::any();
    kani::assume(gop_len >= 1 && gop_len <= u64::from(u32::MAX));
    kani::assume(idr <= u64::MAX - gop_len);

    let seek = u64::from(kani::any::<u8>() % 3);
    kani::assume(seek < gop_len);
    let mut cold = ForwardModel::new();
    assert_eq!(
        cold.fetch(key, idr, idr + seek, frame_bytes, max_bytes),
        idr + seek
    );
    assert_eq!(cold.idr_restarts, 1, "a cold seek starts at the IDR once");
    assert_eq!(cold.decoded, seek + 1, "seek decodes IDR..=target once");

    let prev: u64 = kani::any();
    kani::assume(idr <= prev && prev - idr < gop_len - 1);
    let (frames, cached) = any_retained_window(idr, prev + 1, max_bytes);
    let retained = frames.len();
    let mut m = ForwardModel {
        frames,
        next: prev + 1,
        cached,
        key: Some(key),
        idr_restarts: 0,
        decoded: 0,
    };
    assert!(m.reuses(key, prev + 1), "+1 step keeps the cursor");
    assert_eq!(
        m.fetch(key, idr, prev + 1, frame_bytes, max_bytes),
        prev + 1
    );
    assert_eq!(m.idr_restarts, 0, "no IDR restart on a +1 step");
    assert_eq!(m.decoded, 1, "+1 step decodes exactly one frame");
    assert_window_invariant(&m.frames, m.cached, max_bytes, m.next);
    kani::cover!(
        max_bytes == BUDGET_128_MIB
            && frame_bytes == 1920 * 1080 * 4
            && gop_len == 30
            && prev == idr + 28,
        "Full-HD RGBA 30-frame GOP (237 MiB) stepped under 128 MiB"
    );
    kani::cover!(
        retained == usize::from(MAX_RETAINED) && m.frames.len() <= retained,
        "full window evicts while stepping"
    );
    kani::cover!(
        frame_bytes > max_bytes,
        "frame larger than the budget served uncached"
    );
}

/// From any window state the cursor can hold after serving `prev` in GOP `a` (up to
/// `MAX_RETAINED` retained frames), a request for `target` reuses the window only for the same
/// GOP and format and only when `target` is ahead of the decoder or still retained; a reused
/// backward step serves the retained frame without decoding. Otherwise (backward past the
/// window, another GOP or format) decoding restarts from the target GOP's IDR
/// `idr <= target < frame_count` and decodes exactly `IDR..=target`. The served frame is always
/// `target`, never a frame of another GOP.
#[kani::proof]
#[kani::solver(minisat)]
#[kani::unwind(5)]
fn k12_backward_or_cross_gop_seek_restarts_from_target_idr_and_bounds() {
    const MAX_GOP: u64 = 3;
    let frame_bytes: usize = kani::any();
    let max_bytes: usize = any_budget();
    let frame_count: u64 = kani::any();
    let (fmt_a, fmt_b) = (any_format(), any_format());
    let (gop_a, gop_b): (u64, u64) = (kani::any(), kani::any());
    let (idr_a, idr_b): (u64, u64) = (kani::any(), kani::any());
    let len_a = 1 + u64::from(kani::any::<u8>()) % MAX_GOP;
    let len_b = 1 + u64::from(kani::any::<u8>()) % MAX_GOP;
    kani::assume(idr_a <= u64::MAX - MAX_GOP && idr_b <= u64::MAX - MAX_GOP);
    kani::assume(idr_a + len_a <= frame_count && idr_b + len_b <= frame_count);
    // GOP ids name disjoint frame ranges (Mp4VideoTrack index, TCB).
    kani::assume(
        (gop_a == gop_b && idr_a == idr_b && len_a == len_b)
            || (gop_a != gop_b && (idr_a + len_a <= idr_b || idr_b + len_b <= idr_a)),
    );
    let (off_a, off_b): (u64, u64) = (kani::any(), kani::any());
    kani::assume(off_a < len_a && off_b < len_b);
    let prev = idr_a + off_a;
    let target = idr_b + off_b;

    let (frames, cached) = any_retained_window(idr_a, prev + 1, max_bytes);
    let mut m = ForwardModel {
        frames,
        next: prev + 1,
        cached,
        key: Some((gop_a, fmt_a)),
        idr_restarts: 0,
        decoded: 0,
    };
    let key_b = (gop_b, fmt_b);
    let reused = m.reuses(key_b, target);
    let retained = window_frame(&m.frames, target).is_some();
    assert_eq!(
        reused,
        gop_a == gop_b && fmt_a == fmt_b && (target > prev || retained),
        "reuse only for same GOP/format, ahead or retained"
    );
    let served = m.fetch(key_b, idr_b, target, frame_bytes, max_bytes);
    assert_eq!(served, target, "served frame is the target");
    assert!(idr_b <= target && target < frame_count);
    if reused {
        assert_eq!(m.idr_restarts, 0);
        if target <= prev {
            assert_eq!(m.decoded, 0, "retained backward step decodes nothing");
        } else {
            assert_eq!(
                m.decoded,
                target - prev,
                "forward reuse decodes prev+1..=target"
            );
        }
    } else {
        assert_eq!(m.idr_restarts, 1, "restart from the target IDR");
        assert_eq!(m.decoded, target - idr_b + 1, "decodes IDR..=target");
    }
    kani::cover!(
        reused && target < prev,
        "backward step served from the window"
    );
    kani::cover!(
        !reused && gop_a == gop_b && fmt_a == fmt_b && target < prev,
        "backward past the window restarts"
    );
    kani::cover!(!reused && gop_a != gop_b, "cross-GOP seek restarts");
    kani::cover!(
        !reused && gop_a == gop_b && fmt_a != fmt_b,
        "format change restarts"
    );
}

/// Two `filmstrip.bin` files with valid checksums and no entries, one with the current
/// `ARBFS002` header and one with the legacy `ARBFS001` header, both recording file length,
/// frame count and SHA-256 content digest of a source track. Against a source with any
/// (symbolic) file length, frame count and digest, the current file is accepted exactly when
/// all three fingerprint fields are equal: any digest mismatch is a cache miss even when
/// `file_len` and `frame_count` match. The legacy header is always rejected.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
#[kani::unwind(62)]
fn k12_filmstrip_cache_rejects_mismatched_content_digest() {
    const LEN: u64 = 0x0123_4567_89ab_cdef;
    const COUNT: u64 = 1_800;
    const DIGEST: [u8; 32] = [0xa5; 32];
    fn bundle(magic: &[u8; 8]) -> [u8; 68] {
        let mut bytes = [0u8; 68];
        bytes[..8].copy_from_slice(magic);
        bytes[8..16].copy_from_slice(&LEN.to_le_bytes());
        bytes[16..24].copy_from_slice(&COUNT.to_le_bytes());
        bytes[24..56].copy_from_slice(&DIGEST);
        let checksum = fnv1a_64(&bytes[..60]);
        bytes[60..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }
    let legacy: bool = kani::any();
    let bytes = if legacy {
        bundle(b"ARBFS001")
    } else {
        bundle(&FILMSTRIP_MAGIC)
    };
    let expected_len: u64 = kani::any();
    let expected_count: u64 = kani::any();
    let expected_digest: [u8; 32] = kani::any();

    let r = keep(FilmstripCache::from_bytes(
        &bytes,
        expected_len,
        expected_count,
        &expected_digest,
    ));
    let len_ok = expected_len == LEN;
    let count_ok = expected_count == COUNT;
    let digest_ok = expected_digest == DIGEST;
    assert_eq!(
        r.is_ok(),
        !legacy && len_ok && count_ok && digest_ok,
        "accepted exactly on ARBFS002 with a full fingerprint match"
    );
    kani::cover!(r.is_ok(), "matching cache is a hit");
    kani::cover!(
        r.is_err() && !legacy && len_ok && count_ok && !digest_ok,
        "digest-only mismatch is a miss"
    );
    kani::cover!(
        r.is_err() && legacy && len_ok && count_ok && digest_ok,
        "legacy header is a miss"
    );
}

fn k13_range() -> ([u64; 3], u64, u64, u64) {
    let interval = || u64::from(kani::any::<u16>()) + 1;
    let second = interval();
    let third = second + interval();
    let end = third + interval();
    let (start, stop) = (u64::from(kani::any::<u32>()), u64::from(kani::any::<u32>()));
    kani::assume(start < stop && stop <= end);
    ([0, second, third], end, start, stop)
}

#[kani::proof]
#[kani::solver(minisat)]
#[kani::unwind(5)]
fn k13_time_range_trim_valid_bounds_and_positive_durations() {
    let (pts, end, start, stop) = k13_range();
    let (first, last, mut durations) =
        crate::timeline::trim_intervals(&pts, end, start, stop).unwrap();
    assert!(first <= last && last < pts.len());
    assert!(durations.all(|d| d > 0));
    kani::cover!(first == 1 && last == 1, "single middle frame");
}

#[kani::proof]
#[kani::solver(minisat)]
#[kani::unwind(5)]
fn k13_time_range_trim_exact_duration_conservation() {
    let (pts, end, start, stop) = k13_range();
    let (first, last, durations) = crate::timeline::trim_intervals(&pts, end, start, stop).unwrap();
    assert_eq!(durations.sum::<u64>(), stop - start);
    kani::cover!(
        start > 0 && stop < end && last - first == 2,
        "both ends shortened"
    );
}

#[kani::proof]
#[kani::solver(minisat)]
#[kani::unwind(5)]
fn k13_time_range_trim_frame_equivalence_at_every_time() {
    let (pts, end, start, stop) = k13_range();
    let t = u64::from(kani::any::<u32>());
    kani::assume(t < stop - start);
    let (first, _, durations) = crate::timeline::trim_intervals(&pts, end, start, stop).unwrap();
    let mut elapsed = 0;
    let mut displayed = None;
    for (offset, d) in durations.enumerate() {
        elapsed += d;
        if t < elapsed {
            displayed = Some(first + offset);
            break;
        }
    }
    let expected = pts.iter().rposition(|&stamp| stamp <= start + t);
    assert_eq!(displayed, expected);
    kani::cover!(start + t == pts[1], "exact internal boundary");
}

#[kani::proof]
#[kani::solver(minisat)]
#[kani::unwind(5)]
fn k13_time_range_trim_rejects_invalid_ranges() {
    let (pts, end, _, _) = k13_range();
    let (start, stop): (u64, u64) = (kani::any(), kani::any());
    kani::assume(start >= stop || stop > end);
    assert!(crate::timeline::plan_time_range_trim(&pts, end, start, stop).is_err());
    assert!(crate::timeline::plan_time_range_trim(&[], end, 0, end).is_err());
    kani::cover!(start == stop, "empty selection");
    kani::cover!(stop > end && start < stop, "past track end");
}

#[kani::proof]
#[kani::solver(minisat)]
#[kani::unwind(5)]
fn k13_quantize_durations_positive_and_drift_free() {
    // 8 ns/tick; every residue including exact half ticks and sub-tick samples is reachable.
    let durations = [(); 3].map(|()| 1 + u64::from(kani::any::<u8>()));
    let target = (durations.iter().sum::<u64>() + 4) / 8;
    let result = crate::timeline::quantize_durations_ns_to_ticks(&durations, 125_000_000);
    if target < 3 {
        assert!(result.is_err());
    } else {
        let ticks = result.unwrap();
        assert_eq!(ticks.len(), 3);
        assert!(ticks.iter().all(|&d| d >= 1));
        assert_eq!(ticks.iter().map(|&d| u64::from(d)).sum::<u64>(), target);
    }
    kani::cover!(
        durations[0] == 1 && durations[2] == 1 && target >= 3,
        "two sub-tick edges"
    );
}

/// K19 exercises the production partition traversal with bounded storage. Each output
/// bucket is observed as a bit set independently of the selector's mutable scratch.
#[kani::proof]
#[kani::unwind(19)]
fn k19_contact_sheet_partitions_range_and_keeps_anchors() {
    use crate::contact_sheet::visit_contact_sheet_indices;
    let start: usize = kani::any();
    let span = usize::from(kani::any::<u8>() % 17);
    kani::assume(start <= usize::MAX - span);
    let end = start + span;
    let budget: usize = kani::any();
    kani::assume(budget >= 2);
    let anchors: [usize; 3] = kani::any();
    let mut scratch = [false; 17];
    let mut previous = None;
    let (mut selected, mut omitted) = (0_u32, 0_u32);
    let mut count = 0;
    visit_contact_sheet_indices(
        start,
        &mut scratch[..=span],
        budget,
        &anchors,
        |index, keep| {
            assert!((start..=end).contains(&index));
            assert!(previous.is_none_or(|p| p < index));
            previous = Some(index);
            let bit = 1 << (index - start);
            if keep {
                selected |= bit;
                count += 1;
            } else {
                omitted |= bit;
            }
        },
    );
    assert!(count == budget.min(span + 1));
    assert!(selected & omitted == 0 && selected | omitted == (2 << span) - 1);
    let ends = 1 | (1 << span);
    assert!(selected & ends == ends);
    let mut required: u32 = ends;
    for a in anchors {
        if (start..=end).contains(&a) {
            required |= 1 << (a - start);
        }
    }
    let fits = required.count_ones() as usize <= budget;
    if fits {
        assert!(selected & required == required);
    }
    kani::cover!(
        fits && required != ends && omitted != 0,
        "anchor retained while sampling"
    );
    kani::cover!(!fits, "more anchors than slots");
}

/// Pin the public collector to the two buckets verified by the symbolic traversal.
#[kani::proof]
#[kani::unwind(6)]
fn k19_public_selection_returns_both_buckets() {
    let (anchor, omitted) = if kani::any() { (421, 422) } else { (422, 421) };
    let (kept, dropped) =
        crate::contact_sheet::select_contact_sheet_indices_with_anchors(420, 423, 3, &[anchor]);
    assert_eq!(kept.len(), 3);
    assert!(kept[0] == 420 && kept[1] == anchor && kept[2] == 423);
    assert!(dropped.len() == 1 && dropped[0] == omitted);
}

/// K21: the crop placement used by `tap_crop_rect` for any extent `1..=total` and any tap in
/// the frame stays inside the frame and contains the tap.
#[kani::proof]
fn k21_place_crop_stays_in_frame_and_contains_tap() {
    let total: u32 = kani::any();
    let extent: u32 = kani::any();
    let tap: u32 = kani::any();
    kani::assume(extent >= 1 && extent <= total && tap < total);
    let (origin, rel) = crate::contact_sheet::place_crop(total, extent, tap);
    assert!(u64::from(origin) + u64::from(extent) <= u64::from(total));
    assert!(origin <= tap && tap - origin < extent);
    assert!(rel == tap - origin && rel < extent);
    // Centred whenever the frame leaves room on both sides.
    if tap >= extent / 2 && tap - extent / 2 <= total - extent {
        assert!(rel == extent / 2);
    }
    kani::cover!(
        origin == 0 && tap < extent / 2,
        "clamped at the leading edge"
    );
    kani::cover!(
        origin == total - extent && origin > 0,
        "clamped at the trailing edge"
    );
    kani::cover!(rel == extent / 2 && origin > 0, "centred");
}

/// K21: `tap_crop_rect` with the published default options, for any positive frame and any tap
/// inside it, returns a non-empty rectangle inside the frame that contains the tap, with
/// `tap_rel` inside the rectangle. All positive u32 dimensions are covered.
#[kani::proof]
#[kani::stub(std::fmt::format, format_stub)]
fn k21_tap_crop_rect_bounds_and_contains_tap() {
    let video_w: u32 = kani::any();
    let video_h: u32 = kani::any();
    let tx: u32 = kani::any();
    let ty: u32 = kani::any();
    kani::assume(tx < video_w && ty < video_h);
    let rect = crate::contact_sheet::tap_crop_rect(
        video_w,
        video_h,
        tx,
        ty,
        &crate::contact_sheet::TapCropOptions::default(),
    );
    let Some(r) = rect else {
        panic!("a tap inside a non-empty frame always has a crop");
    };
    assert!(r.crop_width > 0 && r.crop_height > 0);
    assert!(u64::from(r.crop_x) + u64::from(r.crop_width) <= u64::from(video_w));
    assert!(u64::from(r.crop_y) + u64::from(r.crop_height) <= u64::from(video_h));
    assert!(r.crop_x <= tx && tx < r.crop_x + r.crop_width);
    assert!(r.crop_y <= ty && ty < r.crop_y + r.crop_height);
    assert!(r.tap_rel_x == tx - r.crop_x && r.tap_rel_x < r.crop_width);
    assert!(r.tap_rel_y == ty - r.crop_y && r.tap_rel_y < r.crop_height);
    kani::cover!(r.crop_width == video_w, "crop spans the whole width");
    kani::cover!(r.crop_width < video_w && r.crop_x > 0, "interior crop");
}

/// K21: a tap outside the frame never yields a crop.
#[kani::proof]
fn k21_tap_crop_rect_rejects_taps_outside_the_frame() {
    let video_w: u32 = kani::any();
    let video_h: u32 = kani::any();
    let tx: u32 = kani::any();
    let ty: u32 = kani::any();
    kani::assume(tx >= video_w || ty >= video_h);
    assert!(
        crate::contact_sheet::tap_crop_rect(
            video_w,
            video_h,
            tx,
            ty,
            &crate::contact_sheet::TapCropOptions::default()
        )
        .is_none()
    );
    kani::cover!(video_w == 0, "empty frame");
}

/// K23 (#586-G): for every positive `u32` frame size and every positive `u32` box edge, the
/// JPEG tile size is positive, inside the box, unchanged when the frame already fits, and the
/// longer edge reaches the box otherwise. Kani's default checks also show the arithmetic never
/// overflows and never divides by zero.
#[kani::proof]
fn k23_fit_within_bounds_without_overflow() {
    let w: u32 = kani::any();
    let h: u32 = kani::any();
    let max: u32 = kani::any();
    kani::assume(w >= 1 && h >= 1 && max >= 1);
    let (ow, oh) = crate::jpeg::fit_within(w, h, max);
    assert!(ow >= 1 && oh >= 1 && ow <= max && oh <= max);
    if w <= max && h <= max {
        assert!(ow == w && oh == h);
    } else if w >= h {
        assert!(ow == max && oh <= ow);
    } else {
        assert!(oh == max && ow <= oh);
    }
    kani::cover!(w <= max && h <= max, "already fits");
    kani::cover!(w > max && w >= h && oh > 1, "landscape scaled");
    kani::cover!(h > max && h > w && ow > 1, "portrait scaled");
    kani::cover!(w > max && oh == 1, "short edge clamped to one pixel");
}

/// K23 (extended): with the published edge (1024 px) and any frame a baseline JPEG can carry
/// (edges up to 65,535), the shorter edge is the nearest integer to the exact ratio (1 when that
/// rounds to 0) and neither neighbouring size is closer. The products make this nonlinear; it did
/// not finish within five minutes locally, so it runs only in the `kani-extended` job. The
/// proptest `fit_within_keeps_the_aspect_ratio` samples the same property over all of `u32`.
#[cfg_attr(feature = "kani-extended", kani::proof)]
fn k23_fit_within_rounds_to_the_nearest_ratio() {
    let w: u32 = kani::any();
    let h: u32 = kani::any();
    kani::assume(w >= 1 && h >= 1 && w <= 65_535 && h <= 65_535);
    let max = crate::jpeg::DEFAULT_JPEG_MAX_EDGE;
    let (ow, oh) = crate::jpeg::fit_within(w, h, max);
    if w > max || h > max {
        let (long, short, out_short) = if w >= h { (w, h, oh) } else { (h, w, ow) };
        let (long, short, out_short) = (u64::from(long), u64::from(short), u64::from(out_short));
        let exact = short * u64::from(max);
        let err = (out_short * long).abs_diff(exact);
        assert!(2 * err <= long || (out_short == 1 && 2 * exact < long));
        assert!(((out_short + 1) * long).abs_diff(exact) >= err);
        if out_short > 1 {
            assert!(((out_short - 1) * long).abs_diff(exact) >= err);
        }
    }
    kani::cover!(w > max && oh > 1, "scaled");
}
