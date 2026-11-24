#![forbid(unsafe_code)]

//! Three-zone persistence layer for Android Replay Buffer (ARB).
//!
//! Owns:
//! - [`ring`] (Zone 1): append-only immutable live ring chunks, durable Phase-1
//!   pinning journals (`save_jobs/<job-id>.json`), and host free-disk budget
//!   guarding (`StoragePressureStop`).
//! - [`bundle_io`] (Zone 2): atomic WORM v2 bundle writer/reader (`manifest.json`,
//!   `video.mp4`, optional `audio.m4a`, read-only `timeline.sqlite`) plus a
//!   strictly read-only adapter for legacy schema 3 (`v0.2.0`) and schema 4
//!   (`develop`, including log-only saves) folders.
//! - [`catalog`] (Zone 3): `SQLite` WAL catalog (`bundles`, `leases`, `save_jobs`,
//!   `edit_sessions`), PID-aware startup crash recovery, and lease-guarded
//!   deferred garbage collection.

pub mod bundle_io;
pub mod catalog;
pub mod config;
pub mod error;
pub mod ring;

pub use crate::config::sha256_hex;
pub use crate::error::StorageError;

/// Returns the crate identifier alongside the underlying domain schema version.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-storage", arb_os::crate_info().1)
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<StorageError>();
    assert_send_sync::<ring::OsDiskSpaceProvider>();
    assert_send_sync::<ring::PinJournal>();
    assert_send_sync::<ring::RingStore>();
    assert_send_sync::<bundle_io::V2BundleWriter>();
    assert_send_sync::<bundle_io::V2BundleReader>();
    assert_send_sync::<bundle_io::LegacyBundleReader>();
    assert_send_sync::<catalog::Catalog>();
    assert_send_sync::<config::ConfigStore>();
};

#[cfg(test)]
mod tests {
    use super::crate_info;

    #[test]
    fn smoke_crate_info_links_domain_and_os() {
        assert_eq!(
            crate_info(),
            ("arb-storage", 2),
            "arb-storage must report schema v2 from arb-os/arb-core"
        );
    }
}
