//! Minimal OS signal / console-close hook for graceful `arbd` binary shutdown (R3 S11).
//!
//! Uses only standard C / Win32 runtime entry points already linked into every Rust binary so
//! no extra third-party crates are needed. Every `unsafe` block performs a single FFI call and
//! installs an async-signal-safe handler that only sets an [`AtomicBool`].

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN_SIGNALLED: AtomicBool = AtomicBool::new(false);

/// Returns `true` once SIGINT / SIGTERM / SIGHUP (Unix) or a console close event (Windows)
/// has been delivered to this process.
pub(crate) fn is_signalled() -> bool {
    SHUTDOWN_SIGNALLED.load(Ordering::Acquire)
}

#[cfg(unix)]
mod imp {
    use std::ffi::c_int;
    use std::sync::atomic::Ordering;

    use super::SHUTDOWN_SIGNALLED;

    // Standard POSIX signal numbers shared by Linux, macOS, and BSDs.
    const SIGHUP: c_int = 1;
    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;

    type SignalHandler = extern "C" fn(c_int);

    unsafe extern "C" {
        fn signal(sig: c_int, handler: SignalHandler) -> usize;
    }

    extern "C" fn handle_unix_signal(_sig: c_int) {
        SHUTDOWN_SIGNALLED.store(true, Ordering::Release);
    }

    pub(super) fn install() {
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `handle_unix_signal` has `extern "C" fn(c_int)` ABI and only performs a
            // lock-free atomic store, which is async-signal-safe under POSIX.
            let _ = unsafe { signal(sig, handle_unix_signal) };
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::Ordering;

    use super::SHUTDOWN_SIGNALLED;

    type ConsoleHandler = extern "system" fn(u32) -> i32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<ConsoleHandler>, add: i32) -> i32;
    }

    extern "system" fn handle_win_ctrl(_ctrl_type: u32) -> i32 {
        SHUTDOWN_SIGNALLED.store(true, Ordering::Release);
        1
    }

    pub(super) fn install() {
        // SAFETY: `handle_win_ctrl` has the Win32 `PHANDLER_ROUTINE` ABI and only performs an
        // atomic store before returning `TRUE` (1) to request clean shutdown.
        let _ = unsafe { SetConsoleCtrlHandler(Some(handle_win_ctrl), 1) };
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub(super) fn install() {}
}

/// Installs OS termination signal handlers so `arbd` stops capture, cleans remote agents, and
/// removes SHM files cleanly instead of dying abruptly.
pub(crate) fn install_shutdown_handlers() {
    imp::install();
}
