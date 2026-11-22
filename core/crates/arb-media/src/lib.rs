//! GPL-free media container, codec, trim, and playback engine for ARB v2.
//!
//! Provides pure-Rust ISO-BMFF (`avc1` + in-band SPS/PPS) parsing and muxing,
//! a pluggable `VideoCodecBackend` (`OpenH264` universal baseline + macOS
//! `VideoToolbox` hardware default), zero-`elst` frame-accurate head-GOP trim,
//! and GOP-LRU frame caching for interactive scrubbing and filmstrips.

/// Returns the crate identifier alongside the underlying domain schema version.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-media", arb_core::BUNDLE_SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_crate_info_links_arb_core() {
        assert_eq!(crate_info(), ("arb-media", 2));
    }
}
