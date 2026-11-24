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
use std::io::{self, Read};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use arb_os::parent_watch::spawn_parent_watch;
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

/// After the parent closes our stdin (`--exit-on-stdin-eof`), how long the UI loop gets to act
/// on `QuitNow` before the watcher exits the process itself. Covers a UI thread stuck in OS menu
/// tracking (macOS keeps the run loop in tracking mode while the menu is open), so a dead parent
/// can never leave a tray behind holding `arb-tray.lock`; the OS releases the lock on exit.
pub const PARENT_GONE_QUIT_GRACE: Duration = Duration::from_millis(1_500);

/// How [`run`] binds the tray process to its launcher and whether it shows an icon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrayRunOptions {
    /// Parent-bound lifecycle (`--exit-on-stdin-eof`): quit when stdin reaches EOF, i.e. when
    /// the parent holding the stdin pipe (Android Studio) exits, crashes or closes it.
    pub exit_on_stdin_eof: bool,
    /// Run the `arbd` status worker without a native tray icon (`--headless`): no tray host or
    /// GUI session is needed, so CI and diagnostics can drive the real process lifecycle.
    pub headless: bool,
}

/// Side length of the rasterized tray icon in physical pixels: the macOS menu bar draws status
/// items at 18-22 pt, so 44 px gives a crisp `@2x` Retina bitmap (Windows / Linux hosts scale it
/// down cleanly).
const ICON_SIZE: u32 = 44;
/// Supersampling grid per pixel axis for anti-aliased edges (`4 x 4 = 16` samples).
const ICON_SAMPLES: u32 = 4;
/// Neutral glyph colour readable on both light and dark menu bars / task bars when the icon is
/// not drawn as a macOS template image (template images only use the alpha channel).
const GLYPH_RGB: [u8; 3] = [142, 142, 147];

/// A rasterized tray icon: straight RGBA pixels plus whether macOS should draw it as a template
/// (monochrome, automatically adapting to light / dark menu bars).
#[derive(Debug, Clone, PartialEq, Eq)]
struct IconImage {
    rgba: Vec<u8>,
    size: u32,
    template: bool,
}

/// Geometry of the replay glyph and the status dot, in pixel units of the 44 px canvas.
mod glyph {
    /// Centre of the circular arrow.
    pub(super) const CENTER: f64 = 21.0;
    /// Radius of the arrow's centre line.
    pub(super) const RADIUS: f64 = 13.0;
    /// Stroke width of the arrow ring.
    pub(super) const STROKE: f64 = 4.25;
    /// Arc start angle (degrees, counter-clockwise from +x, y up); carries a round cap.
    pub(super) const ARC_START_DEG: f64 = 100.0;
    /// Arc sweep (degrees, counter-clockwise); the arrowhead sits at the arc end.
    pub(super) const ARC_SWEEP_DEG: f64 = 300.0;
    /// Arrowhead length along the tangent.
    pub(super) const HEAD_LENGTH: f64 = 7.5;
    /// Arrowhead half-width across the ring.
    pub(super) const HEAD_HALF_WIDTH: f64 = 6.25;
    /// Status dot centre (bottom-right) and radius.
    pub(super) const DOT_CENTER: f64 = 35.0;
    pub(super) const DOT_RADIUS: f64 = 6.5;
    /// Transparent ring separating the dot from the glyph.
    pub(super) const DOT_CUTOUT_RADIUS: f64 = 9.0;
}

/// `true` when canvas point `(x, y)` (pixel units, y down) lies on the circular replay arrow.
fn replay_glyph_contains(x: f64, y: f64) -> bool {
    use glyph::{
        ARC_START_DEG, ARC_SWEEP_DEG, CENTER, HEAD_HALF_WIDTH, HEAD_LENGTH, RADIUS, STROKE,
    };
    let dx = x - CENTER;
    let dy = CENTER - y;
    let radius = dx.hypot(dy);
    let angle = dy.atan2(dx).to_degrees();
    let into_arc = (angle - ARC_START_DEG).rem_euclid(360.0);
    if (radius - RADIUS).abs() <= STROKE / 2.0 && into_arc <= ARC_SWEEP_DEG {
        return true;
    }

    // Round cap at the arc start.
    let start = ARC_START_DEG.to_radians();
    let (cap_x, cap_y) = (RADIUS * start.cos(), RADIUS * start.sin());
    if (dx - cap_x).hypot(dy - cap_y) <= STROKE / 2.0 {
        return true;
    }

    // Arrowhead at the arc end, pointing counter-clockwise (the "replay" direction).
    let end = (ARC_START_DEG + ARC_SWEEP_DEG).to_radians();
    let (px, py) = (RADIUS * end.cos(), RADIUS * end.sin());
    let (tx, ty) = (-end.sin(), end.cos());
    let (nx, ny) = (end.cos(), end.sin());
    let tip = (px + tx * HEAD_LENGTH, py + ty * HEAD_LENGTH);
    let base_a = (
        px - tx + nx * HEAD_HALF_WIDTH,
        py - ty + ny * HEAD_HALF_WIDTH,
    );
    let base_b = (
        px - tx - nx * HEAD_HALF_WIDTH,
        py - ty - ny * HEAD_HALF_WIDTH,
    );
    point_in_triangle((dx, dy), tip, base_a, base_b)
}

