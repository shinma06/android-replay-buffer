//! Convert evdev absolute-axis values into physical display pixels before gesture classification.

use std::collections::BTreeMap;

use arb_adb::wire::TouchEventPayload;

#[derive(Debug)]
struct Axis {
    min: i32,
    max: i32,
    pixels: i32,
}

impl Axis {
    fn project(&self, value: i32) -> Option<i32> {
        if !(self.min..=self.max).contains(&value) {
            return None;
        }
        let last_pixel = self.pixels - 1;
        // Some drivers advertise a maximum equal to the dimension instead of its last index.
        if self.min == 0 && (self.max == self.pixels || self.max == last_pixel) {
            return Some(value.min(last_pixel));
        }
        let span = i128::from(self.max) - i128::from(self.min);
        let offset = i128::from(value) - i128::from(self.min);
        i32::try_from((offset * i128::from(last_pixel) + span / 2) / span).ok()
    }
}

#[derive(Debug, Default)]
pub(super) struct TouchCalibration {
    devices: BTreeMap<String, BTreeMap<u16, Axis>>,
}

impl TouchCalibration {
    pub(super) fn from_getevent(output: &str, width: u32, height: u32) -> Self {
        let mut calibration = Self::default();
        let mut device = None;
        for line in output.lines().map(str::trim) {
            if line.starts_with("add device ") {
                device = line.split_once(": ").map(|(_, path)| path.trim());
                continue;
            }
            let Some(device) = device else { continue };
            // getevent -lp prefixes the first absolute axis with "ABS (0003):".
            let axis_line = line.strip_prefix("ABS (0003):").unwrap_or(line).trim();
            let Some((label, fields)) = axis_line.split_once(':') else {
                continue;
            };
            let (code, pixels) = match label.trim() {
                "ABS_X" => (0x00, width),
                "ABS_Y" => (0x01, height),
                "ABS_MT_POSITION_X" => (0x35, width),
                "ABS_MT_POSITION_Y" => (0x36, height),
                _ => continue,
            };
            let field = |prefix| {
                fields
                    .split(',')
                    .find_map(|f| f.trim().strip_prefix(prefix)?.trim().parse::<i32>().ok())
            };
            if let (Some(min), Some(max), Ok(pixels)) =
                (field("min "), field("max "), i32::try_from(pixels))
                && min < max
                && pixels > 0
            {
                calibration
                    .devices
                    .entry(device.to_owned())
                    .or_default()
                    .insert(code, Axis { min, max, pixels });
            }
        }
        calibration
    }

    pub(super) fn display_value(&self, event: &TouchEventPayload) -> Option<i32> {
        if event.ev_type != 3 {
            return None;
        }
        self.devices
            .get(&event.device_path)?
            .get(&event.ev_code)?
            .project(event.ev_value)
    }
}

/// Layer-1 PDD (K6a): evdev axis projection.
///
/// With a symbolic `(min, max)` the divisor of the `i128` division in [`Axis::project`] is
/// symbolic, so CBMC bit-blasts a 128-bit by 128-bit multiplier; with fixed axis bounds it is
/// a constant. The required harnesses therefore fix the axis bounds to real touchscreen
/// ranges and keep the value (any `i32`) symbolic. The two harnesses over symbolic `(min, max)`
/// stay `kani-extended`.
#[cfg(kani)]
mod kani_proofs {
    #![cfg_attr(not(feature = "kani-extended"), allow(dead_code))]
    use super::Axis;

    /// Realistic evdev axes: `min`/`max` within `i16` and up to 2^13 pixels. Over the full
    /// `i32` domain the symbolic `i128` division in `project` did not finish within 13
    /// minutes; the special-cased `min == 0` driver quirk stays reachable in this domain.
    /// The range harness did not finish within 10 minutes with `min`/`max` limited to 8-bit
    /// values either (6 bits: 171 seconds, 4 bits: 49 seconds).
    fn any_axis() -> Axis {
        let axis = Axis {
            min: i32::from(kani::any::<i16>()),
            max: i32::from(kani::any::<i16>()),
            pixels: any_pixels(),
        };
        kani::assume(axis.min < axis.max);
        axis
    }

