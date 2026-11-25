//! Shared fixture F7: time discontinuities (host suspend `PcSleep`, device sleep `DeviceSleep`,
//! wall-clock rollback `DeviceWallJump`, device reboot / `boot_id` change `DeviceReboot`, and
//! equal-PTS / half-width error tolerance boundaries) verified against independent per-epoch
//! oracles for selection membership, gaps, unanchored quarantine, and playback ordering.

use arb_core::bundle::{FrameRecord, LogRecord, RecordBucket};
use arb_core::selection::{
    Membership, SelectionError, SelectionPolicy, UncertainPlacement, WindowSpec,
    classify_alignment, select_records,
};
use arb_core::time::{
    ClockError, ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityKind,
    PcSyncProbe, TimeAlignment, TimestampSemantics, UncertaintyReason,
};

const SEC: i64 = 1_000_000_000;
const MS: i64 = 1_000_000;
const WALL_BASE: i64 = 1_730_000_000 * SEC;

fn make_frame(idx: u64, boot_id: &str, bt_ns: i64, pc_ingest_ns: i64) -> FrameRecord {
    FrameRecord {
        frame_index: idx,
        boot_id: boot_id.to_owned(),
        pts_device_mono_ns: bt_ns,
        pc_ingest_cont_ns: pc_ingest_ns,
        alignment: TimeAlignment::Anchored {
            boot_id: boot_id.to_owned(),
            boottime_ns: bt_ns,
            half_width_ns: 2 * u64::try_from(MS).expect("u64"),
            semantics: TimestampSemantics::VideoEncoderInputEstimate,
        },
        is_keyframe: idx.is_multiple_of(10),
        gop_id: idx / 10,
        mp4_sample_offset: idx * 256,
        mp4_sample_size: 256,
    }
}

fn make_log(
    seq: u64,
    boot_id: Option<&str>,
    alignment: TimeAlignment,
    pc_ingest_ns: i64,
) -> LogRecord {
    LogRecord {
        seq,
        boot_id: boot_id.map(str::to_owned),
        header_epoch_ns: None,
        device_read_boottime_ns: None,
        pc_ingest_cont_ns: pc_ingest_ns,
        alignment,
        bucket: RecordBucket::Main,
        level: Some("I".to_owned()),
        tag: Some("F7".to_owned()),
        pid: Some(1000),
        tid: Some(1000),
        uid: None,
        package_name: None,
        message: format!("f7-log-{seq}"),
    }
}

fn build_f7_discontinuity_timeline() -> ClockTimeline {
    let mut tl = ClockTimeline::new(ClockTimelineConfig::new());

    let d0 = tl.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "boot-alpha".to_owned(),
            boottime_ns: 100 * SEC,
            monotonic_ns: 100 * SEC,
            realtime_ns: WALL_BASE + 100 * SEC,
        },
        1_000 * SEC,
    );
    assert_eq!(d0.len(), 0, "initial triple must emit zero discontinuities");

    let p0 = tl.ingest_pc_sync_probe(PcSyncProbe {
        boot_id: "boot-alpha".to_owned(),
        pc_send_cont_ns: 1_000 * SEC,
        pc_send_active_ns: 1_000 * SEC,
        dev_boottime_ns: 100 * SEC,
        dev_monotonic_ns: 100 * SEC,
        pc_recv_cont_ns: 1_000 * SEC + 4 * MS,
        pc_recv_active_ns: 1_000 * SEC + 4 * MS,
    });
    assert_eq!(p0.len(), 0, "healthy initial probe emits no discontinuity");

    let d_pc_sleep = tl.ingest_pc_sync_probe(PcSyncProbe {
        boot_id: "boot-alpha".to_owned(),
        pc_send_cont_ns: 1_045 * SEC,
        pc_send_active_ns: 1_001 * SEC,
        dev_boottime_ns: 145 * SEC,
        dev_monotonic_ns: 145 * SEC,
        pc_recv_cont_ns: 1_045 * SEC + 4 * MS,
        pc_recv_active_ns: 1_001 * SEC + 4 * MS,
    });
    assert!(
        d_pc_sleep
            .iter()
            .any(|e| e.kind == DiscontinuityKind::PcSleep && e.delta_ns == Some(44 * SEC)),
        "host suspend must emit PcSleep with exact +44s delta, got {d_pc_sleep:?}"
    );
    tl
}

