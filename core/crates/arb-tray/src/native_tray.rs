#![allow(
    clippy::print_stderr,
    reason = "the native tray has no GUI window to surface background poll/dispatch failures, \
              so it logs them to stderr (the console the tray was launched from)"
)]

//! Native OS menu bar / system tray frontend for `arb-tray`.
//!
//! Converts the pure, unit-tested `controller::TrayViewState` / `controller::build_menu` model
//! into a real `tray_icon::TrayIcon` + `muda::Menu`, and dispatches activated menu items back
//! through `controller::TrayController` (`arbd` JSON-RPC calls only — zero direct file writes).
//!
//! Behaviour is identical across the three supported OSes, but the underlying OS integration
//! differs (`SPEC-01` requires this to stay the *only* place that branches on OS):
//! - macOS / Windows: `tray-icon`'s `AppKit` / Win32 backends need an OS event loop pumped on this
//!   (main) thread, provided here by `tao`.
//! - Linux / BSD: `tray-icon` is built with its `ksni` feature (pure-Rust D-Bus
//!   `StatusNotifierItem`, no GTK), which runs its own worker thread and needs no event loop, so
//!   this module just polls on a plain sleep loop instead.
//!
//! Blocking `arbd` IPC (`poll_once` / `dispatch`) never runs on the UI thread (R4 M3): a
//! dedicated `arb-tray-ipc` worker thread owns the [`TrayController`] and communicates with the
//! UI over `mpsc` channels, so a hung `arbd` or a multi-second `save.trigger` can never freeze
//! the tray menu or block `Quit`. Menu clicks resolve through content-stable [`MenuId`]s
//! (R4 M2), so a menu rebuild between click queueing and dispatch can never alias a click onto
//! a different action, bundle, or job.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use arb_os::paths::ArbPaths;
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::controller::{
    TrayAlert, TrayBadge, TrayController, TrayMenuEvent, TrayMenuItem as ModelMenuItem,
    TrayViewState, build_menu,
};
use crate::rpc::{AutoStartMode, RpcClient};

/// Interval between UI-thread wakeups that drain worker updates and queued menu clicks.
/// Identical on all OSes (R4 M3): updates/clicks are channel-driven, so this only bounds
/// worst-case UI latency — it never drives `arbd` polling itself.
const UI_TICK: Duration = Duration::from_millis(100);

