//! Tray controller: connects to `arbd` over local IPC, polls its state on a configurable
//! interval, and derives a crisp, honest [`TrayViewState`] / menu model (`SPEC-01`, `SPEC-03`).
//!
//! `arb-tray` performs zero direct ring/catalog/media file writes: every mutation (`save.trigger`,
//! `save.screenshot`, `save.logs`, `save.retry`, `save.discard`, `bundle.trash`, `bundle.restore`,
//! `capture.start`/`capture.stop`, `system.shutdown`) is a JSON-RPC 2.0 call dispatched to `arbd`.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use arb_core::config::{CaptureMode, DEFAULT_SAVE_WINDOW_SECS, DEFAULT_VIDEO_MAX_FPS};
use arb_core::device::{DeviceSelectionOutcome, DeviceSelectionPolicy};
use arb_core::state::{BundleLifecycleState, SaveJobState};
use arb_os::paths::ArbPaths;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::rpc::{AutoStartMode, RpcClient, RpcClientError};

/// Default interval between `arbd` status refresh cycles.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum number of recent bundles surfaced in the `最近の保存` submenu.
pub const RECENT_BUNDLES_LIMIT: usize = 5;

const fn default_save_window_secs() -> u32 {
    DEFAULT_SAVE_WINDOW_SECS
}

/// Catalog bundle storage format badge shown in the Recent Saves menu.
///
/// Mirrors the wire string of `arb_storage::catalog::CatalogFormatVersion` without depending on
/// `arb-storage` directly, keeping `arb-tray`'s crate DAG limited to `arb-core` + `arb-os`
/// (`SPEC-01`): clients decode `arbd`'s JSON-RPC wire shapes, not its internal types.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleFormatBadge {
    /// Managed or external immutable WORM v2 bundle.
    V2,
    /// Legacy schema 3/4 folder opened strictly read-only.
    LegacyRo,
}

impl BundleFormatBadge {
    fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("v2") => Self::V2,
            _ => Self::LegacyRo,
        }
    }

    /// Short badge label shown next to each Recent Saves entry.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::V2 => "ARB",
            Self::LegacyRo => "Legacy (RO)",
        }
    }
}

/// Tray badge state shown by the OS native icon (`src/native_tray.rs`).
///
/// Precedence when multiple conditions hold simultaneously (most to least urgent):
/// `DaemonOffline` > `StoragePressure` > `Disconnected` > `SaveAttentionNeeded` > `Capturing` >
/// `Idle`. `derive_badge` implements and the module unit tests exercise this order.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayBadge {
    /// `arbd` is unreachable over local IPC (not running, or killed mid-session).
    DaemonOffline,
    /// Host storage pressure forced capture to stop (`StoragePressureStop`).
    StoragePressure,
    /// Capture is marked active but the device transport is currently disconnected.
    Disconnected,
    /// At least one save job is `FailedRetryable` and needs user attention.
    SaveAttentionNeeded,
    /// Capture is active and the device transport is connected.
    Capturing,
    /// `arbd` is reachable, nothing needs attention, and capture is not active.
    Idle,
}

/// Capture activity + device transport connectivity, as one tri-state value.
///
/// Replaces the two related `active`/`connected` booleans from `system.status.capture` with a
/// single enum (workspace lint policy forbids `bool` function parameters and discourages more
/// than a few `bool` struct fields): `Idle` (no session), `Capturing` (active + connected), and
/// `Disconnected` (active but the device transport dropped).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    /// No capture session is active.
    Idle,
    /// Capture is active and the device transport is connected.
    Capturing,
    /// Capture is active but the device transport is currently disconnected.
    Disconnected,
}

/// Host storage pressure state (`system.status.capture.storage_pressure_stopped`), as a
/// two-variant enum instead of a `bool` function parameter (see [`CaptureStatus`] doc).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePressureState {
    /// No storage pressure condition.
    Normal,
    /// Capture stopped due to host storage pressure (`StoragePressureStop`).
    Pressured,
}

/// One tracked save job surfaced in the Failed Save Jobs submenu and alert diffing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveJobSummary {
    /// Unique save job identifier.
    pub job_id: String,
    /// Lifecycle state reported by `arbd` (`save.list_jobs`).
    pub state: SaveJobState,
    /// Target bundle directory path.
    pub destination_path: PathBuf,
    /// Last error message when `state == FailedRetryable`.
    pub last_error: Option<String>,
}

/// One catalog bundle surfaced in the Recent Saves submenu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentBundleSummary {
    /// Globally unique bundle identifier.
    pub bundle_id: String,
    /// On-disk bundle directory path.
    pub path: PathBuf,
    /// Storage format badge (`ARB` or `Legacy (RO)`).
    pub format: BundleFormatBadge,
    /// Lifecycle state (`active`, `trashed`, `purging`).
    pub lifecycle: BundleLifecycleState,
    /// Creation timestamp in UTC (ISO-8601) used to order the latest saves newest-first.
    #[serde(default)]
    pub created_at_utc: String,
}

/// Derived, honest view of the live `arbd` state used to render the tray menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayViewState {
    /// Tray badge state (see [`TrayBadge`] precedence).
    pub badge: TrayBadge,
    /// `true` once `system.hello`/`system.status` succeeded on the current connection.
    pub daemon_online: bool,
    /// Capture activity + device transport connectivity.
    pub capture: CaptureStatus,
    /// Active (or configured default) continuous capture mode (`video_and_logs` or `logs_only`).
    #[serde(default)]
    pub capture_mode: CaptureMode,
    /// `true` when 2+ ADB devices are online and no target serial has been selected yet.
    #[serde(default)]
    pub multiple_devices_need_selection: bool,
    /// Bound target serial waiting for reconnection when offline (never auto-switched).
    #[serde(default)]
    pub waiting_for_preferred_serial: Option<String>,
    /// Active device serial, if capturing.
    pub device_serial: Option<String>,
    /// Human-readable device label (`model (serial)`), if resolvable via `device.list`.
    pub device_label: Option<String>,
    /// Configured save lookback window in real elapsed seconds (from `config.get`).
    #[serde(default = "default_save_window_secs")]
    pub save_window_secs: u32,
    /// Estimated buffered capture duration in seconds (clamped to `save_window_secs`).
    #[serde(default)]
    pub buffered_secs: u32,
    /// Count of recorded discontinuities in the live session.
    pub discontinuities: usize,
    /// Count of `StaticHold` detections in the live session.
    pub static_hold_events: usize,
    /// `true` when capture stopped due to host storage pressure.
    pub storage_pressure_stopped: bool,
    /// All tracked save jobs (non-terminal and terminal), most recent first.
    pub jobs: Vec<SaveJobSummary>,
    /// Catalog bundle entries returned by `bundle.list`.
    pub bundles: Vec<RecentBundleSummary>,
    /// Always-on background capture setting (`system.status.capture_enabled`, falling back to
    /// `config.get.config.capture_enabled`); `None` when an older `arbd` reports neither.
    #[serde(default)]
    pub capture_enabled: Option<bool>,
    /// Per-section `arbd` application errors from the last poll (`"<method>: <error>"`).
    ///
    /// A daemon-reported `Rpc` error on one polled method (e.g. `device.list` failing when
    /// `adb` is absent) is surfaced here instead of collapsing the whole tray to
    /// "arbd オフライン" — the daemon is still online and the other sections still refreshed.
    /// Transport-level failures never reach this list: they mark the daemon offline.
    #[serde(default)]
    pub section_errors: Vec<String>,
}

impl TrayViewState {
    /// The crisp "`arbd` is unreachable" view, used whenever a connection attempt fails.
    #[must_use]
    pub const fn offline() -> Self {
        Self {
            badge: TrayBadge::DaemonOffline,
            daemon_online: false,
            capture: CaptureStatus::Idle,
            capture_mode: CaptureMode::VideoAndLogs,
            multiple_devices_need_selection: false,
            waiting_for_preferred_serial: None,
            device_serial: None,
            device_label: None,
            save_window_secs: DEFAULT_SAVE_WINDOW_SECS,
            buffered_secs: 0,
            discontinuities: 0,
            static_hold_events: 0,
            storage_pressure_stopped: false,
            jobs: Vec::new(),
            bundles: Vec::new(),
            capture_enabled: None,
            section_errors: Vec::new(),
        }
    }

