//! Dual-clock sleep-aware host clock abstraction.
//!
//! Provides a coincident sample of:
//! - `active_mono_ns`: monotonic nanoseconds that pause during host PC sleep,
//! - `continuous_mono_ns`: monotonic nanoseconds that keep advancing during host PC sleep, and
//! - `wall_epoch_ns`: Unix epoch nanoseconds for human display only.
//!
//! On macOS, `CLOCK_UPTIME_RAW` and `CLOCK_MONOTONIC_RAW` are paired via
//! `clock_gettime_nsec_np`. `CLOCK_MONOTONIC` is explicitly avoided because
//! Spike S2 measured `-4.365 s` of NTP/adjtime slew divergence and 1000 ns
//! granularity relative to `CLOCK_UPTIME_RAW`.

use crate::error::OsError;

/// Default threshold (50 ms) above which a continuous-minus-active divergence
/// is classified as a host PC sleep interval.
pub const DEFAULT_PC_SLEEP_THRESHOLD_NS: i64 = 50_000_000;

/// Coincident sample of the host's active monotonic, continuous monotonic, and wall clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostClockSample {
    /// Monotonic nanoseconds that pause while the host PC is asleep (`PcActiveMono`).
    pub active_mono_ns: i64,
    /// Monotonic nanoseconds that advance while the host PC is asleep (`PcContinuousMono`).
    pub continuous_mono_ns: i64,
    /// Unix epoch nanoseconds from the host wall clock (`PcWallEpoch`), for display only.
    pub wall_epoch_ns: i64,
}

/// Samples the host's active monotonic, continuous monotonic, and wall clocks.
///
/// # Errors
///
/// Returns [`OsError::ClockRead`] if an underlying OS clock syscall fails, or
/// [`OsError::ClockOverflow`] if a clock value cannot be represented in `i64`
/// nanoseconds.
pub fn sample_host_clock() -> Result<HostClockSample, OsError> {
    imp::sample_host_clock()
}

/// Computes the estimated host sleep duration between two clock samples.
///
/// Defined as `(curr.continuous_mono_ns - prev.continuous_mono_ns) - (curr.active_mono_ns - prev.active_mono_ns)`.
/// During active execution without sleep, the delta stays near zero (within
/// clock-sampling call skew). After a host sleep of duration `S`, the delta
/// approximates `+S`.
#[must_use]
pub const fn sleep_delta_ns(prev: HostClockSample, curr: HostClockSample) -> i64 {
    let cont_delta = curr
        .continuous_mono_ns
        .saturating_sub(prev.continuous_mono_ns);
    let active_delta = curr.active_mono_ns.saturating_sub(prev.active_mono_ns);
    cont_delta.saturating_sub(active_delta)
}