/// Builds a small flat-color square [`Icon`] for `badge` (no external asset files or image
/// decoding dependencies: just a solid RGBA buffer).
fn badge_icon(badge: TrayBadge) -> Icon {
    const SIZE: u32 = 22;
    const PIXEL_COUNT: usize = 22 * 22;
    let (r, g, b) = match badge {
        TrayBadge::DaemonOffline => (120_u8, 120_u8, 120_u8),
        TrayBadge::StoragePressure => (214, 69, 54),
        TrayBadge::Disconnected => (230, 126, 34),
        TrayBadge::SaveAttentionNeeded => (241, 196, 15),
        TrayBadge::Capturing => (39, 174, 96),
        TrayBadge::Idle => (52, 152, 219),
    };
    let mut rgba = Vec::with_capacity(PIXEL_COUNT * 4);
    for _ in 0..PIXEL_COUNT {
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    Icon::from_rgba(rgba, SIZE, SIZE)
        .expect("a freshly built SIZE*SIZE*4-byte RGBA buffer is always a valid icon")
}

/// 64-bit FNV-1a over a path's raw bytes; produces a compact, deterministic suffix for
/// `RevealBundle` menu ids (paths may contain characters unsafe to embed in an id verbatim).
fn stable_path_hash(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Deterministic, content-stable [`MenuId`] for an event-carrying leaf item (R4 M2).
///
/// The id encodes the exact action *and* its target (`arb-tray:trash:<bundle_id>`,
/// `arb-tray:retry:<job_id>`…), so a menu rebuild between the OS queueing a click and the UI
/// loop dispatching it can never alias the click onto a different action, bundle, or job: the
/// id either still resolves to the same [`TrayMenuEvent`] or resolves to nothing and is safely
/// dropped.
fn event_menu_id(event: &TrayMenuEvent) -> MenuId {
    let id = match event {
        TrayMenuEvent::SaveLast30 => "arb-tray:save:30".to_owned(),
        TrayMenuEvent::SaveLast60 => "arb-tray:save:60".to_owned(),
        TrayMenuEvent::RetryJob { job_id } => format!("arb-tray:retry:{job_id}"),
        TrayMenuEvent::DiscardJob { job_id } => format!("arb-tray:discard:{job_id}"),
        TrayMenuEvent::TrashBundle { bundle_id } => format!("arb-tray:trash:{bundle_id}"),
        TrayMenuEvent::RestoreBundle { bundle_id } => {
            format!("arb-tray:restore:{bundle_id}")
        }
        TrayMenuEvent::RevealBundle { path } => {
            format!("arb-tray:reveal:{:016x}", stable_path_hash(path))
        }
        TrayMenuEvent::ToggleCapture => "arb-tray:toggle-capture".to_owned(),
        TrayMenuEvent::QuitTray => "arb-tray:quit".to_owned(),
        TrayMenuEvent::StopDaemonAndQuit => "arb-tray:stop-daemon-quit".to_owned(),
    };
    MenuId::new(id)
}

/// Pure `MenuId -> TrayMenuEvent` lookup built from the menu *model* — no real `muda` items
/// are constructed, so this is fully unit-testable headlessly on every OS (R4 M2).
fn event_map_for(items: &[ModelMenuItem]) -> HashMap<MenuId, TrayMenuEvent> {
    fn walk(items: &[ModelMenuItem], map: &mut HashMap<MenuId, TrayMenuEvent>) {
        for item in items {
            if let Some(event) = &item.event {
                map.insert(event_menu_id(event), event.clone());
            }
            walk(&item.children, map);
        }
    }
    let mut map = HashMap::new();
    walk(items, &mut map);
    map
}

/// Recursively converts the pure [`ModelMenuItem`] tree into real `muda` items. Event-carrying
/// leaves get their content-stable [`event_menu_id`]; inert rows get positional
/// `arb-tray:item:<n>` ids (they can never dispatch, so aliasing them is harmless).
fn build_native_items(items: &[ModelMenuItem], next_id: &mut u64) -> Vec<Box<dyn IsMenuItem>> {
    items
        .iter()
        .map(|item| -> Box<dyn IsMenuItem> {
            if item.separator {
                return Box::new(PredefinedMenuItem::separator());
            }
            if !item.children.is_empty() {
                let children = build_native_items(&item.children, next_id);
                let child_refs: Vec<&dyn IsMenuItem> = children.iter().map(AsRef::as_ref).collect();
                return Box::new(
                    Submenu::with_items(&item.label, item.enabled, &child_refs).expect(
                        "submenu construction from freshly built in-process items cannot fail",
                    ),
                );
            }
            let menu_id = item.event.as_ref().map_or_else(
                || {
                    let id = MenuId::new(format!("arb-tray:item:{next_id}"));
                    *next_id += 1;
                    id
                },
                event_menu_id,
            );
            Box::new(MenuItem::with_id(menu_id, &item.label, item.enabled, None))
        })
        .collect()
}

/// Builds a full native tray [`Menu`] plus its `MenuId -> TrayMenuEvent` lookup table from a
/// [`crate::controller::TrayViewState`].
fn build_native_menu(state: &TrayViewState) -> (Menu, HashMap<MenuId, TrayMenuEvent>) {
    let model_items = build_menu(state);
    let event_map = event_map_for(&model_items);
    let mut next_id = 0_u64;
    let native_items = build_native_items(&model_items, &mut next_id);
    let menu = Menu::new();
    let refs: Vec<&dyn IsMenuItem> = native_items.iter().map(AsRef::as_ref).collect();
    menu.append_items(&refs)
        .expect("appending freshly built items to a brand-new empty Menu cannot fail");
    (menu, event_map)
}

/// Reveals `path` in the host OS file manager (local-only action, never reaches `arbd`).
fn reveal_path(path: &Path) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();

    if let Err(err) = result {
        eprintln!("[arb-tray] failed to reveal {}: {err}", path.display());
    }
}

/// Probes `paths.ipc_endpoint()` once: `true` only when `arbd` is genuinely unreachable
/// (connect or `system.hello` fails), `false` when a healthy daemon answered (S2).
fn daemon_confirmed_offline(paths: &ArbPaths) -> bool {
    RpcClient::connect(paths, AutoStartMode::Disabled).is_err()
}

/// Commands sent from the tray UI thread to the background IPC worker (R4 M3).
#[derive(Debug)]
enum TrayCommand {
    /// Dispatch an RPC-backed menu event against `arbd`.
    Dispatch(TrayMenuEvent),
}

/// Updates published from the IPC worker back to the tray UI thread (R4 M3).
#[derive(Debug)]
enum TrayUpdate {
    /// A fresh poll result: latest view state plus any transition alerts.
    Refreshed {
        /// Freshly derived tray view state.
        state: TrayViewState,
        /// Transition alerts versus the previous observation.
        alerts: Vec<TrayAlert>,
    },
    /// An RPC-backed dispatch finished; `error` is `Some(message)` when it failed.
    ActionFinished {
        /// The event that was dispatched.
        event: TrayMenuEvent,
        /// Human-readable failure, if the dispatch failed.
        error: Option<String>,
    },
    /// `system.shutdown` succeeded — or the daemon is confirmed offline — so the UI must exit
    /// now (S2: "Stop Daemon && Quit" exits only in these cases).
    QuitNow,
}

/// Dedicated IPC worker (R4 M3): owns the [`TrayController`] so blocking `arbd` calls never
/// touch the UI thread. Polls every `poll_interval` and drains dispatch commands between
/// polls; exits when the UI drops its command channel or sends `QuitNow`.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the worker thread owns the ArbPaths and channel endpoints for its 'static lifetime"
)]
fn ipc_worker(
    paths: ArbPaths,
    auto_start: AutoStartMode,
    poll_interval: Duration,
    command_rx: mpsc::Receiver<TrayCommand>,
    update_tx: mpsc::Sender<TrayUpdate>,
) {
    let mut controller = TrayController::new(paths.clone(), auto_start, poll_interval);
    let mut next_poll = Instant::now();

    'worker: loop {
        let timeout = next_poll.saturating_duration_since(Instant::now());
        match command_rx.recv_timeout(timeout) {
            Ok(TrayCommand::Dispatch(event)) => {
                if matches!(event, TrayMenuEvent::StopDaemonAndQuit) {
                    match controller.dispatch(&event) {
                        Ok(_) => {
                            let _ = update_tx.send(TrayUpdate::QuitNow);
                            break 'worker;
                        }
                        Err(err) => {
                            // S2: stay running when `system.shutdown` failed but the daemon is
                            // still alive (it answered — or is reachable again); exit only
                            // when the daemon is confirmed gone.
                            if daemon_confirmed_offline(&paths) {
                                let _ = update_tx.send(TrayUpdate::QuitNow);
                                break 'worker;
                            }
                            if update_tx
                                .send(TrayUpdate::ActionFinished {
                                    event,
                                    error: Some(err.to_string()),
                                })
                                .is_err()
                            {
                                break 'worker;
                            }
                        }
                    }
                } else {
                    let result = controller.dispatch(&event);
                    if update_tx
                        .send(TrayUpdate::ActionFinished {
                            event,
                            error: result.err().map(|err| err.to_string()),
                        })
                        .is_err()
                    {
                        break 'worker;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let (state, alerts) = controller.poll_once();
                if update_tx
                    .send(TrayUpdate::Refreshed { state, alerts })
                    .is_err()
                {
                    break 'worker;
                }
                next_poll = Instant::now() + poll_interval;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'worker,
        }
    }
}

/// UI-thread render state: the live tray icon and the click→event lookup of the menu it shows.
struct TrayUi {
    tray_icon: Option<TrayIcon>,
    event_map: HashMap<MenuId, TrayMenuEvent>,
}

impl TrayUi {
    fn new() -> Self {
        Self {
            tray_icon: None,
            event_map: HashMap::new(),
        }
    }

    /// Rebuilds the native menu + icon from `state`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the very first [`TrayIconBuilder::build`] fails (e.g. no tray host
    /// or GUI session exists — S6) so the caller can print a clean diagnostic and exit
    /// non-zero instead of panicking.
    fn render(&mut self, state: &TrayViewState) -> Result<(), String> {
        let (menu, map) = build_native_menu(state);
        let icon = badge_icon(state.badge);
        let tooltip = state.status_line();

        match self.tray_icon {
            Some(ref existing) => {
                existing.set_menu(Some(Box::new(menu)));
                if let Err(err) = existing.set_tooltip(Some(tooltip)) {
                    eprintln!("[arb-tray] failed to update tray tooltip: {err}");
                }
                if let Err(err) = existing.set_icon(Some(icon)) {
                    eprintln!("[arb-tray] failed to update tray icon: {err}");
                }
            }
            None => {
                self.tray_icon = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu))
                        .with_tooltip(tooltip)
                        .with_icon(icon)
                        .build()
                        .map_err(|err| {
                            format!(
                                "failed to build tray icon (no tray host / GUI session?): {err}"
                            )
                        })?,
                );
            }
        }
        self.event_map = map;
        Ok(())
    }
}

