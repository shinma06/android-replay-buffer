//! Deterministic UTC ISO-8601 formatting and monotonic identifier helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use arb_core::time::format_unix_ms_iso8601;
pub use arb_core::time::format_unix_secs_iso8601;

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

/// Formats the current system wall time as an ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SS.sssZ`).
#[must_use]
pub fn utc_now_iso8601() -> String {
    format_unix_ms_iso8601(i64::try_from(current_unix_millis()).unwrap_or(i64::MAX))
}

/// Formats a Unix epoch nanosecond timestamp as an ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SS.sssZ`).
#[must_use]
pub fn format_epoch_ns_iso8601(epoch_ns: i64) -> String {
    format_unix_ms_iso8601(epoch_ns.div_euclid(1_000_000))
}

#[cfg(test)]
mod tests {
    use super::next_id;
    use arb_core::time::format_unix_secs_iso8601;

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
