//! Error types for the `arb-media` crate.

use std::io;

use thiserror::Error;

/// Error returned by media container, codec, trim, PNG export, and playback operations.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum MediaError {
    /// Underlying filesystem or I/O error.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),

    /// Malformed, truncated, or unsupported ISO-BMFF container structure.
    #[error("invalid mp4 container: {0}")]
    InvalidContainer(String),

    /// Operation requires a media feature that is explicitly rejected to avoid silent corruption.
    #[error("unsupported media feature: {0}")]
    UnsupportedFeature(String),

    /// Requested single frame index is outside the track's `[0, frame_count)` bounds.
    #[error("frame index {frame_index} out of range (frame count {frame_count})")]
    FrameOutOfRange {
        /// Requested zero-based frame index.
        frame_index: u64,
        /// Total frames available in the video track.
        frame_count: u64,
    },

    /// Requested inclusive frame range `[in_frame_index, out_frame_index]` is invalid.
    #[error(
        "invalid frame range [{in_frame_index}..={out_frame_index}] for track with {frame_count} frames"
    )]
    InvalidFrameRange {
        /// Inclusive starting frame index.
        in_frame_index: u64,
        /// Inclusive ending frame index.
        out_frame_index: u64,
        /// Total frames available in the video track.
        frame_count: u64,
    },

    /// Video decoder or encoder failure.
    #[error("codec error: {0}")]
    Codec(String),

    /// PNG encoding or decoding failure.
    #[error("png error: {0}")]
    Png(String),

    /// External filmstrip cache file is missing, incompatible, or corrupted.
    #[error("corrupt filmstrip cache: {0}")]
    CacheCorrupt(String),

    /// Invalid cache identifier or caller-supplied configuration parameter.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}
