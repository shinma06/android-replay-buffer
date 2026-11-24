//! GPL-free media container, codec, trim, and playback engine for ARB.
//!
//! Provides:
//! - [`audio`]: Pure-Rust ISO-BMFF AAC-LC (`audio.m4a`) faststart muxer, `AudioSpecificConfig`
//!   parser/builder, `Mp4AudioTrack` indexer, and zero-`elst` time-range trimmer.
//! - [`mp4`]: Pure-Rust ISO-BMFF parser, frame/GOP indexer, raw H.264 muxer, and faststart
//!   single-`stsd` `avc1` + in-band SPS/PPS splice muxer with strictly zero `edts`/`elst` boxes.
//! - [`mux_av`]: Single-file H.264 + AAC-LC faststart zero-`elst` `share.mp4` muxer
//!   ([`mux_share_mp4`]) with honest `audio_lead_in_offset_ns` handling.
//! - [`contact_sheet`]: Zero-dependency AI contact-sheet, tap-crop PNG renderer and deterministic
//!   frame selector (`arb_inspect_frames`).
//! - [`codec`]: Trait-abstracted [`codec::VideoCodecBackend`] with universal [`codec::OpenH264Backend`]
//!   (`BSD-2-Clause`) and macOS hardware `codec::VideoToolboxBackend` (direct C FFI, zero crates).
//! - [`trim`]: Frame-accurate integer `frame_index` trim and PNG frame export with `tEXt` metadata.
//! - [`playback`]: External [`playback::FilmstripCache`], bounded [`playback::GopLruCache`],
//!   latest-wins [`playback::ScrubController`], and [`playback::PresentationClock`].

pub mod audio;
pub mod audio_decode;
pub mod burn_in;
pub mod codec;
pub mod contact_sheet;
pub mod error;
pub mod mp4;
pub mod mux_av;
pub use output::copy_new;
mod output;
pub mod playback;
pub mod trim;

pub use crate::audio::{
    AAC_LC_OBJECT_TYPE, AAC_LC_SAMPLES_PER_FRAME, AAC_LC_SHORT_SAMPLES_PER_FRAME, AacMuxInput,
    AacPacket, AudioMuxReport, AudioSpecificConfig, AudioTrimReport, IndexedAudioPacket,
    Mp4AudioTrack, build_aac_lc_audio_specific_config, build_mp4a_sample_entry, mux_aac_to_m4a,
    parse_audio_specific_config, strip_adts_header, trim_audio_m4a,
};
pub use crate::audio_decode::{
    AudioDecodeReport, WavHeaderInfo, decode_audio_m4a_to_wav, inspect_wav_header,
};
pub use crate::burn_in::{
    BurnInDisplayGeometry, BurnInReport, BurnInTouchEvent, DEFAULT_BURN_IN_KEYFRAME_INTERVAL,
    SWIPE_FADE_DURATION_NS, TAP_FADE_DURATION_NS, burn_in_taps_video,
    composite_touch_overlays_rgb24,
};
pub use crate::contact_sheet::{
    ContactSheetOptions, ContactSheetTile, MAX_CONTACT_SHEET_TILES, TapCropOptions, TapCropResult,
    render_contact_sheet_png, render_single_frame_png, render_tap_crop_png,
    select_contact_sheet_indices,
};
pub use crate::error::MediaError;
pub use crate::mux_av::{AvMuxReport, mux_share_mp4};

#[cfg(test)]
mod r2_regression_tests;

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
