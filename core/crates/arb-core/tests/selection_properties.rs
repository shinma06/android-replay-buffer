//! Property-based and regression tests for `arb-core`:
//! - 3-valued selection conservation, delayed-arrival exclusion (49s),
//!   unanchored proximity + cap bounds (137s), and Save↔Trim invariance
//! - `WindowSpec::from_frame_indices` integer frame boundary accuracy
//! - `ClockTimeline` reboot/sleep/wall-jump/PC-sleep invariants
//! - `RingEvictionPlanner` pin protection & storage pressure stop invariants

use arb_core::bundle::FrameRecord;
use arb_core::selection::{
    Membership, SelectionError, SelectionPolicy, TimestampedRecord, WindowSpec, classify_alignment,
    select_for_trim, select_records,
};
use arb_core::state::{
    BundleLifecycleState, PinStatus, RingBudgetPolicy, RingChunkMeta, RingChunkStatus,
    RingEvictionPlanner, RingWriteDecision, SaveJob, SaveJobEvent, SaveJobState,
};
use arb_core::time::{
    ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityKind, TimeAlignment,
    TimestampSemantics, UncertaintyReason,
};
use proptest::prelude::{Just, Strategy, prop_assert, prop_assert_eq, prop_oneof, proptest};

fn arb_alignment() -> impl Strategy<Value = TimeAlignment> {
    let boot = prop_oneof![Just("boot-1".to_owned()), Just("boot-2".to_owned())];
    prop_oneof![
        (boot.clone(), -50_000_i64..250_000_i64, 0_u64..10_000_u64).prop_map(
            |(boot_id, boottime_ns, half_width_ns)| TimeAlignment::Anchored {
                boot_id,
                boottime_ns,
                half_width_ns,
                semantics: TimestampSemantics::LogcatHeaderWall,
            }
        ),
        (boot, -50_000_i64..250_000_i64, 0_i64..40_000_i64).prop_map(|(boot_id, start, span)| {
            TimeAlignment::Bounded {
                boot_id,
                min_boottime_ns: start,
                max_boottime_ns: start + span,
                reason: UncertaintyReason::DeviceWallJumpSpanned,
            }
        }),
        (-100_000_i64..300_000_i64).prop_map(|pc_ingest_cont_ns| TimeAlignment::Unanchored {
            pc_ingest_cont_ns,
            reason: UncertaintyReason::NoClockAnchor,
        }),
    ]
}

fn arb_record() -> impl Strategy<Value = TimestampedRecord<u64>> {
    (
        0_u64..10_000_u64,
        arb_alignment(),
        -100_000_i64..300_000_i64,
        1_usize..512_usize,
    )
        .prop_map(|(payload, alignment, pc_ingest_cont_ns, estimated_bytes)| {
            TimestampedRecord {
                payload,
                alignment,
                pc_ingest_cont_ns,
                estimated_bytes,
            }
        })
}

fn arb_ring_chunk() -> impl Strategy<Value = RingChunkMeta> {
    (
        0_u32..10_000_u32,
        0_i64..1_000_000_i64,
        1_u64..100_000_u64,
        1_u64..500_u64,
        prop_oneof![Just(RingChunkStatus::Open), Just(RingChunkStatus::Closed)],
        prop_oneof![Just(PinStatus::Pinned), Just(PinStatus::Unpinned)],
    )
        .prop_map(
            |(id, created_cont_ns, duration_ns, bytes, status, pin)| RingChunkMeta {
                chunk_id: format!("chunk-{id:05}"),
                created_cont_ns,
                duration_ns,
                bytes,
                status,
                pin,
            },
        )
}

