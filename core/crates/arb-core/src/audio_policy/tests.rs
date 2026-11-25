//! Unit, golden-table (G8-8) and property (G8-1/2/4/5/9) tests for the audio policy.

use std::collections::BTreeSet;

use proptest::prelude::*;
use serde_json::Value;

use super::{
    AudioCaptureState, AudioCause, AudioLabelClass, AudioLaunchPolicy, AudioPolicyInput,
    MAX_STDERR_TAIL_BYTES, MIN_AUDIO_PLAYBACK_API_LEVEL, RuntimeAudioLatch, ScrcpyAudioOutcome,
    StickyAudioLatch, api_below_min_reason, evaluate, plan_requested_launch,
    runtime_failure_reason, stderr_tail,
};
use crate::config::{AudioCaptureMode, CaptureMode};

/// The shared golden table, read verbatim by the IDE `client-core` tests too.
const GOLDEN_TABLE: &str = include_str!("../../../../../docs/specs/audio-policy-table.json");

fn outcome_from_wire(wire: &str) -> ScrcpyAudioOutcome {
    serde_json::from_value(Value::String(wire.to_owned()))
        .unwrap_or_else(|err| panic!("unknown outcome `{wire}` in golden table: {err}"))
}

fn mode_from_wire(wire: &str) -> CaptureMode {
    serde_json::from_value(Value::String(wire.to_owned()))
        .unwrap_or_else(|err| panic!("unknown mode `{wire}` in golden table: {err}"))
}

fn table() -> Value {
    serde_json::from_str(GOLDEN_TABLE).expect("docs/specs/audio-policy-table.json is valid JSON")
}

fn modes() -> [CaptureMode; 2] {
    [CaptureMode::VideoAndLogs, CaptureMode::LogsOnly]
}

#[test]
fn g8_8_golden_table_cause_list_matches_rust_enum_and_label_classes() {
    let table = table();
    assert_eq!(
        table["min_audio_playback_api_level"].as_u64(),
        Some(u64::from(MIN_AUDIO_PLAYBACK_API_LEVEL)),
        "table floor matches MIN_AUDIO_PLAYBACK_API_LEVEL"
    );
    let causes = table["causes"].as_array().expect("causes array");
    let wire: Vec<&str> = causes
        .iter()
        .map(|c| c["cause"].as_str().expect("cause string"))
        .collect();
    let rust: Vec<&str> = AudioCause::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(
        wire, rust,
        "golden cause list == AudioCause::ALL (same order)"
    );
    for (entry, cause) in causes.iter().zip(AudioCause::ALL) {
        assert_eq!(
            entry["label_class"].as_str(),
            Some(cause.label_class().as_str()),
            "label class of {cause:?}"
        );
        let json = serde_json::to_value(cause).expect("serialize cause");
        assert_eq!(
            json.as_str(),
            Some(cause.as_str()),
            "serde wire of {cause:?}"
        );
    }
    let outcomes: Vec<String> = ScrcpyAudioOutcome::ALL
        .iter()
        .map(|o| {
            serde_json::to_value(o)
                .expect("serialize outcome")
                .as_str()
                .expect("outcome string")
                .to_owned()
        })
        .collect();
    let table_outcomes: Vec<String> = table["outcomes"]
        .as_array()
        .expect("outcomes array")
        .iter()
        .map(|o| o.as_str().expect("outcome string").to_owned())
        .collect();
    assert_eq!(
        table_outcomes, outcomes,
        "golden outcome list == ScrcpyAudioOutcome::ALL"
    );
}

