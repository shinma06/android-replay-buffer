//! Issues #564 / #570: stream coverage ledger, static-screen carry-in, tail hold, log coverage
//! and the one-to-one warning projection, exercised end to end through Phase 1 / Phase 2.
//!
//! Each scenario feeds real `OpenH264` frames and beacons into a [`CapturePipeline`], saves, and
//! reads the published bundle back. The `*_without_ledger_*` halves rerun the same pinned job
//! with the coverage snapshot removed (the job format written before the ledger existed), which
//! reproduces the original failures: `video_frame_count == 0` for a static screen and
//! `log_shortfall == true` for a healthy logcat stream with quiet edges.

#![allow(
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "end-to-end save scenarios read best unsplit"
)]

use std::path::Path;
use std::sync::Arc;

use arb_adb::mock::CaptureEvent;
use arb_adb::scrcpy::{VideoPacket, VideoPacketKind};
use arb_core::selection::SelectionPolicy;
use arb_core::state::RingBudgetPolicy;
use arb_core::time::{DiscontinuityEvent, DiscontinuityKind, StreamKind};
use arb_media::codec::{
    DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat, VideoCodecBackend as _,
};
use arb_os::OsError;
use arb_os::fs::DiskSpaceInfo;
use arb_storage::catalog::Catalog;
use arb_storage::ring::DiskSpaceProvider;
use tempfile::TempDir;

use super::{
    CapturePipeline, SaveMode, SharedDiskSpaceProvider, TailCompleteness, phase2_materialize,
    read_job_params, write_job_params,
};
use crate::protocol::SaveOutcomeResponse;

const SEC: i64 = 1_000_000_000;
const BOOT: &str = "boot-t";

#[derive(Debug)]
struct PlentyOfDisk;

impl DiskSpaceProvider for PlentyOfDisk {
    fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
        Ok(DiskSpaceInfo {
            total_bytes: 1 << 40,
            available_bytes: 1 << 40,
        })
    }
}

fn pipeline(tmp: &TempDir) -> CapturePipeline {
    let mut cap = CapturePipeline::open(
        tmp.path(),
        RingBudgetPolicy::default(),
        SharedDiskSpaceProvider::new(Arc::new(PlentyOfDisk)),
    )
    .unwrap();
    cap.begin_session("A");
    cap
}

fn heartbeat(t: i64) -> CaptureEvent {
    CaptureEvent::Heartbeat {
        pc_cont_ns: t,
        boot_id: BOOT.into(),
        payload: arb_adb::wire::HeartbeatPayload {
            boottime_ns: t,
            monotonic_ns: t,
            realtime_ns: t,
            // The companion reports its logcat reader as running, as on a real device.
            streams: vec![arb_adb::wire::StreamHealthEntry {
                stream_id: arb_adb::wire::StreamId::Logcat,
                state: arb_adb::wire::StreamLifecycleState::Running,
                restart_count: 0,
                emitted_seq: 0,
                last_pts_ns: 0,
            }],
        },
    }
}

fn log_row(t: i64, msg: &str) -> CaptureEvent {
    let mut bytes = vec![4_u8];
    bytes.extend_from_slice(b"Tag");
    bytes.push(0);
    bytes.extend_from_slice(msg.as_bytes());
    bytes.push(0);
    CaptureEvent::LogRecord {
        pc_cont_ns: t,
        boot_id: BOOT.into(),
        flags: arb_adb::wire::FrameFlags::default(),
        payload: arb_adb::wire::LogRecordPayload {
            device_read_boottime_ns: t,
            log_sec: u32::try_from(t / SEC).unwrap(),
            log_nsec: u32::try_from(t % SEC).unwrap(),
            pid: 1,
            tid: 1,
            lid: 0,
            uid: 10_001,
            hdr_size: 28,
            priority: 4,
            record_flags: 0,
            payload: bytes,
        },
    }
}

/// Heartbeats every 300 ms over `[from, to]` (inclusive when aligned).
fn beacons(cap: &mut CapturePipeline, from: i64, to: i64) {
    let mut t = from;
    while t <= to {
        cap.ingest_event(heartbeat(t)).unwrap();
        t += 300_000_000;
    }
}