proptest! {
    #[test]
    fn prop_selection_conservation_and_unanchored_isolation(
        records in proptest::collection::vec(arb_record(), 0..80),
        w_start in 50_000_i64..100_000_i64,
        w_len in 1_000_i64..50_000_i64,
        margin in 0_u64..10_000_u64,
        max_recs in 0_usize..25_usize,
        max_bytes in 0_usize..4_096_usize,
    ) {
        let w_end = w_start + w_len;
        let window = WindowSpec::RealElapsed {
            boot_id: "boot-1".into(),
            start_boottime_ns: w_start,
            end_boottime_ns: w_end,
            ingest_start_cont_ns: w_start,
            ingest_end_cont_ns: w_end,
        };
        let policy = SelectionPolicy {
            unanchored_ingest_margin_ns: margin,
            max_unanchored_records: max_recs,
            max_unanchored_bytes: max_bytes,
            drain_timeout_ms: 500,
        };

        let total_input = records.len();
        let part = select_records(records, &window, &policy);

        // Conservation law: every input record is accounted for exactly once.
        prop_assert_eq!(
            part.main.len()
                + part.unanchored.len()
                + part.unanchored_truncated_count
                + part.excluded_count,
            total_input,
            "every record must land in main, unanchored, truncated, or excluded"
        );

        // Unanchored and cross-boot records NEVER enter `main`.
        for rec in &part.main {
            prop_assert_eq!(
                classify_alignment(&rec.alignment, &window),
                Membership::InRange,
                "only InRange records may enter main"
            );
            match &rec.alignment {
                TimeAlignment::Anchored { boot_id, .. }
                | TimeAlignment::Bounded { boot_id, .. } => {
                    prop_assert_eq!(boot_id.as_str(), "boot-1", "main cannot contain cross-boot records");
                }
                TimeAlignment::Unanchored { .. } => {
                    prop_assert!(false, "main cannot contain Unanchored records");
                }
                _ => {}
            }
        }

        // Quarantine caps are strictly enforced.
        prop_assert!(
            part.unanchored.len() <= max_recs,
            "unanchored count {} exceeded max {}",
            part.unanchored.len(),
            max_recs
        );
        let unanchored_bytes: usize = part.unanchored.iter().map(|r| r.estimated_bytes).sum();
        prop_assert!(
            unanchored_bytes <= max_bytes,
            "unanchored bytes {} exceeded max {}",
            unanchored_bytes,
            max_bytes
        );
    }

    #[test]
    fn prop_save_and_full_range_trim_invariance(
        records in proptest::collection::vec(arb_record(), 0..60),
        w_start in 40_000_i64..90_000_i64,
        w_len in 5_000_i64..40_000_i64,
    ) {
        let w_end = w_start + w_len;
        let save_window = WindowSpec::RealElapsed {
            boot_id: "boot-1".into(),
            start_boottime_ns: w_start,
            end_boottime_ns: w_end,
            ingest_start_cont_ns: w_start,
            ingest_end_cont_ns: w_end,
        };
        let policy = SelectionPolicy::default();
        let saved = select_records(records, &save_window, &policy);

        // Full-range trim covering the exact same boottime bounds must be a pure identity!
        let trim_window = WindowSpec::FrameRange {
            boot_id: "boot-1".into(),
            in_frame_index: 0,
            out_frame_index: 100,
            start_boottime_ns: w_start,
            end_boottime_ns: w_end,
            ingest_start_cont_ns: w_start,
            ingest_end_cont_ns: w_end,
        };

        let trimmed = select_for_trim(
            saved.main.clone(),
            saved.unanchored.clone(),
            &trim_window,
            &policy,
        );

        prop_assert_eq!(
            trimmed.main,
            saved.main,
            "full-range trim must preserve main timeline identically"
        );
        prop_assert_eq!(
            trimmed.unanchored,
            saved.unanchored,
            "full-range trim must carry forward unanchored records transparently"
        );
        prop_assert_eq!(
            trimmed.excluded_count,
            0,
            "full-range trim must not drop any saved records"
        );
        prop_assert_eq!(
            trimmed.unanchored_truncated_count,
            0,
            "full-range trim within default caps must not truncate carried unanchored records"
        );
    }

    #[test]
    fn prop_delayed_anchored_record_strictly_outside_window_is_always_excluded(
        delay_before_window in 1_i64..200_000_000_000_i64,
        half_width in 0_u64..50_000_000_u64,
        late_ingest_offset in 0_i64..100_000_000_000_i64,
    ) {
        let w_start = 500_000_000_000_i64;
        let w_end = 530_000_000_000_i64;
        let window = WindowSpec::RealElapsed {
            boot_id: "boot-1".into(),
            start_boottime_ns: w_start,
            end_boottime_ns: w_end,
            ingest_start_cont_ns: w_start,
            ingest_end_cont_ns: w_end,
        };

        // Ensure boottime + half_width < w_start strictly.
        let hw_i64 = i64::try_from(half_width).expect("half_width fits in i64");
        let boottime_ns = w_start - hw_i64 - delay_before_window;
        let record = TimestampedRecord {
            payload: 1_u8,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-1".into(),
                boottime_ns,
                half_width_ns: half_width,
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            pc_ingest_cont_ns: w_start + (late_ingest_offset % (w_end - w_start + 1)),
            estimated_bytes: 64,
        };

        let part = select_records([record], &window, &SelectionPolicy::default());
        prop_assert!(part.main.is_empty(), "strictly old anchored record must never enter main");
        prop_assert!(part.unanchored.is_empty(), "strictly old anchored record must never enter unanchored");
        prop_assert_eq!(part.excluded_count, 1, "strictly old anchored record must always be excluded");
    }

    #[test]
    fn prop_ring_planner_never_evicts_pinned_or_open_chunks_and_respects_disk_floor(
        chunks in proptest::collection::vec(arb_ring_chunk(), 0..40),
        incoming_bytes in 0_u64..1_000_u64,
        host_free_bytes in 0_u64..10_000_u64,
        max_ring_bytes in 200_u64..4_000_u64,
        min_free_disk_bytes in 500_u64..5_000_u64,
    ) {
        let policy = RingBudgetPolicy {
            max_ring_bytes,
            max_ring_duration_ns: 500_000,
            min_free_disk_bytes,
        };
        let planner = RingEvictionPlanner::new(policy);
        let decision = planner.plan(&chunks, incoming_bytes, host_free_bytes);

        if host_free_bytes < min_free_disk_bytes
            || host_free_bytes.saturating_sub(incoming_bytes) < min_free_disk_bytes
        {
            prop_assert!(
                matches!(decision, RingWriteDecision::StopStoragePressure { .. }),
                "low free disk space must always emit StopStoragePressure"
            );
        } else if let RingWriteDecision::Allow { evict_chunk_ids } = decision {
            for evicted_id in &evict_chunk_ids {
                // Every evicted chunk must correspond to a closed, unpinned chunk.
                let matched = chunks.iter().any(|c| {
                    &c.chunk_id == evicted_id
                        && c.status == RingChunkStatus::Closed
                        && c.pin == PinStatus::Unpinned
                });
                prop_assert!(
                    matched,
                    "evicted chunk '{}' must be closed and unpinned",
                    evicted_id
                );
            }
        }
    }
}

