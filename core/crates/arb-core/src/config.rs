//! Validated, schema-versioned user configuration (`config.json`) for ARB.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::device::validate_adb_serial;
use crate::state::{DEFAULT_MAX_RING_DURATION_NS, RingBudgetPolicy};

/// Current schema version persisted in `config.json`.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// Default lookback save window in real elapsed seconds (`5 minutes`).
pub const DEFAULT_SAVE_WINDOW_SECS: u32 = 300;
/// Minimum allowed save window in real elapsed seconds.
pub const MIN_SAVE_WINDOW_SECS: u32 = 5;
/// Maximum allowed save window in real elapsed seconds (`10 minutes`).
pub const MAX_SAVE_WINDOW_SECS: u32 = 600;

/// Default live ring buffer byte budget (`1.5 GiB`).
pub const DEFAULT_RING_BUDGET_BYTES: u64 = 1_610_612_736;
/// Minimum allowed live ring buffer byte budget (`16 MiB`).
pub const MIN_RING_BUDGET_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum allowed live ring buffer byte budget (`64 GiB`).
pub const MAX_RING_BUDGET_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Default minimum free host disk space floor (`3 GiB`).
pub const DEFAULT_MIN_FREE_DISK_BYTES: u64 = 3_221_225_472;
/// Minimum allowed free host disk space safety floor (`64 MiB`).
pub const MIN_FREE_DISK_FLOOR_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum allowed free host disk space safety floor (`1 TiB`).
pub const MAX_FREE_DISK_FLOOR_BYTES: u64 = 1_024 * 1024 * 1024 * 1024;

/// Default maximum encoded video dimension in pixels (`1080p` long edge).
pub const DEFAULT_VIDEO_MAX_SIZE: u32 = 1_920;
/// Minimum allowed video max dimension in pixels.
pub const MIN_VIDEO_MAX_SIZE: u32 = 256;
/// Maximum allowed video max dimension in pixels (`4K` long edge).
pub const MAX_VIDEO_MAX_SIZE: u32 = 4_096;

/// Default target maximum video frame rate in frames per second.
pub const DEFAULT_VIDEO_MAX_FPS: u32 = 30;
/// Minimum allowed video frame rate in frames per second.
pub const MIN_VIDEO_MAX_FPS: u32 = 1;
/// Maximum allowed video frame rate in frames per second.
pub const MAX_VIDEO_MAX_FPS: u32 = 120;

/// Default cooldown between automatic crash-triggered saves in seconds.
pub const DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS: u32 = 15;
/// Minimum allowed crash auto-save cooldown in seconds (prevents crash-loop spam).
pub const MIN_CRASH_AUTOSAVE_COOLDOWN_SECS: u32 = 1;
/// Maximum allowed crash auto-save cooldown in seconds (`1 hour`).
pub const MAX_CRASH_AUTOSAVE_COOLDOWN_SECS: u32 = 3_600;

/// Continuous capture payload mode for live `arbd` sessions.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Capture H.264 video, optional AAC audio, logcat, and touch input.
    #[default]
    VideoAndLogs,
    /// Low-overhead continuous capture: logcat + clock probe only (zero `MediaCodec` encoder load).
    LogsOnly,
}

impl CaptureMode {
    /// Stable `snake_case` identifier for CLI/JSON interchange.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VideoAndLogs => "video_and_logs",
            Self::LogsOnly => "logs_only",
        }
    }
}

impl fmt::Display for CaptureMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CaptureMode {
    type Err = ConfigValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "video_and_logs" => Ok(Self::VideoAndLogs),
            "logs_only" => Ok(Self::LogsOnly),
            other => Err(ConfigValidationError::InvalidEnumValue {
                field: "capture_mode",
                value: other.to_owned(),
                expected: "video_and_logs | logs_only",
            }),
        }
    }
}

/// Default action taken by the IDE plugin after a manual save completes.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeDefaultSaveAction {
    /// Save the bundle and immediately open it in the viewer.
    #[default]
    SaveAndOpen,
    /// Save the bundle without stealing editor focus.
    SaveOnly,
}