#[test]
fn g8_8_golden_table_rows_match_evaluate() {
    let table = table();
    let rows = table["rows"].as_array().expect("rows array");
    assert!(rows.len() >= 86, "golden table keeps its coverage");
    let mut ids = BTreeSet::new();
    let mut covered_outcomes = BTreeSet::new();
    let mut covered_causes = BTreeSet::new();
    for row in rows {
        let id = row["id"].as_str().expect("row id");
        assert!(ids.insert(id.to_owned()), "duplicate row id {id}");
        let api = row["api"]
            .as_u64()
            .map(|v| u32::try_from(v).expect("api fits u32"));
        let input = AudioPolicyInput {
            audio_capture_mode: serde_json::from_value(row["audio_capture_mode"].clone())
                .expect("audio_capture_mode"),
            api,
            mode: mode_from_wire(row["mode"].as_str().expect("mode")),
            requested: row["requested"].as_bool().expect("requested"),
            runtime_disabled: row["runtime_disabled"].as_bool().expect("runtime_disabled"),
            outcome: outcome_from_wire(row["outcome"].as_str().expect("outcome")),
        };
        let verdict = evaluate(input);
        assert_eq!(
            verdict.policy.kind(),
            row["policy"].as_str().expect("policy"),
            "{id}: policy"
        );
        assert_eq!(
            verdict.state.as_str(),
            row["state"].as_str().expect("state"),
            "{id}: state"
        );
        assert_eq!(
            verdict.cause.as_str(),
            row["cause"].as_str().expect("cause"),
            "{id}: cause"
        );
        assert_eq!(
            verdict.cause.label_class().as_str(),
            row["label_class"].as_str().expect("label_class"),
            "{id}: label_class"
        );
        assert_eq!(
            Some(verdict.next_launch_audio),
            row["next_launch_audio"].as_bool(),
            "{id}: next_launch_audio"
        );
        assert_eq!(
            Some(verdict.video_expected),
            row["video_expected"].as_bool(),
            "{id}: video_expected"
        );
        covered_outcomes.insert(input.outcome.is_fatal());
        covered_causes.insert(verdict.cause.as_str());
    }
    for cause in AudioCause::ALL {
        if cause != AudioCause::Unknown {
            assert!(
                covered_causes.contains(cause.as_str()),
                "golden rows produce cause {cause:?}"
            );
        }
    }
    assert_eq!(
        covered_outcomes.len(),
        2,
        "fatal and non-fatal outcomes are both covered"
    );
}

#[test]
fn unknown_wire_cause_deserializes_to_unknown_for_forward_compatibility() {
    let cause: AudioCause =
        serde_json::from_str("\"some_future_cause\"").expect("forward-compatible cause");
    assert_eq!(
        cause,
        AudioCause::Unknown,
        "unknown wire value maps to Unknown"
    );
    assert_eq!(
        AudioCause::default(),
        AudioCause::None,
        "missing field defaults to none"
    );
}

#[test]
fn api_33_plans_audio_and_api_32_plans_proactive_off() {
    assert_eq!(
        plan_requested_launch(Some(33), RuntimeAudioLatch::Clear),
        AudioLaunchPolicy::TryAudio,
        "API 33 asks for audio"
    );
    assert_eq!(
        plan_requested_launch(Some(32), RuntimeAudioLatch::Clear),
        AudioLaunchPolicy::ProactiveOff { api: 32 },
        "API 32 proactively disables audio"
    );
    assert_eq!(
        plan_requested_launch(None, RuntimeAudioLatch::Clear),
        AudioLaunchPolicy::TryAudio,
        "unknown API asks for audio"
    );
    assert_eq!(
        plan_requested_launch(Some(33), RuntimeAudioLatch::StickyOff),
        AudioLaunchPolicy::RuntimeStickyOff,
        "sticky latch turns audio off"
    );
}

#[test]
fn api_below_min_reason_is_only_for_known_levels_below_33() {
    assert_eq!(
        api_below_min_reason(0),
        None,
        "unknown API has no API reason"
    );
    assert_eq!(api_below_min_reason(33), None, "API 33 has no API reason");
    assert!(
        api_below_min_reason(28).is_some_and(|r| r.contains("detected API 28")),
        "API 28 reason names the level"
    );
}

#[test]
fn sticky_latch_is_per_key_and_cleared_by_reset() {
    let mut latch = StickyAudioLatch::new();
    assert!(
        !latch.record("boot-a", ScrcpyAudioOutcome::Code0),
        "code0 is non-fatal"
    );
    assert!(!latch.is_disabled_for(&"boot-a"), "code0 does not latch");
    assert!(
        latch.record("boot-a", ScrcpyAudioOutcome::Code1),
        "code1 latches"
    );
    assert!(latch.is_disabled_for(&"boot-a"), "same boot stays off");
    assert!(
        !latch.is_disabled_for(&"boot-b"),
        "another boot is unaffected"
    );
    latch.reset();
    assert!(
        !latch.is_disabled_for(&"boot-a"),
        "explicit restart clears the latch"
    );
}