#[test]
fn frame_range_window_derives_exact_boottime_and_validates_boundaries() {
    let frames = vec![
        FrameRecord {
            frame_index: 0,
            boot_id: "boot-1".into(),
            pts_device_mono_ns: 1_000,
            pc_ingest_cont_ns: 10_000,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-1".into(),
                boottime_ns: 1_000_000,
                half_width_ns: 500,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: true,
            gop_id: 0,
            mp4_sample_offset: 0,
            mp4_sample_size: 1024,
        },
        FrameRecord {
            frame_index: 1,
            boot_id: "boot-1".into(),
            pts_device_mono_ns: 2_000,
            pc_ingest_cont_ns: 20_000,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-1".into(),
                boottime_ns: 2_000_000,
                half_width_ns: 500,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: false,
            gop_id: 0,
            mp4_sample_offset: 1024,
            mp4_sample_size: 512,
        },
        FrameRecord {
            frame_index: 2,
            boot_id: "boot-2".into(),
            pts_device_mono_ns: 500,
            pc_ingest_cont_ns: 30_000,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-2".into(),
                boottime_ns: 500_000,
                half_width_ns: 500,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: true,
            gop_id: 1,
            mp4_sample_offset: 1536,
            mp4_sample_size: 1024,
        },
    ];

    let win = WindowSpec::from_frame_indices(&frames, 0, 1)
        .expect("valid same-boot frame range must succeed");
    assert_eq!(
        win.start_boottime_ns(),
        1_000_000,
        "start boottime must match frame 0"
    );
    assert_eq!(
        win.end_boottime_ns(),
        2_000_000,
        "end boottime must match frame 1"
    );

    assert!(
        matches!(
            WindowSpec::from_frame_indices(&frames, 1, 0),
            Err(SelectionError::InvertedFrameRange { .. })
        ),
        "inverted frame indices must fail"
    );
    assert!(
        matches!(
            WindowSpec::from_frame_indices(&frames, 0, 2),
            Err(SelectionError::CrossBootFrameRange { .. })
        ),
        "cross-boot frame range must fail"
    );
    assert!(
        matches!(
            WindowSpec::from_frame_indices(&frames, 0, 99),
            Err(SelectionError::FrameNotFound { frame_index: 99 })
        ),
        "missing frame index must fail"
    );
}