impl IdeDefaultSaveAction {
    /// Stable `snake_case` identifier for CLI/JSON interchange.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SaveAndOpen => "save_and_open",
            Self::SaveOnly => "save_only",
        }
    }
}

impl fmt::Display for IdeDefaultSaveAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for IdeDefaultSaveAction {
    type Err = ConfigValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "save_and_open" => Ok(Self::SaveAndOpen),
            "save_only" => Ok(Self::SaveOnly),
            other => Err(ConfigValidationError::InvalidEnumValue {
                field: "ide_default_save_action",
                value: other.to_owned(),
                expected: "save_and_open | save_only",
            }),
        }
    }
}

/// Validation failure returned by [`ArbConfig::validate`] or [`ArbConfig::apply_patch`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigValidationError {
    /// `config_version` was not [`CONFIG_SCHEMA_VERSION`].
    #[error("unsupported config_version {found}; expected {CONFIG_SCHEMA_VERSION}")]
    UnsupportedVersion {
        /// Observed schema version.
        found: u32,
    },
    /// Numeric field fell outside its permitted inclusive range.
    #[error("config field '{field}' value {value} is out of range ({min}..={max})")]
    OutOfRange {
        /// Field name.
        field: &'static str,
        /// Rejected value.
        value: u64,
        /// Minimum inclusive bound.
        min: u64,
        /// Maximum inclusive bound.
        max: u64,
    },
    /// Path field was empty or relative.
    #[error("config field '{field}' is invalid: {reason}")]
    InvalidPath {
        /// Field name.
        field: &'static str,
        /// Explanation of the violation.
        reason: &'static str,
    },
    /// Enum string value was unrecognized.
    #[error("invalid value '{value}' for '{field}'; expected {expected}")]
    InvalidEnumValue {
        /// Field name.
        field: &'static str,
        /// Rejected token.
        value: String,
        /// Allowed tokens.
        expected: &'static str,
    },
    /// ADB device serial failed safety validation.
    #[error("config field 'device_serial' is invalid ('{value}'): {reason}")]
    InvalidSerial {
        /// Rejected serial string.
        value: String,
        /// Explanation of the violation.
        reason: &'static str,
    },
}

/// Persisted user configuration stored atomically in `<data_dir>/config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArbConfig {
    /// Configuration schema version; must equal [`CONFIG_SCHEMA_VERSION`] (`1`).
    pub config_version: u32,
    /// Persistently arm capture and attach when a device becomes eligible.
    #[serde(default)]
    pub capture_enabled: bool,
    /// Continuous capture mode (`video_and_logs` default, or low-overhead `logs_only`).
    pub capture_mode: CaptureMode,
    /// Optional preferred ADB device serial (USB, Wi-Fi IP:port, IPv6, or mDNS TLS).
    /// When set, capture stays strictly bound to this serial and never auto-switches.
    pub device_serial: Option<String>,
    /// Default lookback save window in real elapsed seconds (`5..=600`, default `300`).
    pub save_window_secs: u32,
    /// Maximum live Zone-1 ring buffer byte budget (default `1.5 GiB`).
    pub ring_budget_bytes: u64,
    /// Minimum free host disk space safety floor in bytes (default `3 GiB`).
    pub min_free_disk_bytes: u64,
    /// Default maximum encoded video dimension in pixels (default `1920`).
    pub video_max_size: u32,
    /// Default maximum encoded video frame rate (default `30`).
    pub video_max_fps: u32,
    /// Whether playback audio capture is enabled by default on supported devices.
    pub audio_enabled: bool,
    /// Whether live `am_crash` events trigger background WORM saves.
    pub crash_autosave_enabled: bool,
    /// Minimum cooldown in seconds between crash-triggered auto-saves (default `15`).
    pub crash_autosave_cooldown_secs: u32,
    /// Optional absolute output directory override for saved bundles and screenshots.
    pub output_dir_override: Option<PathBuf>,
    /// Optional absolute output directory override exclusively for `save.screenshot`
    /// (falls back to `output_dir_override`, then `<data_dir>/screenshots`).
    pub screenshot_dir_override: Option<PathBuf>,
    /// Default post-save IDE plugin action (`save_and_open` or `save_only`).
    pub ide_default_save_action: IdeDefaultSaveAction,
}