/// Real H.264: one IDR and `count - 1` deltas, 40 ms apart, starting at `start`.
fn frames(cap: &mut CapturePipeline, start: i64, count: usize, luma: u8) {
    cap.ingest_event(CaptureEvent::VideoSession {
        pc_cont_ns: start,
        boot_id: BOOT.into(),
        meta: arb_adb::scrcpy::VideoSessionMeta {
            header_tag: 0,
            width: 64,
            height: 64,
        },
    })
    .unwrap();
    let decoded: Vec<DecodedFrame> = (0..count)
        .map(|i| DecodedFrame {
            frame_index: u64::try_from(i).unwrap(),
            width: 64,
            height: 64,
            format: PixelFormat::Yuv420p,
            data: vec![luma + u8::try_from(i * 10).unwrap(); 64 * 64 * 3 / 2],
        })
        .collect();
    let packets = OpenH264Backend::new()
        .encode_yuv420p_sequence(&decoded, 30, 40_000_000, EncodeParams::default())
        .unwrap();
    let mut t = start;
    for pkt in packets {
        let mut config = Vec::new();
        let mut picture = Vec::new();
        for nal in arb_media::mp4::split_annexb_nals(&pkt.annex_b) {
            let out = if matches!(nal[0] & 31, 7 | 8) {
                &mut config
            } else {
                &mut picture
            };
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        let mut push = |kind, data| {
            cap.ingest_event(CaptureEvent::VideoFrame {
                pc_cont_ns: t,
                boot_id: BOOT.into(),
                packet: VideoPacket {
                    kind,
                    data,
                    pts_us: u64::try_from(t / 1000).unwrap(),
                },
            })
            .unwrap();
        };
        if !config.is_empty() {
            push(VideoPacketKind::Config, config);
        }
        push(
            if pkt.is_keyframe {
                VideoPacketKind::Keyframe
            } else {
                VideoPacketKind::DeltaFrame
            },
            picture,
        );
        t += 40_000_000;
    }
}

fn video_gap(t: i64) -> CaptureEvent {
    CaptureEvent::Discontinuity(DiscontinuityEvent {
        kind: DiscontinuityKind::StreamGap,
        stream: Some(StreamKind::Video),
        boot_id: Some(BOOT.into()),
        start_boottime_ns: None,
        end_boottime_ns: None,
        pc_cont_start_ns: t,
        pc_cont_end_ns: None,
        delta_ns: None,
        detail: Some("video_state_not_running".into()),
    })
}

fn connection_lost(t: i64) -> CaptureEvent {
    CaptureEvent::Discontinuity(DiscontinuityEvent {
        kind: DiscontinuityKind::ConnectionLost,
        stream: None,
        boot_id: Some(BOOT.into()),
        start_boottime_ns: None,
        end_boottime_ns: None,
        pc_cont_start_ns: t,
        pc_cont_end_ns: None,
        delta_ns: None,
        detail: Some("usb unplugged".into()),
    })
}

/// How the pinned job is materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ledger {
    /// As pinned: the coverage snapshot is used.
    Keep,
    /// Rewritten as a job pinned before the ledger existed (fail-first reproduction).
    Strip,
}

/// Saves the last `secs` seconds.
fn save(
    cap: &mut CapturePipeline,
    tmp: &TempDir,
    name: &str,
    secs: u64,
    mode: SaveMode,
    ledger: Ledger,
) -> (SaveOutcomeResponse, Vec<String>) {
    let out = tmp.path().join(name);
    let catalog = Catalog::open(tmp.path().join(format!("{name}-cat")), &out).unwrap();
    let pending = cap
        .phase1_pin_with_options(
            secs,
            &out,
            SelectionPolicy::default(),
            mode,
            super::SaveTriggerKind::Manual,
        )
        .unwrap();
    let pinned = pending.pinned_chunk_ids().to_vec();
    let (job, plan) = cap
        .phase1_commit(&catalog, pending, TailCompleteness::Complete)
        .unwrap();
    if ledger == Ledger::Strip {
        let mut params = read_job_params(tmp.path(), &job.job.job_id).unwrap();
        assert!(params.coverage.is_some(), "Phase 1 persists the snapshot");
        params.coverage = None;
        write_job_params(tmp.path(), &job.job.job_id, &params).unwrap();
    }
    let outcome = phase2_materialize(&plan, &catalog, &job.job.job_id).unwrap();
    (outcome, pinned)
}

