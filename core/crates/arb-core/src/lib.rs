//! Pure domain logic for Android Replay Buffer (ARB) v2.
//!
//! This crate has zero OS, ADB, SQLite, IPC, or media-codec dependencies.
//! It owns the dual-layer time model, the shared 3-valued selection engine
//! used by both save and trim, WORM bundle v2 schemas, legacy read-only
//! parsing helpers, and save/lease/ring-budget state machines.

pub mod analytics;
pub mod bundle;
pub mod config;
pub mod device;
pub mod legacy;
pub mod selection;
pub mod state;
pub mod time;

/// Schema version for ARB v2 immutable saved bundles.
pub const BUNDLE_SCHEMA_VERSION: u32 = 2;

/// Returns the crate identifier and bundle schema version for smoke diagnostics.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-core", BUNDLE_SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::{BUNDLE_SCHEMA_VERSION, crate_info};

    #[test]
    fn smoke_crate_info_reports_schema_v2() {
        assert_eq!(
            crate_info(),
            ("arb-core", BUNDLE_SCHEMA_VERSION),
            "crate_info must report arb-core with schema v2"
        );
    }
}