fn point_in_triangle(p: (f64, f64), a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> bool {
    let cross = |o: (f64, f64), u: (f64, f64), v: (f64, f64)| {
        (u.0 - o.0) * (v.1 - o.1) - (u.1 - o.1) * (v.0 - o.0)
    };
    let d1 = cross(a, b, p);
    let d2 = cross(b, c, p);
    let d3 = cross(c, a, p);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// Visual recipe per badge: glyph opacity (0-255), optional status dot colour, template flag.
const fn badge_style(badge: TrayBadge) -> (u32, Option<[u8; 3]>, bool) {
    match badge {
        // Monochrome template icons adapt to light / dark menu bars automatically on macOS.
        TrayBadge::Idle => (255, None, true),
        // Dimmed template glyph: the daemon is unreachable.
        TrayBadge::DaemonOffline => (110, None, true),
        TrayBadge::Capturing => (255, Some([48, 209, 88]), false),
        TrayBadge::Disconnected => (255, Some([255, 159, 10]), false),
        TrayBadge::SaveAttentionNeeded => (255, Some([255, 214, 10]), false),
        TrayBadge::StoragePressure => (255, Some([255, 59, 48]), false),
    }
}

/// Rasterizes the anti-aliased circular replay arrow on a fully transparent background, plus a
/// status dot (with a transparent separation ring) for attention / capture states.
fn render_badge_icon(badge: TrayBadge) -> IconImage {
    let (glyph_alpha, dot, template) = badge_style(badge);
    let samples_per_pixel = ICON_SAMPLES * ICON_SAMPLES;
    let mut rgba = Vec::new();
    for py in 0..ICON_SIZE {
        for px in 0..ICON_SIZE {
            let mut glyph_hits = 0_u32;
            let mut dot_hits = 0_u32;
            for sy in 0..ICON_SAMPLES {
                for sx in 0..ICON_SAMPLES {
                    let x = f64::from(px) + (f64::from(sx) + 0.5) / f64::from(ICON_SAMPLES);
                    let y = f64::from(py) + (f64::from(sy) + 0.5) / f64::from(ICON_SAMPLES);
                    let dot_distance = (x - glyph::DOT_CENTER).hypot(y - glyph::DOT_CENTER);
                    if dot.is_some() && dot_distance <= glyph::DOT_RADIUS {
                        dot_hits += 1;
                    } else if (dot.is_none() || dot_distance > glyph::DOT_CUTOUT_RADIUS)
                        && replay_glyph_contains(x, y)
                    {
                        glyph_hits += 1;
                    }
                }
            }
            // Weighted coverage (0..=255 * samples) of the glyph and the dot.
            let glyph_weight = glyph_hits * glyph_alpha;
            let dot_weight = dot_hits * 255;
            let total_weight = glyph_weight + dot_weight;
            let alpha = (total_weight / samples_per_pixel).min(255);
            let dot_rgb = dot.unwrap_or(GLYPH_RGB);
            let mut pixel = [0_u8; 4];
            for (channel, slot) in pixel.iter_mut().take(3).enumerate() {
                let weighted = u32::from(GLYPH_RGB[channel]) * glyph_weight
                    + u32::from(dot_rgb[channel]) * dot_weight;
                if let Some(mixed) = weighted.checked_div(total_weight) {
                    *slot = u8::try_from(mixed).unwrap_or(u8::MAX);
                }
            }
            pixel[3] = u8::try_from(alpha).unwrap_or(u8::MAX);
            rgba.extend_from_slice(&pixel);
        }
    }
    IconImage {
        rgba,
        size: ICON_SIZE,
        template,
    }
}

/// Converts a rasterized [`IconImage`] into a `tray-icon` [`Icon`].
fn to_tray_icon(image: &IconImage) -> Icon {
    Icon::from_rgba(image.rgba.clone(), image.size, image.size)
        .expect("a freshly rasterized SIZE*SIZE*4-byte RGBA buffer is always a valid icon")
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
        TrayMenuEvent::SaveWindow { duration_secs } => format!("arb-tray:save:{duration_secs}"),
        TrayMenuEvent::SaveScreenshot => "arb-tray:screenshot".to_owned(),
        TrayMenuEvent::SaveLogs { window_secs } => format!("arb-tray:save-logs:{window_secs}"),
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

/// Menu id assigned to a model item: content-stable [`event_menu_id`] for event-carrying leaves,
/// positional `arb-tray:item:<n>` ids for inert rows (they can never dispatch, so aliasing them is
/// harmless). Shared by the native builder and the structural fingerprint so both agree.
fn model_item_id(item: &ModelMenuItem, next_id: &mut u64) -> MenuId {
    item.event.as_ref().map_or_else(
        || {
            let id = MenuId::new(format!("arb-tray:item:{next_id}"));
            *next_id += 1;
            id
        },
        event_menu_id,
    )
}

/// Structural fingerprint of one menu node: everything except its label and enabled state,
/// which [`InPlaceUpdate`]s can change on the live menu without rebuilding it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MenuShape {
    Separator,
    Leaf(MenuId),
    Submenu(Vec<MenuShape>),
}

fn menu_shape(items: &[ModelMenuItem]) -> Vec<MenuShape> {
    fn walk(items: &[ModelMenuItem], next_id: &mut u64) -> Vec<MenuShape> {
        items
            .iter()
            .map(|item| {
                if item.separator {
                    MenuShape::Separator
                } else if item.children.is_empty() {
                    MenuShape::Leaf(model_item_id(item, next_id))
                } else {
                    MenuShape::Submenu(walk(&item.children, next_id))
                }
            })
            .collect()
    }
    let mut next_id = 0_u64;
    walk(items, &mut next_id)
}

/// A label / enabled change applied in place to the live native item at `path` (child indices
/// from the root menu) — no menu swap, so an open menu stays open and just refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InPlaceUpdate {
    path: Vec<usize>,
    label: Option<String>,
    enabled: Option<bool>,
}

/// How the live native menu must change to show a new model.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MenuPlan {
    /// Nothing visible changed.
    Unchanged,
    /// Same structure; only labels / enabled states changed (the common 2 s poll tick).
    InPlace(Vec<InPlaceUpdate>),
    /// Items were added, removed, or re-identified: the menu items must be rebuilt.
    Rebuild,
}

