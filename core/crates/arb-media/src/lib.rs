//! GPL-free media container, codec, trim, and playback engine for ARB v2.
//!
//! Provides:
//! - [`mp4`]: Pure-Rust ISO-BMFF parser, frame/GOP indexer, raw H.264 muxer, and faststart
//!   single-`stsd` `avc1` + in-band SPS/PPS splice muxer with strictly zero `edts`/`elst` boxes.
//! - [`codec`]: Trait-abstracted [`codec::VideoCodecBackend`] with universal [`codec::OpenH264Backend`]
//!   (`BSD-2-Clause`) and macOS hardware [`codec::VideoToolboxBackend`] (direct C FFI, zero crates).
//! - [`trim`]: Frame-accurate integer `frame_index` trim and PNG frame export with `tEXt` metadata.
//! - [`playback`]: External [`playback::FilmstripCache`], bounded [`playback::GopLruCache`],
//!   latest-wins [`playback::ScrubController`], and [`playback::PresentationClock`].

pub mod codec;
pub mod error;
pub mod mp4;
pub mod playback;
pub mod trim;

pub use crate::error::MediaError;

/// Returns the crate identifier alongside the underlying domain schema version.
pub const fn crate_info() -> (&'static str, u32) {
    ("arb-media", arb_core::BUNDLE_SCHEMA_VERSION)
}

#[cfg(test)]
mod tests {
    use super::crate_info;

    #[test]
    fn smoke_crate_info_links_arb_core() {
        assert_eq!(
            crate_info(),
            ("arb-media", 2),
            "crate_info must link arb-media with arb-core schema v2"
        );
    }
}
