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

/// Layer-1 PDD (K6a): evdev axis projection over the full `i32` domain.
#[cfg(kani)]
mod kani_proofs {
    use super::Axis;

    fn any_axis() -> Axis {
        let axis = Axis {
            min: kani::any(),
            max: kani::any(),
            pixels: kani::any(),
        };
        kani::assume(axis.min < axis.max && axis.pixels > 0);
        axis
    }

    #[kani::proof]
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

    #[kani::proof]
    fn k6_axis_project_monotonic() {
        let axis = any_axis();
        let (v1, v2): (i32, i32) = (kani::any(), kani::any());
        kani::assume(v1 <= v2);
        if let (Some(p1), Some(p2)) = (axis.project(v1), axis.project(v2)) {
            assert!(p1 <= p2, "projection is non-decreasing");
        }
    }
}