fn plan_menu_update(previous: &[ModelMenuItem], next: &[ModelMenuItem]) -> MenuPlan {
    fn diff(
        previous: &[ModelMenuItem],
        next: &[ModelMenuItem],
        prefix: &[usize],
        out: &mut Vec<InPlaceUpdate>,
    ) {
        for (index, (old, new)) in previous.iter().zip(next).enumerate() {
            let mut path = prefix.to_vec();
            path.push(index);
            if !new.separator {
                let label = (old.label != new.label).then(|| new.label.clone());
                let enabled = (old.enabled != new.enabled).then_some(new.enabled);
                if label.is_some() || enabled.is_some() {
                    out.push(InPlaceUpdate {
                        path: path.clone(),
                        label,
                        enabled,
                    });
                }
            }
            diff(&old.children, &new.children, &path, out);
        }
    }
    if menu_shape(previous) != menu_shape(next) {
        return MenuPlan::Rebuild;
    }
    let mut updates = Vec::new();
    diff(previous, next, &[], &mut updates);
    if updates.is_empty() {
        MenuPlan::Unchanged
    } else {
        MenuPlan::InPlace(updates)
    }
}

/// The OS-facing half of the tray UI. [`TrayUi`] decides *what* changes (pure, unit-tested with
/// a recording fake); implementations only apply it.
trait TrayBackend {
    /// Creates the tray icon with its (single, never swapped) root menu.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no tray host / GUI session exists (S6).
    fn create(
        &mut self,
        items: &[ModelMenuItem],
        icon: &IconImage,
        tooltip: &str,
    ) -> Result<(), String>;
    /// Updates labels / enabled states of existing live items.
    fn apply_in_place(&mut self, updates: &[InPlaceUpdate]);
    /// Replaces the items *inside* the existing root menu (never `set_menu`).
    fn replace_menu_items(&mut self, items: &[ModelMenuItem]);
    fn set_icon(&mut self, icon: &IconImage);
    fn set_tooltip(&mut self, tooltip: &str);
    /// `true` while the user has the tray menu open (macOS menu tracking).
    fn menu_is_tracking(&self) -> bool;
}

/// Icon-less backend for `--headless`: accepts every render without touching the OS.
#[derive(Debug, Default)]
struct HeadlessBackend;

impl TrayBackend for HeadlessBackend {
    fn create(
        &mut self,
        _items: &[ModelMenuItem],
        _icon: &IconImage,
        _tooltip: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    fn apply_in_place(&mut self, _updates: &[InPlaceUpdate]) {}
    fn replace_menu_items(&mut self, _items: &[ModelMenuItem]) {}
    fn set_icon(&mut self, _icon: &IconImage) {}
    fn set_tooltip(&mut self, _tooltip: &str) {}
    fn menu_is_tracking(&self) -> bool {
        false
    }
}

/// One live native menu node, mirroring the model tree for in-place updates.
enum NativeNode {
    Item(MenuItem),
    Submenu(Submenu, Vec<NativeNode>),
    Separator,
}

/// Builds real `muda` items for `items`, returning the boxed items to append and the live node
/// handles (`muda` items are reference-counted, so both refer to the same native item).
fn build_native_items(
    items: &[ModelMenuItem],
    next_id: &mut u64,
) -> (Vec<Box<dyn IsMenuItem>>, Vec<NativeNode>) {
    let mut boxed: Vec<Box<dyn IsMenuItem>> = Vec::with_capacity(items.len());
    let mut nodes = Vec::with_capacity(items.len());
    for item in items {
        if item.separator {
            boxed.push(Box::new(PredefinedMenuItem::separator()));
            nodes.push(NativeNode::Separator);
        } else if item.children.is_empty() {
            let native = MenuItem::with_id(
                model_item_id(item, next_id),
                &item.label,
                item.enabled,
                None,
            );
            boxed.push(Box::new(native.clone()));
            nodes.push(NativeNode::Item(native));
        } else {
            let (children, child_nodes) = build_native_items(&item.children, next_id);
            let child_refs: Vec<&dyn IsMenuItem> = children.iter().map(AsRef::as_ref).collect();
            let submenu = Submenu::with_items(&item.label, item.enabled, &child_refs)
                .expect("submenu construction from freshly built in-process items cannot fail");
            boxed.push(Box::new(submenu.clone()));
            nodes.push(NativeNode::Submenu(submenu, child_nodes));
        }
    }
    (boxed, nodes)
}

fn node_at<'a>(nodes: &'a [NativeNode], path: &[usize]) -> Option<&'a NativeNode> {
    let (first, rest) = path.split_first()?;
    let node = nodes.get(*first)?;
    if rest.is_empty() {
        return Some(node);
    }
    match node {
        NativeNode::Submenu(_, children) => node_at(children, rest),
        NativeNode::Item(_) | NativeNode::Separator => None,
    }
}

