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

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use arb_os::paths::ArbPaths;
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::controller::{
    TrayBadge, TrayController, TrayMenuEvent, TrayMenuItem as ModelMenuItem, build_menu,
};
use crate::rpc::AutoStartMode;

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

/// Recursively converts the pure [`ModelMenuItem`] tree into real `muda` items, recording a
/// `MenuId -> TrayMenuEvent` lookup table for the event-dispatch loops below.
fn build_native_items(
    items: &[ModelMenuItem],
    next_id: &mut u64,
    event_map: &mut HashMap<MenuId, TrayMenuEvent>,
) -> Vec<Box<dyn IsMenuItem>> {
    items
        .iter()
        .map(|item| -> Box<dyn IsMenuItem> {
            if item.separator {
                return Box::new(PredefinedMenuItem::separator());
            }
            if !item.children.is_empty() {
                let children = build_native_items(&item.children, next_id, event_map);
                let child_refs: Vec<&dyn IsMenuItem> = children.iter().map(AsRef::as_ref).collect();
                return Box::new(
                    Submenu::with_items(&item.label, item.enabled, &child_refs).expect(
                        "submenu construction from freshly built in-process items cannot fail",
                    ),
                );
            }
            let menu_id = MenuId::new(format!("arb-tray-{next_id}"));
            *next_id += 1;
            if let Some(event) = &item.event {
                event_map.insert(menu_id.clone(), event.clone());
            }
            Box::new(MenuItem::with_id(menu_id, &item.label, item.enabled, None))
        })
        .collect()
}

/// Builds a full native tray [`Menu`] plus its `MenuId -> TrayMenuEvent` lookup table from a
/// [`crate::controller::TrayViewState`].
fn build_native_menu(
    state: &crate::controller::TrayViewState,
) -> (Menu, HashMap<MenuId, TrayMenuEvent>) {
    let model_items = build_menu(state);
    let mut event_map = HashMap::new();
    let mut next_id = 0_u64;
    let native_items = build_native_items(&model_items, &mut next_id, &mut event_map);
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

/// Polls `controller` once, prints any transition alerts, and (re)builds the tray icon/menu.
fn refresh_tray(
    controller: &mut TrayController,
    tray_icon: &mut Option<TrayIcon>,
    event_map: &mut HashMap<MenuId, TrayMenuEvent>,
) {
    let (state, alerts) = controller.poll_once();
    for alert in &alerts {
        eprintln!("[arb-tray] {}", alert.message());
    }

    let (menu, map) = build_native_menu(&state);
    *event_map = map;
    let icon = badge_icon(state.badge);
    let tooltip = state.status_line();

    match tray_icon {
        Some(existing) => {
            existing.set_menu(Some(Box::new(menu)));
            if let Err(err) = existing.set_tooltip(Some(tooltip)) {
                eprintln!("[arb-tray] failed to update tray tooltip: {err}");
            }
            if let Err(err) = existing.set_icon(Some(icon)) {
                eprintln!("[arb-tray] failed to update tray icon: {err}");
            }
        }
        None => {
            *tray_icon = Some(
                TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip(tooltip)
                    .with_icon(icon)
                    .build()
                    .expect("building the tray icon on the main thread must succeed"),
            );
        }
    }
}

/// Handles one activated menu event: dispatches RPC-backed events through `controller`, runs
/// local-only events (`RevealBundle`) directly, and reports whether the tray process should
/// exit (`QuitTray` or `StopDaemonAndQuit`).
fn handle_menu_event(controller: &mut TrayController, action: &TrayMenuEvent) -> bool {
    match action {
        TrayMenuEvent::QuitTray => true,
        TrayMenuEvent::RevealBundle { path } => {
            reveal_path(path);
            false
        }
        other => {
            if let Err(err) = controller.dispatch(other) {
                eprintln!("[arb-tray] action failed: {err}");
            }
            matches!(other, TrayMenuEvent::StopDaemonAndQuit)
        }
    }
}

/// Runs the native tray UI until the user chooses "Quit Tray" / "Stop Daemon && Quit".
///
/// # Errors
///
/// Returns an error string if the tray UI cannot be initialized at all.
pub fn run(
    paths: ArbPaths,
    auto_start: AutoStartMode,
    poll_interval: Duration,
) -> Result<(), String> {
    let controller = TrayController::new(paths, auto_start, poll_interval);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        run_with_event_loop(controller)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        run_polling_loop(controller)
    }
}

/// macOS / Windows: `tray-icon`'s `AppKit` / Win32 backends require an active OS event loop
/// pumped on this (main) thread; `tao` provides one with no window ever created.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_with_event_loop(mut controller: TrayController) -> Result<(), String> {
    use tao::event_loop::{ControlFlow, EventLoop};

    let event_loop: EventLoop<()> = EventLoop::new();
    let mut tray_icon: Option<TrayIcon> = None;
    let mut event_map: HashMap<MenuId, TrayMenuEvent> = HashMap::new();
    let mut next_poll = Instant::now();

    event_loop.run(move |_event, _target, control_flow| {
        if Instant::now() >= next_poll {
            refresh_tray(&mut controller, &mut tray_icon, &mut event_map);
            next_poll = Instant::now() + controller.poll_interval();
        }
        *control_flow = ControlFlow::WaitUntil(next_poll);

        if let Ok(menu_event) = MenuEvent::receiver().try_recv()
            && let Some(action) = event_map.get(&menu_event.id).cloned()
            && handle_menu_event(&mut controller, &action)
        {
            *control_flow = ControlFlow::ExitWithCode(0);
        }
    })
}

/// Linux / BSD: the `ksni` backend manages its own D-Bus worker thread, so no event loop is
/// required here — just a plain poll/sleep loop on the current thread.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_polling_loop(mut controller: TrayController) -> Result<(), String> {
    const IDLE_TICK: Duration = Duration::from_millis(100);

    let mut tray_icon: Option<TrayIcon> = None;
    let mut event_map: HashMap<MenuId, TrayMenuEvent> = HashMap::new();
    let mut next_poll = Instant::now();

    loop {
        if Instant::now() >= next_poll {
            refresh_tray(&mut controller, &mut tray_icon, &mut event_map);
            next_poll = Instant::now() + controller.poll_interval();
        }
        if let Ok(menu_event) = MenuEvent::receiver().try_recv()
            && let Some(action) = event_map.get(&menu_event.id).cloned()
            && handle_menu_event(&mut controller, &action)
        {
            return Ok(());
        }
        std::thread::sleep(IDLE_TICK);
    }
}
