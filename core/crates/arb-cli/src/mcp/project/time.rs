//! Opt-in host wall clock beside the canonical UTC fields.
//!
//! Evidence always carries UTC. Only `time_zone: "host"` adds a local RFC 3339 rendering with an
//! explicit numeric offset, so default responses never depend on the host's zone.
use serde_json::{Map, Value};

/// Where a local offset comes from.
#[derive(Clone, Copy)]
enum Source {
    /// The host's zone at each instant (daylight saving included).
    Host,
    /// A fixed offset in seconds east of UTC, for deterministic tests.
    #[cfg(test)]
    Fixed(i32),
}

/// The caller's `time_zone` choice. `None` keeps responses zone-independent.
#[derive(Clone, Copy)]
pub(in crate::mcp) struct Zone(Option<Source>);

impl Zone {
    pub(in crate::mcp) const UTC: Self = Self(None);

    pub(in crate::mcp) fn from_args(args: &Map<String, Value>) -> Self {
        if args.get("time_zone").and_then(Value::as_str) == Some("host") {
            Self(Some(Source::Host))
        } else {
            Self::UTC
        }
    }

    #[cfg(test)]
    pub(in crate::mcp) const fn fixed(offset_secs: i32) -> Self {
        Self(Some(Source::Fixed(offset_secs)))
    }

    /// Adds `local_key` rendered from the UTC text at `utc_key` (null when it cannot be read).
    pub(in crate::mcp) fn localize(self, row: &mut Value, utc_key: &str, local_key: &str) {
        let ms = row.get(utc_key).and_then(Value::as_str).and_then(parse_ms);
        self.insert(row, ms, local_key);
    }

    /// For log rows: with `time_zone: "host"` the row carries `local_key` *instead of* `utc_key`.
    /// The local text has an explicit numeric offset, so it names the same instant and converts
    /// back to UTC exactly (#591). Unknown instants stay null. A missing host zone uses +00:00. The default
    /// zone leaves the row untouched.
    pub(in crate::mcp) fn localize_row(self, row: &mut Value, utc_key: &str, local_key: &str) {
        if self.0.is_none() {
            return;
        }
        self.localize(row, utc_key, local_key);
        if let Some(obj) = row.as_object_mut() {
            let utc = obj.remove(utc_key);
            if obj.get(local_key).is_none_or(Value::is_null)
                && let Some(ms) = utc.as_ref().and_then(Value::as_str).and_then(parse_ms)
            {
                obj.insert(local_key.into(), Value::String(format_local(ms, 0)));
            }
        }
    }

    /// Adds `local_key` rendered from the Unix milliseconds at `ms_key`.
    pub(in crate::mcp) fn localize_unix_ms(self, row: &mut Value, ms_key: &str, local_key: &str) {
        let ms = row.get(ms_key).and_then(Value::as_i64);
        self.insert(row, ms, local_key);
    }

    fn insert(self, row: &mut Value, unix_ms: Option<i64>, local_key: &str) {
        let Some(source) = self.0 else { return };
        let offset_at = |ms: i64| match source {
            Source::Host => arb_os::clock::host_utc_offset_secs(ms.div_euclid(1000)),
            #[cfg(test)]
            Source::Fixed(offset) => Some(offset),
        };
        let local = unix_ms.and_then(|ms| offset_at(ms).map(|offset| format_local(ms, offset)));
        if let Some(obj) = row.as_object_mut() {
            obj.insert(local_key.into(), local.map_or(Value::Null, Value::String));
        }
    }
}