impl ArbConfig {
    /// Creates a new configuration populated with all schema-v1 defaults.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            config_version: CONFIG_SCHEMA_VERSION,
            capture_enabled: false,
            capture_mode: CaptureMode::VideoAndLogs,
            device_serial: None,
            save_window_secs: DEFAULT_SAVE_WINDOW_SECS,
            ring_budget_bytes: DEFAULT_RING_BUDGET_BYTES,
            min_free_disk_bytes: DEFAULT_MIN_FREE_DISK_BYTES,
            video_max_size: DEFAULT_VIDEO_MAX_SIZE,
            video_max_fps: DEFAULT_VIDEO_MAX_FPS,
            audio_enabled: true,
            crash_autosave_enabled: true,
            crash_autosave_cooldown_secs: DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS,
            output_dir_override: None,
            screenshot_dir_override: None,
            ide_default_save_action: IdeDefaultSaveAction::SaveAndOpen,
        }
    }

    /// Validates all schema version, range, and path invariants.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigValidationError`] describing the first violated constraint.
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.config_version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigValidationError::UnsupportedVersion {
                found: self.config_version,
            });
        }
        if let Some(serial) = &self.device_serial {
            validate_adb_serial(serial).map_err(|err| match err {
                crate::device::SerialValidationError::Invalid { value, reason } => {
                    ConfigValidationError::InvalidSerial { value, reason }
                }
            })?;
        }
        check_range_u32(
            "save_window_secs",
            self.save_window_secs,
            MIN_SAVE_WINDOW_SECS,
            MAX_SAVE_WINDOW_SECS,
        )?;
        check_range_u64(
            "ring_budget_bytes",
            self.ring_budget_bytes,
            MIN_RING_BUDGET_BYTES,
            MAX_RING_BUDGET_BYTES,
        )?;
        check_range_u64(
            "min_free_disk_bytes",
            self.min_free_disk_bytes,
            MIN_FREE_DISK_FLOOR_BYTES,
            MAX_FREE_DISK_FLOOR_BYTES,
        )?;
        check_range_u32(
            "video_max_size",
            self.video_max_size,
            MIN_VIDEO_MAX_SIZE,
            MAX_VIDEO_MAX_SIZE,
        )?;
        check_range_u32(
            "video_max_fps",
            self.video_max_fps,
            MIN_VIDEO_MAX_FPS,
            MAX_VIDEO_MAX_FPS,
        )?;
        check_range_u32(
            "crash_autosave_cooldown_secs",
            self.crash_autosave_cooldown_secs,
            MIN_CRASH_AUTOSAVE_COOLDOWN_SECS,
            MAX_CRASH_AUTOSAVE_COOLDOWN_SECS,
        )?;
        if let Some(dir) = &self.output_dir_override {
            validate_override_dir("output_dir_override", dir)?;
        }
        if let Some(dir) = &self.screenshot_dir_override {
            validate_override_dir("screenshot_dir_override", dir)?;
        }
        Ok(())
    }

    /// Applies `patch` onto a copy of `self`, validates the candidate, and returns it.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigValidationError`] if the resulting configuration fails validation.
    pub fn apply_patch(&self, patch: &ArbConfigPatch) -> Result<Self, ConfigValidationError> {
        let mut next = self.clone();
        if let Some(enabled) = patch.capture_enabled {
            next.capture_enabled = enabled;
        }
        if let Some(mode) = patch.capture_mode {
            next.capture_mode = mode;
        }
        match &patch.device_serial {
            DeviceSerialPatch::Unchanged => {}
            DeviceSerialPatch::Clear => next.device_serial = None,
            DeviceSerialPatch::Set(serial) => next.device_serial = Some(serial.clone()),
        }
        if let Some(v) = patch.save_window_secs {
            next.save_window_secs = v;
        }
        if let Some(v) = patch.ring_budget_bytes {
            next.ring_budget_bytes = v;
        }
        if let Some(v) = patch.min_free_disk_bytes {
            next.min_free_disk_bytes = v;
        }
        if let Some(v) = patch.video_max_size {
            next.video_max_size = v;
        }
        if let Some(v) = patch.video_max_fps {
            next.video_max_fps = v;
        }
        if let Some(v) = patch.audio_enabled {
            next.audio_enabled = v;
        }
        if let Some(v) = patch.crash_autosave_enabled {
            next.crash_autosave_enabled = v;
        }
        if let Some(v) = patch.crash_autosave_cooldown_secs {
            next.crash_autosave_cooldown_secs = v;
        }
        match &patch.output_dir_override {
            OutputDirPatch::Unchanged => {}
            OutputDirPatch::Clear => next.output_dir_override = None,
            OutputDirPatch::Set(dir) => next.output_dir_override = Some(dir.clone()),
        }
        match &patch.screenshot_dir_override {
            OutputDirPatch::Unchanged => {}
            OutputDirPatch::Clear => next.screenshot_dir_override = None,
            OutputDirPatch::Set(dir) => next.screenshot_dir_override = Some(dir.clone()),
        }
        if let Some(action) = patch.ide_default_save_action {
            next.ide_default_save_action = action;
        }
        next.validate()?;
        Ok(next)
    }

    /// Projects this configuration into a Zone-1 [`RingBudgetPolicy`].
    #[must_use]
    pub const fn to_ring_budget_policy(&self) -> RingBudgetPolicy {
        RingBudgetPolicy {
            max_ring_bytes: self.ring_budget_bytes,
            max_ring_duration_ns: DEFAULT_MAX_RING_DURATION_NS,
            min_free_disk_bytes: self.min_free_disk_bytes,
        }
    }
}

