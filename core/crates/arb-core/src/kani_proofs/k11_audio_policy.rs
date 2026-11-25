//! K11 (PDD G8): Kani harnesses for the live audio policy (`crate::audio_policy`).
//!
//! The harnesses run the production `plan_launch` / `evaluate` / `StickyAudioLatch` over the
//! whole `u32` API domain and every mode, request flag, latch state and `scrcpy-server` audio
//! outcome. Expected values come from independent oracles written here, not from the
//! production helpers (`is_fatal`, `label_class`), so a one-line production mutation fails.

use crate::audio_policy::{
    AudioCaptureState, AudioCause, AudioLabelClass, AudioLaunchPolicy, AudioPolicyInput,
    ScrcpyAudioOutcome, StickyAudioLatch, evaluate, plan_requested_launch,
};
use crate::config::CaptureMode;

fn any_mode() -> CaptureMode {
    if kani::any() {
        CaptureMode::VideoAndLogs
    } else {
        CaptureMode::LogsOnly
    }
}

fn any_outcome() -> ScrcpyAudioOutcome {
    let index: u8 = kani::any();
    kani::assume(index < 8);
    match index {
        0 => ScrcpyAudioOutcome::NotAttempted,
        1 => ScrcpyAudioOutcome::Aac,
        2 => ScrcpyAudioOutcome::Code0,
        3 => ScrcpyAudioOutcome::Code1,
        4 => ScrcpyAudioOutcome::HandshakeIo,
        5 => ScrcpyAudioOutcome::ConnectFailed,
        6 => ScrcpyAudioOutcome::LaunchFailedAudioOk,
        _ => ScrcpyAudioOutcome::LaunchFailedBoth,
    }
}

/// Independent oracle: the fatal outcomes after which a device boot stays on `audio=false`.
fn oracle_is_fatal(outcome: ScrcpyAudioOutcome) -> bool {
    matches!(
        outcome,
        ScrcpyAudioOutcome::Code1 | ScrcpyAudioOutcome::LaunchFailedAudioOk
    )
}

fn any_input(api: Option<u32>) -> AudioPolicyInput {
    AudioPolicyInput {
        api,
        mode: any_mode(),
        requested: kani::any(),
        runtime_disabled: kani::any(),
        outcome: any_outcome(),
    }
}

/// G8-1: for every API level >= 33 and every mode / request / latch / outcome, the cause is
/// never `api_below_min` and the presentation is never "unsupported".
#[kani::proof]
fn k11_g8_1_api_33_plus_is_never_api_below_min() {
    let api: u32 = kani::any();
    kani::assume(api >= 33);
    let verdict = evaluate(any_input(Some(api)));
    assert!(
        verdict.cause != AudioCause::ApiBelowMin,
        "API >= 33 is never api_below_min"
    );
    assert!(
        !matches!(verdict.policy, AudioLaunchPolicy::ProactiveOff { .. }),
        "API >= 33 is never proactively off"
    );
    assert!(
        verdict.cause.label_class() != AudioLabelClass::UnsupportedApi,
        "API >= 33 is never presented as unsupported"
    );
}

/// G8-2: an unknown API (`None` or `Some(0)`) is attempted, never `api_below_min`.
#[kani::proof]
fn k11_g8_2_unknown_api_is_never_api_below_min() {
    let api = if kani::any() { None } else { Some(0) };
    let input = any_input(api);
    let verdict = evaluate(input);
    assert!(
        verdict.cause != AudioCause::ApiBelowMin,
        "unknown API is never api_below_min"
    );
    if input.mode == CaptureMode::VideoAndLogs && input.requested && !input.runtime_disabled {
        assert!(
            verdict.policy == AudioLaunchPolicy::TryAudio,
            "unknown API asks for audio"
        );
    }
}

