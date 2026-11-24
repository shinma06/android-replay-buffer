//! Deterministic UTC ISO-8601 formatting and monotonic identifier helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: i64 = 86_400;
const SECS_PER_HOUR: i64 = 3_600;
const SECS_PER_MINUTE: i64 = 60;
const NANOS_PER_SEC: i64 = 1_000_000_000;

static ID_SEQ: AtomicU64 = AtomicU64::new(1);

/// Generates a process-unique monotonic identifier with `prefix`.
#[must_use]
pub fn next_id(prefix: &str) -> String {
    let now_ms = current_unix_millis();
    let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("{prefix}-{now_ms:013}-{pid}-{seq:04}")
}

/// Returns current Unix epoch milliseconds, saturating at zero if system clock precedes 1970.
#[must_use]
pub fn current_unix_millis() -> u64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(dur.as_millis()).unwrap_or(u64::MAX)
}

/// Formats the current system wall time as an ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SSZ`).
#[must_use]
pub fn utc_now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_i64, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    format_unix_secs_iso8601(secs)
}

/// Formats a Unix epoch nanosecond timestamp as an ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SSZ`).
#[must_use]
pub fn format_epoch_ns_iso8601(epoch_ns: i64) -> String {
    format_unix_secs_iso8601(epoch_ns.div_euclid(NANOS_PER_SEC))
}

/// Formats Unix epoch seconds into an ISO-8601 UTC string using civil-from-days arithmetic.
#[must_use]
pub fn format_unix_secs_iso8601(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(SECS_PER_DAY);
    let secs_of_day = unix_secs.rem_euclid(SECS_PER_DAY);
    let hour = secs_of_day / SECS_PER_HOUR;
    let minute = (secs_of_day % SECS_PER_HOUR) / SECS_PER_MINUTE;
    let second = secs_of_day % SECS_PER_MINUTE;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Converts days since `1970-01-01` into proleptic Gregorian `(year, month, day)`.
///
/// Algorithm due to Howard Hinnant (`civil_from_days`).
const fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::{format_unix_secs_iso8601, next_id};

    #[test]
    fn formats_known_utc_timestamps_accurately() {
        assert_eq!(
            format_unix_secs_iso8601(0),
            "1970-01-01T00:00:00Z",
            "Unix epoch must format to 1970-01-01T00:00:00Z"
        );
        assert_eq!(
            format_unix_secs_iso8601(1_704_067_200),
            "2024-01-01T00:00:00Z",
            "2024-01-01T00:00:00Z must match"
        );
        assert_eq!(
            format_unix_secs_iso8601(1_709_251_199),
            "2024-02-29T23:59:59Z",
            "leap day 2024-02-29 must format accurately"
        );
    }

    #[test]
    fn generates_unique_monotonic_ids() {
        let a = next_id("job");
        let b = next_id("job");
        assert_ne!(a, b, "sequential IDs must be distinct");
        assert!(a.starts_with("job-"), "ID must keep prefix");
    }
}