/// Applies one worker [`TrayUpdate`] to the UI. Returns `Some(exit_code)` when the tray
/// process should exit (graceful `QuitNow`, or a fatal first-build error — S6).
fn apply_update(ui: &mut TrayUi, update: TrayUpdate) -> Option<i32> {
    match update {
        TrayUpdate::Refreshed { state, alerts } => {
            for alert in &alerts {
                eprintln!("[arb-tray] {}", alert.message());
            }
            if let Err(err) = ui.render(&state) {
                eprintln!("[arb-tray] {err}");
                return Some(1);
            }
        }
        TrayUpdate::ActionFinished { event, error } => {
            if let Some(err) = error {
                eprintln!("[arb-tray] action {event:?} failed: {err}");
            }
        }
        TrayUpdate::QuitNow => return Some(0),
    }
    None
}

/// Routes one queued OS menu click: local-only actions run inline on the UI thread, and every
/// RPC-backed event is forwarded to the IPC worker — never blocking the UI on `arbd` (R4 M3).
/// Returns `true` when the tray process should exit (`QuitTray`).
fn route_menu_click(ui: &TrayUi, command_tx: &mpsc::Sender<TrayCommand>, id: &MenuId) -> bool {
    let Some(action) = ui.event_map.get(id).cloned() else {
        // Unknown id (e.g. an inert row): nothing to dispatch — never alias another action.
        return false;
    };
    match action {
        TrayMenuEvent::QuitTray => true,
        TrayMenuEvent::RevealBundle { path } => {
            reveal_path(&path);
            false
        }
        other => {
            if command_tx.send(TrayCommand::Dispatch(other)).is_err() {
                eprintln!("[arb-tray] ipc worker unavailable; action dropped");
            }
            false
        }
    }
}