#[test]
fn end_to_end_reboot_sleep_wall_jump_and_lifecycle_transitions() {
    let mut timeline = ClockTimeline::new(ClockTimelineConfig::default());
    let base_wall = 1_700_000_000_000_000_000_i64;

    // Boot 1 normal -> sleep -> wall jump -> Boot 2 reboot
    let _ = timeline.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "b1".into(),
            boottime_ns: 10_000_000_000,
            monotonic_ns: 10_000_000_000,
            realtime_ns: base_wall + 10_000_000_000,
        },
        10_000_000_000,
    );
    let sleep_ev = timeline.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "b1".into(),
            boottime_ns: 25_000_000_000,
            monotonic_ns: 15_000_000_000,
            realtime_ns: base_wall + 25_000_000_000,
        },
        25_000_000_000,
    );
    assert!(
        sleep_ev
            .iter()
            .any(|e| e.kind == DiscontinuityKind::DeviceSleep),
        "10s deep sleep must emit DeviceSleep"
    );

    let jump_ev = timeline.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "b1".into(),
            boottime_ns: 26_000_000_000,
            monotonic_ns: 16_000_000_000,
            realtime_ns: base_wall + 126_000_000_000,
        },
        26_000_000_000,
    );
    assert!(
        jump_ev
            .iter()
            .any(|e| e.kind == DiscontinuityKind::DeviceWallJump),
        "100s forward wall jump must emit DeviceWallJump"
    );

    let reboot_ev = timeline.ingest_device_triple(
        DeviceClockTriple {
            boot_id: "b2".into(),
            boottime_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            realtime_ns: base_wall + 160_000_000_000,
        },
        60_000_000_000,
    );
    assert_eq!(
        reboot_ev.len(),
        1,
        "reboot must emit only DeviceReboot, never sleep/wall delta across boots"
    );
    assert_eq!(
        reboot_ev[0].kind,
        DiscontinuityKind::DeviceReboot,
        "kind must be DeviceReboot"
    );

    // SaveJob discard path and BundleLifecycle restore path
    let mut job = SaveJob::new("job-discard", "/tmp/out", vec!["c1".into()]);
    let _ = job
        .apply(SaveJobEvent::FailRetryable {
            error_message: "permission denied".into(),
        })
        .expect("Pinned -> FailedRetryable must succeed");
    assert!(
        job.has_pinned_chunks(),
        "FailedRetryable must retain pinned chunks"
    );
    assert_eq!(
        job.apply(SaveJobEvent::Discard),
        Ok(SaveJobState::Discarded),
        "FailedRetryable -> Discard must succeed"
    );
    assert!(
        !job.has_pinned_chunks(),
        "Discarded job must release pinned chunks"
    );

    let trashed = BundleLifecycleState::Active
        .trash()
        .expect("Active -> Trashed must succeed");
    let restored = trashed.restore().expect("Trashed -> Active must succeed");
    assert_eq!(
        restored,
        BundleLifecycleState::Active,
        "restored bundle must be Active"
    );
}
