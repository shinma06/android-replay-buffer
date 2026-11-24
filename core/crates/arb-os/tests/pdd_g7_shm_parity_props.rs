//! Layer-3 PDD (G7): SHM odd-width / stride / padding parity. The same golden table is
//! asserted by `ide-plugin/client-core/.../ShmLayoutParityTest.kt` against the JVM reader.

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    clippy::fn_params_excessive_bools,
    clippy::trivially_copy_pass_by_ref,
    clippy::print_stdout,
    clippy::collapsible_if,
    clippy::similar_names,
    clippy::format_collect,
    clippy::as_conversions,
    clippy::clone_on_ref_ptr,
    reason = "PDD model/property code: long linearized models and generators read best unsplit"
)]

use arb_os::shm::{FrameDescriptor, PixelFormat, ShmFrameRingReader, ShmFrameRingWriter};
use proptest::prelude::*;

/// (format, width, height, stride, required bytes). Keep in sync with `ShmLayoutParityTest.kt`.
const GOLDEN: &[(PixelFormat, u32, u32, u32, Option<usize>)] = &[
    (PixelFormat::Bgra8, 1, 1, 3, None),
    (PixelFormat::Bgra8, 1, 1, 4, Some(4)),
    (PixelFormat::Bgra8, 1, 1, 9, Some(9)),
    (PixelFormat::Bgra8, 3, 2, 11, None),
    (PixelFormat::Bgra8, 3, 2, 12, Some(24)),
    (PixelFormat::Bgra8, 3, 2, 17, Some(34)),
    (PixelFormat::Bgra8, 3, 3, 11, None),
    (PixelFormat::Bgra8, 3, 3, 12, Some(36)),
    (PixelFormat::Bgra8, 3, 3, 17, Some(51)),
    (PixelFormat::Bgra8, 17, 15, 67, None),
    (PixelFormat::Bgra8, 17, 15, 68, Some(1020)),
    (PixelFormat::Bgra8, 17, 15, 73, Some(1095)),
    (PixelFormat::Rgba8, 1, 1, 3, None),
    (PixelFormat::Rgba8, 1, 1, 4, Some(4)),
    (PixelFormat::Rgba8, 1, 1, 9, Some(9)),
    (PixelFormat::Rgba8, 3, 2, 11, None),
    (PixelFormat::Rgba8, 3, 2, 12, Some(24)),
    (PixelFormat::Rgba8, 3, 2, 17, Some(34)),
    (PixelFormat::Rgba8, 3, 3, 11, None),
    (PixelFormat::Rgba8, 3, 3, 12, Some(36)),
    (PixelFormat::Rgba8, 3, 3, 17, Some(51)),
    (PixelFormat::Rgba8, 17, 15, 67, None),
    (PixelFormat::Rgba8, 17, 15, 68, Some(1020)),
    (PixelFormat::Rgba8, 17, 15, 73, Some(1095)),
    (PixelFormat::Rgb24, 1, 1, 2, None),
    (PixelFormat::Rgb24, 1, 1, 3, Some(3)),
    (PixelFormat::Rgb24, 1, 1, 8, Some(8)),
    (PixelFormat::Rgb24, 3, 2, 8, None),
    (PixelFormat::Rgb24, 3, 2, 9, Some(18)),
    (PixelFormat::Rgb24, 3, 2, 14, Some(28)),
    (PixelFormat::Rgb24, 3, 3, 8, None),
    (PixelFormat::Rgb24, 3, 3, 9, Some(27)),
    (PixelFormat::Rgb24, 3, 3, 14, Some(42)),
    (PixelFormat::Rgb24, 17, 15, 50, None),
    (PixelFormat::Rgb24, 17, 15, 51, Some(765)),
    (PixelFormat::Rgb24, 17, 15, 56, Some(840)),
    (PixelFormat::Nv12, 1, 1, 1, None),
    (PixelFormat::Nv12, 1, 1, 2, Some(4)),
    (PixelFormat::Nv12, 1, 1, 7, Some(14)),
    (PixelFormat::Nv12, 3, 2, 3, None),
    (PixelFormat::Nv12, 3, 2, 4, Some(12)),
    (PixelFormat::Nv12, 3, 2, 9, Some(27)),
    (PixelFormat::Nv12, 3, 3, 3, None),
    (PixelFormat::Nv12, 3, 3, 4, Some(20)),
    (PixelFormat::Nv12, 3, 3, 9, Some(45)),
    (PixelFormat::Nv12, 17, 15, 17, None),
    (PixelFormat::Nv12, 17, 15, 18, Some(414)),
    (PixelFormat::Nv12, 17, 15, 23, Some(529)),
];

#[test]
fn g7_golden_required_buffer_bytes_parity_table() {
    for &(fmt, w, h, s, want) in GOLDEN {
        assert_eq!(
            fmt.required_buffer_bytes(w, h, s),
            want,
            "{fmt:?} {w}x{h} stride {s}"
        );
    }
}

fn fmt() -> impl Strategy<Value = PixelFormat> {
    prop_oneof![
        Just(PixelFormat::Bgra8),
        Just(PixelFormat::Rgba8),
        Just(PixelFormat::Rgb24),
        Just(PixelFormat::Nv12)
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(48),
        ..ProptestConfig::default()
    })]

    #[test]
    fn g7_publish_read_roundtrip_with_padding_and_short_plane_rejection(
        f in fmt(), w in 1u32..20, h in 1u32..16, pad in 0u32..9, seed in any::<u8>(),
    ) {
        let min_row = match f {
            PixelFormat::Bgra8 | PixelFormat::Rgba8 => w * 4,
            PixelFormat::Rgb24 => w * 3,
            PixelFormat::Nv12 => w.next_multiple_of(2),
            _ => return Err(TestCaseError::fail("unknown pixel format")),
        };
        let stride = min_row + pad;
        let need = f.required_buffer_bytes(w, h, stride).expect("valid layout");
        prop_assert!(f.required_buffer_bytes(w, h, min_row - 1).is_none(), "under-sized stride rejected");
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("ring.shm");
        let mut writer = ShmFrameRingWriter::create(&path, need + 64).expect("create");
        let reader = ShmFrameRingReader::open(&path).expect("open");
        let pixels: Vec<u8> = (0..need).map(|i| u8::try_from(i % 251).expect("u8") ^ seed).collect();
        let d = FrameDescriptor { frame_index: 7, is_approximate: false, width: w, height: h, stride, pixel_format: f, boottime_ns: 1 };
        prop_assert!(writer.publish(d, &pixels[..need - 1]).is_err(), "short plane rejected");
        let generation = writer.publish(d, &pixels).expect("publish");
        let mut out = Vec::new();
        let meta = reader.read_latest(&mut out).expect("read").expect("frame");
        prop_assert_eq!(meta.generation, generation);
        prop_assert_eq!((meta.width, meta.height, meta.stride), (w, h, stride));
        prop_assert_eq!(&out[..need], &pixels[..]);
    }
}