    /// One-line label for the always-on background capture setting, or `None` when the daemon
    /// does not report it: `常時録画: ON・REC` (armed and recording), `常時録画: ON・待機中`
    /// (armed, waiting for a device / reconnect), or `常時録画: OFF`.
    #[must_use]
    pub fn capture_enabled_label(&self) -> Option<&'static str> {
        if !self.daemon_online {
            return None;
        }
        match (self.capture_enabled?, self.capture) {
            (true, CaptureStatus::Capturing) => Some("常時録画: ON・REC"),
            (true, _) => Some("常時録画: ON・待機中"),
            (false, _) => Some("常時録画: OFF"),
        }
    }

    /// Save jobs currently in `FailedRetryable`, in `save.list_jobs` order.
    pub fn failed_jobs(&self) -> impl Iterator<Item = &SaveJobSummary> {
        self.jobs
            .iter()
            .filter(|job| job.state == SaveJobState::FailedRetryable)
    }

    /// One-line human-readable status string in strict seconds notation (zero emoji).
    ///
    /// Examples:
    /// - `REC Pixel 9 (emulator-5554) · 272s / 300s`
    /// - `再接続中 Pixel 9 · 120s / 300s`
    /// - `待機中`
    /// - `arbd オフライン`
    #[must_use]
    pub fn status_line(&self) -> String {
        if !self.daemon_online {
            return "arbd オフライン".to_owned();
        }
        let device = self
            .device_label
            .as_deref()
            .or(self.device_serial.as_deref());
        let mode_suffix = if self.capture_mode == CaptureMode::LogsOnly {
            " (ログのみ)"
        } else {
            ""
        };
        let mut line = if self.storage_pressure_stopped {
            match device {
                Some(dev) => format!(
                    "容量逼迫停止{mode_suffix} {dev} · {}s / {}s",
                    self.buffered_secs, self.save_window_secs
                ),
                None => format!(
                    "容量逼迫停止{mode_suffix} · {}s / {}s",
                    self.buffered_secs, self.save_window_secs
                ),
            }
        } else {
            match self.capture {
                CaptureStatus::Capturing => format!(
                    "REC{mode_suffix} {} · {}s / {}s",
                    device.unwrap_or("端末未選択"),
                    self.buffered_secs,
                    self.save_window_secs
                ),
                CaptureStatus::Disconnected => format!(
                    "再接続中{mode_suffix} {} · {}s / {}s",
                    device.unwrap_or("端末未選択"),
                    self.buffered_secs,
                    self.save_window_secs
                ),
                CaptureStatus::Idle => {
                    if self.multiple_devices_need_selection {
                        format!("複数端末接続中（端末を選択してください）{mode_suffix}")
                    } else if let Some(pref) = &self.waiting_for_preferred_serial {
                        format!("再接続待機中 {pref}{mode_suffix}")
                    } else {
                        match device {
                            Some(dev) => format!("待機中{mode_suffix} · {dev}"),
                            None => format!("待機中{mode_suffix}"),
                        }
                    }
                }
            }
        };
        if self.discontinuities > 0 {
            let _ = write!(line, " · ギャップ {} 件", self.discontinuities);
        }
        if self.static_hold_events > 0 {
            let _ = write!(line, " · 静止 {} 回", self.static_hold_events);
        }
        for error in &self.section_errors {
            let _ = write!(line, " · {error}");
        }
        line
    }
}

/// Derives [`TrayBadge`] from a successfully fetched [`TrayViewState`] (daemon known online).
fn derive_badge(
    capture: CaptureStatus,
    storage: StoragePressureState,
    jobs: &[SaveJobSummary],
) -> TrayBadge {
    if storage == StoragePressureState::Pressured {
        TrayBadge::StoragePressure
    } else if capture == CaptureStatus::Disconnected {
        TrayBadge::Disconnected
    } else if jobs
        .iter()
        .any(|job| job.state == SaveJobState::FailedRetryable)
    {
        TrayBadge::SaveAttentionNeeded
    } else if capture == CaptureStatus::Capturing {
        TrayBadge::Capturing
    } else {
        TrayBadge::Idle
    }
}

/// A pure, human-readable alert emitted only on a state *transition* (never spammed every poll).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrayAlert {
    /// `arbd` became unreachable.
    DaemonOffline,
    /// `arbd` became reachable again.
    DaemonOnline,
    /// Device transport dropped mid-capture.
    ConnectionLost,
    /// Device transport reconnected mid-capture.
    ConnectionRestored,
    /// Capture stopped due to host storage pressure.
    StoragePressureStop,
    /// Storage pressure condition cleared.
    StoragePressureCleared,
    /// Capture session started.
    CaptureStarted,
    /// Capture session stopped.
    CaptureStopped,
    /// A save job completed successfully.
    SaveCompleted {
        /// Completed save job identifier.
        job_id: String,
    },
    /// A save job newly entered `FailedRetryable` and needs user attention.
    SaveFailedRetryable {
        /// Failing save job identifier.
        job_id: String,
        /// Last reported error message, if any.
        error: Option<String>,
    },
}

impl TrayAlert {
    /// Human-readable alert text suitable for a tray notification banner (zero emoji).
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::DaemonOffline => "arbd が応答を停止しました".to_owned(),
            Self::DaemonOnline => "arbd に再接続しました".to_owned(),
            Self::ConnectionLost => "録画中に端末との接続が切断されました".to_owned(),
            Self::ConnectionRestored => "端末との接続が復旧しました".to_owned(),
            Self::StoragePressureStop => "空き容量不足のため録画を停止しました".to_owned(),
            Self::StoragePressureCleared => "空き容量不足が解消されました".to_owned(),
            Self::CaptureStarted => "録画を開始しました".to_owned(),
            Self::CaptureStopped => "録画を停止しました".to_owned(),
            Self::SaveCompleted { job_id } => format!("保存が完了しました ({job_id})"),
            Self::SaveFailedRetryable { job_id, error } => match error {
                Some(err) => format!("保存に失敗しました（再試行可・{job_id}）: {err}"),
                None => format!("保存に失敗しました（再試行可・{job_id}）"),
            },
        }
    }
}

/// Diffs `prev` against `next` and returns the alerts a user should see for this transition.
///
/// Pure and side-effect free so it can be exhaustively unit tested; emits nothing on the very
/// first observation except `DaemonOffline` (never spams on startup).
#[must_use]
pub fn diff_alerts(prev: Option<&TrayViewState>, next: &TrayViewState) -> Vec<TrayAlert> {
    let mut alerts = Vec::new();
    let Some(prev) = prev else {
        if !next.daemon_online {
            alerts.push(TrayAlert::DaemonOffline);
        }
        return alerts;
    };

    if prev.daemon_online && !next.daemon_online {
        alerts.push(TrayAlert::DaemonOffline);
        return alerts;
    }
    if !prev.daemon_online && next.daemon_online {
        alerts.push(TrayAlert::DaemonOnline);
    }
    if !next.daemon_online {
        return alerts;
    }

    if !prev.storage_pressure_stopped && next.storage_pressure_stopped {
        alerts.push(TrayAlert::StoragePressureStop);
    } else if prev.storage_pressure_stopped && !next.storage_pressure_stopped {
        alerts.push(TrayAlert::StoragePressureCleared);
    }

    match (prev.capture, next.capture) {
        (CaptureStatus::Idle, CaptureStatus::Capturing | CaptureStatus::Disconnected) => {
            alerts.push(TrayAlert::CaptureStarted);
        }
        (CaptureStatus::Capturing | CaptureStatus::Disconnected, CaptureStatus::Idle) => {
            alerts.push(TrayAlert::CaptureStopped);
        }
        (CaptureStatus::Capturing, CaptureStatus::Disconnected) => {
            alerts.push(TrayAlert::ConnectionLost);
        }
        (CaptureStatus::Disconnected, CaptureStatus::Capturing) => {
            alerts.push(TrayAlert::ConnectionRestored);
        }
        _ => {}
    }

    for job in &next.jobs {
        let prev_state = prev
            .jobs
            .iter()
            .find(|candidate| candidate.job_id == job.job_id)
            .map(|candidate| candidate.state);
        if prev_state == Some(job.state) {
            continue;
        }
        match job.state {
            SaveJobState::FailedRetryable => alerts.push(TrayAlert::SaveFailedRetryable {
                job_id: job.job_id.clone(),
                error: job.last_error.clone(),
            }),
            SaveJobState::Completed => alerts.push(TrayAlert::SaveCompleted {
                job_id: job.job_id.clone(),
            }),
            // `Pinned`/`Draining`/`Materializing`/`Discarded`, and any future
            // `#[non_exhaustive]` state: no alert by default (matches "no spam").
            _ => {}
        }
    }

    alerts
}