fn assert_warnings_one_to_one(outcome: &SaveOutcomeResponse) {
    let e = &outcome.evidence;
    let codes = e.warning_codes.clone().unwrap();
    let messages = e.warnings.clone().unwrap();
    let details = e.warning_details.clone().unwrap();
    assert_eq!(codes.len(), messages.len(), "{codes:?} vs {messages:?}");
    assert_eq!(details.len(), codes.len());
    for ((d, c), m) in details.iter().zip(&codes).zip(&messages) {
        assert_eq!(&d.code, c);
        assert_eq!(&d.message, m);
        assert_ne!(m, "");
    }
}

fn manifest_carried_in(outcome: &SaveOutcomeResponse) -> Option<arb_core::bundle::CarriedInFrame> {
    let reader =
        arb_storage::bundle_io::open_bundle(outcome.bundle_path.as_ref().unwrap()).unwrap();
    reader
        .manifest_summary()
        .streams
        .video
        .as_ref()
        .and_then(|v| v.carried_in.clone())
}

/// #564: 30 s of a static screen (0 frames in the window, beacons alive) saves one carried-in
/// frame held for the whole window. The same job without the ledger reproduces the bug.
#[test]
fn static_screen_30s_carries_in_one_frame_held_for_the_window() {
    let tmp = TempDir::new().unwrap();
    let mut cap = pipeline(&tmp);
    beacons(&mut cap, 0, 900_000_000);
    cap.ingest_event(log_row(SEC, "screen settled")).unwrap();
    frames(&mut cap, SEC, 3, 40);
    beacons(&mut cap, 1_300_000_000, 40 * SEC);

    // Fail-first: the job as written before the ledger (no coverage snapshot).
    let (old, _) = save(&mut cap, &tmp, "old", 30, SaveMode::Full, Ledger::Strip);
    assert_eq!(old.evidence.video_frame_count, Some(0), "#564 reproduced");
    assert_eq!(old.evidence.video_shortfall, Some(true));
    assert_eq!(old.evidence.log_shortfall, Some(true), "#570 reproduced");
    assert_eq!(old.evidence.carried_in_frame, Some(false));
    assert_warnings_one_to_one(&old);

    let (new, pinned) = save(&mut cap, &tmp, "new", 30, SaveMode::Full, Ledger::Keep);
    // The pinned set reaches back to the GOP of the last displayed frame (IDR .. f_prev).
    assert!(
        pinned.iter().any(|id| cap
            .chunk_contents
            .get(id)
            .is_some_and(|c| c.picture == Some(VideoPacketKind::Keyframe))),
        "pinned chunks must include the IDR before the window: {pinned:?}"
    );
    let e = &new.evidence;
    assert_eq!(e.video_frame_count, Some(1));
    assert_eq!(e.carried_in_frame, Some(true));
    assert_eq!(e.video_shortfall, Some(false));
    let video_ms = e.actual_video_duration_ms.unwrap();
    assert!((29_950..=30_000).contains(&video_ms), "video {video_ms} ms");
    assert!(e.static_hold_ms.unwrap() >= 29_950);
    assert_eq!(
        e.info_codes.as_deref(),
        Some(&["video_static_hold".to_owned()][..])
    );
    assert!(e.video_coverage_ms.unwrap() >= 29_900);
    // Logcat stayed running: no rows in the window is not a log shortfall.
    assert_eq!(e.actual_log_duration_ms, Some(0));
    assert!(e.log_coverage_ms.unwrap() >= 29_900);
    assert_eq!(e.log_shortfall, Some(false));
    assert_eq!(e.window_shortfall, Some(false));
    assert!(
        !e.warning_codes
            .as_ref()
            .unwrap()
            .iter()
            .any(|c| c.ends_with("shortfall"))
    );
    assert_warnings_one_to_one(&new);

    // WORM honesty: the record keeps its capture time; the manifest records the display start.
    let carried = manifest_carried_in(&new).expect("carried_in recorded");
    assert_eq!(carried.frame_index, 0);
    assert_eq!(carried.display_start_boottime_ns, 10 * SEC);
    assert!(carried.captured_boottime_ns < 10 * SEC);
    let reader = arb_storage::bundle_io::open_bundle(new.bundle_path.as_ref().unwrap()).unwrap();
    let frames = reader.frames().unwrap();
    assert_eq!(frames.len(), 1);
    // The stored record keeps its capture time; only readers move its display start to W0.
    assert_eq!(
        arb_core::bundle::frame_display_start_ns(&frames[0], None),
        Some(carried.captured_boottime_ns)
    );
    assert_eq!(
        arb_core::bundle::frame_display_start_ns(&frames[0], Some(&carried)),
        Some(10 * SEC)
    );
    // The MP4 is one decodable sample, no edit list, held ~30 s.
    let video = reader.video_path().unwrap();
    let atoms = arb_media::mp4::inspect_mp4_atoms(&video).unwrap();
    assert_eq!(atoms.elst_count, 0);
    let track = arb_media::mp4::Mp4VideoTrack::open(&video).unwrap();
    assert_eq!(track.frame_count(), 1);
    let tail = track.last_presentation_duration_ns().unwrap().unwrap();
    assert!(
        (29_950_000_000..=30_000_000_000).contains(&tail),
        "tail {tail}"
    );
    let decoded = OpenH264Backend::new()
        .decode_frame(&track, 0, PixelFormat::Yuv420p)
        .unwrap();
    assert!(
        decoded.data[0].abs_diff(60) <= 4,
        "the last displayed frame (luma 60)"
    );
}