#[test]
fn f7_host_suspend_device_sleep_wall_rollback_and_reboot_emit_exact_discontinuities() {
    let mut tl = build_f7_discontinuity_timeline();

    // Device deep sleep (+30 s boottime vs +1 s monotonic -> +29 s DeviceSleep).
    let d_dev_sleep = tl.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "boot-alpha".to_owned(),
            boottime_ns: 130 * SEC,
            monotonic_ns: 101 * SEC,
            realtime_ns: WALL_BASE + 130 * SEC,
        },
        1_030 * SEC,
    );
    assert!(
        d_dev_sleep
            .iter()
            .any(|e| e.kind == DiscontinuityKind::DeviceSleep && e.delta_ns == Some(29 * SEC)),
        "device sleep must emit DeviceSleep with +29s delta, got {d_dev_sleep:?}"
    );
    let sleep_span = tl.map_device_monotonic(
        "boot-alpha",
        100 * SEC + 500 * MS,
        TimestampSemantics::VideoEncoderInputEstimate,
        1_015 * SEC,
    );
    assert!(
        matches!(
            sleep_span,
            TimeAlignment::Bounded {
                reason: UncertaintyReason::DeviceSleepSpanned,
                ..
            }
        ),
        "monotonic stamp spanning device sleep must be Bounded(DeviceSleepSpanned), got {sleep_span:?}"
    );

    // Wall-clock rollback (-600 s jump backward while boottime advances +2 s).
    let d_wall_back = tl.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "boot-alpha".to_owned(),
            boottime_ns: 132 * SEC,
            monotonic_ns: 103 * SEC,
            realtime_ns: WALL_BASE + 132 * SEC - 600 * SEC,
        },
        1_032 * SEC,
    );
    assert!(
        d_wall_back
            .iter()
            .any(|e| e.kind == DiscontinuityKind::DeviceWallJump && e.delta_ns == Some(-600 * SEC)),
        "wall rollback must emit DeviceWallJump with -600s delta, got {d_wall_back:?}"
    );
    let _ = tl.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "boot-alpha".to_owned(),
            boottime_ns: 134 * SEC,
            monotonic_ns: 105 * SEC,
            realtime_ns: WALL_BASE + 134 * SEC - 600 * SEC,
        },
        1_034 * SEC,
    );
    let ambiguous_wall = tl.map_logcat_wall(
        "boot-alpha",
        WALL_BASE + 131 * SEC,
        Some(135 * SEC),
        1_035 * SEC,
    );
    assert!(
        !matches!(ambiguous_wall, TimeAlignment::Anchored { .. })
            || matches!(
                ambiguous_wall,
                TimeAlignment::Anchored { boottime_ns, .. } if boottime_ns <= 132 * SEC
            ),
        "wall-header mapping across rollback must never produce a post-rollback boottime for pre-rollback wall time: {ambiguous_wall:?}"
    );

    // Device reboot (`boot_id` changes to `boot-beta`, boottime resets to 5 s < 134 s).
    let d_reboot = tl.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "boot-beta".to_owned(),
            boottime_ns: 5 * SEC,
            monotonic_ns: 5 * SEC,
            realtime_ns: WALL_BASE + 200 * SEC,
        },
        1_100 * SEC,
    );
    assert!(
        d_reboot.iter().any(|e| {
            e.kind == DiscontinuityKind::DeviceReboot && e.boot_id.as_deref() == Some("boot-beta")
        }),
        "boot_id change must emit DeviceReboot for boot-beta, got {d_reboot:?}"
    );
    assert_eq!(
        ClockTimeline::elapsed_same_boot("boot-alpha", 130 * SEC, "boot-beta", 5 * SEC),
        Err(ClockError::CrossBootArithmetic {
            left_boot_id: "boot-alpha".to_owned(),
            right_boot_id: "boot-beta".to_owned(),
        }),
        "cross-boot elapsed arithmetic must be strictly forbidden"
    );
}