/// One rendered tray/menu-bar menu item (flat `MenuItem` when `children` is empty, otherwise a
/// `Submenu`). Mirrors exactly what `src/native_tray.rs` builds into `muda::Menu`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayMenuItem {
    /// Visible menu item label.
    pub label: String,
    /// `false` for disabled/informational rows (e.g., the status line).
    pub enabled: bool,
    /// Event fired when this leaf item is activated (`None` for submenus/separators/labels).
    pub event: Option<TrayMenuEvent>,
    /// `true` for a visual separator row (label/enabled/event/children are ignored).
    pub separator: bool,
    /// Nested items for a submenu; empty for a flat leaf item.
    pub children: Vec<TrayMenuItem>,
}

impl TrayMenuItem {
    /// An enabled, RPC-backed leaf item. Use struct-update syntax
    /// (`TrayMenuItem { enabled: ..., ..TrayMenuItem::action(...) }`) for conditionally disabled
    /// items, keeping this constructor itself free of a `bool` parameter.
    fn action(label: impl Into<String>, event: TrayMenuEvent) -> Self {
        Self {
            label: label.into(),
            enabled: true,
            event: Some(event),
            separator: false,
            children: Vec::new(),
        }
    }

    fn info(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            enabled: false,
            event: None,
            separator: false,
            children: Vec::new(),
        }
    }

    fn submenu(label: impl Into<String>, children: Vec<Self>) -> Self {
        Self {
            label: label.into(),
            enabled: !children.is_empty(),
            event: None,
            separator: false,
            children,
        }
    }

    const fn separator() -> Self {
        Self {
            label: String::new(),
            enabled: false,
            event: None,
            separator: true,
            children: Vec::new(),
        }
    }
}

/// A user-triggered tray menu event. RPC-backed variants map 1:1 onto an `arbd` JSON-RPC call via
/// [`TrayMenuEvent::to_rpc`]; `QuitTray` and `RevealBundle` are handled locally by the tray
/// process and never reach `arbd`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrayMenuEvent {
    /// `save.trigger` with the configured lookback duration in seconds.
    SaveWindow {
        /// Requested lookback duration in real elapsed seconds.
        duration_secs: u32,
    },
    /// `save.screenshot` capturing the current Android screen as PNG.
    SaveScreenshot,
    /// `save.logs` saving only the recent logcat window without interrupting video capture.
    SaveLogs {
        /// Requested lookback duration in real elapsed seconds.
        window_secs: u32,
    },
    /// `save.retry` for a `FailedRetryable` job.
    RetryJob {
        /// Target save job identifier.
        job_id: String,
    },
    /// `save.discard` for a `FailedRetryable` job.
    DiscardJob {
        /// Target save job identifier.
        job_id: String,
    },
    /// `bundle.trash` for an `Active` bundle.
    TrashBundle {
        /// Target bundle identifier.
        bundle_id: String,
    },
    /// `bundle.restore` for a `Trashed` bundle.
    RestoreBundle {
        /// Target bundle identifier.
        bundle_id: String,
    },
    /// Reveals a bundle's folder in the host OS file manager (local-only, no RPC).
    RevealBundle {
        /// Bundle directory path to reveal.
        path: PathBuf,
    },
    /// `capture.start` or `capture.stop`, depending on current state.
    ToggleCapture,
    /// Exits the tray process only; `arbd` keeps running.
    QuitTray,
    /// Requests `system.shutdown` then exits the tray process.
    StopDaemonAndQuit,
}

impl TrayMenuEvent {
    /// Maps an RPC-backed event to its `(method, params)` JSON-RPC 2.0 call, or `None` for
    /// locally handled events (`QuitTray`, `RevealBundle`).
    #[must_use]
    pub fn to_rpc(&self, capture: CaptureStatus) -> Option<(&'static str, Value)> {
        match self {
            Self::SaveWindow { duration_secs } => {
                Some(("save.trigger", json!({ "duration_sec": duration_secs })))
            }
            Self::SaveScreenshot => Some(("save.screenshot", json!({}))),
            Self::SaveLogs { window_secs } => {
                Some(("save.logs", json!({ "window_secs": window_secs })))
            }
            Self::RetryJob { job_id } => Some(("save.retry", json!({ "job_id": job_id }))),
            Self::DiscardJob { job_id } => Some(("save.discard", json!({ "job_id": job_id }))),
            Self::TrashBundle { bundle_id } => {
                Some(("bundle.trash", json!({ "bundle_id": bundle_id })))
            }
            Self::RestoreBundle { bundle_id } => {
                Some(("bundle.restore", json!({ "bundle_id": bundle_id })))
            }
            Self::ToggleCapture if capture == CaptureStatus::Idle => {
                Some(("capture.start", json!({})))
            }
            Self::ToggleCapture => Some(("capture.stop", json!({}))),
            Self::StopDaemonAndQuit => Some(("system.shutdown", json!({}))),
            Self::RevealBundle { .. } | Self::QuitTray => None,
        }
    }
}

