//! Pure live audio-capture policy (PDD G8).
//!
//! This module is the single decision source for live audio capture. `arb-adb` derives the
//! `scrcpy-server` audio arguments from [`MIN_AUDIO_PLAYBACK_API_LEVEL`], `arbd` plans each
//! launch with [`plan_launch`] and classifies every runtime outcome with [`evaluate`], and the
//! IDE `client-core` mirrors [`AudioCause::label_class`] through the shared golden table
//! `docs/specs/audio-policy-table.json`.
//!
//! The policy keeps two kinds of "no audio" strictly apart:
//! - [`AudioCause::ApiBelowMin`]: the device OS cannot capture audio without muting its own
//!   playback (Android 9–12, API 28–32). Only this cause may be presented as "unsupported".
//! - runtime failures on a capable device (API 33+ or unknown API): the device was asked for
//!   audio and something concrete failed (`codec_id = 0` / `1`, tunnel, launch). These always
//!   carry their own cause and are never reported as an OS limitation.

use serde::{Deserialize, Serialize};

use crate::config::CaptureMode;

/// Minimum Android API level for non-muting `audio_source=playback audio_dup=true`
/// (Android 13 / API 33).
///
/// `scrcpy-server` itself can capture audio from API 30, but below API 33 only by muting the
/// device's own playback, so ARB never captures audio below this level.
pub const MIN_AUDIO_PLAYBACK_API_LEVEL: u32 = 33;

/// Maximum number of bytes of `scrcpy-server` stderr kept in an `audio_reason`.
pub const MAX_STDERR_TAIL_BYTES: usize = 600;

/// Live audio capture availability reported in `capture.audio_state`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCaptureState {
    /// Audio availability has not been determined yet (idle or connecting).
    #[default]
    Unknown,
    /// Audio capture is active and streaming AAC frames.
    Available,
    /// Audio capture was intentionally disabled (`audio_enabled=false` or `logs_only`).
    Disabled,
    /// Audio capture is not running for the reason in [`AudioCause`]; video + logs + taps
    /// continue.
    Unavailable,
}

impl AudioCaptureState {
    /// Stable wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Available => "available",
            Self::Disabled => "disabled",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Machine-readable reason behind the live audio state (`capture.audio_cause`).
///
/// Clients must decide user-facing wording from this value, never from the free-text
/// `audio_reason`. Values unknown to an older client deserialize as [`AudioCause::Unknown`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCause {
    /// No audio problem (capturing, or not decided yet).
    #[default]
    None,
    /// The user turned audio off (`audio_enabled=false`).
    UserDisabled,
    /// `logs_only` capture mode (no `scrcpy-server` at all).
    LogsOnly,
    /// The device API level is below [`MIN_AUDIO_PLAYBACK_API_LEVEL`].
    ApiBelowMin,
    /// `scrcpy-server` reported `codec_id = 0`: playback capture could not start
    /// (`AudioPlaybackCapture` / `registerAudioPolicy` failed). Non-fatal; video continues.
    DeviceCaptureFailed,
    /// `scrcpy-server` reported `codec_id = 1`: a fatal audio encoder/configuration error that
    /// also terminates the video stream.
    EncoderFatal,
    /// The audio tunnel closed or timed out during its codec handshake.
    HandshakeIo,
    /// The audio tunnel socket could not be connected.
    TunnelConnectFailed,
    /// `scrcpy-server` failed to start with `audio=true` but started with `audio=false`.
    LaunchFailedAudioTrue,
    /// `scrcpy-server` failed to start with both `audio=true` and `audio=false`.
    LaunchFailedBoth,
    /// Audio stays off for this device boot after a fatal audio failure (sticky degradation).
    RuntimeStickyDisabled,
    /// A cause this build does not know (forward compatibility).
    #[serde(other)]
    Unknown,
}