/// Real `tray-icon` / `muda` backend. The root [`Menu`] is attached exactly once at creation;
/// later changes mutate it in place so an open menu is never replaced out from under the user.
struct NativeBackend {
    tray_icon: Option<TrayIcon>,
    root: Menu,
    nodes: Vec<NativeNode>,
}

impl NativeBackend {
    fn new() -> Self {
        Self {
            tray_icon: None,
            root: Menu::new(),
            nodes: Vec::new(),
        }
    }

    fn populate_root(&mut self, items: &[ModelMenuItem]) {
        let mut next_id = 0_u64;
        let (boxed, nodes) = build_native_items(items, &mut next_id);
        let refs: Vec<&dyn IsMenuItem> = boxed.iter().map(AsRef::as_ref).collect();
        if let Err(err) = self.root.append_items(&refs) {
            eprintln!("[arb-tray] failed to append tray menu items: {err}");
        }
        self.nodes = nodes;
    }

    /// Linux `ksni` serves a D-Bus snapshot that is only re-read when the tray is updated, so
    /// re-announce the same root menu after mutating it. macOS / Windows mutate the live
    /// `NSMenu` / `HMENU` directly and must never re-attach it (that dismisses an open menu).
    #[cfg_attr(
        any(target_os = "macos", target_os = "windows"),
        expect(
            clippy::unused_self,
            reason = "only the Linux ksni backend re-announces the menu"
        )
    )]
    fn refresh_snapshot(&self) {
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        if let Some(tray) = &self.tray_icon {
            tray.set_menu(Some(Box::new(self.root.clone())));
        }
    }
}

impl TrayBackend for NativeBackend {
    fn create(
        &mut self,
        items: &[ModelMenuItem],
        icon: &IconImage,
        tooltip: &str,
    ) -> Result<(), String> {
        self.populate_root(items);
        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(self.root.clone()))
            .with_tooltip(tooltip);
        #[cfg(target_os = "macos")]
        let builder = if icon.template {
            builder.with_icon_templated(to_tray_icon(icon))
        } else {
            builder.with_icon(to_tray_icon(icon))
        };
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(to_tray_icon(icon));
        self.tray_icon = Some(builder.build().map_err(|err| {
            format!("failed to build tray icon (no tray host / GUI session?): {err}")
        })?);
        Ok(())
    }

    fn apply_in_place(&mut self, updates: &[InPlaceUpdate]) {
        for update in updates {
            match node_at(&self.nodes, &update.path) {
                Some(NativeNode::Item(item)) => {
                    if let Some(label) = &update.label {
                        item.set_text(label);
                    }
                    if let Some(enabled) = update.enabled {
                        item.set_enabled(enabled);
                    }
                }
                Some(NativeNode::Submenu(submenu, _)) => {
                    if let Some(label) = &update.label {
                        submenu.set_text(label);
                    }
                    if let Some(enabled) = update.enabled {
                        submenu.set_enabled(enabled);
                    }
                }
                Some(NativeNode::Separator) | None => {}
            }
        }
        self.refresh_snapshot();
    }

    fn replace_menu_items(&mut self, items: &[ModelMenuItem]) {
        while self.root.remove_at(0).is_some() {}
        self.populate_root(items);
        self.refresh_snapshot();
    }

    fn set_icon(&mut self, icon: &IconImage) {
        let Some(tray) = &self.tray_icon else { return };
        #[cfg(target_os = "macos")]
        let result = if icon.template {
            tray.set_icon_templated(Some(to_tray_icon(icon)))
        } else {
            tray.set_icon(Some(to_tray_icon(icon)))
        };
        #[cfg(not(target_os = "macos"))]
        let result = tray.set_icon(Some(to_tray_icon(icon)));
        if let Err(err) = result {
            eprintln!("[arb-tray] failed to update tray icon: {err}");
        }
    }

    fn set_tooltip(&mut self, tooltip: &str) {
        if let Some(tray) = &self.tray_icon
            && let Err(err) = tray.set_tooltip(Some(tooltip))
        {
            eprintln!("[arb-tray] failed to update tray tooltip: {err}");
        }
    }

    fn menu_is_tracking(&self) -> bool {
        menu_tracking::is_tracking()
    }
}

/// Detects whether a tray menu is currently open (being tracked).
///
/// macOS runs the main run loop in `NSEventTrackingRunLoopMode` while an `NSMenu` is open; the
/// Win32 popup menu is modal (no tray updates run while it is shown) and Linux menus are drawn
/// by the out-of-process `StatusNotifierItem` host, so both report `false`.
#[cfg(target_os = "macos")]
mod menu_tracking {
    use std::ffi::{c_char, c_void};