/// One failed-save-jobs submenu entry (`再試行` / `破棄`).
fn build_failed_job_item(job: &SaveJobSummary) -> TrayMenuItem {
    let label = job.destination_path.file_name().map_or_else(
        || job.job_id.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    TrayMenuItem::submenu(
        label,
        vec![
            TrayMenuItem::action(
                "再試行",
                TrayMenuEvent::RetryJob {
                    job_id: job.job_id.clone(),
                },
            ),
            TrayMenuItem::action(
                "破棄",
                TrayMenuEvent::DiscardJob {
                    job_id: job.job_id.clone(),
                },
            ),
        ],
    )
}

/// Conditional "保存失敗ジョブ (N)" submenu, or `None` when healthy.
fn build_failed_jobs_section(state: &TrayViewState) -> Option<TrayMenuItem> {
    let children: Vec<TrayMenuItem> = state.failed_jobs().map(build_failed_job_item).collect();
    if children.is_empty() {
        return None;
    }
    let count = children.len();
    Some(TrayMenuItem::submenu(
        format!("保存失敗ジョブ ({count})"),
        children,
    ))
}

/// One recent-saves submenu leaf item opening the bundle folder in the OS file manager.
fn build_recent_bundle_item(bundle: &RecentBundleSummary) -> TrayMenuItem {
    let label = format!("{} [{}]", bundle.bundle_id, bundle.format.label());
    TrayMenuItem::action(
        label,
        TrayMenuEvent::RevealBundle {
            path: bundle.path.clone(),
        },
    )
}

/// The `最近の保存` submenu (latest 5 active bundles, newest first -> 1-click file manager open).
fn build_recent_bundles_section(state: &TrayViewState) -> TrayMenuItem {
    let mut active: Vec<&RecentBundleSummary> = state
        .bundles
        .iter()
        .filter(|bundle| bundle.lifecycle == BundleLifecycleState::Active)
        .collect();
    // `bundle.list` returns oldest-first (`created_at_utc ASC`); reverse to newest-first while
    // keeping explicit UTC ordering deterministic when timestamps are present.
    active.reverse();
    // Stable sort: equal (or missing) timestamps keep the newest-first list order from `reverse`.
    active.sort_by(|a, b| b.created_at_utc.cmp(&a.created_at_utc));
    let children: Vec<TrayMenuItem> = active
        .into_iter()
        .take(RECENT_BUNDLES_LIMIT)
        .map(build_recent_bundle_item)
        .collect();
    TrayMenuItem::submenu("最近の保存", children)
}

/// Builds the full tray menu item tree from a [`TrayViewState`] (pure, unit-testable, zero emoji,
/// strict seconds notation).
#[must_use]
pub fn build_menu(state: &TrayViewState) -> Vec<TrayMenuItem> {
    let mut items = vec![TrayMenuItem::info(state.status_line())];
    if let Some(label) = state.capture_enabled_label() {
        items.push(TrayMenuItem::info(label));
    }
    items.push(TrayMenuItem::separator());

    items.push(TrayMenuItem {
        enabled: state.daemon_online,
        ..TrayMenuItem::action(
            format!("直前 {}s を保存", state.save_window_secs),
            TrayMenuEvent::SaveWindow {
                duration_secs: state.save_window_secs,
            },
        )
    });
    items.push(TrayMenuItem {
        enabled: state.daemon_online,
        ..TrayMenuItem::action("スクリーンショットを保存", TrayMenuEvent::SaveScreenshot)
    });
    items.push(TrayMenuItem {
        enabled: state.daemon_online,
        ..TrayMenuItem::action(
            "直前のログのみ保存",
            TrayMenuEvent::SaveLogs {
                window_secs: state.save_window_secs,
            },
        )
    });

    if let Some(failed_section) = build_failed_jobs_section(state) {
        items.push(TrayMenuItem::separator());
        items.push(failed_section);
    }

    items.push(TrayMenuItem::separator());
    items.push(build_recent_bundles_section(state));

    let (toggle_label, toggle_enabled) = if state.capture == CaptureStatus::Idle {
        if state.multiple_devices_need_selection {
            ("録画を開始（端末を選択してください）", false)
        } else {
            ("録画を開始", state.daemon_online)
        }
    } else {
        ("録画を停止", state.daemon_online)
    };
    items.push(TrayMenuItem {
        enabled: toggle_enabled,
        ..TrayMenuItem::action(toggle_label, TrayMenuEvent::ToggleCapture)
    });

    items.push(TrayMenuItem::separator());
    items.push(TrayMenuItem::action(
        "トレイのみ終了（録画継続）",
        TrayMenuEvent::QuitTray,
    ));
    items.push(TrayMenuItem {
        enabled: state.daemon_online,
        ..TrayMenuItem::action("録画デーモンごと終了", TrayMenuEvent::StopDaemonAndQuit)
    });

    items
}

fn parse_save_job(raw: &Value) -> Option<SaveJobSummary> {
    let job_id = raw.get("job_id")?.as_str()?.to_owned();
    let state: SaveJobState = serde_json::from_value(raw.get("state")?.clone()).ok()?;
    let destination_path = raw
        .get("destination_path")
        .and_then(Value::as_str)
        .map_or_else(PathBuf::new, PathBuf::from);
    let last_error = raw
        .get("last_error")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(SaveJobSummary {
        job_id,
        state,
        destination_path,
        last_error,
    })
}

fn parse_bundle(raw: &Value) -> Option<RecentBundleSummary> {
    let bundle_id = raw.get("bundle_id")?.as_str()?.to_owned();
    let path = raw
        .get("path")
        .and_then(Value::as_str)
        .map_or_else(PathBuf::new, PathBuf::from);
    let format = BundleFormatBadge::parse(raw.get("format_version").and_then(Value::as_str));
    let lifecycle: BundleLifecycleState = serde_json::from_value(raw.get("state")?.clone()).ok()?;
    let created_at_utc = raw
        .get("created_at_utc")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some(RecentBundleSummary {
        bundle_id,
        path,
        format,
        lifecycle,
        created_at_utc,
    })
}

/// Resolves a human label (`model (serial)`) for `serial` from a `device.list` JSON response.
fn resolve_device_label(devices_response: &Value, serial: &str) -> Option<String> {
    let devices = devices_response.get("devices")?.as_array()?;
    for device in devices {
        if device.get("serial").and_then(Value::as_str) == Some(serial) {
            let model = device.get("model").and_then(Value::as_str);
            return Some(match model {
                Some(model) => format!("{model} ({serial})"),
                None => serial.to_owned(),
            });
        }
    }
    None
}

fn parse_config_timing(config_response: &Value) -> (u32, u32) {
    let cfg = config_response.get("config").unwrap_or(&Value::Null);
    let save_window_secs = cfg
        .get("save_window_secs")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_SAVE_WINDOW_SECS);
    let video_max_fps = cfg
        .get("video_max_fps")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_VIDEO_MAX_FPS);
    (save_window_secs, video_max_fps)
}

fn parse_capture_mode_field(value: Option<&Value>) -> Option<CaptureMode> {
    match value.and_then(Value::as_str) {
        Some("logs_only") => Some(CaptureMode::LogsOnly),
        Some("video_and_logs") => Some(CaptureMode::VideoAndLogs),
        _ => None,
    }
}

fn online_device_serials(devices_response: &Value) -> Vec<String> {
    devices_response
        .get("devices")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |arr| {
            arr.iter()
                .filter(|d| d.get("state").and_then(Value::as_str) == Some("device"))
                .filter_map(|d| d.get("serial").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
}

fn evaluate_tray_device_selection(cfg_obj: &Value, devices: &Value) -> (bool, Option<String>) {
    let preferred_serial = cfg_obj
        .get("device_serial")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let online_serials = online_device_serials(devices);
    match DeviceSelectionPolicy::evaluate(preferred_serial, &online_serials) {
        DeviceSelectionOutcome::MultipleDevicesNeedSelection { .. } => (true, None),
        DeviceSelectionOutcome::WaitingForPreferredDevice {
            preferred_serial, ..
        } => (false, Some(preferred_serial)),
        _ => (false, None),
    }
}

/// Builds a [`TrayViewState`] from the raw JSON results of the five polled RPC methods.
fn build_view_state(
    status: &Value,
    config: &Value,
    devices: &Value,
    jobs_response: &Value,
    bundles_response: &Value,
) -> TrayViewState {
    let capture = status.get("capture").cloned().unwrap_or(Value::Null);
    let capture_active = capture
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let capture_connected = capture
        .get("connected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let capture_status = match (capture_active, capture_connected) {
        (true, true) => CaptureStatus::Capturing,
        (true, false) => CaptureStatus::Disconnected,
        (false, _) => CaptureStatus::Idle,
    };
    let cfg_obj = config.get("config").unwrap_or(&Value::Null);
    let capture_mode = if capture_active {
        parse_capture_mode_field(capture.get("capture_mode"))
            .or_else(|| parse_capture_mode_field(cfg_obj.get("capture_mode")))
            .unwrap_or(CaptureMode::VideoAndLogs)
    } else {
        parse_capture_mode_field(cfg_obj.get("capture_mode"))
            .or_else(|| parse_capture_mode_field(capture.get("capture_mode")))
            .unwrap_or(CaptureMode::VideoAndLogs)
    };
    let (multiple_devices_need_selection, waiting_for_preferred_serial) =
        evaluate_tray_device_selection(cfg_obj, devices);
    let device_serial = capture
        .get("serial")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let device_label = device_serial
        .as_deref()
        .and_then(|serial| resolve_device_label(devices, serial));
    let (save_window_secs, video_max_fps) = parse_config_timing(config);
    let buffered_secs_u64 =
        if let Some(real_buffered) = status.get("buffered_secs").and_then(Value::as_u64) {
            real_buffered.min(u64::from(save_window_secs))
        } else {
            let video_frames = capture
                .get("video_frames")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (video_frames / u64::from(video_max_fps.max(1))).min(u64::from(save_window_secs))
        };
    let buffered_secs = u32::try_from(buffered_secs_u64).unwrap_or(save_window_secs);
    let discontinuities = capture
        .get("discontinuities")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let static_hold_events = capture
        .get("static_hold_events")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let storage_pressure_stopped = capture
        .get("storage_pressure_stopped")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let jobs: Vec<SaveJobSummary> = jobs_response
        .get("jobs")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |arr| {
            arr.iter().filter_map(parse_save_job).collect()
        });

    let bundles: Vec<RecentBundleSummary> = bundles_response
        .get("bundles")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |arr| {
            arr.iter().filter_map(parse_bundle).collect()
        });

    let storage_state = if storage_pressure_stopped {
        StoragePressureState::Pressured
    } else {
        StoragePressureState::Normal
    };
    let badge = derive_badge(capture_status, storage_state, &jobs);
    let capture_enabled = status
        .get("capture_enabled")
        .and_then(Value::as_bool)
        .or_else(|| cfg_obj.get("capture_enabled").and_then(Value::as_bool));

    TrayViewState {
        badge,
        daemon_online: true,
        capture: capture_status,
        capture_mode,
        multiple_devices_need_selection,
        waiting_for_preferred_serial,
        device_serial,
        device_label,
        save_window_secs,
        buffered_secs,
        discontinuities,
        static_hold_events,
        storage_pressure_stopped,
        jobs,
        bundles,
        capture_enabled,
        section_errors: Vec::new(),
    }
}

