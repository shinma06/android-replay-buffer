//! K15 (W3B): Kani harnesses for the orphan-head trim and the WORM video commit invariant
//! (`crate::video_commit`).
//!
//! The oracles are written here independently of the production code, so a mutation of the
//! keyframe search, the status choice or the commit check is caught.

use crate::bundle::StreamStatus;
use crate::video_commit::{check_video_commit, decodable_start, saved_video_track};

fn any_status() -> Option<StreamStatus> {
    match kani::any::<u8>() % 5 {
        0 => Some(StreamStatus::Present),
        1 => Some(StreamStatus::Partial),
        2 => Some(StreamStatus::Absent),
        3 => Some(StreamStatus::Excluded),
        _ => None,
    }
}

#[kani::proof]
#[kani::unwind(6)]
pub fn k15_decodable_start_is_the_first_decodable_frame() {
    let keys: [bool; 4] = kani::any();
    let len: usize = kani::any();
    kani::assume(len <= keys.len());
    let head_ref: bool = kani::any();
    let frames = &keys[..len];
    let got = decodable_start(frames, head_ref);
    match got {
        None => {
            assert!(
                len == 0 || (!head_ref && !frames.iter().any(|&k| k)),
                "None only when nothing is decodable"
            );
        }
        Some(i) => {
            assert!(i < len);
            if head_ref {
                assert!(i == 0, "a referenced head is kept");
            } else {
                assert!(frames[i], "the kept head is a keyframe");
                let mut j = 0;
                while j < i {
                    assert!(!frames[j], "only orphan frames are dropped");
                    j += 1;
                }
            }
        }
    }
    kani::cover!(got.is_some_and(|i| i > 0), "orphan head dropped");
    kani::cover!(got.is_none() && len > 0, "no decodable frame");
}

#[kani::proof]
pub fn k15_saved_track_always_passes_the_commit_check() {
    let selected = u64::from(kani::any::<u8>());
    let dropped = u64::from(kani::any::<u8>());
    kani::assume(dropped <= selected);
    let written: bool = kani::any();
    let mp4_bytes: u64 = kani::any();
    kani::assume(mp4_bytes > 0);
    let (status, count) = saved_video_track(selected, dropped, written);
    assert!(count <= selected - dropped);
    assert!((status == StreamStatus::Absent) == (count == 0));
    assert!(status != StreamStatus::Partial || dropped > 0);
    assert!(status != StreamStatus::Present || dropped == 0);
    // Only a written track may keep frames; the rows written equal the declared count.
    let file = (count > 0).then_some(mp4_bytes);
    assert!(written || count == 0, "no frames without a written file");
    assert!(
        check_video_commit(Some(status), count, count, file).is_ok(),
        "the saved track passes the commit check"
    );
    kani::cover!(status == StreamStatus::Partial, "partial suffix written");
    kani::cover!(
        status == StreamStatus::Absent && selected > 0,
        "frames dropped to absent"
    );
}

#[kani::proof]
pub fn k15_commit_check_matches_the_oracle() {
    let status = any_status();
    let frame_count = u64::from(kani::any::<u8>());
    let rows = u64::from(kani::any::<u8>());
    let mp4_bytes: Option<u64> = if kani::any() {
        Some(u64::from(kani::any::<u8>()))
    } else {
        None
    };
    let declares = frame_count > 0
        || matches!(
            status,
            Some(StreamStatus::Present) | Some(StreamStatus::Partial)
        );
    let file_ok = matches!(mp4_bytes, Some(b) if b > 0);
    let oracle = if declares {
        file_ok && frame_count > 0 && rows == frame_count
    } else {
        mp4_bytes.is_none() && rows == 0
    };
    let ok = check_video_commit(status, frame_count, rows, mp4_bytes).is_ok();
    assert!(ok == oracle, "commit check matches the oracle");
    // No accepted bundle declares video without a non-empty file.
    assert!(!ok || !declares || file_ok);
    kani::cover!(!ok && declares && mp4_bytes.is_none(), "video without file");
}