/// Runs the native tray UI until the user chooses "Quit Tray" / "Stop Daemon && Quit".
///
/// Spawns the `arb-tray-ipc` worker thread, paints an initial honest offline icon, then
/// renders worker-published state until a `QuitNow` update or `QuitTray` click ends it.
///
/// # Errors
///
/// Returns an error string if the IPC worker cannot be spawned or the tray icon cannot be
/// built at all (e.g. no tray host — S6).
pub fn run(
    paths: ArbPaths,
    auto_start: AutoStartMode,
    poll_interval: Duration,
) -> Result<(), String> {
    let (command_tx, command_rx) = mpsc::channel::<TrayCommand>();
    let (update_tx, update_rx) = mpsc::channel::<TrayUpdate>();
    thread::Builder::new()
        .name("arb-tray-ipc".to_owned())
        .spawn(move || ipc_worker(paths, auto_start, poll_interval, command_rx, update_tx))
        .map_err(|err| format!("failed to spawn arb-tray IPC worker thread: {err}"))?;

    let mut ui = TrayUi::new();
    ui.render(&TrayViewState::offline())?;

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        run_with_event_loop(ui, command_tx, update_rx)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        run_polling_loop(ui, &command_tx, &update_rx)
    }
}

/// macOS / Windows: `tray-icon`'s `AppKit` / Win32 backends require an active OS event loop
/// pumped on this (main) thread; `tao` provides one with no window ever created.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_with_event_loop(
    mut ui: TrayUi,
    command_tx: mpsc::Sender<TrayCommand>,
    update_rx: mpsc::Receiver<TrayUpdate>,
) -> ! {
    use tao::event_loop::{ControlFlow, EventLoop};

    let event_loop: EventLoop<()> = EventLoop::new();
    event_loop.run(move |_event, _target, control_flow| {
        while let Ok(update) = update_rx.try_recv() {
            if let Some(code) = apply_update(&mut ui, update) {
                *control_flow = ControlFlow::ExitWithCode(code);
                return;
            }
        }
        while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
            if route_menu_click(&ui, &command_tx, &menu_event.id) {
                *control_flow = ControlFlow::ExitWithCode(0);
                return;
            }
        }
        *control_flow = ControlFlow::WaitUntil(Instant::now() + UI_TICK);
    })
}