/// Connects to `arbd` over local IPC, polls it on an interval, and exposes the derived tray
/// state/menu plus transition alerts. Reconnects gracefully whenever `arbd` restarts.
#[derive(Debug)]
pub struct TrayController {
    paths: ArbPaths,
    auto_start: AutoStartMode,
    poll_interval: Duration,
    client: Option<RpcClient>,
    last_state: Option<TrayViewState>,
}

impl TrayController {
    /// Creates a controller that will connect to `paths.ipc_endpoint()`.
    #[must_use]
    pub const fn new(paths: ArbPaths, auto_start: AutoStartMode, poll_interval: Duration) -> Self {
        Self {
            paths,
            auto_start,
            poll_interval,
            client: None,
            last_state: None,
        }
    }

    /// Configured polling interval between refresh cycles.
    #[must_use]
    pub const fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// Performs one `system.status` + `config.get` + `device.list` + `save.list_jobs` +
    /// `bundle.list` refresh cycle, reconnecting first if the connection was previously lost, and
    /// returns the fresh [`TrayViewState`] alongside any transition [`TrayAlert`]s versus the prior
    /// observation.
    pub fn poll_once(&mut self) -> (TrayViewState, Vec<TrayAlert>) {
        let next = self.fetch_state();
        let alerts = diff_alerts(self.last_state.as_ref(), &next);
        self.last_state = Some(next.clone());
        (next, alerts)
    }

    fn fetch_state(&mut self) -> TrayViewState {
        if self.client.is_none()
            && let Ok(client) = RpcClient::connect(&self.paths, self.auto_start)
        {
            self.client = Some(client);
        }
        if self.client.is_none() {
            return TrayViewState::offline();
        }
        // Only transport-level errors reach the `else` (per-section `Rpc` errors are
        // collected inside `fetch_state_inner`), so dropping the connection is honest.
        if let Ok(state) = self.fetch_state_inner() {
            state
        } else {
            self.client = None;
            TrayViewState::offline()
        }
    }

    fn fetch_state_inner(&mut self) -> Result<TrayViewState, RpcClientError> {
        let client = self
            .client
            .as_mut()
            .expect("fetch_state_inner is only called once self.client is Some");
        let mut section_errors = Vec::new();
        let status = call_section(client, "system.status", &mut section_errors)?;
        let config = call_section(client, "config.get", &mut section_errors)?;
        let devices = call_section(client, "device.list", &mut section_errors)?;
        let jobs = call_section(client, "save.list_jobs", &mut section_errors)?;
        let bundles = call_section(client, "bundle.list", &mut section_errors)?;
        let mut state = build_view_state(
            &status.unwrap_or_default(),
            &config.unwrap_or_default(),
            &devices.unwrap_or_default(),
            &jobs.unwrap_or_default(),
            &bundles.unwrap_or_default(),
        );
        state.section_errors = section_errors;
        Ok(state)
    }