    type CFRunLoopRef = *mut c_void;
    type CFStringRef = *const c_void;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const TRACKING_MODE: &[u8] = b"NSEventTrackingRunLoopMode";

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetMain() -> CFRunLoopRef;
        fn CFRunLoopCopyCurrentMode(run_loop: CFRunLoopRef) -> CFStringRef;
        fn CFStringGetCString(
            string: CFStringRef,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    pub(super) fn is_tracking() -> bool {
        // SAFETY: `CFRunLoopGetMain` has no preconditions and returns a non-owned reference.
        let main_loop = unsafe { CFRunLoopGetMain() };
        // SAFETY: `main_loop` is a valid run loop; the returned mode (or null when the loop is
        // not running) is owned by us and released below.
        let mode = unsafe { CFRunLoopCopyCurrentMode(main_loop) };
        if mode.is_null() {
            return false;
        }
        let mut buffer = [0_u8; 64];
        let capacity = isize::try_from(buffer.len()).unwrap_or(isize::MAX);
        // SAFETY: `mode` is a valid CFString and `buffer` is writable for `capacity` bytes.
        let copied = unsafe {
            CFStringGetCString(
                mode,
                buffer.as_mut_ptr().cast::<c_char>(),
                capacity,
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        // SAFETY: `mode` came from a `Copy` function, so we own exactly one reference.
        unsafe { CFRelease(mode) };
        if copied == 0 {
            return false;
        }
        let len = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        buffer.get(..len) == Some(TRACKING_MODE)
    }
}

#[cfg(not(target_os = "macos"))]
mod menu_tracking {
    pub(super) const fn is_tracking() -> bool {
        false
    }
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

/// UI-thread render state: the live tray (through a [`TrayBackend`]), what it currently shows,
/// and the click→event lookup of that menu.
///
/// Rendering is incremental so an open menu is never dismissed (macOS `NSStatusItem` closes an
/// open menu when its menu or image is replaced): unchanged icon / tooltip are skipped, label and
/// enabled-state changes are applied in place on the live items, and structural changes, icon and
/// tooltip swaps are deferred while the menu is being tracked, then flushed by [`TrayUi::tick`].
struct TrayUi<B: TrayBackend> {
    backend: B,
    created: bool,
    shown_menu: Vec<ModelMenuItem>,
    shown_badge: Option<TrayBadge>,
    shown_tooltip: Option<String>,
    /// Latest state whose (deferred) parts are not yet fully shown.
    pending: Option<TrayViewState>,
    event_map: HashMap<MenuId, TrayMenuEvent>,
}

impl<B: TrayBackend> TrayUi<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            created: false,
            shown_menu: Vec::new(),
            shown_badge: None,
            shown_tooltip: None,
            pending: None,
            event_map: HashMap::new(),
        }
    }

    /// Shows `state`, touching the native tray only where something actually changed.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the very first tray build fails (e.g. no tray host or GUI session
    /// exists — S6) so the caller can print a clean diagnostic and exit non-zero.
    fn render(&mut self, state: &TrayViewState) -> Result<(), String> {
        let model = build_menu(state);
        let tooltip = state.status_line();
        if !self.created {
            self.backend
                .create(&model, &render_badge_icon(state.badge), &tooltip)?;
            self.created = true;
            self.event_map = event_map_for(&model);
            self.shown_menu = model;
            self.shown_badge = Some(state.badge);
            self.shown_tooltip = Some(tooltip);
            self.pending = None;
            return Ok(());
        }

        let tracking = self.backend.menu_is_tracking();
        let mut deferred = false;
        match plan_menu_update(&self.shown_menu, &model) {
            MenuPlan::Unchanged => {}
            MenuPlan::InPlace(updates) => {
                self.backend.apply_in_place(&updates);
                self.event_map = event_map_for(&model);
                self.shown_menu = model;
            }
            MenuPlan::Rebuild if tracking => deferred = true,
            MenuPlan::Rebuild => {
                self.backend.replace_menu_items(&model);
                self.event_map = event_map_for(&model);
                self.shown_menu = model;
            }
        }
        if self.shown_badge != Some(state.badge) {
            if tracking {
                deferred = true;
            } else {
                self.backend.set_icon(&render_badge_icon(state.badge));
                self.shown_badge = Some(state.badge);
            }
        }
        if self.shown_tooltip.as_deref() != Some(tooltip.as_str()) {
            if tracking {
                deferred = true;
            } else {
                self.backend.set_tooltip(&tooltip);
                self.shown_tooltip = Some(tooltip);
            }
        }
        self.pending = deferred.then(|| state.clone());
        Ok(())
    }

    /// Flushes changes deferred while the menu was open, once tracking has finished.
    fn tick(&mut self) {
        if self.pending.is_some()
            && !self.backend.menu_is_tracking()
            && let Some(state) = self.pending.take()
        {
            // `render` only fails on the first build, which has already succeeded.
            let _ = self.render(&state);
        }
    }
}

/// Applies one worker [`TrayUpdate`] to the UI. Returns `Some(exit_code)` when the tray
/// process should exit (graceful `QuitNow`, or a fatal first-build error — S6).
fn apply_update<B: TrayBackend>(ui: &mut TrayUi<B>, update: TrayUpdate) -> Option<i32> {
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
fn route_menu_click<B: TrayBackend>(
    ui: &TrayUi<B>,
    command_tx: &mpsc::Sender<TrayCommand>,
    id: &MenuId,
) -> bool {
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
    options: TrayRunOptions,
) -> Result<(), String> {
    let (command_tx, command_rx) = mpsc::channel::<TrayCommand>();
    let (update_tx, update_rx) = mpsc::channel::<TrayUpdate>();
    if options.exit_on_stdin_eof {
        spawn_parent_quit_watch(
            io::stdin(),
            update_tx.clone(),
            PARENT_GONE_QUIT_GRACE,
            || std::process::exit(0),
        )?;
    }
    thread::Builder::new()
        .name("arb-tray-ipc".to_owned())
        .spawn(move || ipc_worker(paths, auto_start, poll_interval, command_rx, update_tx))
        .map_err(|err| format!("failed to spawn arb-tray IPC worker thread: {err}"))?;

    if options.headless {
        let mut ui = TrayUi::new(HeadlessBackend);
        ui.render(&TrayViewState::offline())?;
        return run_polling_loop(ui, &command_tx, &update_rx);
    }

    let ui = TrayUi::new(NativeBackend::new());

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        run_with_event_loop(ui, command_tx, update_rx)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut ui = ui;
        ui.render(&TrayViewState::offline())?;
        run_polling_loop(ui, &command_tx, &update_rx)
    }
}

/// macOS / Windows: `tray-icon`'s `AppKit` / Win32 backends require an active OS event loop
/// pumped on this (main) thread; `tao` provides one with no window ever created.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_with_event_loop(
    mut ui: TrayUi<NativeBackend>,
    command_tx: mpsc::Sender<TrayCommand>,
    update_rx: mpsc::Receiver<TrayUpdate>,
) -> ! {
    use tao::event_loop::{ControlFlow, EventLoop};

    #[cfg_attr(
        not(target_os = "macos"),
        expect(unused_mut, reason = "only macOS sets an activation policy")
    )]
    let mut event_loop: EventLoop<()> = EventLoop::new();
    // macOS: run as a menu-bar-only accessory (like `LSUIElement`), so arb-tray never shows a
    // Dock icon or a Cmd+Tab entry while its status item and menu stay fully interactive.
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    event_loop.run(move |event, _target, control_flow| {
        // AppKit needs a running main event loop before creating the status item.
        if matches!(
            event,
            tao::event::Event::NewEvents(tao::event::StartCause::Init)
        ) && let Err(err) = ui.render(&TrayViewState::offline())
        {
            eprintln!("[arb-tray] {err}");
            *control_flow = ControlFlow::ExitWithCode(1);
            return;
        }
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
        ui.tick();
        *control_flow = ControlFlow::WaitUntil(Instant::now() + UI_TICK);
    })
}