/// User-facing presentation class of an [`AudioCause`], shared with the IDE.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioLabelClass {
    /// No audio banner.
    None,
    /// Audio intentionally off (no warning banner).
    Disabled,
    /// "Unsupported" — the device OS is below the audio floor.
    UnsupportedApi,
    /// A concrete runtime failure on a capable device.
    RuntimeFailure,
}

impl AudioLabelClass {
    /// Stable wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Disabled => "disabled",
            Self::UnsupportedApi => "unsupported_api",
            Self::RuntimeFailure => "runtime_failure",
        }
    }
}

impl AudioCause {
    /// Every cause, in wire order (used by the shared golden table).
    pub const ALL: [Self; 12] = [
        Self::None,
        Self::UserDisabled,
        Self::LogsOnly,
        Self::ApiBelowMin,
        Self::DeviceCaptureFailed,
        Self::EncoderFatal,
        Self::HandshakeIo,
        Self::TunnelConnectFailed,
        Self::LaunchFailedAudioTrue,
        Self::LaunchFailedBoth,
        Self::RuntimeStickyDisabled,
        Self::Unknown,
    ];

    /// Stable wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::UserDisabled => "user_disabled",
            Self::LogsOnly => "logs_only",
            Self::ApiBelowMin => "api_below_min",
            Self::DeviceCaptureFailed => "device_capture_failed",
            Self::EncoderFatal => "encoder_fatal",
            Self::HandshakeIo => "handshake_io",
            Self::TunnelConnectFailed => "tunnel_connect_failed",
            Self::LaunchFailedAudioTrue => "launch_failed_audio_true",
            Self::LaunchFailedBoth => "launch_failed_both",
            Self::RuntimeStickyDisabled => "runtime_sticky_disabled",
            Self::Unknown => "unknown",
        }
    }

    /// Presentation class: only [`AudioCause::ApiBelowMin`] is "unsupported".
    #[must_use]
    pub const fn label_class(self) -> AudioLabelClass {
        match self {
            Self::None => AudioLabelClass::None,
            Self::UserDisabled | Self::LogsOnly => AudioLabelClass::Disabled,
            Self::ApiBelowMin => AudioLabelClass::UnsupportedApi,
            Self::DeviceCaptureFailed
            | Self::EncoderFatal
            | Self::HandshakeIo
            | Self::TunnelConnectFailed
            | Self::LaunchFailedAudioTrue
            | Self::LaunchFailedBoth
            | Self::RuntimeStickyDisabled
            | Self::Unknown => AudioLabelClass::RuntimeFailure,
        }
    }
}

/// What happened to the audio side of one `scrcpy-server` launch.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrcpyAudioOutcome {
    /// Audio was not requested from `scrcpy-server` (or the result is still pending).
    NotAttempted,
    /// The audio tunnel negotiated AAC and streams frames.
    Aac,
    /// The audio tunnel reported `codec_id = 0` (playback capture failed, non-fatal).
    Code0,
    /// The audio tunnel reported `codec_id = 1` (fatal encoder/configuration error).
    Code1,
    /// The audio tunnel closed or timed out during its handshake.
    HandshakeIo,
    /// The audio tunnel socket could not be connected.
    ConnectFailed,
    /// The `audio=true` launch failed and the `audio=false` retry started.
    LaunchFailedAudioOk,
    /// Both the `audio=true` launch and the `audio=false` retry failed.
    LaunchFailedBoth,
}

impl ScrcpyAudioOutcome {
    /// Every outcome (used by the shared golden table and the Kani harnesses).
    pub const ALL: [Self; 8] = [
        Self::NotAttempted,
        Self::Aac,
        Self::Code0,
        Self::Code1,
        Self::HandshakeIo,
        Self::ConnectFailed,
        Self::LaunchFailedAudioOk,
        Self::LaunchFailedBoth,
    ];

    /// `true` for failures after which the same device boot must not be asked for audio again:
    /// `codec_id = 1` terminates `scrcpy-server` (video included) and a failed `audio=true`
    /// launch already proved that `audio=false` is what works.
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        matches!(self, Self::Code1 | Self::LaunchFailedAudioOk)
    }
}

