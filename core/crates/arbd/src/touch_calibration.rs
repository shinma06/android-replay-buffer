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
        let span = i64::from(self.max) - i64::from(self.min);
        let offset = i64::from(value) - i64::from(self.min);
        i32::try_from((offset * i64::from(last_pixel) + span / 2) / span).ok()
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