/// #564: a video gap after the last frame means the screen is not proven on display at `W0`:
/// nothing is carried in and the shortfall is reported.
#[test]
fn stream_gap_before_the_window_never_carries_in() {
    let tmp = TempDir::new().unwrap();
    let mut cap = pipeline(&tmp);
    beacons(&mut cap, 0, 900_000_000);
    frames(&mut cap, SEC, 3, 40);
    beacons(&mut cap, 1_200_000_000, 4_800_000_000);
    cap.ingest_event(video_gap(5 * SEC)).unwrap();
    beacons(&mut cap, 5_100_000_000, 40 * SEC);
    let (outcome, _) = save(&mut cap, &tmp, "gap", 30, SaveMode::Full, Ledger::Keep);
    let e = &outcome.evidence;
    assert_eq!(e.video_frame_count, Some(0));
    assert_eq!(e.carried_in_frame, Some(false));
    assert_eq!(e.video_shortfall, Some(true));
    assert_eq!(e.video_coverage_ms, Some(0));
    assert!(manifest_carried_in(&outcome).is_none());
    assert!(
        e.warning_codes
            .as_ref()
            .unwrap()
            .contains(&"video_shortfall".to_owned())
    );
    assert_warnings_one_to_one(&outcome);
}

/// #564: unplugging 20 s into the window ends the hold there: the video is cut honestly at the
/// end of coverage and reported short.
#[test]
fn disconnect_mid_window_cuts_the_static_hold_at_coverage_end() {
    let tmp = TempDir::new().unwrap();
    let mut cap = pipeline(&tmp);
    beacons(&mut cap, 0, 900_000_000);
    frames(&mut cap, SEC, 3, 40);
    beacons(&mut cap, 1_200_000_000, 30 * SEC);
    cap.ingest_event(connection_lost(30_100_000_000)).unwrap();
    // The trigger arrives at 40 s with no further beacons.
    cap.ingest_event(CaptureEvent::Shutdown {
        pc_cont_ns: 40 * SEC,
        payload: arb_adb::wire::ShutdownPayload {
            boottime_ns: 0,
            code: arb_adb::wire::ShutdownCode::HostDisconnect,
            reason: "unplugged".into(),
        },
    })
    .unwrap();
    let (outcome, _) = save(&mut cap, &tmp, "cut", 30, SaveMode::Full, Ledger::Keep);
    let e = &outcome.evidence;
    assert_eq!(e.video_frame_count, Some(1));
    assert_eq!(e.carried_in_frame, Some(true));
    let video_ms = e.actual_video_duration_ms.unwrap();
    assert!(
        (19_900..=20_000).contains(&video_ms),
        "held until coverage end: {video_ms}"
    );
    assert_eq!(e.video_shortfall, Some(true));
    assert_warnings_one_to_one(&outcome);
}

