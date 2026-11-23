//! Tray controller: connects to `arbd` over local IPC, polls its state on a configurable
//! interval, and derives a crisp, honest [`TrayViewState`] / menu model (`SPEC-01`, `SPEC-03`).
//!
//! `arb-tray` performs zero direct ring/catalog/media file writes: every mutation (`save.trigger`,
//! `save.retry`, `save.discard`, `bundle.trash`, `bundle.restore`, `capture.start`/`capture.stop`,
//! `system.shutdown`) is a JSON-RPC 2.0 call dispatched to `arbd`.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use arb_core::state::{BundleLifecycleState, SaveJobState};
use arb_os::paths::ArbPaths;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::rpc::{AutoStartMode, RpcClient, RpcClientError};

/// Default interval between `arbd` status refresh cycles.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Catalog bundle storage format badge shown in the Recent Bundles menu.
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

    /// Short badge label shown next to each Recent Bundles entry.
    pub const fn label(self) -> &'static str {
        match self {
            Self::V2 => "v2",
            Self::LegacyRo => "Legacy (RO)",
        }
    }
}

/// Tray badge state shown by the OS native icon (`src/native_tray.rs`).
///
/// Precedence when multiple conditions hold simultaneously (most to least urgent):
/// `DaemonOffline` > `StoragePressure` > `Disconnected` > `SaveAttentionNeeded` > `Capturing` >
/// `Idle`. [`derive_badge`] implements and [`crate::controller::tests`] exercises this order.
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

/// One catalog bundle surfaced in the Recent Bundles submenu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentBundleSummary {
    /// Globally unique bundle identifier.
    pub bundle_id: String,
    /// On-disk bundle directory path.
    pub path: PathBuf,
    /// Storage format badge (`v2` or `Legacy (RO)`).
    pub format: BundleFormatBadge,
    /// Lifecycle state (`active`, `trashed`, `purging`).
    pub lifecycle: BundleLifecycleState,
}

/// Derived, honest view of the live `arbd` state used to render the tray menu.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrayViewState {
    /// Tray badge state (see [`TrayBadge`] precedence).
    pub badge: TrayBadge,
    /// `true` once `system.hello`/`system.status` succeeded on the current connection.
    pub daemon_online: bool,
    /// Capture activity + device transport connectivity.
    pub capture: CaptureStatus,
    /// Active device serial, if capturing.
    pub device_serial: Option<String>,
    /// Human-readable device label (`model (serial)`), if resolvable via `device.list`.
    pub device_label: Option<String>,
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
}

impl TrayViewState {
    /// The crisp "`arbd` is unreachable" view, used whenever a connection attempt fails.
    pub fn offline() -> Self {
        Self {
            badge: TrayBadge::DaemonOffline,
            daemon_online: false,
            capture: CaptureStatus::Idle,
            device_serial: None,
            device_label: None,
            discontinuities: 0,
            static_hold_events: 0,
            storage_pressure_stopped: false,
            jobs: Vec::new(),
            bundles: Vec::new(),
        }
    }

    /// Save jobs currently in `FailedRetryable`, in `save.list_jobs` order.
    pub fn failed_jobs(&self) -> impl Iterator<Item = &SaveJobSummary> {
        self.jobs
            .iter()
            .filter(|job| job.state == SaveJobState::FailedRetryable)
    }