/// Parent-bound quit (`--exit-on-stdin-eof`): when `reader` (stdin) reaches EOF, send `QuitNow`
/// to the UI loop — the same exit path as "Stop Daemon && Quit" — then, if the process is still
/// alive after `grace`, call `force_exit` so a UI stuck in OS menu tracking cannot outlive the
/// parent.
fn spawn_parent_quit_watch<R, F>(
    reader: R,
    update_tx: mpsc::Sender<TrayUpdate>,
    grace: Duration,
    force_exit: F,
) -> Result<(), String>
where
    R: Read + Send + 'static,
    F: FnOnce() + Send + 'static,
{
    spawn_parent_watch("arb-tray-parent-watch", reader, move |gone| {
        eprintln!("[arb-tray] parent gone ({gone:?}); quitting");
        let _ = update_tx.send(TrayUpdate::QuitNow);
        thread::sleep(grace);
        eprintln!("[arb-tray] UI loop did not quit within {grace:?}; exiting now");
        force_exit();
    })
    .map(drop)
    .map_err(|err| format!("failed to spawn arb-tray parent watch thread: {err}"))
}

/// Linux / BSD (and `--headless` on every OS): the `ksni` backend manages its own D-Bus worker
/// thread, so no event loop is required here — just a plain update/click drain loop on the
/// current thread.
fn run_polling_loop<B: TrayBackend>(
    mut ui: TrayUi<B>,
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
        ui.tick();
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
            created_at_utc: "2026-11-23T00:00:00Z".to_owned(),
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
            capture_mode: arb_core::config::CaptureMode::VideoAndLogs,
            multiple_devices_need_selection: false,
            waiting_for_preferred_serial: None,
            device_serial: None,
            device_label: None,
            save_window_secs: 300,
            buffered_secs: 0,
            discontinuities: 0,
            static_hold_events: 0,
            storage_pressure_stopped: false,
            jobs,
            bundles,
            capture_enabled: None,
            section_errors: Vec::new(),
        }
    }

    #[test]
    fn event_menu_ids_encode_action_and_target_deterministically() {
        assert_eq!(
            event_id(&TrayMenuEvent::SaveWindow { duration_secs: 300 }),
            "arb-tray:save:300"
        );
        assert_eq!(
            event_id(&TrayMenuEvent::SaveScreenshot),
            "arb-tray:screenshot"
        );
        assert_eq!(
            event_id(&TrayMenuEvent::SaveLogs { window_secs: 300 }),
            "arb-tray:save-logs:300"
        );
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
        let b1_path = PathBuf::from("/bundles").join("b-1");
        let b1_reveal = event_menu_id(&TrayMenuEvent::RevealBundle {
            path: b1_path.clone(),
        });
        assert_eq!(
            map_b.get(&b1_reveal),
            Some(&TrayMenuEvent::RevealBundle { path: b1_path }),
            "bundle reveal id must not shift when siblings are inserted"
        );
    }

    fn alpha_at(image: &IconImage, x: u32, y: u32) -> u8 {
        let index = usize::try_from((y * image.size + x) * 4 + 3).expect("index fits usize");
        image.rgba[index]
    }

    fn rgb_at(image: &IconImage, x: u32, y: u32) -> [u8; 3] {
        let base = usize::try_from((y * image.size + x) * 4).expect("index fits usize");
        [image.rgba[base], image.rgba[base + 1], image.rgba[base + 2]]
    }

    const ALL_BADGES: [TrayBadge; 6] = [
        TrayBadge::Idle,
        TrayBadge::DaemonOffline,
        TrayBadge::Capturing,
        TrayBadge::Disconnected,
        TrayBadge::SaveAttentionNeeded,
        TrayBadge::StoragePressure,
    ];

    #[test]
    fn badge_icons_are_transparent_replay_glyphs_not_solid_squares() {
        for badge in ALL_BADGES {
            let image = render_badge_icon(badge);
            assert_eq!(image.size, ICON_SIZE, "{badge:?}: retina-sized icon");
            assert_eq!(
                image.rgba.len(),
                usize::try_from(ICON_SIZE * ICON_SIZE * 4).expect("len"),
                "{badge:?}: RGBA buffer size"
            );
            let last = ICON_SIZE - 1;
            for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
                assert_eq!(
                    alpha_at(&image, x, y),
                    0,
                    "{badge:?}: corner ({x},{y}) transparent"
                );
            }
            // The glyph centre (inside the ring) stays transparent: it is a ring, not a fill.
            assert_eq!(
                alpha_at(&image, 21, 21),
                0,
                "{badge:?}: ring interior must be transparent"
            );
            let opaque = image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[3] > 0)
                .count();
            let total = usize::try_from(ICON_SIZE * ICON_SIZE).expect("count");
            assert!(
                opaque > 150,
                "{badge:?}: glyph must draw visible pixels ({opaque})"
            );
            assert!(
                opaque < total / 2,
                "{badge:?}: icon must not be a filled square ({opaque}/{total} opaque)"
            );
            let partial = image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[3] > 0 && px[3] < 255)
                .count();
            assert!(partial > 0, "{badge:?}: edges must be anti-aliased");
            let icon = Icon::from_rgba(image.rgba.clone(), image.size, image.size);
            assert!(icon.is_ok(), "{badge:?}: must be a valid tray icon");
        }
    }

    #[test]
    fn each_badge_renders_a_distinct_icon_with_template_only_for_neutral_states() {
        let images: Vec<IconImage> = ALL_BADGES.iter().map(|b| render_badge_icon(*b)).collect();
        for (i, a) in images.iter().enumerate() {
            for (j, b) in images.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    a, b,
                    "{:?} and {:?} must render distinct icons",
                    ALL_BADGES[i], ALL_BADGES[j]
                );
            }
        }
        for (badge, image) in ALL_BADGES.iter().zip(&images) {
            let neutral = matches!(badge, TrayBadge::Idle | TrayBadge::DaemonOffline);
            assert_eq!(
                image.template, neutral,
                "{badge:?}: template iff Idle / DaemonOffline"
            );
            let dot_alpha = alpha_at(image, 35, 35);
            if neutral {
                assert_eq!(dot_alpha, 0, "{badge:?}: no status dot");
            } else {
                assert_eq!(dot_alpha, 255, "{badge:?}: opaque status dot");
            }
        }
        assert_eq!(
            rgb_at(&render_badge_icon(TrayBadge::Capturing), 35, 35),
            [48, 209, 88],
            "capturing dot is green"
        );
        assert_eq!(
            rgb_at(&render_badge_icon(TrayBadge::StoragePressure), 35, 35),
            [255, 59, 48],
            "storage pressure dot is red"
        );
    }

    #[test]
    fn parent_pipe_eof_sends_quit_now_then_force_exits_only_after_the_grace() {
        use std::io::Write;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let (reader, mut writer) = std::io::pipe().expect("anonymous pipe");
        let (update_tx, update_rx) = mpsc::channel::<TrayUpdate>();
        let forced = Arc::new(AtomicBool::new(false));
        let forced_flag = Arc::clone(&forced);
        spawn_parent_quit_watch(reader, update_tx, Duration::from_millis(400), move || {
            forced_flag.store(true, Ordering::SeqCst);
        })
        .expect("spawn parent watch");

        writer.write_all(b"alive").expect("parent keep-alive write");
        assert!(
            update_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "a live parent must not quit the tray"
        );

        drop(writer);
        let update = update_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("parent EOF must reach the UI loop within 2 s");
        let mut ui = TrayUi::new(FakeBackend::default());
        assert_eq!(
            apply_update(&mut ui, update),
            Some(0),
            "parent EOF must take the QuitNow exit path with code 0"
        );
        assert!(
            !forced.load(Ordering::SeqCst),
            "the fallback exit must wait for the grace period"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !forced.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            forced.load(Ordering::SeqCst),
            "a UI loop that never quits must be force-exited after the grace period"
        );
    }

    #[test]
    fn headless_backend_renders_without_a_tray_host() {
        let mut ui = TrayUi::new(HeadlessBackend);
        ui.render(&TrayViewState::offline())
            .expect("headless render never needs a tray host");
        assert_eq!(
            apply_update(&mut ui, TrayUpdate::QuitNow),
            Some(0),
            "headless mode quits through the same QuitNow path"
        );
    }

    /// Recording fake backend: counts every native call made by [`TrayUi`].
    #[derive(Default)]
    struct FakeBackend {
        creates: usize,
        in_place: Vec<InPlaceUpdate>,
        in_place_calls: usize,
        menu_rebuilds: usize,
        icon_sets: usize,
        tooltip_sets: usize,
        tracking: bool,
    }

    impl TrayBackend for FakeBackend {
        fn create(
            &mut self,
            _items: &[ModelMenuItem],
            _icon: &IconImage,
            _tooltip: &str,
        ) -> Result<(), String> {
            self.creates += 1;
            Ok(())
        }
        fn apply_in_place(&mut self, updates: &[InPlaceUpdate]) {
            self.in_place_calls += 1;
            self.in_place.extend_from_slice(updates);
        }
        fn replace_menu_items(&mut self, _items: &[ModelMenuItem]) {
            self.menu_rebuilds += 1;
        }
        fn set_icon(&mut self, _icon: &IconImage) {
            self.icon_sets += 1;
        }
        fn set_tooltip(&mut self, _tooltip: &str) {
            self.tooltip_sets += 1;
        }
        fn menu_is_tracking(&self) -> bool {
            self.tracking
        }
    }

    fn capturing_state(buffered_secs: u32) -> TrayViewState {
        let mut state = state_with(Vec::new(), vec![active_bundle("b-1")]);
        state.capture = CaptureStatus::Capturing;
        state.badge = TrayBadge::Capturing;
        state.device_serial = Some("emulator-5554".to_owned());
        state.buffered_secs = buffered_secs;
        state
    }

    #[test]
    fn repeated_polls_with_ticking_counters_never_swap_the_menu_or_icon() {
        let mut ui = TrayUi::new(FakeBackend::default());
        ui.render(&capturing_state(10)).expect("first render");
        assert_eq!(ui.backend.creates, 1, "tray created once");

        for secs in [12, 14, 16, 18, 20] {
            ui.render(&capturing_state(secs)).expect("poll render");
        }
        assert_eq!(ui.backend.creates, 1, "never re-created");
        assert_eq!(
            ui.backend.menu_rebuilds, 0,
            "ticking counters must not rebuild the menu"
        );
        assert_eq!(
            ui.backend.icon_sets, 0,
            "unchanged badge must not reset the icon"
        );
        assert_eq!(
            ui.backend.in_place_calls, 5,
            "each tick updates the live status row in place"
        );
        assert!(
            ui.backend
                .in_place
                .iter()
                .all(|u| u.path == vec![0] && u.enabled.is_none()),
            "only the status row label changes: {:?}",
            ui.backend.in_place
        );

        let tooltip_sets = ui.backend.tooltip_sets;
        ui.render(&capturing_state(20)).expect("identical render");
        assert_eq!(
            ui.backend.in_place_calls, 5,
            "identical state: no menu call"
        );
        assert_eq!(
            ui.backend.tooltip_sets, tooltip_sets,
            "identical state: no tooltip call"
        );
    }

    #[test]
    fn structural_changes_rebuild_items_but_are_deferred_while_menu_is_open() {
        let mut ui = TrayUi::new(FakeBackend::default());
        ui.render(&capturing_state(10)).expect("first render");

        let mut grown = capturing_state(10);
        grown.bundles.insert(0, active_bundle("b-0"));
        ui.render(&grown).expect("structural render");
        assert_eq!(
            ui.backend.menu_rebuilds, 1,
            "a new bundle rebuilds the items once"
        );
        assert!(
            ui.event_map
                .contains_key(&event_menu_id(&TrayMenuEvent::RevealBundle {
                    path: PathBuf::from("/bundles").join("b-0"),
                })),
            "event map follows the rebuilt menu"
        );

        // The user opens the menu; a failed job appears and the badge changes meanwhile.
        ui.backend.tracking = true;
        let mut attention = grown.clone();
        attention.jobs.push(failed_job("job-9"));
        attention.badge = TrayBadge::SaveAttentionNeeded;
        attention.buffered_secs = 12;
        ui.render(&attention).expect("render while open");
        assert_eq!(
            ui.backend.menu_rebuilds, 1,
            "no structural swap while the menu is open"
        );
        assert_eq!(
            ui.backend.icon_sets, 0,
            "no icon swap while the menu is open"
        );
        ui.tick();
        assert_eq!(ui.backend.menu_rebuilds, 1, "still open: still deferred");

        ui.backend.tracking = false;
        ui.tick();
        assert_eq!(
            ui.backend.menu_rebuilds, 2,
            "deferred rebuild flushed after close"
        );
        assert_eq!(ui.backend.icon_sets, 1, "deferred icon flushed after close");
        assert!(
            ui.event_map
                .contains_key(&MenuId::new("arb-tray:retry:job-9")),
            "event map updated after the deferred rebuild"
        );
        ui.tick();
        assert_eq!(ui.backend.menu_rebuilds, 2, "nothing left to flush");
    }

    #[test]
    fn menu_plan_classifies_label_enabled_and_structure_changes() {
        let base = build_menu(&capturing_state(10));
        assert_eq!(
            plan_menu_update(&base, &base),
            MenuPlan::Unchanged,
            "identical"
        );

        let ticked = build_menu(&capturing_state(30));
        assert!(
            matches!(plan_menu_update(&base, &ticked), MenuPlan::InPlace(ref u) if u.len() == 1),
            "counter tick is a single in-place label update"
        );

        let mut offline = capturing_state(10);
        offline.daemon_online = false;
        assert!(
            matches!(
                plan_menu_update(&base, &build_menu(&offline)),
                MenuPlan::InPlace(_) | MenuPlan::Rebuild
            ),
            "offline transition changes enabled states"
        );

        let mut more = capturing_state(10);
        more.bundles.push(active_bundle("b-2"));
        assert_eq!(
            plan_menu_update(&base, &build_menu(&more)),
            MenuPlan::Rebuild,
            "added bundle is structural"
        );
    }
}