/// #570: a healthy 30 s log stream whose first/last rows are 0.5 s inside the window is not
/// short; a recording that started 10 s before the save is.
#[test]
fn log_shortfall_is_judged_by_logcat_coverage() {
    let tmp = TempDir::new().unwrap();
    let mut cap = pipeline(&tmp);
    beacons(&mut cap, 0, 10 * SEC);
    // Window [10 s, 40 s]; rows 0.501 s / 0.5 s inside both edges: span 28,999 ms.
    cap.ingest_event(log_row(10_501_000_000, "first")).unwrap();
    beacons(&mut cap, 10_600_000_000, 39_400_000_000);
    cap.ingest_event(log_row(39_500_000_000, "last")).unwrap();
    beacons(&mut cap, 39_700_000_000, 40 * SEC);

    let (old, _) = save(&mut cap, &tmp, "old", 30, SaveMode::LogsOnly, Ledger::Strip);
    assert_eq!(old.evidence.actual_log_duration_ms, Some(28_999));
    assert_eq!(old.evidence.log_shortfall, Some(true), "#570 reproduced");
    assert_eq!(old.evidence.log_coverage_ms, None);
    assert_warnings_one_to_one(&old);

    let (healthy, _) = save(
        &mut cap,
        &tmp,
        "healthy",
        30,
        SaveMode::LogsOnly,
        Ledger::Keep,
    );
    let e = &healthy.evidence;
    assert_eq!(
        e.actual_log_duration_ms,
        Some(28_999),
        "row span keeps its meaning"
    );
    assert!(e.log_coverage_ms.unwrap() >= 29_900);
    assert_eq!(e.log_shortfall, Some(false));
    assert_eq!(e.window_shortfall, Some(false));
    assert_eq!(
        e.video_coverage_ms, None,
        "logs-only saves report no video fields"
    );
    assert_warnings_one_to_one(&healthy);

    // Recording started 10 s before a 30 s save.
    let tmp = TempDir::new().unwrap();
    let mut cap = pipeline(&tmp);
    cap.ingest_event(heartbeat(30 * SEC)).unwrap();
    cap.ingest_event(log_row(30 * SEC, "started")).unwrap();
    beacons(&mut cap, 30_400_000_000, 40 * SEC);
    let (late, _) = save(&mut cap, &tmp, "late", 30, SaveMode::LogsOnly, Ledger::Keep);
    let e = &late.evidence;
    let covered = e.log_coverage_ms.unwrap();
    assert!((9_900..=10_000).contains(&covered), "covered {covered}");
    assert_eq!(e.log_shortfall, Some(true));
    assert_eq!(e.window_shortfall, Some(true));
    let codes = e.warning_codes.clone().unwrap();
    assert_eq!(&codes[..2], ["log_shortfall", "window_shortfall"]);
    assert_eq!(
        e.warnings.as_ref().unwrap()[1],
        "要求した保存時間に達していない記録があります"
    );
    assert_warnings_one_to_one(&late);
}