/// Detects whether the host PC slept between `prev` and `curr`.
///
/// Returns `Some(delta_ns)` when [`sleep_delta_ns`] strictly exceeds
/// `threshold_ns` (and is positive); otherwise returns `None`.
#[must_use]
pub const fn detect_pc_sleep(
    prev: HostClockSample,
    curr: HostClockSample,
    threshold_ns: i64,
) -> Option<i64> {
    let delta = sleep_delta_ns(prev, curr);
    let effective_threshold = if threshold_ns > 0 { threshold_ns } else { 0 };
    if delta > effective_threshold {
        Some(delta)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;
    use std::mem::MaybeUninit;

    use crate::clock::HostClockSample;
    use crate::error::OsError;

    const NANOS_PER_SECOND: i64 = 1_000_000_000;

    unsafe extern "C" {
        fn clock_gettime_nsec_np(clock_id: libc::clockid_t) -> u64;
    }

    fn read_nsec_np(clock_id: libc::clockid_t, name: &'static str) -> Result<i64, OsError> {
        // SAFETY: `clock_gettime_nsec_np` is a thread-safe libSystem syscall
        // taking a scalar `clockid_t` value and returning 0 on error.
        let raw_ns = unsafe { clock_gettime_nsec_np(clock_id) };
        if raw_ns == 0 {
            return Err(OsError::ClockRead {
                clock: name,
                source: io::Error::last_os_error(),
            });
        }
        i64::try_from(raw_ns).map_err(|_| OsError::ClockOverflow { clock: name })
    }

    fn timespec_field_to_i64<T: TryInto<i64>>(
        value: T,
        clock: &'static str,
    ) -> Result<i64, OsError> {
        value
            .try_into()
            .map_err(|_| OsError::ClockOverflow { clock })
    }

    fn read_realtime_ns() -> Result<i64, OsError> {
        let mut ts = MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: `ts.as_mut_ptr()` points to writable stack storage sized for
        // `libc::timespec`, and `CLOCK_REALTIME` is a valid POSIX clock ID.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, ts.as_mut_ptr()) };
        if rc != 0 {
            return Err(OsError::ClockRead {
                clock: "CLOCK_REALTIME",
                source: io::Error::last_os_error(),
            });
        }
        // SAFETY: `clock_gettime` returned 0, so `ts` is fully initialized.
        let ts = unsafe { ts.assume_init() };
        let secs = timespec_field_to_i64(ts.tv_sec, "CLOCK_REALTIME")?;
        let nanos = timespec_field_to_i64(ts.tv_nsec, "CLOCK_REALTIME")?;
        secs.checked_mul(NANOS_PER_SECOND)
            .and_then(|s| s.checked_add(nanos))
            .ok_or(OsError::ClockOverflow {
                clock: "CLOCK_REALTIME",
            })
    }

    pub(super) fn sample_host_clock() -> Result<HostClockSample, OsError> {
        let active_mono_ns = read_nsec_np(libc::CLOCK_UPTIME_RAW, "CLOCK_UPTIME_RAW")?;
        // Use CLOCK_MONOTONIC_RAW (never CLOCK_MONOTONIC) per Spike S2.
        let continuous_mono_ns = read_nsec_np(libc::CLOCK_MONOTONIC_RAW, "CLOCK_MONOTONIC_RAW")?;
        let wall_epoch_ns = read_realtime_ns()?;
        Ok(HostClockSample {
            active_mono_ns,
            continuous_mono_ns,
            wall_epoch_ns,
        })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;
    use std::mem::MaybeUninit;

    use crate::clock::HostClockSample;
    use crate::error::OsError;

    const NANOS_PER_SECOND: i64 = 1_000_000_000;

    fn timespec_field_to_i64<T: TryInto<i64>>(
        value: T,
        clock: &'static str,
    ) -> Result<i64, OsError> {
        value
            .try_into()
            .map_err(|_| OsError::ClockOverflow { clock })
    }

    fn read_clock_ns(clock_id: libc::clockid_t, name: &'static str) -> Result<i64, OsError> {
        let mut ts = MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: `ts.as_mut_ptr()` points to writable stack storage sized for
        // `libc::timespec`, and `clock_id` is a valid Linux clock identifier.
        let rc = unsafe { libc::clock_gettime(clock_id, ts.as_mut_ptr()) };
        if rc != 0 {
            return Err(OsError::ClockRead {
                clock: name,
                source: io::Error::last_os_error(),
            });
        }
        // SAFETY: `clock_gettime` returned 0, so `ts` is fully initialized.
        let ts = unsafe { ts.assume_init() };
        let secs = timespec_field_to_i64(ts.tv_sec, name)?;
        let nanos = timespec_field_to_i64(ts.tv_nsec, name)?;
        secs.checked_mul(NANOS_PER_SECOND)
            .and_then(|s| s.checked_add(nanos))
            .ok_or(OsError::ClockOverflow { clock: name })
    }

    pub(super) fn sample_host_clock() -> Result<HostClockSample, OsError> {
        let active_mono_ns = read_clock_ns(libc::CLOCK_MONOTONIC, "CLOCK_MONOTONIC")?;
        let continuous_mono_ns = read_clock_ns(libc::CLOCK_BOOTTIME, "CLOCK_BOOTTIME")?;
        let wall_epoch_ns = read_clock_ns(libc::CLOCK_REALTIME, "CLOCK_REALTIME")?;
        Ok(HostClockSample {
            active_mono_ns,
            continuous_mono_ns,
            wall_epoch_ns,
        })
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::mem::MaybeUninit;

    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows_sys::Win32::System::WindowsProgramming::{
        QueryInterruptTimePrecise, QueryUnbiasedInterruptTimePrecise,
    };

    use crate::clock::HostClockSample;
    use crate::error::OsError;

    /// Number of 100-nanosecond ticks between 1601-01-01 (Windows FILETIME epoch)
    /// and 1970-01-01 (Unix epoch).
    const WINDOWS_TO_UNIX_EPOCH_100NS: i128 = 116_444_736_000_000_000;
    const NANOS_PER_HUNDRED_NS_TICK: i128 = 100;

    fn ticks_100ns_to_i64(ticks: u64, clock: &'static str) -> Result<i64, OsError> {
        let nanos = i128::from(ticks)
            .checked_mul(NANOS_PER_HUNDRED_NS_TICK)
            .ok_or(OsError::ClockOverflow { clock })?;
        i64::try_from(nanos).map_err(|_| OsError::ClockOverflow { clock })
    }

    fn read_unbiased_interrupt_time_ns() -> Result<i64, OsError> {
        let mut ticks: u64 = 0;
        // SAFETY: `&raw mut ticks` is a valid, properly aligned pointer to a
        // `u64` out-parameter for `QueryUnbiasedInterruptTimePrecise`.
        unsafe { QueryUnbiasedInterruptTimePrecise(&raw mut ticks) };
        ticks_100ns_to_i64(ticks, "QueryUnbiasedInterruptTimePrecise")
    }

    fn read_interrupt_time_ns() -> Result<i64, OsError> {
        let mut ticks: u64 = 0;
        // SAFETY: `&raw mut ticks` is a valid, properly aligned pointer to a
        // `u64` out-parameter for `QueryInterruptTimePrecise`.
        unsafe { QueryInterruptTimePrecise(&raw mut ticks) };
        ticks_100ns_to_i64(ticks, "QueryInterruptTimePrecise")
    }

    fn read_wall_epoch_ns() -> Result<i64, OsError> {
        let mut ft = MaybeUninit::<FILETIME>::uninit();
        // SAFETY: `ft.as_mut_ptr()` points to writable stack storage sized for
        // `FILETIME`, which `GetSystemTimePreciseAsFileTime` initializes.
        unsafe { GetSystemTimePreciseAsFileTime(ft.as_mut_ptr()) };
        // SAFETY: `GetSystemTimePreciseAsFileTime` unconditionally initializes `ft`.
        let ft = unsafe { ft.assume_init() };
        let ticks_u64 = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
        let unix_100ns = i128::from(ticks_u64) - WINDOWS_TO_UNIX_EPOCH_100NS;
        let unix_ns =
            unix_100ns
                .checked_mul(NANOS_PER_HUNDRED_NS_TICK)
                .ok_or(OsError::ClockOverflow {
                    clock: "GetSystemTimePreciseAsFileTime",
                })?;
        i64::try_from(unix_ns).map_err(|_| OsError::ClockOverflow {
            clock: "GetSystemTimePreciseAsFileTime",
        })
    }

    pub(super) fn sample_host_clock() -> Result<HostClockSample, OsError> {
        let active_mono_ns = read_unbiased_interrupt_time_ns()?;
        let continuous_mono_ns = read_interrupt_time_ns()?;
        let wall_epoch_ns = read_wall_epoch_ns()?;
        Ok(HostClockSample {
            active_mono_ns,
            continuous_mono_ns,
            wall_epoch_ns,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::{
        DEFAULT_PC_SLEEP_THRESHOLD_NS, HostClockSample, detect_pc_sleep, sample_host_clock,
        sleep_delta_ns,
    };

    #[test]
    fn host_clock_is_monotonic_over_ten_thousand_samples() {
        let mut prev = sample_host_clock().expect("initial host clock sample must succeed");
        assert!(
            prev.active_mono_ns > 0,
            "active_mono_ns must be positive, got {}",
            prev.active_mono_ns
        );
        assert!(
            prev.continuous_mono_ns > 0,
            "continuous_mono_ns must be positive, got {}",
            prev.continuous_mono_ns
        );
        assert!(
            prev.wall_epoch_ns > 1_577_836_800_000_000_000,
            "wall_epoch_ns should be after 2020-01-01, got {}",
            prev.wall_epoch_ns
        );

        for i in 0..10_000_u32 {
            let curr = sample_host_clock().expect("host clock sample must succeed");
            assert!(
                curr.active_mono_ns >= prev.active_mono_ns,
                "active_mono_ns moved backwards at sample {i}: prev={} curr={}",
                prev.active_mono_ns,
                curr.active_mono_ns
            );
            assert!(
                curr.continuous_mono_ns >= prev.continuous_mono_ns,
                "continuous_mono_ns moved backwards at sample {i}: prev={} curr={}",
                prev.continuous_mono_ns,
                curr.continuous_mono_ns
            );
            prev = curr;
        }
    }

    #[test]
    fn active_and_continuous_clocks_agree_across_100ms_sleep() {
        let before = sample_host_clock().expect("clock sample before sleep must succeed");
        thread::sleep(Duration::from_millis(100));
        let after = sample_host_clock().expect("clock sample after sleep must succeed");

        let active_delta = after.active_mono_ns - before.active_mono_ns;
        let cont_delta = after.continuous_mono_ns - before.continuous_mono_ns;
        let divergence = sleep_delta_ns(before, after).abs();

        assert!(
            active_delta >= 90_000_000,
            "expected active_delta >= 90ms after 100ms sleep, got {active_delta} ns"
        );
        assert!(
            cont_delta >= 90_000_000,
            "expected cont_delta >= 90ms after 100ms sleep, got {cont_delta} ns"
        );
        assert!(
            divergence < 5_000_000,
            "expected active vs continuous divergence < 5ms, got {divergence} ns (active={active_delta}, cont={cont_delta})"
        );
        assert_eq!(
            detect_pc_sleep(before, after, DEFAULT_PC_SLEEP_THRESHOLD_NS),
            None,
            "normal thread::sleep must not trigger PC sleep detection"
        );
    }

    #[test]
    fn detect_pc_sleep_identifies_synthetic_sleep_deltas() {
        let prev = HostClockSample {
            active_mono_ns: 1_000_000_000,
            continuous_mono_ns: 5_000_000_000,
            wall_epoch_ns: 1_700_000_000_000_000_000,
        };

        // 10ms divergence is below 50ms threshold.
        let minor_skew = HostClockSample {
            active_mono_ns: 2_000_000_000,
            continuous_mono_ns: 6_010_000_000,
            wall_epoch_ns: 1_700_000_001_000_000_000,
        };
        assert_eq!(
            sleep_delta_ns(prev, minor_skew),
            10_000_000,
            "expected 10ms synthetic sleep delta"
        );
        assert_eq!(
            detect_pc_sleep(prev, minor_skew, DEFAULT_PC_SLEEP_THRESHOLD_NS),
            None,
            "10ms delta should stay below 50ms threshold"
        );

        // Exact threshold boundary (50ms) does not fire strict >.
        let at_threshold = HostClockSample {
            active_mono_ns: 2_000_000_000,
            continuous_mono_ns: 6_050_000_000,
            wall_epoch_ns: 1_700_000_001_050_000_000,
        };
        assert_eq!(
            detect_pc_sleep(prev, at_threshold, DEFAULT_PC_SLEEP_THRESHOLD_NS),
            None,
            "exact threshold equality should not trigger sleep detection"
        );

        // 2.5s synthetic host sleep must fire and return the exact sleep delta.
        let after_sleep = HostClockSample {
            active_mono_ns: 2_000_000_000,
            continuous_mono_ns: 8_500_000_000,
            wall_epoch_ns: 1_700_000_003_500_000_000,
        };
        assert_eq!(
            detect_pc_sleep(prev, after_sleep, DEFAULT_PC_SLEEP_THRESHOLD_NS),
            Some(2_500_000_000),
            "expected 2.5s synthetic PC sleep to be detected"
        );

        // Negative call-order jitter must never fire.
        let negative_jitter = HostClockSample {
            active_mono_ns: 2_000_005_000,
            continuous_mono_ns: 6_000_000_000,
            wall_epoch_ns: 1_700_000_001_000_000_000,
        };
        assert_eq!(
            detect_pc_sleep(prev, negative_jitter, 0),
            None,
            "negative delta must not trigger sleep detection"
        );
    }
}
