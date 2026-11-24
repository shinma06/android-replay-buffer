//! OS-specific process detachment for auto-started `arbd` daemons (R3 S16a).
//!
//! Isolates the Unix `process_group(0)` and Windows `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`
//! configuration so callers never branch on the host OS.

use std::process::Command;

#[cfg(unix)]
pub(crate) fn configure_detached_daemon(cmd: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    cmd.process_group(0);
}

#[cfg(windows)]
pub(crate) fn configure_detached_daemon(cmd: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn configure_detached_daemon(_cmd: &mut Command) {}
