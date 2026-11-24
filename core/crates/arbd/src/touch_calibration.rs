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
/// The `_standard_spans` harnesses fix real device spans and display dimensions and keep the
/// value symbolic. The symbolic-axis harnesses are `kani-extended`: with a symbolic span or
/// pixel count the `i128` division in [`Axis::project`] did not verify within five minutes
/// (monotonicity with 1..=65536 pixels on a fixed span also ran past 5 minutes, with CaDiCaL
/// and Kissat alike).
#[cfg(kani)]
mod kani_proofs {
    #![cfg_attr(not(feature = "kani-extended"), allow(dead_code))]
    use super::Axis;

    /// Realistic evdev axes: `min`/`max` within `i16` and up to 2^13 pixels. Over the full
    /// `i32` domain the symbolic `i128` division in `project` did not finish within 13
    /// minutes; the special-cased `min == 0` driver quirk stays reachable in this domain.
    fn any_axis() -> Axis {
        let axis = Axis {
            min: i32::from(kani::any::<i16>()),
            max: i32::from(kani::any::<i16>()),
            pixels: i32::from(kani::any::<u16>() % (1 << 13)) + 1,
        };
        kani::assume(axis.min < axis.max);
        axis
    }

    #[cfg_attr(feature = "kani-extended", kani::proof)]
    fn k6_axis_project_range_and_endpoints() {
        let axis = any_axis();
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
    #[cfg_attr(feature = "kani-extended", kani::proof)]
    fn k6_axis_project_monotonic() {
        let axis = any_axis();
        let v: i32 = kani::any();
        kani::assume(axis.min <= v && v < axis.max);
        let (Some(p1), Some(p2)) = (axis.project(v), axis.project(v + 1)) else {
            panic!("values inside [min, max] always project");
        };
        assert!(p1 <= p2, "projection is non-decreasing");
    }

    /// Evdev spans seen on real devices: the emulator's `0..=32767`, 12-bit controllers'
    /// `0..=4095`, drivers whose maximum is the last pixel index (`0..=1079`) or the dimension
    /// itself (`0..=1080`), and a span centred on zero.
    const STANDARD_SPANS: [(i32, i32); 5] =
        [(0, 32767), (0, 4095), (0, 1079), (0, 1080), (-1000, 1000)];

    /// Display dimensions in pixels: a one-pixel axis, common panel widths/heights (720, 2400),
    /// and 1080 so both `min == 0` driver quirks take the shortcut path.
    const STANDARD_PIXELS: [i32; 4] = [1, 720, 1080, 2400];

    /// Range and endpoints for every standard span × display dimension, with a fully symbolic
    /// `value`. Concrete spans and dimensions keep the `i128` division's operands mostly
    /// constant; `k6_axis_project_range_and_endpoints` is the symbolic-axis generalisation.
    #[kani::proof]
    #[kani::unwind(6)]
    fn k6_axis_project_range_and_endpoints_standard_spans() {
        for (min, max) in STANDARD_SPANS {
            for pixels in STANDARD_PIXELS {
                let axis = Axis { min, max, pixels };
                let v: i32 = kani::any();
                let r = axis.project(v);
                assert_eq!(
                    r.is_some(),
                    min <= v && v <= max,
                    "Some iff value in [min,max]"
                );
                if let Some(p) = r {
                    assert!(0 <= p && p < pixels, "projection stays on the display");
                }
                assert_eq!(axis.project(min), Some(0), "min maps to first pixel");
                assert_eq!(
                    axis.project(max),
                    Some(pixels - 1),
                    "max maps to last pixel"
                );
            }
        }
    }

    /// Adjacent-value monotonicity for every standard span × display dimension (all pairs
    /// follow by induction); `k6_axis_project_monotonic` is the symbolic-axis generalisation.
    #[kani::proof]
    #[kani::unwind(6)]
    fn k6_axis_project_monotonic_standard_spans() {
        for (min, max) in STANDARD_SPANS {
            for pixels in STANDARD_PIXELS {
                let axis = Axis { min, max, pixels };
                let v: i32 = kani::any();
                kani::assume(min <= v && v < max);
                let (p1, p2) = (axis.project(v), axis.project(v + 1));
                assert!(
                    p1.is_some() && p2.is_some(),
                    "values inside [min, max] always project"
                );
                assert!(p1 <= p2, "projection is non-decreasing");
            }
        }
    }
}