/// G8-4: API 28..=32 with audio requested degrades cleanly — `audio=false`, video continues,
/// cause `api_below_min` — whatever the latch and outcome.
#[kani::proof]
fn k11_g8_4_api_28_to_32_degrades_to_video_and_logs() {
    let api: u32 = kani::any();
    kani::assume((28..=32).contains(&api));
    let verdict = evaluate(AudioPolicyInput {
        api: Some(api),
        mode: CaptureMode::VideoAndLogs,
        requested: true,
        runtime_disabled: kani::any(),
        outcome: any_outcome(),
    });
    assert!(
        verdict.policy == AudioLaunchPolicy::ProactiveOff { api },
        "API 28..=32 is proactively off"
    );
    assert!(
        !verdict.policy.launches_audio(),
        "never audio=true below API 33"
    );
    assert!(verdict.video_expected, "video continues");
    assert!(
        verdict.cause == AudioCause::ApiBelowMin,
        "cause is api_below_min"
    );
    assert!(
        verdict.state == AudioCaptureState::Unavailable,
        "state is unavailable"
    );
    assert!(!verdict.next_launch_audio, "no later launch asks for audio");
}

/// G8-9: `logs_only` or audio not requested is `disabled` for every API, latch and outcome.
#[kani::proof]
fn k11_g8_9_disabled_modes_ignore_every_outcome() {
    let api: Option<u32> = if kani::any() { Some(kani::any()) } else { None };
    let input = any_input(api);
    kani::assume(input.mode == CaptureMode::LogsOnly || !input.requested);
    let verdict = evaluate(input);
    assert!(
        verdict.state == AudioCaptureState::Disabled,
        "state is disabled"
    );
    assert!(
        verdict.policy == AudioLaunchPolicy::Disabled,
        "policy is disabled"
    );
    assert!(!verdict.next_launch_audio, "never asks for audio");
    assert!(
        verdict.cause == AudioCause::LogsOnly || verdict.cause == AudioCause::UserDisabled,
        "cause is logs_only or user_disabled"
    );
    assert!(
        verdict.video_expected == (input.mode == CaptureMode::VideoAndLogs),
        "video only without logs_only"
    );
}

/// Number of (re)connect launches in the bounded G8-6 model.
const K: usize = 4;

/// G8-6: bounded reconnect supervisor (k <= 4 launches, device boots only move forward, the
/// user may stop → start before any launch). After a fatal audio outcome, no later launch of
/// the same `(serial, boot_id)` asks for audio until an explicit restart, and after a restart
/// (or on a new boot) the first launch asks for audio again.
#[kani::proof]
#[kani::unwind(5)]
fn k11_g8_6_fatal_audio_failure_is_sticky_until_restart() {
    let api: u32 = kani::any();
    kani::assume(api >= 33);
    let mut latch: StickyAudioLatch<u8> = StickyAudioLatch::new();
    // Oracle state: boot that saw a fatal outcome since the last explicit restart.
    let mut fatal_boot: Option<u8> = None;
    let mut boot: u8 = 0;
    for _ in 0..K {
        if kani::any() {
            // The device rebooted: a new boot id (boots never come back).
            boot += 1;
        }
        let restart: bool = kani::any();
        if restart {
            latch.reset();
            fatal_boot = None;
        }
        let runtime_disabled = latch.is_disabled_for(&boot);
        let policy = plan_requested_launch(Some(api), latch.state_for(&boot));
        let launches_audio = policy.launches_audio();
        if fatal_boot == Some(boot) {
            assert!(!launches_audio, "a fatal boot never relaunches audio=true");
        } else {
            assert!(launches_audio, "a healthy or restarted boot asks for audio");
        }
        let outcome = if launches_audio {
            any_outcome()
        } else {
            ScrcpyAudioOutcome::NotAttempted
        };
        let verdict = evaluate(AudioPolicyInput {
            api: Some(api),
            mode: CaptureMode::VideoAndLogs,
            requested: true,
            runtime_disabled,
            outcome,
        });
        assert!(
            verdict.video_expected || outcome == ScrcpyAudioOutcome::LaunchFailedBoth,
            "video continues unless both launches failed"
        );
        if oracle_is_fatal(outcome) {
            assert!(
                !verdict.next_launch_audio,
                "a fatal outcome turns the next launch off"
            );
            fatal_boot = Some(boot);
        }
        if !launches_audio {
            assert!(
                verdict.cause == AudioCause::RuntimeStickyDisabled,
                "a sticky launch reports runtime_sticky_disabled"
            );
        }
        latch.record(boot, outcome);
    }
}
