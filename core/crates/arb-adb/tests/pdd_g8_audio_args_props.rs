//! Layer-3 PDD (G8-3): the `scrcpy-server` audio arguments follow the shared audio policy.
//!
//! For every API level from the audio floor (33) to `u32::MAX`, an audio-enabled launch passes
//! the non-muting playback triple `audio=true audio_source=playback audio_dup=true`; below the
//! floor it passes `audio=false` and never any audio source.

use arb_adb::scrcpy::{
    AudioCaptureMode, AudioTunnelStatus, CODEC_ID_AAC, MIN_AUDIO_PLAYBACK_API_LEVEL,
    ScrcpyLaunchOptions,
};
use arb_core::audio_policy::{AudioLaunchPolicy, RuntimeAudioLatch, plan_requested_launch};
use proptest::prelude::*;

fn args_for(api: u32, mode: AudioCaptureMode) -> (Vec<String>, AudioTunnelStatus) {
    let mut opts = ScrcpyLaunchOptions::fhd30("1234abcd");
    opts.audio_mode = mode;
    opts.build_server_args(api)
}

fn has(args: &[String], arg: &str) -> bool {
    args.iter().any(|a| a == arg)
}

#[test]
fn g8_3_floor_is_the_shared_policy_constant() {
    assert_eq!(
        MIN_AUDIO_PLAYBACK_API_LEVEL,
        arb_core::audio_policy::MIN_AUDIO_PLAYBACK_API_LEVEL,
        "arb-adb uses the arb-core audio floor"
    );
    let (args, status) = args_for(33, AudioCaptureMode::Enabled);
    assert!(has(&args, "audio=true"), "API 33 asks for audio: {args:?}");
    assert_eq!(
        status,
        AudioTunnelStatus::Available {
            codec_id: CODEC_ID_AAC
        },
        "API 33 expects an AAC tunnel"
    );
}

proptest! {
    /// G8-3: API >= 33 with audio requested passes the non-muting playback triple, and agrees
    /// with `arb_core::audio_policy::plan_launch`.
    #[test]
    fn g8_3_api_33_plus_passes_playback_dup_audio_args(api in MIN_AUDIO_PLAYBACK_API_LEVEL..=u32::MAX) {
        let (args, status) = args_for(api, AudioCaptureMode::Enabled);
        prop_assert!(has(&args, "audio=true"), "{:?}", args);
        prop_assert!(has(&args, "audio_source=playback"), "{:?}", args);
        prop_assert!(has(&args, "audio_dup=true"), "{:?}", args);
        prop_assert!(!has(&args, "audio=false"), "{:?}", args);
        prop_assert_eq!(status, AudioTunnelStatus::Available { codec_id: CODEC_ID_AAC });
        prop_assert_eq!(
            plan_requested_launch(Some(api), RuntimeAudioLatch::Clear),
            AudioLaunchPolicy::TryAudio
        );
    }

    /// Below the floor (and for a disabled launch at any level) no audio source is requested.
    #[test]
    fn g8_3_below_floor_or_disabled_passes_audio_false(
        api in 0u32..MIN_AUDIO_PLAYBACK_API_LEVEL,
        any_api in any::<u32>(),
    ) {
        let (args, status) = args_for(api, AudioCaptureMode::Enabled);
        prop_assert!(has(&args, "audio=false"), "{:?}", args);
        prop_assert!(!args.iter().any(|a| a.starts_with("audio_source=")), "{:?}", args);
        prop_assert_eq!(
            status,
            AudioTunnelStatus::UnavailableApiLevel { api_level: api, min_required: MIN_AUDIO_PLAYBACK_API_LEVEL }
        );
        let (disabled, _) = args_for(any_api, AudioCaptureMode::Disabled);
        prop_assert!(has(&disabled, "audio=false"), "{:?}", disabled);
        prop_assert!(!has(&disabled, "audio=true"), "{:?}", disabled);
    }
}