fn build_f7_epoch_logs() -> Vec<LogRecord> {
    vec![
        make_log(
            1,
            Some("boot-alpha"),
            TimeAlignment::Anchored {
                boot_id: "boot-alpha".to_owned(),
                boottime_ns: 11 * SEC,
                half_width_ns: u64::try_from(MS).expect("u64"),
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            1_001 * SEC,
        ),
        make_log(
            2,
            Some("boot-alpha"),
            TimeAlignment::Anchored {
                boot_id: "boot-alpha".to_owned(),
                boottime_ns: 11 * SEC,
                half_width_ns: u64::try_from(MS).expect("u64"),
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            1_001 * SEC,
        ),
        make_log(
            3,
            Some("boot-alpha"),
            TimeAlignment::Bounded {
                boot_id: "boot-alpha".to_owned(),
                min_boottime_ns: 11 * SEC + 900 * MS,
                max_boottime_ns: 12 * SEC + 500 * MS,
                reason: UncertaintyReason::DeviceWallJumpSpanned,
            },
            1_002 * SEC,
        ),
        make_log(
            4,
            None,
            TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 1_003 * SEC,
                reason: UncertaintyReason::NoClockAnchor,
            },
            1_003 * SEC,
        ),
        make_log(
            5,
            None,
            TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 1_500 * SEC,
                reason: UncertaintyReason::NoClockAnchor,
            },
            1_500 * SEC,
        ),
        make_log(
            6,
            Some("boot-beta"),
            TimeAlignment::Anchored {
                boot_id: "boot-beta".to_owned(),
                boottime_ns: 11 * SEC,
                half_width_ns: u64::try_from(MS).expect("u64"),
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            2_001 * SEC,
        ),
    ]
}

#[test]
fn f7_per_epoch_selection_isolates_epochs_quarantines_unanchored_and_preserves_order() {
    let frames = vec![
        make_frame(0, "boot-alpha", 10 * SEC, 1_000 * SEC),
        make_frame(1, "boot-alpha", 11 * SEC, 1_001 * SEC),
        make_frame(2, "boot-alpha", 12 * SEC, 1_002 * SEC),
        make_frame(3, "boot-beta", 10 * SEC, 2_000 * SEC),
        make_frame(4, "boot-beta", 11 * SEC, 2_001 * SEC),
        make_frame(5, "boot-beta", 12 * SEC, 2_002 * SEC),
    ];

    assert_eq!(
        WindowSpec::from_frame_indices(&frames, 1, 4),
        Err(SelectionError::CrossBootFrameRange {
            in_boot_id: "boot-alpha".to_owned(),
            out_boot_id: "boot-beta".to_owned(),
        }),
        "WindowSpec::from_frame_indices must reject cross-boot frame spans"
    );

    let win_alpha = WindowSpec::from_frame_indices(&frames, 0, 2).expect("same-epoch alpha window");
    let win_beta = WindowSpec::from_frame_indices(&frames, 3, 5).expect("same-epoch beta window");
    let policy = SelectionPolicy::new();
    let logs = build_f7_epoch_logs();

    let part_alpha = select_records(logs.clone(), &win_alpha, &policy);
    assert_eq!(
        part_alpha.main.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![1, 2],
        "boot-alpha main must contain only anchored boot-alpha logs in seq order"
    );
    assert_eq!(
        part_alpha
            .unanchored
            .iter()
            .map(|r| r.seq)
            .collect::<Vec<_>>(),
        vec![3, 4],
        "boot-alpha unanchored must quarantine partial-overlap and in-margin unanchored logs only"
    );
    assert_eq!(
        part_alpha.excluded_count, 2,
        "boot-alpha must exclude far-unanchored and boot-beta records"
    );

    let part_beta = select_records(logs, &win_beta, &policy);
    assert_eq!(
        part_beta.main.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![6],
        "boot-beta main must contain only boot-beta logs (zero cross-epoch bleed)"
    );
    assert_eq!(
        part_beta.unanchored,
        Vec::<LogRecord>::new(),
        "boot-beta unanchored must be empty"
    );
    assert_eq!(
        part_beta.excluded_count, 5,
        "boot-beta must exclude all boot-alpha and distant unanchored records"
    );

    let frames_alpha = select_records(frames.clone(), &win_alpha, &policy);
    let frames_beta = select_records(frames, &win_beta, &policy);
    assert_eq!(
        frames_alpha
            .main
            .iter()
            .map(|f| f.frame_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "boot-alpha frame playback order must be [0, 1, 2]"
    );
    assert_eq!(
        frames_beta
            .main
            .iter()
            .map(|f| f.frame_index)
            .collect::<Vec<_>>(),
        vec![3, 4, 5],
        "boot-beta frame playback order must be [3, 4, 5]"
    );
}

#[test]
fn f7_half_width_tolerance_boundaries_match_independent_oracle() {
    let win_alpha = WindowSpec::RealElapsed {
        boot_id: "boot-alpha".to_owned(),
        start_boottime_ns: 10 * SEC,
        end_boottime_ns: 12 * SEC,
        ingest_start_cont_ns: 1_000 * SEC,
        ingest_end_cont_ns: 1_002 * SEC,
    };

    // Anchored half-width boundary: `boottime - half_width == window_end` overlaps -> InRange;
    // `+1 ns` past window_end is strictly disjoint -> OutOfRange.
    let anchored_touching_end = TimeAlignment::Anchored {
        boot_id: "boot-alpha".to_owned(),
        boottime_ns: 12 * SEC + 5 * MS,
        half_width_ns: u64::try_from(5 * MS).expect("u64"),
        semantics: TimestampSemantics::LogcatHeaderWall,
    };
    assert_eq!(
        classify_alignment(&anchored_touching_end, &win_alpha),
        Membership::InRange,
        "anchored interval with boottime - half_width == window_end must be InRange"
    );

    let anchored_past_end = TimeAlignment::Anchored {
        boot_id: "boot-alpha".to_owned(),
        boottime_ns: 12 * SEC + 5 * MS + 1,
        half_width_ns: u64::try_from(5 * MS).expect("u64"),
        semantics: TimestampSemantics::LogcatHeaderWall,
    };
    assert_eq!(
        classify_alignment(&anchored_past_end, &win_alpha),
        Membership::OutOfRange,
        "anchored interval with boottime - half_width == window_end + 1ns must be OutOfRange"
    );

    // Bounded interval boundary: contained (`max == window_end`) -> InRange;
    // spilling by +1 ns -> PartialOverlap; disjoint (`min == window_end + 1`) -> OutOfRange.
    let bounded_exact = TimeAlignment::Bounded {
        boot_id: "boot-alpha".to_owned(),
        min_boottime_ns: 11 * SEC,
        max_boottime_ns: 12 * SEC,
        reason: UncertaintyReason::DeviceSleepSpanned,
    };
    assert_eq!(
        classify_alignment(&bounded_exact, &win_alpha),
        Membership::InRange,
        "bounded interval fully contained up to window_end must be InRange"
    );

    let bounded_partial = TimeAlignment::Bounded {
        boot_id: "boot-alpha".to_owned(),
        min_boottime_ns: 11 * SEC,
        max_boottime_ns: 12 * SEC + 1,
        reason: UncertaintyReason::DeviceSleepSpanned,
    };
    assert_eq!(
        classify_alignment(&bounded_partial, &win_alpha),
        Membership::UncertainTime(UncertainPlacement::PartialOverlap),
        "bounded interval exceeding window_end by 1ns must be PartialOverlap"
    );

    let bounded_disjoint = TimeAlignment::Bounded {
        boot_id: "boot-alpha".to_owned(),
        min_boottime_ns: 12 * SEC + 1,
        max_boottime_ns: 13 * SEC,
        reason: UncertaintyReason::DeviceSleepSpanned,
    };
    assert_eq!(
        classify_alignment(&bounded_disjoint, &win_alpha),
        Membership::OutOfRange,
        "bounded interval starting at window_end + 1ns must be OutOfRange"
    );
}