#[test]
fn stderr_tail_joins_lines_and_keeps_the_tail() {
    assert_eq!(stderr_tail("  \n\n", 100), None, "blank stderr has no tail");
    assert_eq!(
        stderr_tail("ERROR: Could not capture audio playback\njava.lang.RuntimeException: registerAudioPolicy() returned -1\n", 600)
            .as_deref(),
        Some("ERROR: Could not capture audio playback | java.lang.RuntimeException: registerAudioPolicy() returned -1"),
        "lines are joined with ` | `"
    );
    let cut = stderr_tail("aaaa\nbbbbbbbbbb", 6).expect("tail");
    assert_eq!(cut, "…bbbbbb", "long stderr keeps its tail");
    let multibyte = stderr_tail("あいうえお", 4).expect("tail");
    assert!(multibyte.starts_with('…'), "cut marker present");
}

#[test]
fn runtime_reason_mentions_codec_and_stderr() {
    let reason = runtime_failure_reason(
        AudioCause::DeviceCaptureFailed,
        None,
        Some("[server] ERROR: Could not capture audio playback\n"),
    );
    assert!(reason.contains("codec_id = 0"), "{reason}");
    assert!(reason.contains("AudioPlaybackCapture"), "{reason}");
    assert!(
        reason.contains("Could not capture audio playback"),
        "{reason}"
    );
    assert!(
        reason.ends_with("Continuing with video + logs + taps."),
        "{reason}"
    );
    let fatal = runtime_failure_reason(AudioCause::EncoderFatal, None, None);
    assert!(fatal.contains("codec_id = 1"), "{fatal}");
    let both = runtime_failure_reason(AudioCause::LaunchFailedBoth, Some("io"), None);
    assert!(both.ends_with("Logs + taps continue."), "{both}");
}

fn any_mode() -> impl Strategy<Value = CaptureMode> {
    prop_oneof![Just(CaptureMode::VideoAndLogs), Just(CaptureMode::LogsOnly)]
}

fn any_outcome() -> impl Strategy<Value = ScrcpyAudioOutcome> {
    proptest::sample::select(ScrcpyAudioOutcome::ALL.to_vec())
}

