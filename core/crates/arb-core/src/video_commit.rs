//! WORM video commit consistency (K15).
//!
//! A saved window can start with P-frames whose IDR was already evicted from the ring. No
//! decoder can show them, so they are dropped up to the first decodable keyframe and the
//! survivors are written as an honest `Partial` track. A bundle may declare a video track only
//! when `video.mp4` was written and its sample count equals the timeline rows; otherwise the
//! track is committed as `Absent` with no frames.

use crate::bundle::StreamStatus;

/// Index of the first selected frame a decoder can start from.
///
/// `head_has_reference` is true when the head frame is decodable without a keyframe of its own
/// (preroll packets from its IDR, or a carried-in frame re-encoded as an IDR). Otherwise the
/// result is the first keyframe. `None` means no frame can be decoded.
#[must_use]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "one keyframe flag per frame plus the head's reference, as recorded"
)]
pub fn decodable_start(keyframes: &[bool], head_has_reference: bool) -> Option<usize> {
    if keyframes.is_empty() {
        return None;
    }
    if head_has_reference {
        return Some(0);
    }
    keyframes.iter().position(|&k| k)
}

/// Stream status and frame count of a saved video track.
///
/// `written` is true only when `video.mp4` was produced from `selected - dropped_head` frames.
/// A track without a file or without frames is `Absent` with zero frames; a track that lost
/// its head is `Partial`; a complete track is `Present`.
#[must_use]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "`written` is the single mux outcome the status depends on"
)]
pub fn saved_video_track(selected: u64, dropped_head: u64, written: bool) -> (StreamStatus, u64) {
    let kept = selected.saturating_sub(dropped_head);
    if !written || kept == 0 {
        (StreamStatus::Absent, 0)
    } else if dropped_head > 0 {
        (StreamStatus::Partial, kept)
    } else {
        (StreamStatus::Present, kept)
    }
}

/// A committed bundle whose video declaration disagrees with its file or timeline rows.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCommitViolation {
    /// The manifest declares video, but `video.mp4` is missing or empty.
    DeclaredWithoutFile,
    /// The manifest declares video with zero frames.
    DeclaredWithoutFrames,
    /// The timeline row count differs from `streams.video.frame_count`.
    RowCountMismatch,
    /// `video.mp4` or frame rows exist, but the manifest declares no video.
    UndeclaredVideo,
}

/// Checks the video invariant every committed WORM bundle must satisfy.
///
/// The manifest declares video when `status` is neither `Absent` nor `Excluded`, or when
/// `frame_count > 0`. A declared track needs a non-empty `video.mp4`, a positive
/// `frame_count`, and exactly `frame_count` timeline rows. An undeclared track needs no file,
/// no rows and `frame_count == 0`. `status: None` means the manifest has no video entry.
///
/// # Errors
///
/// Returns the first [`VideoCommitViolation`] found.
pub fn check_video_commit(
    status: Option<StreamStatus>,
    frame_count: u64,
    timeline_rows: u64,
    mp4_bytes: Option<u64>,
) -> Result<(), VideoCommitViolation> {
    let declared = frame_count > 0
        || status.is_some_and(|s| !matches!(s, StreamStatus::Absent | StreamStatus::Excluded));
    if declared {
        if mp4_bytes.is_none_or(|b| b == 0) {
            return Err(VideoCommitViolation::DeclaredWithoutFile);
        }
        if frame_count == 0 {
            return Err(VideoCommitViolation::DeclaredWithoutFrames);
        }
        if timeline_rows != frame_count {
            return Err(VideoCommitViolation::RowCountMismatch);
        }
        Ok(())
    } else if mp4_bytes.is_some() || timeline_rows > 0 {
        Err(VideoCommitViolation::UndeclaredVideo)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_head_is_dropped_to_the_first_keyframe() {
        let mut keys = vec![false; 43];
        keys.extend([true, false, false]);
        assert_eq!(decodable_start(&keys, false), Some(43));
        assert_eq!(decodable_start(&keys, true), Some(0));
        assert_eq!(decodable_start(&[false; 5], false), None);
        assert_eq!(decodable_start(&[], true), None);
    }

    #[test]
    fn saved_track_status_follows_the_written_file() {
        assert_eq!(
            saved_video_track(319, 43, true),
            (StreamStatus::Partial, 276)
        );
        assert_eq!(saved_video_track(30, 0, true), (StreamStatus::Present, 30));
        assert_eq!(saved_video_track(319, 0, false), (StreamStatus::Absent, 0));
        assert_eq!(saved_video_track(5, 5, true), (StreamStatus::Absent, 0));
    }

    #[test]
    fn commit_check_rejects_video_without_file() {
        assert_eq!(
            check_video_commit(Some(StreamStatus::Partial), 319, 319, None),
            Err(VideoCommitViolation::DeclaredWithoutFile)
        );
        assert_eq!(
            check_video_commit(Some(StreamStatus::Absent), 0, 3, None),
            Err(VideoCommitViolation::UndeclaredVideo)
        );
        assert_eq!(
            check_video_commit(Some(StreamStatus::Present), 3, 2, Some(10)),
            Err(VideoCommitViolation::RowCountMismatch)
        );
        assert_eq!(
            check_video_commit(Some(StreamStatus::Present), 0, 0, Some(10)),
            Err(VideoCommitViolation::DeclaredWithoutFrames)
        );
        assert!(check_video_commit(Some(StreamStatus::Partial), 276, 276, Some(1)).is_ok());
        assert!(check_video_commit(Some(StreamStatus::Absent), 0, 0, None).is_ok());
        assert!(check_video_commit(None, 0, 0, None).is_ok());
    }
}