    /// One-line human-readable status string: device + capture state + discontinuity alerts.
    pub fn status_line(&self) -> String {
        if !self.daemon_online {
            return "arbd: offline".to_owned();
        }
        let device = self
            .device_label
            .clone()
            .or_else(|| self.device_serial.clone())
            .unwrap_or_else(|| "no device".to_owned());
        let capture = match self.capture {
            CaptureStatus::Idle => "idle",
            CaptureStatus::Capturing => "capturing",
            CaptureStatus::Disconnected => "capturing (disconnected)",
        };
        let mut line = format!("{device} — {capture}");
        if self.storage_pressure_stopped {
            line.push_str(" — storage pressure stop");
        }
        if self.discontinuities > 0 {
            let _ = write!(line, " — {} discontinuities", self.discontinuities);
        }
        if self.static_hold_events > 0 {
            let _ = write!(line, " — {} static holds", self.static_hold_events);
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
    /// Human-readable alert text suitable for a tray notification banner.
    pub fn message(&self) -> String {
        match self {
            Self::DaemonOffline => "arbd stopped responding".to_owned(),
            Self::DaemonOnline => "arbd is back online".to_owned(),
            Self::ConnectionLost => "Device disconnected during capture".to_owned(),
            Self::ConnectionRestored => "Device reconnected".to_owned(),
            Self::StoragePressureStop => "Capture stopped: host storage is running low".to_owned(),
            Self::StoragePressureCleared => "Storage pressure cleared".to_owned(),
            Self::CaptureStarted => "Capture started".to_owned(),
            Self::CaptureStopped => "Capture stopped".to_owned(),
            Self::SaveCompleted { job_id } => format!("Save completed ({job_id})"),
            Self::SaveFailedRetryable { job_id, error } => match error {
                Some(err) => format!("Save failed, retry available ({job_id}): {err}"),
                None => format!("Save failed, retry available ({job_id})"),
            },
        }
    }
}

/// Diffs `prev` against `next` and returns the alerts a user should see for this transition.
///
/// Pure and side-effect free so it can be exhaustively unit tested; emits nothing on the very
/// first observation except `DaemonOffline` (never spams on startup).
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

/// A user-triggered tray menu event. `Some` variants map 1:1 onto an `arbd` JSON-RPC call via
/// [`TrayMenuEvent::to_rpc`]; `QuitTray` and `RevealBundle` are handled locally by the tray
/// process and never reach `arbd`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrayMenuEvent {
    /// `save.trigger` with a 30 second lookback window.
    SaveLast30,
    /// `save.trigger` with a 60 second lookback window.
    SaveLast60,
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
    pub fn to_rpc(&self, capture: CaptureStatus) -> Option<(&'static str, Value)> {
        match self {
            Self::SaveLast30 => Some(("save.trigger", json!({ "duration_sec": 30 }))),
            Self::SaveLast60 => Some(("save.trigger", json!({ "duration_sec": 60 }))),
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

/// One failed-save-jobs submenu entry (`Retry` / `Discard`).
fn build_failed_job_item(job: &SaveJobSummary) -> TrayMenuItem {
    let label = job.destination_path.file_name().map_or_else(
        || job.job_id.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    TrayMenuItem::submenu(
        label,
        vec![
            TrayMenuItem::action(
                "Retry",
                TrayMenuEvent::RetryJob {
                    job_id: job.job_id.clone(),
                },
            ),
            TrayMenuItem::action(
                "Discard",
                TrayMenuEvent::DiscardJob {
                    job_id: job.job_id.clone(),
                },
            ),
        ],
    )
}

/// The "Failed Save Jobs (N)" submenu, or `None` if nothing needs attention.
fn build_failed_jobs_section(state: &TrayViewState) -> Option<TrayMenuItem> {
    let children: Vec<TrayMenuItem> = state.failed_jobs().map(build_failed_job_item).collect();
    if children.is_empty() {
        return None;
    }
    let count = children.len();
    Some(TrayMenuItem::submenu(
        format!("Failed Save Jobs ({count})"),
        children,
    ))
}

/// One Recent Bundles submenu entry (`Trash`/`Restore` + `Reveal Folder`).
fn build_bundle_item(bundle: &RecentBundleSummary) -> TrayMenuItem {
    let label = format!("{} [{}]", bundle.bundle_id, bundle.format.label());
    let mut children = Vec::new();
    match bundle.lifecycle {
        BundleLifecycleState::Active => children.push(TrayMenuItem::action(
            "Trash",
            TrayMenuEvent::TrashBundle {
                bundle_id: bundle.bundle_id.clone(),
            },
        )),
        BundleLifecycleState::Trashed => children.push(TrayMenuItem::action(
            "Restore",
            TrayMenuEvent::RestoreBundle {
                bundle_id: bundle.bundle_id.clone(),
            },
        )),
        // `Purging`, and any future `#[non_exhaustive]` state: no mutating action, which is
        // the safe default.
        BundleLifecycleState::Purging | _ => {}
    }
    children.push(TrayMenuItem::action(
        "Reveal Folder",
        TrayMenuEvent::RevealBundle {
            path: bundle.path.clone(),
        },
    ));
    TrayMenuItem::submenu(label, children)
}

/// The always-present "Recent Bundles" submenu (empty when there are no bundles yet).
fn build_bundles_section(state: &TrayViewState) -> TrayMenuItem {
    let children = state
        .bundles
        .iter()
        .filter(|bundle| bundle.lifecycle != BundleLifecycleState::Purging)
        .map(build_bundle_item)
        .collect::<Vec<_>>();
    TrayMenuItem::submenu("Recent Bundles", children)
}

/// Builds the full tray menu item tree from a [`TrayViewState`] (pure, unit-testable).
pub fn build_menu(state: &TrayViewState) -> Vec<TrayMenuItem> {
    let mut items = vec![
        TrayMenuItem::info(state.status_line()),
        TrayMenuItem::separator(),
    ];

    items.push(TrayMenuItem {
        enabled: state.daemon_online,
        ..TrayMenuItem::action("Save Last 30s", TrayMenuEvent::SaveLast30)
    });
    items.push(TrayMenuItem {
        enabled: state.daemon_online,
        ..TrayMenuItem::action("Save Last 60s", TrayMenuEvent::SaveLast60)
    });

    if let Some(failed_section) = build_failed_jobs_section(state) {
        items.push(TrayMenuItem::separator());
        items.push(failed_section);
    }

    items.push(TrayMenuItem::separator());
    items.push(build_bundles_section(state));

    items.push(TrayMenuItem::separator());
    let toggle_label = if state.capture == CaptureStatus::Idle {
        "Start Capture"
    } else {
        "Stop Capture"
    };
    items.push(TrayMenuItem {
        enabled: state.daemon_online,
        ..TrayMenuItem::action(toggle_label, TrayMenuEvent::ToggleCapture)
    });

    items.push(TrayMenuItem::separator());
    items.push(TrayMenuItem::action("Quit Tray", TrayMenuEvent::QuitTray));
    items.push(TrayMenuItem {
        enabled: state.daemon_online,
        ..TrayMenuItem::action("Stop Daemon && Quit", TrayMenuEvent::StopDaemonAndQuit)
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
    Some(RecentBundleSummary {
        bundle_id,
        path,
        format,
        lifecycle,
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

/// Builds a [`TrayViewState`] from the raw JSON results of the four polled RPC methods.
fn build_view_state(
    status: &Value,
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
    let device_serial = capture
        .get("serial")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let device_label = device_serial
        .as_deref()
        .and_then(|serial| resolve_device_label(devices, serial));
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

    TrayViewState {
        badge,
        daemon_online: true,
        capture: capture_status,
        device_serial,
        device_label,
        discontinuities,
        static_hold_events,
        storage_pressure_stopped,
        jobs,
        bundles,
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
    pub fn new(paths: ArbPaths, auto_start: AutoStartMode, poll_interval: Duration) -> Self {
        Self {
            paths,
            auto_start,
            poll_interval,
            client: None,
            last_state: None,
        }
    }

    /// Configured polling interval between refresh cycles.
    pub const fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// Performs one `system.status` + `device.list` + `save.list_jobs` + `bundle.list` refresh
    /// cycle, reconnecting first if the connection was previously lost, and returns the fresh
    /// [`TrayViewState`] alongside any transition [`TrayAlert`]s versus the prior observation.
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
        let status = client.call("system.status", json!({}))?;
        let devices = client.call("device.list", json!({}))?;
        let jobs = client.call("save.list_jobs", json!({}))?;
        let bundles = client.call("bundle.list", json!({}))?;
        Ok(build_view_state(&status, &devices, &jobs, &bundles))
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
        if result.is_err() {
            self.client = None;
        }
        result.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state() -> TrayViewState {
        TrayViewState {
            badge: TrayBadge::Idle,
            daemon_online: true,
            capture: CaptureStatus::Idle,
            device_serial: None,
            device_label: None,
            discontinuities: 0,
            static_hold_events: 0,
            storage_pressure_stopped: false,
            jobs: Vec::new(),
            bundles: Vec::new(),
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
    fn status_line_reports_offline_without_touching_other_fields() {
        let mut state = TrayViewState::offline();
        state.discontinuities = 7; // should never happen, but status_line must stay honest
        assert_eq!(state.status_line(), "arbd: offline");
    }

    #[test]
    fn status_line_includes_device_capture_and_alerts() {
        let mut state = base_state();
        state.device_label = Some("Pixel 7 (emulator-5554)".to_owned());
        state.capture = CaptureStatus::Capturing;
        state.discontinuities = 2;
        assert_eq!(
            state.status_line(),
            "Pixel 7 (emulator-5554) — capturing — 2 discontinuities"
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
    fn build_menu_offline_disables_mutating_actions() {
        let state = TrayViewState::offline();
        let menu = build_menu(&state);
        let save_30 = menu
            .iter()
            .find(|item| item.label == "Save Last 30s")
            .expect("Save Last 30s item must exist");
        assert!(
            !save_30.enabled,
            "mutating actions must be disabled offline"
        );
        assert_eq!(
            menu.first().map(|i| i.label.as_str()),
            Some("arbd: offline")
        );
    }

    #[test]
    fn build_menu_toggle_capture_label_reflects_state() {
        let mut capturing = base_state();
        capturing.capture = CaptureStatus::Capturing;
        let menu = build_menu(&capturing);
        let toggle = menu
            .iter()
            .find(|item| matches!(item.event, Some(TrayMenuEvent::ToggleCapture)))
            .expect("toggle capture item must exist");
        assert_eq!(toggle.label, "Stop Capture");

        let idle = base_state();
        let menu = build_menu(&idle);
        let toggle = menu
            .iter()
            .find(|item| matches!(item.event, Some(TrayMenuEvent::ToggleCapture)))
            .expect("toggle capture item must exist");
        assert_eq!(toggle.label, "Start Capture");
    }

    #[test]
    fn build_menu_failed_jobs_submenu_has_retry_and_discard() {
        let mut state = base_state();
        state.jobs.push(failed_job("job-42"));
        let menu = build_menu(&state);
        let submenu = menu
            .iter()
            .find(|item| item.label == "Failed Save Jobs (1)")
            .expect("failed jobs submenu must appear when a job is FailedRetryable");
        assert_eq!(submenu.children.len(), 1, "one job => one job submenu");
        let job_submenu = &submenu.children[0];
        let labels: Vec<&str> = job_submenu
            .children
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Retry", "Discard"]);
    }

    #[test]
    fn build_menu_omits_failed_jobs_submenu_when_none_failed() {
        let state = base_state();
        let menu = build_menu(&state);
        assert!(
            !menu
                .iter()
                .any(|item| item.label.starts_with("Failed Save Jobs")),
            "no FailedRetryable jobs => no submenu"
        );
    }

    #[test]
    fn build_menu_recent_bundles_v2_active_shows_trash_and_reveal() {
        let mut state = base_state();
        state.bundles.push(RecentBundleSummary {
            bundle_id: "bundle-1".to_owned(),
            path: PathBuf::from("/data/arb/bundles/bundle-1"),
            format: BundleFormatBadge::V2,
            lifecycle: BundleLifecycleState::Active,
        });
        let menu = build_menu(&state);
        let bundles_submenu = menu
            .iter()
            .find(|item| item.label == "Recent Bundles")
            .expect("Recent Bundles submenu must always exist");
        assert_eq!(bundles_submenu.children.len(), 1);
        let bundle_item = &bundles_submenu.children[0];
        assert_eq!(bundle_item.label, "bundle-1 [v2]");
        let labels: Vec<&str> = bundle_item
            .children
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Trash", "Reveal Folder"]);
    }

    #[test]
    fn build_menu_recent_bundles_legacy_trashed_shows_restore() {
        let mut state = base_state();
        state.bundles.push(RecentBundleSummary {
            bundle_id: "legacy-1".to_owned(),
            path: PathBuf::from("/data/arb/legacy-1"),
            format: BundleFormatBadge::LegacyRo,
            lifecycle: BundleLifecycleState::Trashed,
        });
        let menu = build_menu(&state);
        let bundles_submenu = menu
            .iter()
            .find(|item| item.label == "Recent Bundles")
            .expect("Recent Bundles submenu must always exist");
        let bundle_item = &bundles_submenu.children[0];
        assert_eq!(bundle_item.label, "legacy-1 [Legacy (RO)]");
        let labels: Vec<&str> = bundle_item
            .children
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Restore", "Reveal Folder"]);
    }

    #[test]
    fn to_rpc_toggle_capture_depends_on_current_state() {
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
            Some(("capture.stop", json!({}))),
            "a disconnected-but-active capture must still resolve to capture.stop"
        );
    }

    #[test]
    fn to_rpc_quit_and_reveal_are_local_only() {
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
    fn build_view_state_parses_real_shaped_rpc_responses() {
        let status = json!({
            "pid": 123,
            "uptime_ms": 1000,
            "capture": {
                "active": true,
                "connected": true,
                "serial": "emulator-5554",
                "boot_id": "boot-a",
                "storage_pressure_stopped": false,
                "video_frames": 10,
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

        let view = build_view_state(&status, &devices, &jobs, &bundles);
        assert!(view.daemon_online);
        assert_eq!(view.capture, CaptureStatus::Capturing);
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
}