fn failure_outcome() -> impl Strategy<Value = ScrcpyAudioOutcome> {
    proptest::sample::select(
        ScrcpyAudioOutcome::ALL
            .into_iter()
            .filter(|o| {
                !matches!(
                    o,
                    ScrcpyAudioOutcome::NotAttempted | ScrcpyAudioOutcome::Aac
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn miri_safe_proptest_config() -> ProptestConfig {
    let mut cfg = ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    if cfg!(miri) {
        cfg.cases = 8;
    }
    cfg
}

proptest! {
    #![proptest_config(miri_safe_proptest_config())]

    /// G8-1 (sampled; Kani proves it for every `u32`): API >= 33 never yields `api_below_min`.
    #[test]
    fn g8_1_api_33_plus_is_never_api_below_min(
        api in MIN_AUDIO_PLAYBACK_API_LEVEL..=u32::MAX,
        mode in any_mode(),
        requested in any::<bool>(),
        runtime_disabled in any::<bool>(),
        outcome in any_outcome(),
    ) {
        let v = evaluate(AudioPolicyInput { audio_capture_mode: AudioCaptureMode::PlaybackDup, api: Some(api), mode, requested, runtime_disabled, outcome });
        prop_assert_ne!(v.cause, AudioCause::ApiBelowMin);
        prop_assert_ne!(v.cause.label_class(), AudioLabelClass::UnsupportedApi);
    }

    /// G8-4 (sampled; Kani proves it): API 28..=32 degrades to video + logs, never audio.
    #[test]
    fn g8_4_api_28_to_32_degrades_cleanly(
        api in 28u32..=32,
        runtime_disabled in any::<bool>(),
        outcome in any_outcome(),
    ) {
        let v = evaluate(AudioPolicyInput { audio_capture_mode: AudioCaptureMode::PlaybackDup,
            api: Some(api), mode: CaptureMode::VideoAndLogs, requested: true, runtime_disabled, outcome,
        });
        prop_assert_eq!(v.policy, AudioLaunchPolicy::ProactiveOff { api });
        prop_assert!(v.video_expected);
        prop_assert!(!v.policy.launches_audio());
        prop_assert_eq!(v.cause, AudioCause::ApiBelowMin);
        prop_assert_eq!(v.state, AudioCaptureState::Unavailable);
    }

    /// G8-5: on a capable device every failure outcome maps injectively to its own runtime
    /// cause, and the stderr tail always reaches the reason.
    #[test]
    fn g8_5_failures_are_injective_runtime_causes_with_stderr(
        api in prop_oneof![Just(None), Just(Some(0u32)), (MIN_AUDIO_PLAYBACK_API_LEVEL..=u32::MAX).prop_map(Some)],
        a in failure_outcome(),
        b in failure_outcome(),
        stderr in "[ -~]{1,80}(\n[ -~]{0,80}){0,3}",
    ) {
        let eval = |outcome| evaluate(AudioPolicyInput { audio_capture_mode: AudioCaptureMode::PlaybackDup,
            api, mode: CaptureMode::VideoAndLogs, requested: true, runtime_disabled: false, outcome,
        });
        let (va, vb) = (eval(a), eval(b));
        prop_assert_eq!(va.cause.label_class(), AudioLabelClass::RuntimeFailure);
        prop_assert_eq!(va.state, AudioCaptureState::Unavailable);
        prop_assert_eq!(a == b, va.cause == vb.cause, "outcome -> cause is injective");
        if let Some(tail) = stderr_tail(&stderr, MAX_STDERR_TAIL_BYTES) {
            let reason = runtime_failure_reason(va.cause, Some("detail"), Some(&stderr));
            prop_assert!(reason.contains(&tail), "reason {reason:?} lacks stderr tail {tail:?}");
            prop_assert!(reason.contains("detail"));
        }
    }

    /// G8-9 (sampled; Kani proves it): logs_only or audio off is always `disabled`.
    #[test]
    fn g8_9_disabled_modes_ignore_outcomes(
        api in proptest::option::of(any::<u32>()),
        runtime_disabled in any::<bool>(),
        outcome in any_outcome(),
        logs_only in any::<bool>(),
    ) {
        let mode = if logs_only { CaptureMode::LogsOnly } else { CaptureMode::VideoAndLogs };
        let requested = logs_only && runtime_disabled;
        let v = evaluate(AudioPolicyInput { audio_capture_mode: AudioCaptureMode::PlaybackDup, api, mode, requested, runtime_disabled, outcome });
        prop_assert_eq!(v.state, AudioCaptureState::Disabled);
        prop_assert_eq!(v.cause.label_class(), AudioLabelClass::Disabled);
    }

    /// The stderr tail never exceeds its budget (plus the 3-byte cut marker).
    #[test]
    fn stderr_tail_is_bounded(stderr in ".{0,2000}", max in 1usize..700) {
        if let Some(tail) = stderr_tail(&stderr, max) {
            prop_assert!(tail.len() <= max + '…'.len_utf8());
            prop_assert!(!tail.contains('\n'));
        }
    }
}

#[test]
fn every_mode_and_outcome_keeps_video_unless_logs_only_or_both_failed() {
    for mode in modes() {
        for outcome in ScrcpyAudioOutcome::ALL {
            for api in [None, Some(0), Some(28), Some(33)] {
                let v = evaluate(AudioPolicyInput {
                    audio_capture_mode: AudioCaptureMode::PlaybackDup,
                    api,
                    mode,
                    requested: true,
                    runtime_disabled: false,
                    outcome,
                });
                let expect_video = mode == CaptureMode::VideoAndLogs
                    && !(v.policy == AudioLaunchPolicy::TryAudio
                        && outcome == ScrcpyAudioOutcome::LaunchFailedBoth);
                assert_eq!(
                    v.video_expected, expect_video,
                    "{mode:?} {outcome:?} {api:?}"
                );
            }
        }
    }
}