impl Default for ArbConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Three-state patch for nullable `output_dir_override` in [`ArbConfigPatch`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OutputDirPatch {
    /// Field omitted in JSON: retain existing `output_dir_override`.
    #[default]
    Unchanged,
    /// Field explicitly set to `null`: clear `output_dir_override` back to `None`.
    Clear,
    /// Field set to a path string: update `output_dir_override` to `Some(path)`.
    Set(PathBuf),
}

impl OutputDirPatch {
    const fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

impl Serialize for OutputDirPatch {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Unchanged | Self::Clear => serializer.serialize_none(),
            Self::Set(path) => path.serialize(serializer),
        }
    }
}

fn deserialize_output_dir_patch<'de, D>(deserializer: D) -> Result<OutputDirPatch, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<PathBuf>::deserialize(deserializer)? {
        Some(path) => Ok(OutputDirPatch::Set(path)),
        None => Ok(OutputDirPatch::Clear),
    }
}

/// Three-state patch for nullable `device_serial` in [`ArbConfigPatch`].
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceSerialPatch {
    /// Field omitted in JSON: retain existing `device_serial`.
    #[default]
    Unchanged,
    /// Field explicitly set to `null` or empty string: clear `device_serial` back to `None`.
    Clear,
    /// Field set to a non-empty serial string: update `device_serial` to `Some(serial)`.
    Set(String),
}

impl DeviceSerialPatch {
    const fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

impl Serialize for DeviceSerialPatch {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Unchanged | Self::Clear => serializer.serialize_none(),
            Self::Set(serial) => serial.serialize(serializer),
        }
    }
}

fn deserialize_device_serial_patch<'de, D>(deserializer: D) -> Result<DeviceSerialPatch, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        Some(serial) if !serial.trim().is_empty() => Ok(DeviceSerialPatch::Set(serial)),
        _ => Ok(DeviceSerialPatch::Clear),
    }
}