/// How the next `scrcpy-server` launch treats audio.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioLaunchPolicy {
    /// Audio off by user config or `logs_only` (`audio=false`, or no `scrcpy-server`).
    Disabled,
    /// The device API is below [`MIN_AUDIO_PLAYBACK_API_LEVEL`]: launch with `audio=false`.
    ProactiveOff {
        /// Detected Android API level.
        api: u32,
    },
    /// A fatal audio failure already happened on this device boot: launch with `audio=false`.
    RuntimeStickyOff,
    /// Launch with `audio=true audio_source=playback audio_dup=true`.
    TryAudio,
}

impl AudioLaunchPolicy {
    /// Stable identifier used by the shared golden table.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::ProactiveOff { .. } => "proactive_off",
            Self::RuntimeStickyOff => "runtime_sticky_off",
            Self::TryAudio => "try_audio",
        }
    }

    /// `true` when the launch passes `audio=true` to `scrcpy-server`.
    #[must_use]
    pub const fn launches_audio(self) -> bool {
        matches!(self, Self::TryAudio)
    }
}

/// Inputs of one audio policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioPolicyInput {
    /// Detected Android API level (`None` or `Some(0)` when unknown).
    pub api: Option<u32>,
    /// Live capture mode.
    pub mode: CaptureMode,
    /// Whether the user asked for audio (`audio_enabled`).
    pub requested: bool,
    /// Whether a fatal audio failure already happened on this device boot.
    pub runtime_disabled: bool,
    /// What happened to the audio side of the launch.
    pub outcome: ScrcpyAudioOutcome,
}

/// Result of one audio policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioVerdict {
    /// How the launch treats audio.
    pub policy: AudioLaunchPolicy,
    /// Reported `capture.audio_state`.
    pub state: AudioCaptureState,
    /// Reported `capture.audio_cause`.
    pub cause: AudioCause,
    /// Whether the next (re)connect launch of the same device boot may ask for audio.
    pub next_launch_audio: bool,
    /// Whether video is expected to keep streaming.
    pub video_expected: bool,
}

/// Returns `true` when `api` is a known level below the audio floor.
#[must_use]
pub const fn api_is_below_audio_floor(api: Option<u32>) -> bool {
    match api {
        Some(level) => level > 0 && level < MIN_AUDIO_PLAYBACK_API_LEVEL,
        None => false,
    }
}

/// Whether the sticky latch turned audio off for the current device boot.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RuntimeAudioLatch {
    /// No fatal audio failure on this device boot.
    #[default]
    Clear,
    /// A fatal audio failure latched audio off for this device boot.
    StickyOff,
}

/// Plans how the next `scrcpy-server` launch treats audio (the outcome is ignored).
///
/// An unknown API (`None` / `Some(0)`) is attempted with audio: only a known level below
/// [`MIN_AUDIO_PLAYBACK_API_LEVEL`] is proactively turned off.
#[must_use]
pub const fn plan_launch(input: AudioPolicyInput) -> AudioLaunchPolicy {
    if matches!(input.mode, CaptureMode::LogsOnly) || !input.requested {
        return AudioLaunchPolicy::Disabled;
    }
    if let Some(level) = input.api
        && api_is_below_audio_floor(input.api)
    {
        return AudioLaunchPolicy::ProactiveOff { api: level };
    }
    if input.runtime_disabled {
        AudioLaunchPolicy::RuntimeStickyOff
    } else {
        AudioLaunchPolicy::TryAudio
    }
}

/// Plans a video launch with audio requested on `api` under the sticky `latch`.
#[must_use]
pub const fn plan_requested_launch(
    api: Option<u32>,
    latch: RuntimeAudioLatch,
) -> AudioLaunchPolicy {
    plan_launch(AudioPolicyInput {
        api,
        mode: CaptureMode::VideoAndLogs,
        requested: true,
        runtime_disabled: matches!(latch, RuntimeAudioLatch::StickyOff),
        outcome: ScrcpyAudioOutcome::NotAttempted,
    })
}