/// Linux / BSD: the `ksni` backend manages its own D-Bus worker thread, so no event loop is
/// required here — just a plain update/click drain loop on the current thread.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_polling_loop(
    mut ui: TrayUi,
    command_tx: &mpsc::Sender<TrayCommand>,
    update_rx: &mpsc::Receiver<TrayUpdate>,
) -> Result<(), String> {
    loop {
        while let Ok(update) = update_rx.try_recv() {
            if apply_update(&mut ui, update).is_some() {
                return Ok(());
            }
        }
        while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
            if route_menu_click(&ui, command_tx, &menu_event.id) {
                return Ok(());
            }
        }
        thread::sleep(UI_TICK);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use arb_core::state::{BundleLifecycleState, SaveJobState};

    use super::*;
    use crate::controller::{
        BundleFormatBadge, CaptureStatus, RecentBundleSummary, SaveJobSummary,
    };

    fn event_id(event: &TrayMenuEvent) -> String {
        event_menu_id(event).0
    }

    fn active_bundle(bundle_id: &str) -> RecentBundleSummary {
        RecentBundleSummary {
            bundle_id: bundle_id.to_owned(),
            path: PathBuf::from("/bundles").join(bundle_id),
            format: BundleFormatBadge::V2,
            lifecycle: BundleLifecycleState::Active,
        }
    }

    fn failed_job(job_id: &str) -> SaveJobSummary {
        SaveJobSummary {
            job_id: job_id.to_owned(),
            state: SaveJobState::FailedRetryable,
            destination_path: PathBuf::from("/out").join(job_id),
            last_error: Some("disk full".to_owned()),
        }
    }

    fn state_with(jobs: Vec<SaveJobSummary>, bundles: Vec<RecentBundleSummary>) -> TrayViewState {
        TrayViewState {
            badge: TrayBadge::Idle,
            daemon_online: true,
            capture: CaptureStatus::Idle,
            device_serial: None,
            device_label: None,
            discontinuities: 0,
            static_hold_events: 0,
            storage_pressure_stopped: false,
            jobs,
            bundles,
            section_errors: Vec::new(),
        }
    }

    #[test]
    fn event_menu_ids_encode_action_and_target_deterministically() {
        assert_eq!(event_id(&TrayMenuEvent::SaveLast30), "arb-tray:save:30");
        assert_eq!(event_id(&TrayMenuEvent::SaveLast60), "arb-tray:save:60");
        assert_eq!(
            event_id(&TrayMenuEvent::RetryJob {
                job_id: "job-7".to_owned()
            }),
            "arb-tray:retry:job-7"
        );
        assert_eq!(
            event_id(&TrayMenuEvent::DiscardJob {
                job_id: "job-7".to_owned()
            }),
            "arb-tray:discard:job-7"
        );
        assert_eq!(
            event_id(&TrayMenuEvent::TrashBundle {
                bundle_id: "b-1".to_owned()
            }),
            "arb-tray:trash:b-1"
        );
        assert_eq!(
            event_id(&TrayMenuEvent::RestoreBundle {
                bundle_id: "b-1".to_owned()
            }),
            "arb-tray:restore:b-1"
        );
        assert_eq!(
            event_id(&TrayMenuEvent::ToggleCapture),
            "arb-tray:toggle-capture"
        );
        assert_eq!(event_id(&TrayMenuEvent::QuitTray), "arb-tray:quit");
        assert_eq!(
            event_id(&TrayMenuEvent::StopDaemonAndQuit),
            "arb-tray:stop-daemon-quit"
        );
        let reveal = event_id(&TrayMenuEvent::RevealBundle {
            path: PathBuf::from("/bundles/b-1"),
        });
        assert!(
            reveal.starts_with("arb-tray:reveal:"),
            "reveal id must be content-derived, got {reveal}"
        );
    }

    #[test]
    fn menu_ids_stay_invariant_across_menu_insertions_and_reorder() {
        // Menu A: one failed job, one bundle.
        let state_a = state_with(vec![failed_job("job-1")], vec![active_bundle("b-1")]);
        // Menu B: an extra failed job (job-0) sorts *before* job-1 and an extra bundle —
        // exactly the rebuild-between-click-and-dispatch scenario from R4 M2.
        let state_b = state_with(
            vec![failed_job("job-0"), failed_job("job-1")],
            vec![active_bundle("b-0"), active_bundle("b-1")],
        );

        let map_a = event_map_for(&build_menu(&state_a));
        let map_b = event_map_for(&build_menu(&state_b));

        for (id, event) in &map_a {
            assert_eq!(
                map_b.get(id),
                Some(event),
                "menu id {id:?} must resolve to the same action after a rebuild"
            );
        }
        assert_eq!(
            map_b.get(&MenuId::new("arb-tray:retry:job-0")),
            Some(&TrayMenuEvent::RetryJob {
                job_id: "job-0".to_owned()
            }),
            "newly inserted job must get its own stable id"
        );
        assert_eq!(
            map_b.get(&MenuId::new("arb-tray:trash:b-1")),
            Some(&TrayMenuEvent::TrashBundle {
                bundle_id: "b-1".to_owned()
            }),
            "bundle trash id must not shift when siblings are inserted"
        );
    }
}
