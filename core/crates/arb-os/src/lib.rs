//! Cross-platform OS abstraction layer for Android Replay Buffer (ARB) v2.
//!
//! All Linux, macOS, and Windows platform differences (dual active/continuous
//! monotonic clocks, atomic staging commits, free-disk guards, Unix Domain
//! Sockets / Windows Named Pipes, and shared-memory frame rings) live here.

/// Returns the crate identifier alongside the underlying domain schema version.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-os", arb_core::BUNDLE_SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_crate_info_links_arb_core() {
        assert_eq!(crate_info(), ("arb-os", 2));
    }
}