/// Classifies one launch into the reported audio state and cause.
///
/// The outcome only matters for [`AudioLaunchPolicy::TryAudio`]; every other policy never asks
/// `scrcpy-server` for audio, so its state and cause are fixed by the policy.
#[must_use]
pub const fn evaluate(input: AudioPolicyInput) -> AudioVerdict {
    let policy = plan_launch(input);
    let logs_only = matches!(input.mode, CaptureMode::LogsOnly);
    let (state, cause, next_launch_audio, video_expected) = match policy {
        AudioLaunchPolicy::Disabled => (
            AudioCaptureState::Disabled,
            if logs_only {
                AudioCause::LogsOnly
            } else {
                AudioCause::UserDisabled
            },
            false,
            !logs_only,
        ),
        AudioLaunchPolicy::ProactiveOff { .. } => (
            AudioCaptureState::Unavailable,
            AudioCause::ApiBelowMin,
            false,
            true,
        ),
        AudioLaunchPolicy::RuntimeStickyOff => (
            AudioCaptureState::Unavailable,
            AudioCause::RuntimeStickyDisabled,
            false,
            true,
        ),
        AudioLaunchPolicy::TryAudio => match input.outcome {
            ScrcpyAudioOutcome::NotAttempted => {
                (AudioCaptureState::Unknown, AudioCause::None, true, true)
            }
            ScrcpyAudioOutcome::Aac => (AudioCaptureState::Available, AudioCause::None, true, true),
            ScrcpyAudioOutcome::Code0 => (
                AudioCaptureState::Unavailable,
                AudioCause::DeviceCaptureFailed,
                true,
                true,
            ),
            ScrcpyAudioOutcome::Code1 => (
                AudioCaptureState::Unavailable,
                AudioCause::EncoderFatal,
                false,
                true,
            ),
            ScrcpyAudioOutcome::HandshakeIo => (
                AudioCaptureState::Unavailable,
                AudioCause::HandshakeIo,
                true,
                true,
            ),
            ScrcpyAudioOutcome::ConnectFailed => (
                AudioCaptureState::Unavailable,
                AudioCause::TunnelConnectFailed,
                true,
                true,
            ),
            ScrcpyAudioOutcome::LaunchFailedAudioOk => (
                AudioCaptureState::Unavailable,
                AudioCause::LaunchFailedAudioTrue,
                false,
                true,
            ),
            ScrcpyAudioOutcome::LaunchFailedBoth => (
                AudioCaptureState::Unavailable,
                AudioCause::LaunchFailedBoth,
                true,
                false,
            ),
        },
    };
    AudioVerdict {
        policy,
        state,
        cause,
        next_launch_audio,
        video_expected,
    }
}

/// Sticky "audio off for this device boot" latch (PDD G8-6).
///
/// `K` identifies a device boot (`arbd` uses the `boot_id`; one latch lives per capture start
/// on one serial). After a fatal outcome ([`ScrcpyAudioOutcome::is_fatal`]) the same key is
/// never launched with audio again until [`StickyAudioLatch::reset`] (explicit stop → start).
/// A different key (another boot) is not affected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickyAudioLatch<K> {
    disabled_for: Option<K>,
}

impl<K> Default for StickyAudioLatch<K> {
    fn default() -> Self {
        Self { disabled_for: None }
    }
}

impl<K: PartialEq> StickyAudioLatch<K> {
    /// Creates an empty latch.
    #[must_use]
    pub const fn new() -> Self {
        Self { disabled_for: None }
    }

    /// Whether audio is latched off for `key`.
    #[must_use]
    pub fn is_disabled_for(&self, key: &K) -> bool {
        self.disabled_for.as_ref() == Some(key)
    }

    /// Latch state for `key`.
    #[must_use]
    pub fn state_for(&self, key: &K) -> RuntimeAudioLatch {
        if self.is_disabled_for(key) {
            RuntimeAudioLatch::StickyOff
        } else {
            RuntimeAudioLatch::Clear
        }
    }