/// RFC 3339 local time with millisecond precision and an explicit `±HH:MM` offset.
///
/// RFC 3339 offsets have minute resolution, so a historical second-level offset (for example
/// -00:44:30) is truncated toward zero and the clock is shifted by that same minute offset: the
/// text always names exactly the instant `unix_ms`.
pub(in crate::mcp) fn format_local(unix_ms: i64, offset_secs: i32) -> String {
    let offset_secs = offset_secs / 60 * 60;
    let shifted = unix_ms.saturating_add(i64::from(offset_secs) * 1000);
    let mut text = arb_core::time::format_unix_ms_iso8601(shifted);
    text.pop();
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let abs = offset_secs.unsigned_abs();
    format!("{text}{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60)
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)` into Unix milliseconds.
pub(in crate::mcp) fn parse_ms(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = b.get(range)?;
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| digits.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    if b.get(4) != Some(&b'-')
        || b.get(7) != Some(&b'-')
        || b.get(10) != Some(&b'T')
        || b.get(13) != Some(&b':')
        || b.get(16) != Some(&b':')
    {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let mut i = 19;
    let mut millis = 0;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        i = start;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
        let mut padded = text[start..i.min(start + 3)].to_owned();
        while padded.len() < 3 {
            padded.push('0');
        }
        millis = padded.parse::<i64>().ok()?;
    }
    let offset_secs = match b.get(i..)? {
        [b'Z'] => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let magnitude = num(i + 1..i + 3)? * 3600 + num(i + 4..i + 6)? * 60;
            if *sign == b'-' { -magnitude } else { magnitude }
        }
        _ => return None,
    };
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_secs;
    Some(secs * 1000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::{Zone, format_local, parse_ms};
    use serde_json::json;

    const TOKYO: i32 = 9 * 3600;
    const ST_JOHNS: i32 = -(3 * 3600 + 30 * 60);

    #[test]
    fn host_fields_are_explicit_rfc3339_offsets() {
        assert_eq!(
            format_local(1_795_829_025_123, 9 * 3600),
            "2026-11-28T10:23:45.123+09:00"
        );
        assert_eq!(
            format_local(1_795_829_025_123, -(3 * 3600 + 30 * 60)),
            "2026-11-27T21:53:45.123-03:30"
        );
        assert_eq!(format_local(-1, 0), "1969-12-31T23:59:59.999+00:00");
        // Monrovia 1970 (-00:44:30): the label keeps minute resolution and the same instant.
        assert_eq!(format_local(0, -2670), "1969-12-31T23:16:00.000-00:44");
        let mut row = json!({"timestamp_utc":"2026-11-28T01:23:45.123Z","created":"2026-11-28T01:23:45Z","ms":1_795_829_025_123_i64});
        Zone::fixed(TOKYO).localize(&mut row, "timestamp_utc", "timestamp_local");
        Zone::fixed(ST_JOHNS).localize(&mut row, "created", "created_local");
        Zone::fixed(TOKYO).localize_unix_ms(&mut row, "ms", "ms_local");
        assert_eq!(row["timestamp_local"], "2026-11-28T10:23:45.123+09:00");
        assert_eq!(row["created_local"], "2026-11-27T21:53:45.000-03:30");
        assert_eq!(row["ms_local"], "2026-11-28T10:23:45.123+09:00");
        // UTC (the default) adds nothing; unreadable input becomes an explicit null.
        let mut plain = json!({"timestamp_utc":"2026-11-28T01:23:45.123Z"});
        Zone::UTC.localize(&mut plain, "timestamp_utc", "timestamp_local");
        assert!(plain.get("timestamp_local").is_none());
        let mut broken = json!({"timestamp_utc":null});
        Zone::fixed(TOKYO).localize(&mut broken, "timestamp_utc", "timestamp_local");
        assert_eq!(broken["timestamp_local"], json!(null));
    }

    #[test]
    fn host_log_rows_replace_utc_with_a_reversible_local_time() {
        let mut row = json!({"timestamp_utc":"2026-11-28T01:23:45.123Z","seq":1});
        Zone::fixed(TOKYO).localize_row(&mut row, "timestamp_utc", "timestamp_local");
        assert!(row.get("timestamp_utc").is_none());
        assert_eq!(row["timestamp_local"], "2026-11-28T10:23:45.123+09:00");
        assert_eq!(
            parse_ms(row["timestamp_local"].as_str().unwrap()),
            parse_ms("2026-11-28T01:23:45.123Z")
        );
        // An unknown timestamp stays unknown on the requested axis.
        let mut unknown = json!({"timestamp_utc":null});
        Zone::fixed(TOKYO).localize_row(&mut unknown, "timestamp_utc", "timestamp_local");
        assert_eq!(unknown, json!({"timestamp_local":null}));
        let mut plain = json!({"timestamp_utc":"2026-11-28T01:23:45.123Z"});
        Zone::UTC.localize_row(&mut plain, "timestamp_utc", "timestamp_local");
        assert_eq!(plain, json!({"timestamp_utc":"2026-11-28T01:23:45.123Z"}));
    }

    #[test]
    fn parser_rejects_malformed_text() {
        for bad in [
            "",
            "2026-11-28",
            "2026-13-28T01:23:45Z",
            "2026-11-28T01:23:45",
            "2026-11-28T01:23:45.Z",
            "2026-11-28T01:23:45+0900",
            "2026-11-28 01:23:45Z",
        ] {
            assert_eq!(parse_ms(bad), None, "{bad}");
        }
        assert_eq!(parse_ms("1970-01-01T00:00:00.5Z"), Some(500));
        assert_eq!(parse_ms("1970-01-01T00:00:00.123456789Z"), Some(123));
    }

    proptest::proptest! {
        #[test]
        fn local_and_utc_renderings_name_the_same_instant(
            ms in -2_208_988_800_000_i64..32_503_680_000_000,
            raw_offset in -(14 * 3600)..=(14 * 3600),
        ) {
            let offset = raw_offset / 60 * 60;
            let utc = arb_core::time::format_unix_ms_iso8601(ms);
            proptest::prop_assert_eq!(parse_ms(&utc), Some(ms));
            let local = format_local(ms, raw_offset);
            proptest::prop_assert_eq!(parse_ms(&local), Some(ms));
            let sign = if offset < 0 { '-' } else { '+' };
            let suffix = format!("{sign}{:02}:{:02}", offset.unsigned_abs() / 3600, offset.unsigned_abs() % 3600 / 60);
            proptest::prop_assert!(local.ends_with(&suffix), "{} {}", local, suffix);
        }
    }
}
