//! Cross-platform OS abstraction layer for Android Replay Buffer (ARB).
//!
//! All Linux, macOS, and Windows platform differences live inside this crate:
//! - [`clock`]: sleep-aware dual host monotonic + wall clock sampling,
//! - [`fs`]: atomic staging/commit directories, disk-space queries, `fsync`,
//!   single-instance locks, and PID liveness checks,
//! - [`ipc`]: local control-plane IPC (Unix Domain Sockets / Windows Named Pipes)
//!   with length-bounded NDJSON framing,
//! - [`parent_watch`]: parent-bound lifecycle (exit when the parent closes our stdin pipe),
//! - [`shm`]: triple-buffered shared-memory frame ring for high-FPS video delivery,
//! - [`paths`]: standard OS data, cache, and runtime directory resolution.

pub mod clock;
pub mod error;
pub mod fs;
pub mod ipc;
pub mod parent_watch;
pub mod paths;
mod process_os;
pub mod rpc;
pub mod shm;
pub mod signal;

pub use crate::error::OsError;

/// Returns the crate identifier alongside the underlying domain schema version.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-os", arb_core::BUNDLE_SCHEMA_VERSION)
}

const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<OsError>();
    assert_send_sync::<clock::HostClockSample>();
    assert_send_sync::<fs::DiskSpaceInfo>();
    assert_send_sync::<fs::AtomicStagingDir>();
    assert_send_sync::<fs::SingleInstanceLock>();
    assert_send::<ipc::LocalIpcListener>();
    assert_send::<ipc::LocalIpcStream>();
    assert_send_sync::<shm::FrameSlotMeta>();
    assert_send_sync::<shm::FrameDescriptor>();
    assert_send_sync::<shm::ShmFrameRingWriter>();
    assert_send_sync::<shm::ShmFrameRingReader>();
    assert_send_sync::<paths::ArbPaths>();
};

#[cfg(test)]
mod tests {
    use super::crate_info;

    #[test]
    fn smoke_crate_info_links_arb_core() {
        assert_eq!(
            crate_info(),
            ("arb-os", 2),
            "arb-os must link arb-core schema v2"
        );
    }
}
