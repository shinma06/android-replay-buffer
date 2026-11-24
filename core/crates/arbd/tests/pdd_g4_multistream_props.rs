//! Layer-3 PDD (G4): multi-stream save → full-range trim conservation and quarantine
//! budget accounting across interleaved logs, audio, taps and strokes, with partial
//! overlaps and cross-boot records, through the same `arb_core::selection` entry points
//! the daemon save path (`select_records`) and trim path (`select_for_trim_with_truncated`)
//! use.

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    clippy::fn_params_excessive_bools,
    clippy::trivially_copy_pass_by_ref,
    clippy::print_stdout,
    clippy::collapsible_if,
    clippy::similar_names,
    clippy::format_collect,
    clippy::as_conversions,
    clippy::clone_on_ref_ptr,
    reason = "PDD model/property code: long linearized models and generators read best unsplit"
)]

use arb_core::selection::{
    SelectionPolicy, TimestampedRecord, WindowSpec, select_for_trim_with_truncated, select_records,
};
use arb_core::time::TimeAlignment;
use proptest::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Log,
    Audio,
    Tap,
    Stroke,
}

const STREAMS: [Stream; 4] = [Stream::Log, Stream::Audio, Stream::Tap, Stream::Stroke];

type Rec = TimestampedRecord<(Stream, u32)>;

fn alignment() -> impl Strategy<Value = TimeAlignment> {
    let boot = prop_oneof![Just("a".to_owned()), Just("b".to_owned())];
    prop_oneof![
        (boot.clone(), 0i64..1_000, 0u64..60).prop_map(|(boot_id, t, hw)| {
            TimeAlignment::Anchored {
                boot_id,
                boottime_ns: t,
                half_width_ns: hw,
                semantics: arb_core::time::TimestampSemantics::LogcatHeaderWall,
            }
        }),
        (boot, 0i64..1_000, 0i64..200).prop_map(|(boot_id, lo, span)| TimeAlignment::Bounded {
            boot_id,
            min_boottime_ns: lo,
            max_boottime_ns: lo + span,
            reason: arb_core::time::UncertaintyReason::DeviceSleepSpanned,
        }),
        (0i64..1_000).prop_map(|pc| TimeAlignment::Unanchored {
            pc_ingest_cont_ns: pc,
            reason: arb_core::time::UncertaintyReason::NoClockAnchor,
        }),
    ]
}

fn records() -> impl Strategy<Value = Vec<Rec>> {
    prop::collection::vec((0usize..4, alignment(), 0i64..1_000, 1usize..4096), 0..40).prop_map(
        |v| {
            v.into_iter()
                .enumerate()
                .map(|(i, (s, alignment, ingest, bytes))| TimestampedRecord {
                    payload: (STREAMS[s], u32::try_from(i).expect("i")),
                    alignment,
                    pc_ingest_cont_ns: ingest,
                    // Giant strokes stress the byte cap.
                    estimated_bytes: if STREAMS[s] == Stream::Stroke {
                        bytes * 64
                    } else {
                        bytes
                    },
                })
                .collect()
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64),
        ..ProptestConfig::default()
    })]

    #[test]
    fn g4_save_then_full_range_trim_conserves_every_record(
        recs in records(), ws in 0i64..1_000, len in 0i64..1_000, max_records in 0usize..8, max_bytes in 0usize..40_000,
    ) {
        let window = WindowSpec::RealElapsed {
            boot_id: "a".to_owned(),
            start_boottime_ns: ws,
            end_boottime_ns: ws + len,
            ingest_start_cont_ns: ws,
            ingest_end_cont_ns: ws + len,
        };
        let policy = SelectionPolicy { max_unanchored_records: max_records, max_unanchored_bytes: max_bytes, ..SelectionPolicy::new() };
        let mut saved_total = 0usize;
        for stream in STREAMS {
            let input: Vec<Rec> = recs.iter().filter(|r| r.payload.0 == stream).cloned().collect();
            // Save: per-stream partition with the daemon's capped policy.
            let save = select_records(input, &window, &policy);
            let excluded = save.excluded_count;
            prop_assert!(save.unanchored.len() <= policy.max_unanchored_records);
            prop_assert!(save.unanchored.iter().map(|r| r.estimated_bytes).sum::<usize>() <= policy.max_unanchored_bytes);
            prop_assert!(save.main.iter().all(|r| matches!(&r.alignment,
                TimeAlignment::Anchored { boot_id, .. } | TimeAlignment::Bounded { boot_id, .. } if boot_id == "a")),
                "cross-boot / unanchored records never enter the main timeline");
            saved_total += save.main.len() + save.unanchored.len() + save.unanchored_truncated_count + excluded;

            // Read back + trim over the full saved range: nothing is lost or promoted.
            let main_ids: Vec<u32> = save.main.iter().map(|r| r.payload.1).collect();
            let quarantine_ids: Vec<u32> = save.unanchored.iter().map(|r| r.payload.1).collect();
            let trim = select_for_trim_with_truncated(
                save.main, save.unanchored, save.unanchored_truncated_count, &window, &policy,
            );
            prop_assert_eq!(trim.main.iter().map(|r| r.payload.1).collect::<Vec<_>>(), main_ids);
            prop_assert_eq!(trim.unanchored.iter().map(|r| r.payload.1).collect::<Vec<_>>(), quarantine_ids);
            prop_assert_eq!(trim.unanchored_truncated_count, save.unanchored_truncated_count,
                "truncated quarantine count carried forward exactly");
        }
        prop_assert_eq!(saved_total, recs.len(), "cross-stream conservation");
    }
}