    fn any_pixels() -> i32 {
        i32::from(kani::any::<u16>() % (1 << 13)) + 1
    }

    /// Display dimensions used by the monotonicity harnesses. With up to 2^13 symbolic
    /// pixels the two products `offset * (pixels - 1)` are symbolic by symbolic, and even a
    /// fixed `0..=4095` axis did not finish within 10 minutes (up to 2^8 pixels: 23 seconds).
    /// 1080 hits the `max == pixels` and `max == pixels - 1` quirks of the fixed axes below.
    const DISPLAY_PIXELS: [i32; 6] = [720, 1080, 1440, 2400, 3120, 4096];

    fn any_display_pixels() -> i32 {
        DISPLAY_PIXELS[usize::from(kani::any::<u8>() % 6)]
    }

    fn check_range_and_endpoints(axis: &Axis) {
        let v: i32 = kani::any();
        let r = axis.project(v);
        assert_eq!(
            r.is_some(),
            axis.min <= v && v <= axis.max,
            "Some iff value in [min,max]"
        );
        if let Some(p) = r {
            assert!(0 <= p && p <= axis.pixels - 1);
        }
        assert_eq!(axis.project(axis.min), Some(0), "min maps to first pixel");
        assert_eq!(
            axis.project(axis.max),
            Some(axis.pixels - 1),
            "max maps to last pixel"
        );
    }

    /// Monotonicity over adjacent values (all pairs follow by induction).
    fn check_monotonic(axis: &Axis) {
        let v: i32 = kani::any();
        kani::assume(axis.min <= v && v < axis.max);
        let (Some(p1), Some(p2)) = (axis.project(v), axis.project(v + 1)) else {
            panic!("values inside [min, max] always project");
        };
        assert!(p1 <= p2, "projection is non-decreasing");
    }

    #[cfg_attr(feature = "kani-extended", kani::proof)]
    fn k6_axis_project_range_and_endpoints() {
        check_range_and_endpoints(&any_axis());
    }

    #[cfg_attr(feature = "kani-extended", kani::proof)]
    fn k6_axis_project_monotonic() {
        check_monotonic(&any_axis());
    }

    /// Real touchscreen axis ranges: 15-bit HID digitizers, 12-bit controllers, drivers that
    /// report the last pixel index or the dimension itself as `max`, and a centred axis.
    const FIXED_AXES: [(i32, i32); 5] =
        [(0, 32767), (0, 4095), (0, 1079), (0, 1080), (-1000, 1000)];

    macro_rules! fixed_axis_harnesses {
        ($($idx:literal => $range:ident, $mono:ident;)*) => {$(
            #[kani::proof]
            fn $range() {
                let (min, max) = FIXED_AXES[$idx];
                check_range_and_endpoints(&Axis { min, max, pixels: any_pixels() });
            }

            #[kani::proof]
            fn $mono() {
                let (min, max) = FIXED_AXES[$idx];
                check_monotonic(&Axis { min, max, pixels: any_display_pixels() });
            }
        )*};
    }

    fixed_axis_harnesses! {
        0 => k6_axis_project_range_and_endpoints_span_0_32767, k6_axis_project_monotonic_span_0_32767_display_pixels;
        1 => k6_axis_project_range_and_endpoints_span_0_4095, k6_axis_project_monotonic_span_0_4095_display_pixels;
        2 => k6_axis_project_range_and_endpoints_span_0_1079, k6_axis_project_monotonic_span_0_1079_display_pixels;
        3 => k6_axis_project_range_and_endpoints_span_0_1080, k6_axis_project_monotonic_span_0_1080_display_pixels;
        4 => k6_axis_project_range_and_endpoints_span_m1000_1000, k6_axis_project_monotonic_span_m1000_1000_display_pixels;
    }
}
