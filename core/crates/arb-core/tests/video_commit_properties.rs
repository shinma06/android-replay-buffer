//! K15 (W3B) properties: orphan head trim and the WORM video commit invariant.
//!
//! For any frame sequence (empty, orphan P-frames only, orphan head before an IDR, IDR head,
//! carried-in or preroll head), the saved track keeps exactly the decodable suffix, and the
//! resulting declaration always passes the commit check. A file-less declaration never does.

use arb_core::bundle::StreamStatus;
use arb_core::video_commit::{
    VideoCommitViolation, check_video_commit, decodable_start, saved_video_track,
};
use proptest::collection::vec;
use proptest::prelude::{any, prop_assert, prop_assert_eq, proptest};

proptest! {
    #[test]
    fn saved_track_keeps_exactly_the_decodable_suffix(
        keys in vec(any::<bool>(), 0..64),
        head_ref in any::<bool>(),
        mp4_bytes in 1_u64..u64::MAX,
    ) {
        let start = decodable_start(&keys, head_ref);
        let selected = u64::try_from(keys.len()).unwrap();
        let dropped = start.map_or(selected, |s| u64::try_from(s).unwrap());
        let (status, count) = saved_video_track(selected, dropped, start.is_some());
        match start {
            None => {
                prop_assert!(keys.is_empty() || (!head_ref && keys.iter().all(|k| !k)));
                prop_assert_eq!((status, count), (StreamStatus::Absent, 0));
            }
            Some(s) => {
                // The first kept frame is decodable and every dropped frame was an orphan.
                prop_assert!(head_ref && s == 0 || keys[s]);
                prop_assert!(keys[..s].iter().all(|k| !k));
                prop_assert_eq!(count, selected - dropped);
                prop_assert_eq!(
                    status,
                    if s == 0 { StreamStatus::Present } else { StreamStatus::Partial }
                );
            }
        }
        let file = (count > 0).then_some(mp4_bytes);
        prop_assert!(check_video_commit(Some(status), count, count, file).is_ok());
    }

    #[test]
    fn declared_video_without_a_file_is_never_committed(
        status in 0_u8..2,
        frame_count in 0_u64..1_000,
        rows in 0_u64..1_000,
    ) {
        let status = if status == 0 { StreamStatus::Present } else { StreamStatus::Partial };
        prop_assert_eq!(
            check_video_commit(Some(status), frame_count, rows, None),
            Err(VideoCommitViolation::DeclaredWithoutFile)
        );
        prop_assert!(check_video_commit(Some(status), frame_count, rows, Some(0)).is_err());
    }

    #[test]
    fn absent_video_needs_no_rows_and_no_file(
        rows in 0_u64..1_000,
        file in proptest::option::of(0_u64..1_000),
    ) {
        let ok = check_video_commit(Some(StreamStatus::Absent), 0, rows, file).is_ok();
        prop_assert_eq!(ok, rows == 0 && file.is_none());
    }
}

/// The Round 3B forensic shape: 43 orphan P-frames, then 5 GOPs (keyframes at 43, 103, ...).
#[test]
fn round3b_forensic_window_keeps_276_frames() {
    let keys: Vec<bool> = (0..319).map(|i| i >= 43 && (i - 43) % 60 == 0).collect();
    let start = decodable_start(&keys, false).unwrap();
    assert_eq!(start, 43);
    let (status, count) = saved_video_track(319, 43, true);
    assert_eq!((status, count), (StreamStatus::Partial, 276));
    assert_eq!(
        check_video_commit(Some(StreamStatus::Partial), 319, 319, None),
        Err(VideoCommitViolation::DeclaredWithoutFile)
    );
}