/// Partial update patch accepted by `config.update`.
///
/// Omitted fields retain their existing values. Passing `"output_dir_override": null`,
/// `"screenshot_dir_override": null`, or `"device_serial": null` explicitly clears the field
/// back to `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArbConfigPatch {
    /// Arm or disarm the background capture supervisor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_enabled: Option<bool>,
    /// Updated continuous capture mode (`video_and_logs` or `logs_only`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_mode: Option<CaptureMode>,
    /// Updated preferred ADB device serial (`Unchanged`, `Clear`, or `Set(serial)`).
    #[serde(
        default,
        deserialize_with = "deserialize_device_serial_patch",
        skip_serializing_if = "DeviceSerialPatch::is_unchanged"
    )]
    pub device_serial: DeviceSerialPatch,
    /// Updated default lookback save window in seconds (`5..=600`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_window_secs: Option<u32>,
    /// Updated live ring byte budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_budget_bytes: Option<u64>,
    /// Updated minimum free host disk floor in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_free_disk_bytes: Option<u64>,
    /// Updated maximum encoded video dimension in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_max_size: Option<u32>,
    /// Updated maximum encoded video frame rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_max_fps: Option<u32>,
    /// Updated audio capture default toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_enabled: Option<bool>,
    /// Updated crash auto-save toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crash_autosave_enabled: Option<bool>,
    /// Updated crash auto-save debounce cooldown in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crash_autosave_cooldown_secs: Option<u32>,
    /// Updated output directory override (`Unchanged`, `Clear`, or `Set(path)`).
    #[serde(
        default,
        deserialize_with = "deserialize_output_dir_patch",
        skip_serializing_if = "OutputDirPatch::is_unchanged"
    )]
    pub output_dir_override: OutputDirPatch,
    /// Updated screenshot-only output directory override (`Unchanged`, `Clear`, or `Set(path)`).
    #[serde(
        default,
        deserialize_with = "deserialize_output_dir_patch",
        skip_serializing_if = "OutputDirPatch::is_unchanged"
    )]
    pub screenshot_dir_override: OutputDirPatch,
    /// Updated default post-save IDE action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ide_default_save_action: Option<IdeDefaultSaveAction>,
}

fn validate_override_dir(field: &'static str, dir: &Path) -> Result<(), ConfigValidationError> {
    if dir.as_os_str().is_empty() {
        return Err(ConfigValidationError::InvalidPath {
            field,
            reason: "path must not be empty",
        });
    }
    if !dir.is_absolute() {
        return Err(ConfigValidationError::InvalidPath {
            field,
            reason: "path must be absolute",
        });
    }
    if dir.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(ConfigValidationError::InvalidPath {
            field,
            reason: "path must not contain '..' segments",
        });
    }
    Ok(())
}

fn check_range_u32(
    field: &'static str,
    value: u32,
    min: u32,
    max: u32,
) -> Result<(), ConfigValidationError> {
    check_range_u64(field, u64::from(value), u64::from(min), u64::from(max))
}

