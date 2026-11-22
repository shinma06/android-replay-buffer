//! Three-zone persistence layer for Android Replay Buffer (ARB) v2.
//!
//! Owns Zone 1 (append-only live ring chunks + Phase-1 pinning + free-disk
//! guard), Zone 2 (atomic WORM v2 bundle writer + read-only v2 and legacy
//! schema 3/4 bundle readers), and Zone 3 (SQLite WAL catalog, leases, and
//! deferred garbage collection).

/// Returns the crate identifier alongside the underlying domain schema version.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-storage", arb_os::crate_info().1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_crate_info_links_domain_and_os() {
        assert_eq!(crate_info(), ("arb-storage", 2));
    }
}