    /// Records the outcome of a launch for `key`; returns `true` when it latched audio off.
    pub fn record(&mut self, key: K, outcome: ScrcpyAudioOutcome) -> bool {
        if outcome.is_fatal() {
            self.disabled_for = Some(key);
            true
        } else {
            false
        }
    }

    /// Clears the latch (explicit capture stop → start).
    pub fn reset(&mut self) {
        self.disabled_for = None;
    }
}

/// Honest reason for proactively disabling audio on `api`, or `None` when `api` is unknown
/// (`0`) or at least [`MIN_AUDIO_PLAYBACK_API_LEVEL`].
#[must_use]
pub fn api_below_min_reason(api: u32) -> Option<String> {
    api_is_below_audio_floor(Some(api)).then(|| {
        format!(
            "Audio capture requires Android 13+ (API 33+, non-muting playback capture); detected API {api}. Continuing with video + logs + taps."
        )
    })
}

/// Normalizes `scrcpy-server` stderr into one line (`" | "`-joined non-empty trimmed lines)
/// and keeps at most `max_bytes` of its tail (prefixed with `…` when cut). Returns `None` when
/// nothing remains.
#[must_use]
pub fn stderr_tail(stderr: &str, max_bytes: usize) -> Option<String> {
    let joined = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    if joined.is_empty() || max_bytes == 0 {
        return None;
    }
    if joined.len() <= max_bytes {
        return Some(joined);
    }
    let mut start = joined.len() - max_bytes;
    while !joined.is_char_boundary(start) {
        start += 1;
    }
    Some(format!("…{}", &joined[start..]))
}

/// Builds the `audio_reason` for a runtime audio failure: a cause-specific explanation, the
/// optional host-side `detail`, and the tail of `scrcpy-server` stderr (PDD G8-5).
#[must_use]
pub fn runtime_failure_reason(
    cause: AudioCause,
    detail: Option<&str>,
    stderr: Option<&str>,
) -> String {
    let detail = detail.map(str::trim).filter(|d| !d.is_empty());
    let with_detail = |text: &str| match detail {
        Some(d) => format!("{text} ({d})"),
        None => text.to_owned(),
    };
    let mut reason = match cause {
        AudioCause::DeviceCaptureFailed => with_detail(
            "Device could not start playback audio capture (scrcpy AudioPlaybackCapture failed, codec_id = 0); another capture or mirroring session, or an OEM AudioPolicy restriction, may be holding it",
        ),
        AudioCause::EncoderFatal => with_detail(
            "Device audio encoder failed fatally (codec_id = 1); audio is turned off for this device boot",
        ),
        AudioCause::HandshakeIo => with_detail("Audio tunnel closed during its handshake"),
        AudioCause::TunnelConnectFailed => with_detail("Audio tunnel could not connect"),
        AudioCause::LaunchFailedAudioTrue => with_detail(
            "scrcpy-server failed to start with audio=true and was restarted with audio=false; audio is turned off for this device boot",
        ),
        AudioCause::LaunchFailedBoth => with_detail(
            "scrcpy-server failed to start with audio=true and with audio=false; no video or audio is captured",
        ),
        AudioCause::RuntimeStickyDisabled => with_detail(
            "Audio stays off for this device boot after a fatal audio failure; stop and start capture to retry audio",
        ),
        AudioCause::None
        | AudioCause::UserDisabled
        | AudioCause::LogsOnly
        | AudioCause::ApiBelowMin
        | AudioCause::Unknown => with_detail("Audio capture unavailable"),
    };
    if let Some(tail) = stderr.and_then(|s| stderr_tail(s, MAX_STDERR_TAIL_BYTES)) {
        reason.push_str("; scrcpy stderr: ");
        reason.push_str(&tail);
    }
    if matches!(cause, AudioCause::LaunchFailedBoth) {
        reason.push_str(". Logs + taps continue.");
    } else {
        reason.push_str(". Continuing with video + logs + taps.");
    }
    reason
}

#[cfg(test)]
mod tests;