    /// Dispatches an RPC-backed [`TrayMenuEvent`] to `arbd`, reconnecting first if necessary.
    ///
    /// Returns `Ok(None)` for locally handled events (`QuitTray`, `RevealBundle`) that never
    /// reach `arbd`.
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError`] if connecting or the RPC call itself fails.
    ///
    /// # Panics
    ///
    /// Never panics: `self.client` is populated immediately above whenever it was `None`, and
    /// the preceding `?` already returns on a failed connect.
    pub fn dispatch(&mut self, event: &TrayMenuEvent) -> Result<Option<Value>, RpcClientError> {
        let capture = self
            .last_state
            .as_ref()
            .map_or(CaptureStatus::Idle, |state| state.capture);
        let Some((method, params)) = event.to_rpc(capture) else {
            return Ok(None);
        };
        if self.client.is_none() {
            self.client = Some(RpcClient::connect(&self.paths, self.auto_start)?);
        }
        let client = self
            .client
            .as_mut()
            .expect("client was just connected above");
        let result = client.call(method, params);
        // An application-level `Rpc` error was answered by a live daemon: keep the
        // connection. Only transport-level failures drop it (S1).
        if let Err(err) = &result
            && err.is_transport()
        {
            self.client = None;
        }
        result.map(Some)
    }
}

/// Runs one polled RPC method, folding application-level `Rpc` errors into `section_errors`
/// (the daemon answered, so it is still online) and propagating only transport-level errors.
fn call_section(
    client: &mut RpcClient,
    method: &str,
    section_errors: &mut Vec<String>,
) -> Result<Option<Value>, RpcClientError> {
    match client.call(method, json!({})) {
        Ok(value) => Ok(Some(value)),
        Err(err @ RpcClientError::Rpc { .. }) => {
            section_errors.push(format!("{method}: {err}"));
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Returns `true` if `text` contains any Unicode emoji / pictograph / variation-selector-16
/// codepoint (shared guard ensuring zero emoji chrome across tray menus and status text).
#[must_use]
pub fn contains_emoji(text: &str) -> bool {
    text.chars().any(|ch| {
        let cp = u32::from(ch);
        matches!(
            cp,
            0x1F300..=0x1FAFF
                | 0x2600..=0x27BF
                | 0x23E9..=0x23FA
                | 0xFE0F
                | 0x200D
                | 0x25B6
                | 0x25C0
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state() -> TrayViewState {
        TrayViewState {
            badge: TrayBadge::Idle,
            daemon_online: true,
            capture: CaptureStatus::Idle,
            capture_mode: CaptureMode::VideoAndLogs,
            multiple_devices_need_selection: false,
            waiting_for_preferred_serial: None,
            device_serial: None,
            device_label: None,
            save_window_secs: DEFAULT_SAVE_WINDOW_SECS,
            buffered_secs: 0,
            discontinuities: 0,
            static_hold_events: 0,
            storage_pressure_stopped: false,
            jobs: Vec::new(),
            bundles: Vec::new(),
            capture_enabled: None,
            section_errors: Vec::new(),
        }
    }

    fn failed_job(job_id: &str) -> SaveJobSummary {
        SaveJobSummary {
            job_id: job_id.to_owned(),
            state: SaveJobState::FailedRetryable,
            destination_path: PathBuf::from("/tmp/bundle-1"),
            last_error: Some("disk full".to_owned()),
        }
    }

    fn collect_menu_labels(items: &[TrayMenuItem], out: &mut Vec<String>) {
        for item in items {
            if !item.label.is_empty() {
                out.push(item.label.clone());
            }
            collect_menu_labels(&item.children, out);
        }
    }

    #[test]
    fn derive_badge_precedence_storage_pressure_beats_everything() {
        let jobs = vec![failed_job("job-1")];
        assert_eq!(
            derive_badge(
                CaptureStatus::Disconnected,
                StoragePressureState::Pressured,
                &jobs
            ),
            TrayBadge::StoragePressure,
            "storage pressure must outrank disconnected + failed jobs + capturing"
        );
    }

    #[test]
    fn derive_badge_precedence_disconnected_beats_failed_jobs_and_capturing() {
        let jobs = vec![failed_job("job-1")];
        assert_eq!(
            derive_badge(
                CaptureStatus::Disconnected,
                StoragePressureState::Normal,
                &jobs
            ),
            TrayBadge::Disconnected,
            "disconnected capture must outrank failed jobs"
        );
    }

    #[test]
    fn derive_badge_precedence_failed_jobs_beat_capturing() {
        let jobs = vec![failed_job("job-1")];
        assert_eq!(
            derive_badge(
                CaptureStatus::Capturing,
                StoragePressureState::Normal,
                &jobs
            ),
            TrayBadge::SaveAttentionNeeded,
            "a FailedRetryable job must outrank plain capturing"
        );
    }

    #[test]
    fn derive_badge_capturing_when_active_and_connected_with_no_issues() {
        assert_eq!(
            derive_badge(CaptureStatus::Capturing, StoragePressureState::Normal, &[]),
            TrayBadge::Capturing
        );
    }

    #[test]
    fn derive_badge_idle_when_nothing_is_happening() {
        assert_eq!(
            derive_badge(CaptureStatus::Idle, StoragePressureState::Normal, &[]),
            TrayBadge::Idle
        );
    }

    #[test]
    fn status_line_uses_strict_seconds_notation_and_zero_emoji() {
        let mut offline = TrayViewState::offline();
        offline.discontinuities = 7;
        assert_eq!(offline.status_line(), "arbd オフライン");

        let idle = base_state();
        assert_eq!(idle.status_line(), "待機中");

        let mut capturing = base_state();
        capturing.device_label = Some("Pixel 9".to_owned());
        capturing.capture = CaptureStatus::Capturing;
        capturing.buffered_secs = 272;
        capturing.save_window_secs = 300;
        assert_eq!(capturing.status_line(), "REC Pixel 9 · 272s / 300s");

        capturing.discontinuities = 2;
        assert_eq!(
            capturing.status_line(),
            "REC Pixel 9 · 272s / 300s · ギャップ 2 件"
        );
    }

    #[test]
    fn diff_alerts_first_observation_emits_nothing_when_online() {
        let next = base_state();
        assert_eq!(diff_alerts(None, &next), Vec::<TrayAlert>::new());
    }

    #[test]
    fn diff_alerts_first_observation_reports_offline() {
        let next = TrayViewState::offline();
        assert_eq!(diff_alerts(None, &next), vec![TrayAlert::DaemonOffline]);
    }

    #[test]
    fn diff_alerts_does_not_spam_unchanged_state() {
        let state = base_state();
        assert_eq!(
            diff_alerts(Some(&state), &state),
            Vec::<TrayAlert>::new(),
            "identical consecutive polls must not emit alerts"
        );
    }

    #[test]
    fn diff_alerts_connection_lost_and_restored() {
        let mut capturing = base_state();
        capturing.capture = CaptureStatus::Capturing;

        let mut disconnected = capturing.clone();
        disconnected.capture = CaptureStatus::Disconnected;

        assert_eq!(
            diff_alerts(Some(&capturing), &disconnected),
            vec![TrayAlert::ConnectionLost]
        );
        assert_eq!(
            diff_alerts(Some(&disconnected), &capturing),
            vec![TrayAlert::ConnectionRestored]
        );
    }

    #[test]
    fn diff_alerts_storage_pressure_stop_and_clear() {
        let normal = base_state();
        let mut pressured = normal.clone();
        pressured.storage_pressure_stopped = true;

        assert_eq!(
            diff_alerts(Some(&normal), &pressured),
            vec![TrayAlert::StoragePressureStop]
        );
        assert_eq!(
            diff_alerts(Some(&pressured), &normal),
            vec![TrayAlert::StoragePressureCleared]
        );
    }

    #[test]
    fn diff_alerts_capture_started_and_stopped() {
        let idle = base_state();
        let mut capturing = idle.clone();
        capturing.capture = CaptureStatus::Capturing;

        assert_eq!(
            diff_alerts(Some(&idle), &capturing),
            vec![TrayAlert::CaptureStarted]
        );
        assert_eq!(
            diff_alerts(Some(&capturing), &idle),
            vec![TrayAlert::CaptureStopped]
        );
    }

    #[test]
    fn diff_alerts_new_failed_job_then_retry_completes() {
        let empty = base_state();
        let mut with_failure = empty.clone();
        with_failure.jobs.push(failed_job("job-1"));

        assert_eq!(
            diff_alerts(Some(&empty), &with_failure),
            vec![TrayAlert::SaveFailedRetryable {
                job_id: "job-1".to_owned(),
                error: Some("disk full".to_owned()),
            }]
        );

        let mut completed = with_failure.clone();
        completed.jobs[0].state = SaveJobState::Completed;
        completed.jobs[0].last_error = None;

        assert_eq!(
            diff_alerts(Some(&with_failure), &completed),
            vec![TrayAlert::SaveCompleted {
                job_id: "job-1".to_owned(),
            }]
        );
    }

    #[test]
    fn diff_alerts_daemon_offline_suppresses_other_transitions() {
        let mut capturing = base_state();
        capturing.capture = CaptureStatus::Capturing;

        let offline = TrayViewState::offline();
        assert_eq!(
            diff_alerts(Some(&capturing), &offline),
            vec![TrayAlert::DaemonOffline],
            "going offline must report exactly one alert, not a spurious CaptureStopped too"
        );

        assert_eq!(
            diff_alerts(Some(&offline), &capturing),
            vec![TrayAlert::DaemonOnline, TrayAlert::CaptureStarted],
            "coming back online while already capturing must report both transitions"
        );
    }

    #[test]
    fn build_menu_offline_disables_mutating_actions_and_keeps_quit_tray_enabled() {
        let state = TrayViewState::offline();
        let menu = build_menu(&state);
        assert_eq!(
            menu.first().map(|i| i.label.as_str()),
            Some("arbd オフライン")
        );
        let save_item = menu
            .iter()
            .find(|item| matches!(item.event, Some(TrayMenuEvent::SaveWindow { .. })))
            .expect("save window item must exist");
        assert_eq!(save_item.label, "直前 300s を保存");
        assert!(
            !save_item.enabled,
            "mutating actions must be disabled offline"
        );
        let quit_tray = menu
            .iter()
            .find(|item| matches!(item.event, Some(TrayMenuEvent::QuitTray)))
            .expect("QuitTray item must exist");
        assert_eq!(quit_tray.label, "トレイのみ終了（録画継続）");
        assert!(quit_tray.enabled, "QuitTray must stay enabled offline");
        let stop_daemon = menu
            .iter()
            .find(|item| matches!(item.event, Some(TrayMenuEvent::StopDaemonAndQuit)))
            .expect("StopDaemonAndQuit item must exist");
        assert_eq!(stop_daemon.label, "録画デーモンごと終了");
        assert!(!stop_daemon.enabled);
    }

    #[test]
    fn build_menu_uses_config_save_window_secs_strict_seconds_and_spec_actions() {
        let mut state = base_state();
        state.save_window_secs = 45;
        state.capture = CaptureStatus::Capturing;
        state.device_label = Some("Pixel 9".to_owned());
        state.buffered_secs = 32;

        let menu = build_menu(&state);
        assert_eq!(
            menu[0].label, "REC Pixel 9 · 32s / 45s",
            "status row must use strict seconds notation"
        );
        let save = menu
            .iter()
            .find(|i| {
                matches!(
                    i.event,
                    Some(TrayMenuEvent::SaveWindow { duration_secs: 45 })
                )
            })
            .expect("SaveWindow 45s item");
        assert_eq!(save.label, "直前 45s を保存");
        assert!(!save.label.contains('分'), "must never round to minutes");

        let shot = menu
            .iter()
            .find(|i| matches!(i.event, Some(TrayMenuEvent::SaveScreenshot)))
            .expect("SaveScreenshot item");
        assert_eq!(shot.label, "スクリーンショットを保存");

        let logs = menu
            .iter()
            .find(|i| matches!(i.event, Some(TrayMenuEvent::SaveLogs { window_secs: 45 })))
            .expect("SaveLogs item");
        assert_eq!(logs.label, "直前のログのみ保存");

        let toggle = menu
            .iter()
            .find(|i| matches!(i.event, Some(TrayMenuEvent::ToggleCapture)))
            .expect("ToggleCapture item");
        assert_eq!(toggle.label, "録画を停止");

        let idle_menu = build_menu(&base_state());
        let idle_toggle = idle_menu
            .iter()
            .find(|i| matches!(i.event, Some(TrayMenuEvent::ToggleCapture)))
            .expect("idle ToggleCapture item");
        assert_eq!(idle_toggle.label, "録画を開始");
    }

    #[test]
    fn build_menu_failed_jobs_submenu_has_retry_and_discard() {
        let mut state = base_state();
        state.jobs.push(failed_job("job-42"));
        let menu = build_menu(&state);
        let submenu = menu
            .iter()
            .find(|item| item.label == "保存失敗ジョブ (1)")
            .expect("failed jobs submenu must appear when a job is FailedRetryable");
        assert_eq!(submenu.children.len(), 1, "one job => one job submenu");
        let job_submenu = &submenu.children[0];
        let labels: Vec<&str> = job_submenu
            .children
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, vec!["再試行", "破棄"]);
    }

    #[test]
    fn build_menu_omits_failed_jobs_submenu_when_none_failed() {
        let state = base_state();
        let menu = build_menu(&state);
        assert!(
            !menu
                .iter()
                .any(|item| item.label.starts_with("保存失敗ジョブ")),
            "no FailedRetryable jobs => no submenu"
        );
    }

    #[test]
    fn build_menu_recent_saves_limits_to_latest_five_active_bundles_with_reveal() {
        let mut state = base_state();
        for idx in 1..=7_u32 {
            state.bundles.push(RecentBundleSummary {
                bundle_id: format!("bundle-{idx:02}"),
                path: PathBuf::from(format!("/data/arb/bundles/bundle-{idx:02}")),
                format: if idx == 7 {
                    BundleFormatBadge::LegacyRo
                } else {
                    BundleFormatBadge::V2
                },
                lifecycle: BundleLifecycleState::Active,
                created_at_utc: format!("2026-11-23T00:0{idx}:00Z"),
            });
        }
        state.bundles.push(RecentBundleSummary {
            bundle_id: "trashed-99".to_owned(),
            path: PathBuf::from("/data/arb/bundles/trashed-99"),
            format: BundleFormatBadge::V2,
            lifecycle: BundleLifecycleState::Trashed,
            created_at_utc: "2026-11-23T09:00:00Z".to_owned(),
        });

        let menu = build_menu(&state);
        let recent = menu
            .iter()
            .find(|item| item.label == "最近の保存")
            .expect("最近の保存 submenu must exist");
        assert_eq!(
            recent.children.len(),
            RECENT_BUNDLES_LIMIT,
            "recent saves must cap at latest 5 active bundles"
        );
        assert_eq!(recent.children[0].label, "bundle-07 [Legacy (RO)]");
        assert_eq!(recent.children[1].label, "bundle-06 [ARB]");
        assert_eq!(recent.children[4].label, "bundle-03 [ARB]");
        assert!(
            matches!(
                recent.children[0].event,
                Some(TrayMenuEvent::RevealBundle { .. })
            ),
            "clicking a recent save item must reveal its folder directly"
        );
    }

    #[test]
    fn to_rpc_maps_save_window_screenshot_logs_and_toggle_accurately() {
        assert_eq!(
            TrayMenuEvent::SaveWindow { duration_secs: 300 }.to_rpc(CaptureStatus::Idle),
            Some(("save.trigger", json!({ "duration_sec": 300 })))
        );
        assert_eq!(
            TrayMenuEvent::SaveScreenshot.to_rpc(CaptureStatus::Capturing),
            Some(("save.screenshot", json!({})))
        );
        assert_eq!(
            TrayMenuEvent::SaveLogs { window_secs: 180 }.to_rpc(CaptureStatus::Capturing),
            Some(("save.logs", json!({ "window_secs": 180 })))
        );
        assert_eq!(
            TrayMenuEvent::ToggleCapture.to_rpc(CaptureStatus::Idle),
            Some(("capture.start", json!({})))
        );
        assert_eq!(
            TrayMenuEvent::ToggleCapture.to_rpc(CaptureStatus::Capturing),
            Some(("capture.stop", json!({})))
        );
        assert_eq!(
            TrayMenuEvent::ToggleCapture.to_rpc(CaptureStatus::Disconnected),
            Some(("capture.stop", json!({})))
        );
        assert_eq!(TrayMenuEvent::QuitTray.to_rpc(CaptureStatus::Idle), None);
        assert_eq!(
            TrayMenuEvent::RevealBundle {
                path: PathBuf::from("/tmp")
            }
            .to_rpc(CaptureStatus::Idle),
            None
        );
    }

    #[test]
    fn build_view_state_parses_real_shaped_rpc_responses_including_config_window() {
        let status = json!({
            "pid": 123,
            "uptime_ms": 1000,
            "capture": {
                "active": true,
                "connected": true,
                "serial": "emulator-5554",
                "boot_id": "boot-a",
                "storage_pressure_stopped": false,
                "video_frames": 900,
                "audio_frames": 10,
                "log_records": 5,
                "tap_records": 0,
                "discontinuities": 1,
                "static_hold_events": 0,
            },
            "ring_chunk_count": 2,
            "ring_total_bytes": 1024,
            "ring_pinned_chunk_count": 0,
            "active_viewports": 0,
            "active_save_jobs": 1,
            "recovery": {
                "removed_staging_dirs": 0,
                "reaped_leases": 0,
                "recovered_jobs": 0,
                "completed_purging_bundles": 0,
            },
        });
        let config = json!({
            "config": {
                "config_version": 1,
                "save_window_secs": 180,
                "video_max_fps": 30,
            },
            "config_path": "/data/arb/config.json",
        });
        let devices = json!({
            "devices": [
                { "serial": "emulator-5554", "state": "device", "model": "sdk_gphone64_arm64" }
            ]
        });
        let jobs = json!({
            "jobs": [
                {
                    "job_id": "job-1",
                    "state": "failed_retryable",
                    "destination_path": "/data/arb/bundles/job-1",
                    "pinned_chunk_ids": [],
                    "last_error": "disk full",
                    "created_at_utc": "2026-01-01T00:00:00Z",
                    "updated_at_utc": "2026-01-01T00:00:01Z",
                }
            ]
        });
        let bundles = json!({
            "bundles": [
                {
                    "bundle_id": "bundle-1",
                    "path": "/data/arb/bundles/bundle-1",
                    "format_version": "v2",
                    "state": "active",
                    "trashed_at_utc": null,
                    "parent_bundle_id": null,
                    "created_at_utc": "2026-01-01T00:00:00Z",
                    "active_leases": 0,
                }
            ]
        });

        let view = build_view_state(&status, &config, &devices, &jobs, &bundles);
        assert!(view.daemon_online);
        assert_eq!(view.capture, CaptureStatus::Capturing);
        assert_eq!(view.save_window_secs, 180);
        assert_eq!(view.buffered_secs, 30, "900 frames / 30 fps = 30s");
        assert_eq!(
            view.device_label.as_deref(),
            Some("sdk_gphone64_arm64 (emulator-5554)")
        );
        assert_eq!(view.discontinuities, 1);
        assert_eq!(view.badge, TrayBadge::SaveAttentionNeeded);
        assert_eq!(view.jobs.len(), 1);
        assert_eq!(view.jobs[0].state, SaveJobState::FailedRetryable);
        assert_eq!(view.bundles.len(), 1);
        assert_eq!(view.bundles[0].format, BundleFormatBadge::V2);
        assert_eq!(view.bundles[0].lifecycle, BundleLifecycleState::Active);
    }

    #[test]
    fn zero_emoji_guard_audits_all_tray_menus_alerts_and_sources() {
        let mut rich_state = base_state();
        rich_state.capture = CaptureStatus::Capturing;
        rich_state.device_label = Some("Pixel 9".to_owned());
        rich_state.buffered_secs = 272;
        rich_state.save_window_secs = 300;
        rich_state.discontinuities = 1;
        rich_state.jobs.push(failed_job("job-1"));
        rich_state.bundles.push(RecentBundleSummary {
            bundle_id: "bundle-1".to_owned(),
            path: PathBuf::from("/data/arb/bundles/bundle-1"),
            format: BundleFormatBadge::V2,
            lifecycle: BundleLifecycleState::Active,
            created_at_utc: "2026-11-23T00:00:00Z".to_owned(),
        });

        let mut labels = Vec::new();
        for state in [TrayViewState::offline(), base_state(), rich_state] {
            labels.push(state.status_line());
            collect_menu_labels(&build_menu(&state), &mut labels);
        }
        let alerts = [
            TrayAlert::DaemonOffline,
            TrayAlert::DaemonOnline,
            TrayAlert::ConnectionLost,
            TrayAlert::ConnectionRestored,
            TrayAlert::StoragePressureStop,
            TrayAlert::StoragePressureCleared,
            TrayAlert::CaptureStarted,
            TrayAlert::CaptureStopped,
            TrayAlert::SaveCompleted {
                job_id: "job-1".to_owned(),
            },
            TrayAlert::SaveFailedRetryable {
                job_id: "job-1".to_owned(),
                error: Some("disk full".to_owned()),
            },
        ];
        for alert in &alerts {
            labels.push(alert.message());
        }

        for label in &labels {
            assert!(
                !contains_emoji(label),
                "tray UI string must not contain emoji: {label:?}"
            );
            assert!(
                !label.contains("エビデンス"),
                "tray UI string must remain generic: {label:?}"
            );
        }

        // Scan all arb-tray src/*.rs files to prevent emoji chrome from slipping into source.
        let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(&src_dir).expect("read arb-tray/src") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let text = std::fs::read_to_string(&path).expect("read source file");
                assert!(
                    !contains_emoji(&text),
                    "arb-tray source file {} must not contain emoji codepoints",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn build_view_state_prefers_honest_buffered_secs_over_frame_estimate() {
        let status = json!({
            "buffered_secs": 42,
            "capture": {
                "active": true,
                "connected": true,
                "serial": "emulator-5554",
                "video_frames": 3,
                "discontinuities": 0,
                "static_hold_events": 0,
                "storage_pressure_stopped": false
            }
        });
        let config = json!({
            "config": {
                "save_window_secs": 300,
                "video_max_fps": 30
            }
        });
        let state = super::build_view_state(
            &status,
            &config,
            &json!({ "devices": [] }),
            &json!({ "jobs": [] }),
            &json!({ "bundles": [] }),
        );
        assert_eq!(
            state.buffered_secs, 42,
            "tray must display honest buffered_secs from system.status even on static screen"
        );

        let capped = super::build_view_state(
            &json!({
                "buffered_secs": 999,
                "capture": { "active": true, "connected": true }
            }),
            &config,
            &json!({ "devices": [] }),
            &json!({ "jobs": [] }),
            &json!({ "bundles": [] }),
        );
        assert_eq!(
            capped.buffered_secs, 300,
            "buffered_secs must clamp cleanly to save_window_secs"
        );
    }

    #[test]
    fn t6c_tray_logs_only_indicator_and_multi_device_guard() {
        let state = super::build_view_state(
            &json!({
                "buffered_secs": 18,
                "capture": {
                    "active": true,
                    "connected": true,
                    "serial": "192.168.1.50:5555",
                    "capture_mode": "logs_only"
                }
            }),
            &json!({ "config": { "save_window_secs": 300, "capture_mode": "logs_only" } }),
            &json!({ "devices": [{ "serial": "192.168.1.50:5555", "state": "device", "model": "Pixel 9" }] }),
            &json!({ "jobs": [] }),
            &json!({ "bundles": [] }),
        );
        assert_eq!(state.capture_mode, CaptureMode::LogsOnly);
        assert_eq!(
            state.status_line(),
            "REC (ログのみ) Pixel 9 (192.168.1.50:5555) · 18s / 300s"
        );
        assert!(!contains_emoji(&state.status_line()));

        let multi = super::build_view_state(
            &json!({ "buffered_secs": 0, "capture": { "active": false, "connected": false } }),
            &json!({ "config": { "save_window_secs": 300 } }),
            &json!({
                "devices": [
                    { "serial": "emulator-5554", "state": "device" },
                    { "serial": "192.168.1.50:5555", "state": "device" }
                ]
            }),
            &json!({ "jobs": [] }),
            &json!({ "bundles": [] }),
        );
        assert!(multi.multiple_devices_need_selection);
        assert!(multi.status_line().contains("複数端末接続中"));
        let menu = build_menu(&multi);
        let toggle = menu
            .iter()
            .find(|item| item.event == Some(TrayMenuEvent::ToggleCapture))
            .expect("toggle menu item");
        assert!(
            !toggle.enabled,
            "tray must disable capture start when 2+ devices are online with no selection"
        );

        let waiting = super::build_view_state(
            &json!({ "buffered_secs": 0, "capture": { "active": false, "connected": false } }),
            &json!({ "config": { "save_window_secs": 300, "device_serial": "192.168.1.50:5555" } }),
            &json!({
                "devices": [
                    { "serial": "emulator-5554", "state": "device" }
                ]
            }),
            &json!({ "jobs": [] }),
            &json!({ "bundles": [] }),
        );
        assert!(!waiting.multiple_devices_need_selection);
        assert_eq!(
            waiting.waiting_for_preferred_serial.as_deref(),
            Some("192.168.1.50:5555")
        );
        assert!(
            waiting
                .status_line()
                .contains("再接続待機中 192.168.1.50:5555")
        );
    }

    #[test]
    fn capture_enabled_is_parsed_from_status_with_config_fallback_and_rendered() {
        let empty = json!({});
        let status_on =
            json!({ "capture": { "active": true, "connected": true }, "capture_enabled": true });
        let on_rec = super::build_view_state(&status_on, &empty, &empty, &empty, &empty);
        assert_eq!(
            on_rec.capture_enabled,
            Some(true),
            "status.capture_enabled must be parsed"
        );
        assert_eq!(
            on_rec.capture_enabled_label(),
            Some("常時録画: ON・REC"),
            "armed + recording"
        );

        let status_idle = json!({ "capture": { "active": false } });
        let config_on = json!({ "config": { "capture_enabled": true } });
        let on_wait = super::build_view_state(&status_idle, &config_on, &empty, &empty, &empty);
        assert_eq!(
            on_wait.capture_enabled,
            Some(true),
            "config fallback must be used"
        );
        assert_eq!(
            on_wait.capture_enabled_label(),
            Some("常時録画: ON・待機中"),
            "armed, waiting"
        );

        let status_off =
            json!({ "capture": { "active": true, "connected": true }, "capture_enabled": false });
        let off = super::build_view_state(&status_off, &config_on, &empty, &empty, &empty);
        assert_eq!(
            off.capture_enabled,
            Some(false),
            "status must take precedence over config"
        );
        assert_eq!(
            off.capture_enabled_label(),
            Some("常時録画: OFF"),
            "disabled"
        );

        let legacy = super::build_view_state(&status_idle, &empty, &empty, &empty, &empty);
        assert_eq!(
            legacy.capture_enabled, None,
            "older arbd reports no capture_enabled"
        );
        assert_eq!(
            legacy.capture_enabled_label(),
            None,
            "no row for older arbd"
        );
        assert_eq!(
            TrayViewState::offline().capture_enabled_label(),
            None,
            "offline daemon shows no capture_enabled row"
        );

        let menu_on = build_menu(&on_wait);
        assert_eq!(
            menu_on.get(1).map(|item| item.label.as_str()),
            Some("常時録画: ON・待機中"),
            "capture_enabled row follows the status line"
        );
        assert!(!menu_on[1].enabled, "capture_enabled row is informational");
        let menu_legacy = build_menu(&legacy);
        assert!(
            menu_legacy[1].separator,
            "no capture_enabled row for older arbd"
        );
        for label in ["常時録画: ON・REC", "常時録画: ON・待機中", "常時録画: OFF"]
        {
            assert!(
                !contains_emoji(label),
                "capture_enabled label must be emoji-free"
            );
        }
    }
}