/// The ledger is generation-scoped: a beacon from a stopped generation never extends the
/// coverage of the next one, and stopping closes coverage at the last evidence.
#[test]
fn stale_generation_beacons_never_extend_new_coverage() {
    let tmp = TempDir::new().unwrap();
    let mut cap = pipeline(&tmp);
    let old_generation = cap.session_generation();
    beacons(&mut cap, 0, 300_000_000);
    frames(&mut cap, SEC, 1, 40);
    cap.mark_stopped();
    assert!(!cap.coverage.video.is_open());
    let new_generation = cap.begin_session("A");
    assert_ne!(old_generation, new_generation);
    cap.ingest_event_for_session(old_generation, heartbeat(1_100_000_000))
        .unwrap();
    cap.ingest_event_for_session(old_generation, heartbeat(1_400_000_000))
        .unwrap();
    assert!(!cap.coverage.video.is_open());
    let snap = cap.coverage.snapshot(i64::MIN, i64::MAX);
    assert_eq!(snap.video.len(), 1);
    assert_eq!(snap.video[0].end_ns, SEC, "closed at the last frame");
    // New evidence of the new generation opens a new interval.
    cap.ingest_event_for_session(new_generation, heartbeat(1_500_000_000))
        .unwrap();
    assert!(
        !cap.coverage.video.is_open(),
        "a beacon alone never opens coverage"
    );
}

/// #564: one media origin. With a carried-in frame followed by in-window frames, MP4 media time
/// 0 is `W0` in the MP4 sample timing, the log coordinates, and the real-time trim mapping; a log
/// before the first displayed frame of a bundle without carry-in has no frame.
#[test]
fn carried_in_frame_shares_one_media_origin_across_layers() {
    let tmp = TempDir::new().unwrap();
    let mut cap = pipeline(&tmp);
    beacons(&mut cap, 0, 900_000_000);
    frames(&mut cap, SEC, 3, 40);
    beacons(&mut cap, 1_300_000_000, 19_900_000_000);
    // Window [10 s, 40 s]: the screen changes at 20 s.
    frames(&mut cap, 20 * SEC, 3, 120);
    cap.ingest_event(log_row(15 * SEC, "held frame")).unwrap();
    cap.ingest_event(log_row(20_050_000_000, "new frame"))
        .unwrap();
    beacons(&mut cap, 20_200_000_000, 40 * SEC);
    let (outcome, _) = save(&mut cap, &tmp, "origin", 30, SaveMode::Full, Ledger::Keep);
    let e = &outcome.evidence;
    assert_eq!(e.carried_in_frame, Some(true));
    assert_eq!(e.video_frame_count, Some(4));
    assert_eq!(e.video_shortfall, Some(false));
    let path = outcome.bundle_path.clone().unwrap();
    let reader = arb_storage::bundle_io::open_bundle(&path).unwrap();

    // MP4: sample 0 (carried in) is displayed from W0 = 10 s until the change at 20 s.
    let track = arb_media::mp4::Mp4VideoTrack::open(reader.video_path().unwrap()).unwrap();
    let pts: Vec<i64> = track.frames().iter().map(|f| f.pts_ns).collect();
    assert_eq!(
        pts[1] - pts[0],
        10 * SEC,
        "head hold W0 -> first window frame"
    );
    assert_eq!(
        arb_media::mp4::inspect_mp4_atoms(reader.video_path().unwrap())
            .unwrap()
            .elst_count,
        0
    );

    // Display view: frame 0 starts at W0, later frames at their capture time.
    let display = reader.display_frames().unwrap();
    let starts: Vec<i64> = display
        .iter()
        .map(|f| arb_core::bundle::frame_display_start_ns(f, None).unwrap())
        .collect();
    assert_eq!(starts[0], 10 * SEC);
    assert_eq!(starts[1], 20 * SEC);

    // Log coordinates use the same origin (20.05 s shows the frame captured at 20.04 s).
    let params: crate::protocol::TimelineQueryParams =
        serde_json::from_value(serde_json::json!({ "bundle_id_or_path": "x" })).unwrap();
    let rows = crate::inspect_export::query_log_response(
        reader.as_ref(),
        Some(arb_core::bundle::RecordBucket::Main),
        &params,
        100,
    )
    .unwrap()
    .logs;
    let located: Vec<(Option<i64>, Option<u64>)> =
        rows.iter().map(|r| (r.offset_ms, r.frame_index)).collect();
    assert_eq!(
        located,
        vec![(Some(5_000), Some(0)), (Some(10_050), Some(2))]
    );

    // Real-time trim [5 s, 15 s) maps to device time [W0 + 5 s, W0 + 15 s).
    let range = crate::trim_range::TrimRange::resolve(
        &track,
        (None, None),
        Some((5 * SEC.unsigned_abs(), 15 * SEC.unsigned_abs())),
    )
    .unwrap();
    let window = range.window(&display).unwrap();
    assert_eq!(window.start_boottime_ns(), 15 * SEC);
    assert_eq!(window.end_boottime_ns(), 25 * SEC - 1);
}