const fn check_range_u64(
    field: &'static str,
    value: u64,
    min: u64,
    max: u64,
) -> Result<(), ConfigValidationError> {
    if value < min || value > max {
        Err(ConfigValidationError::OutOfRange {
            field,
            value,
            min,
            max,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::str::FromStr;

    use super::{
        ArbConfig, ArbConfigPatch, CONFIG_SCHEMA_VERSION, ConfigValidationError,
        DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS, DEFAULT_MIN_FREE_DISK_BYTES,
        DEFAULT_RING_BUDGET_BYTES, DEFAULT_SAVE_WINDOW_SECS, DEFAULT_VIDEO_MAX_FPS,
        DEFAULT_VIDEO_MAX_SIZE, IdeDefaultSaveAction,
    };

    #[test]
    fn default_config_matches_spec_values_and_validates() {
        let cfg = ArbConfig::default();
        assert_eq!(
            cfg.config_version, CONFIG_SCHEMA_VERSION,
            "default config_version must be 1"
        );
        assert_eq!(
            cfg.save_window_secs, DEFAULT_SAVE_WINDOW_SECS,
            "default save_window_secs must be 300"
        );
        assert_eq!(
            cfg.ring_budget_bytes, DEFAULT_RING_BUDGET_BYTES,
            "default ring_budget_bytes must be 1.5 GiB"
        );
        assert_eq!(
            cfg.min_free_disk_bytes, DEFAULT_MIN_FREE_DISK_BYTES,
            "default min_free_disk_bytes must be 3 GiB"
        );
        assert_eq!(
            cfg.video_max_size, DEFAULT_VIDEO_MAX_SIZE,
            "default video_max_size must be 1920"
        );
        assert_eq!(
            cfg.video_max_fps, DEFAULT_VIDEO_MAX_FPS,
            "default video_max_fps must be 30"
        );
        assert!(cfg.audio_enabled, "audio must default to enabled");
        assert!(
            cfg.crash_autosave_enabled,
            "crash auto-save must default to enabled"
        );
        assert_eq!(
            cfg.crash_autosave_cooldown_secs, DEFAULT_CRASH_AUTOSAVE_COOLDOWN_SECS,
            "default cooldown must be 15s"
        );
        assert_eq!(
            cfg.output_dir_override, None,
            "output_dir_override must default to None"
        );
        assert_eq!(
            cfg.ide_default_save_action,
            IdeDefaultSaveAction::SaveAndOpen,
            "default IDE save action must be SaveAndOpen"
        );
        assert_eq!(cfg.validate(), Ok(()), "default config must validate");

        let json = serde_json::to_string(&cfg).expect("serialize config");
        let parsed: ArbConfig = serde_json::from_str(&json).expect("deserialize config");
        assert_eq!(parsed, cfg, "config JSON round-trip must be lossless");
    }

    #[test]
    fn rejects_out_of_range_fields_and_relative_output_dir() {
        let bad_ver = ArbConfig {
            config_version: 2,
            ..ArbConfig::default()
        };
        assert!(
            matches!(
                bad_ver.validate(),
                Err(ConfigValidationError::UnsupportedVersion { found: 2 })
            ),
            "wrong schema version must fail"
        );

        let low_window = ArbConfig {
            save_window_secs: 4,
            ..ArbConfig::default()
        };
        assert!(
            matches!(
                low_window.validate(),
                Err(ConfigValidationError::OutOfRange {
                    field: "save_window_secs",
                    ..
                })
            ),
            "save_window_secs < 5 must fail"
        );

        let high_window = ArbConfig {
            save_window_secs: 601,
            ..ArbConfig::default()
        };
        assert!(
            matches!(
                high_window.validate(),
                Err(ConfigValidationError::OutOfRange {
                    field: "save_window_secs",
                    ..
                })
            ),
            "save_window_secs > 600 must fail"
        );

        let zero_cooldown = ArbConfig {
            crash_autosave_cooldown_secs: 0,
            ..ArbConfig::default()
        };
        assert!(
            matches!(
                zero_cooldown.validate(),
                Err(ConfigValidationError::OutOfRange {
                    field: "crash_autosave_cooldown_secs",
                    ..
                })
            ),
            "zero cooldown must fail to prevent crash-loop spam"
        );

        let rel_path = ArbConfig {
            output_dir_override: Some(PathBuf::from("relative/dir")),
            ..ArbConfig::default()
        };
        assert!(
            matches!(
                rel_path.validate(),
                Err(ConfigValidationError::InvalidPath {
                    field: "output_dir_override",
                    ..
                })
            ),
            "relative output_dir_override must fail"
        );
    }

    #[test]
    fn patch_updates_fields_and_distinguishes_omitted_vs_null_output_dir() {
        let base = ArbConfig::default();
        let abs = std::env::temp_dir().join("arb-test-override");

        let set_patch: ArbConfigPatch = serde_json::from_value(serde_json::json!({
            "save_window_secs": 120,
            "audio_enabled": false,
            "ide_default_save_action": "save_only",
            "output_dir_override": abs,
        }))
        .expect("parse set patch");
        let updated = base.apply_patch(&set_patch).expect("apply set patch");
        assert_eq!(updated.save_window_secs, 120, "save_window_secs updated");
        assert!(!updated.audio_enabled, "audio_enabled updated");
        assert_eq!(
            updated.ide_default_save_action,
            IdeDefaultSaveAction::SaveOnly,
            "ide_default_save_action updated"
        );
        assert_eq!(
            updated.output_dir_override.as_ref(),
            Some(&abs),
            "output_dir_override set"
        );

        // Omitting output_dir_override keeps the existing override.
        let keep_patch: ArbConfigPatch =
            serde_json::from_value(serde_json::json!({ "video_max_fps": 60 }))
                .expect("parse keep patch");
        let kept = updated.apply_patch(&keep_patch).expect("apply keep patch");
        assert_eq!(kept.video_max_fps, 60, "video_max_fps updated");
        assert_eq!(
            kept.output_dir_override.as_ref(),
            Some(&abs),
            "omitted output_dir_override must remain unchanged"
        );

        // Explicit null clears output_dir_override back to None.
        let clear_patch: ArbConfigPatch =
            serde_json::from_value(serde_json::json!({ "output_dir_override": null }))
                .expect("parse clear patch");
        let cleared = kept.apply_patch(&clear_patch).expect("apply clear patch");
        assert_eq!(
            cleared.output_dir_override, None,
            "null output_dir_override must clear override"
        );

        assert_eq!(
            IdeDefaultSaveAction::from_str("save_only"),
            Ok(IdeDefaultSaveAction::SaveOnly),
            "from_str save_only"
        );
        assert!(
            IdeDefaultSaveAction::from_str("unknown").is_err(),
            "unknown ide action must error"
        );
    }

    #[test]
    fn t6c_capture_mode_device_serial_and_screenshot_dir_override_roundtrip_and_validate() {
        use super::CaptureMode;

        // Backward compatibility: older config.json without T-6C fields deserializes cleanly.
        let legacy_json = r#"{"config_version":1,"save_window_secs":180}"#;
        let parsed: ArbConfig = serde_json::from_str(legacy_json).expect("legacy config");
        assert_eq!(parsed.capture_mode, CaptureMode::VideoAndLogs);
        assert_eq!(parsed.device_serial, None);
        assert_eq!(parsed.screenshot_dir_override, None);
        assert_eq!(parsed.save_window_secs, 180);

        assert_eq!(
            CaptureMode::from_str("logs_only"),
            Ok(CaptureMode::LogsOnly)
        );
        assert_eq!(
            CaptureMode::from_str("video_and_logs"),
            Ok(CaptureMode::VideoAndLogs)
        );
        assert!(CaptureMode::from_str("invalid").is_err());

        let shot_dir = std::env::temp_dir().join("arb-shot-override");
        let patch: ArbConfigPatch = serde_json::from_value(serde_json::json!({
            "capture_mode": "logs_only",
            "device_serial": "192.168.1.50:5555",
            "screenshot_dir_override": shot_dir,
        }))
        .expect("parse t6c patch");
        let updated = parsed.apply_patch(&patch).expect("apply t6c patch");
        assert_eq!(updated.capture_mode, CaptureMode::LogsOnly);
        assert_eq!(updated.device_serial.as_deref(), Some("192.168.1.50:5555"));
        assert_eq!(updated.screenshot_dir_override.as_ref(), Some(&shot_dir));

        // Clearing device_serial and screenshot_dir_override via null.
        let clear: ArbConfigPatch = serde_json::from_value(serde_json::json!({
            "device_serial": null,
            "screenshot_dir_override": null,
        }))
        .expect("parse clear patch");
        let cleared = updated.apply_patch(&clear).expect("apply clear patch");
        assert_eq!(cleared.device_serial, None);
        assert_eq!(cleared.screenshot_dir_override, None);
        assert_eq!(cleared.capture_mode, CaptureMode::LogsOnly);

        // Reject shell metacharacters in device_serial and traversal in screenshot_dir_override.
        let bad_serial = ArbConfig {
            device_serial: Some("dev;rm -rf /".to_owned()),
            ..ArbConfig::default()
        };
        assert!(matches!(
            bad_serial.validate(),
            Err(ConfigValidationError::InvalidSerial { .. })
        ));

        let bad_shot_dir = ArbConfig {
            screenshot_dir_override: Some(std::env::temp_dir().join("..").join("escape")),
            ..ArbConfig::default()
        };
        assert!(matches!(
            bad_shot_dir.validate(),
            Err(ConfigValidationError::InvalidPath {
                field: "screenshot_dir_override",
                ..
            })
        ));
    }
}
