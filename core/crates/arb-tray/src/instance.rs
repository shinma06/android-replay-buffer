//! Single-instance guard for the `arb-tray` process.
//!
//! Enabling the tray from several Android Studio windows, across IDE restarts, or from a login
//! item must never produce duplicate menu-bar / system-tray icons. The first `arb-tray` holds an
//! exclusive advisory lock on `<runtime_dir>/arb-tray.lock` (the same
//! [`arb_os::fs::SingleInstanceLock`] mechanism `arbd` uses for `arbd.lock`) for its whole
//! lifetime; later launches observe the contended lock and exit cleanly. The OS releases the
//! lock automatically if the holder crashes, so a stale lock file never blocks a restart.

use arb_os::error::OsError;
use arb_os::fs::SingleInstanceLock;
use arb_os::paths::ArbPaths;

/// Outcome of [`acquire_tray_instance`].
#[derive(Debug)]
#[non_exhaustive]
pub enum TrayInstance {
    /// This process is the only tray instance; keep the guard alive until exit.
    Primary(SingleInstanceLock),
    /// Another live `arb-tray` already owns the lock; this process should exit with success.
    AlreadyRunning,
}

/// Tries to become the single `arb-tray` instance for `paths.runtime_dir`.
///
/// # Errors
///
/// Returns [`OsError`] when the lock file cannot be created or locked for a reason other than
/// another instance holding it.
pub fn acquire_tray_instance(paths: &ArbPaths) -> Result<TrayInstance, OsError> {
    match SingleInstanceLock::try_acquire(paths.tray_lock_file()) {
        Ok(lock) => Ok(TrayInstance::Primary(lock)),
        Err(OsError::LockContended { .. }) => Ok(TrayInstance::AlreadyRunning),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{TrayInstance, acquire_tray_instance};
    use arb_os::paths::ArbPaths;

    #[test]
    fn second_tray_instance_is_rejected_until_the_first_exits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = ArbPaths {
            data_dir: tmp.path().join("data"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: tmp.path().join("run"),
        };

        let first = acquire_tray_instance(&paths).expect("first acquire");
        assert!(
            matches!(first, TrayInstance::Primary(_)),
            "first tray must become primary"
        );
        assert!(
            paths.tray_lock_file().is_file(),
            "lock file must live at <runtime_dir>/arb-tray.lock"
        );

        let second = acquire_tray_instance(&paths).expect("second acquire");
        assert!(
            matches!(second, TrayInstance::AlreadyRunning),
            "a second tray must see the live instance and back off"
        );

        drop(first);
        let third = acquire_tray_instance(&paths).expect("third acquire");
        assert!(
            matches!(third, TrayInstance::Primary(_)),
            "the lock must be reusable once the first tray exits"
        );
    }
}