/// Without carry-in evidence, a log before the first displayed frame is not attributed to it.
#[test]
fn log_before_the_first_displayed_frame_has_no_frame() {
    let tmp = TempDir::new().unwrap();
    let mut cap = pipeline(&tmp);
    beacons(&mut cap, 0, 900_000_000);
    frames(&mut cap, SEC, 3, 40);
    cap.ingest_event(video_gap(1_200_000_000)).unwrap();
    beacons(&mut cap, 1_300_000_000, 19_900_000_000);
    cap.ingest_event(log_row(15 * SEC, "before video")).unwrap();
    frames(&mut cap, 20 * SEC, 2, 120);
    beacons(&mut cap, 20_200_000_000, 40 * SEC);
    let (outcome, _) = save(&mut cap, &tmp, "nocarry", 30, SaveMode::Full, Ledger::Keep);
    assert_eq!(outcome.evidence.carried_in_frame, Some(false));
    let reader = arb_storage::bundle_io::open_bundle(outcome.bundle_path.unwrap()).unwrap();
    let params: crate::protocol::TimelineQueryParams =
        serde_json::from_value(serde_json::json!({ "bundle_id_or_path": "x" })).unwrap();
    let rows = crate::inspect_export::query_log_response(
        reader.as_ref(),
        Some(arb_core::bundle::RecordBucket::Main),
        &params,
        100,
    )
    .unwrap()
    .logs;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].frame_index, None, "no frame was on screen yet");
    assert_eq!(rows[0].offset_ms, Some(-5_000));
}

proptest::proptest! {
    /// #570: for every combination of warning conditions, `warnings` and `warning_codes` have
    /// the same length and order as `warning_details`, every message is non-empty, and
    /// `window_shortfall` appears exactly when a stream is short.
    #[test]
    fn warnings_and_codes_are_always_one_to_one(bits in 0_u8..128) {
        let flags = super::WarningFlags {
            video_shortfall: bits & 1 != 0,
            log_shortfall: bits & 2 != 0,
            logcat_overflow: bits & 4 != 0,
            unanchored_truncated: bits & 8 != 0,
            audio_absent: bits & 16 != 0,
            stream_discontinuity: bits & 32 != 0,
            tail_unconfirmed: bits & 64 != 0,
        };
        let mut evidence = crate::protocol::SaveEvidence::default();
        evidence.set_warnings(super::evidence_warnings(flags));
        let codes = evidence.warning_codes.unwrap();
        let messages = evidence.warnings.unwrap();
        let details = evidence.warning_details.unwrap();
        proptest::prop_assert_eq!(codes.len(), messages.len());
        proptest::prop_assert_eq!(details.len(), codes.len());
        proptest::prop_assert_eq!(codes.len(), usize::try_from(bits.count_ones()).unwrap()
            + usize::from(flags.video_shortfall || flags.log_shortfall));
        for ((d, c), m) in details.iter().zip(&codes).zip(&messages) {
            proptest::prop_assert_eq!(&d.code, c);
            proptest::prop_assert_eq!(&d.message, m);
            proptest::prop_assert!(!m.is_empty());
        }
        proptest::prop_assert_eq!(
            codes.iter().any(|c| c == "window_shortfall"),
            flags.video_shortfall || flags.log_shortfall
        );
    }
}
