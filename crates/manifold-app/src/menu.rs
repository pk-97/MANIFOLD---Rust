//! Native application menu bar (MANIFOLD / File / Edit / View) via `muda`.
//!
//! macOS gets a real `NSMenu` in the system menu bar; Windows a real `HMENU`;
//! Linux a GTK menu. The menu *definition* below is identical on every
//! platform — only the platform attach in [`AppMenu::init_platform`] differs.
//!
//! Clicks are not handled here. [`AppMenu::drain`] pulls fired `MenuEvent`s and
//! maps each back to a [`MenuAction`]; the app then translates those into the
//! exact same `PanelAction` queue the on-screen chrome uses (see
//! `app_render.rs`), so there is one dispatch path, not two.
//!
//! Accelerator scope is deliberate. macOS routes a menu item's key equivalent
//! to the item *app-wide*, before winit ever sees the keystroke. So we only
//! attach accelerators to the unambiguous File ops + Settings — keys with no
//! contextual meaning. The editor's contextual `⌘C`/`⌘V`/`⌘Z`/`⌘G`/… keep
//! flowing to winit untouched because no menu item claims them. Unifying those
//! into context-aware menu items is a follow-up, not this pass.

use std::collections::HashMap;

use muda::accelerator::Accelerator;
use muda::{Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// Logical action a menu item triggers, resolved from its `MenuId` on drain.
/// Decoupled from `PanelAction` so the menu module has no UI-crate dependency;
/// the app maps these onto `PanelAction`s / methods in one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    // File
    New,
    Open,
    OpenRecent,
    Save,
    SaveAs,
    ImportVideo,
    ExportVideo,
    ExportFrame,
    // Edit (no accelerators — keyboard stays with winit context handlers)
    Undo,
    Redo,
    // View
    Perform,
    Monitor,
    Audio,
    // App menu
    Settings,
}

/// Owns the native menu and the id→action map. Must be kept alive for the
/// process lifetime — dropping it tears the menu down.
pub struct AppMenu {
    // Held so the native menu (and its key equivalents) stay registered.
    _menu: Menu,
    actions: HashMap<MenuId, MenuAction>,
}

impl AppMenu {
    /// Build the menu tree. Call once, on the main thread.
    pub fn new() -> Self {
        let mut actions = HashMap::new();
        let menu = build(&mut actions);
        Self {
            _menu: menu,
            actions,
        }
    }

    /// Attach the menu to the platform application.
    ///
    /// macOS: installs it as the app's main menu (system menu bar). Must run on
    /// the main thread after `NSApplication` exists — i.e. inside `resumed`.
    #[cfg(target_os = "macos")]
    pub fn init_platform(&self) {
        // Must run on the main thread (the winit event loop guarantees this).
        self._menu.init_for_nsapp();
    }

    /// Non-macOS attach. Windows wants `init_for_hwnd(hwnd)` per window and
    /// Linux `init_for_gtk_window(...)`; both need the raw window handle, wired
    /// when those platforms are brought up. The menu definition is already
    /// cross-platform — only this attach is pending.
    #[cfg(not(target_os = "macos"))]
    pub fn init_platform(&self) {}

    /// Drain fired menu events into logical actions. Call once per frame on the
    /// main thread; never blocks.
    pub fn drain(&self) -> Vec<MenuAction> {
        let mut out = Vec::new();
        while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
            if let Some(action) = self.actions.get(&ev.id) {
                out.push(*action);
            }
        }
        out
    }
}

/// Create a custom menu item, register its id→action mapping, and return it.
fn item(
    actions: &mut HashMap<MenuId, MenuAction>,
    id: &str,
    action: MenuAction,
    text: &str,
    accel: Option<&str>,
) -> MenuItem {
    // `CmdOrCtrl` resolves to ⌘ on macOS (SUPER) and Ctrl on Windows/Linux —
    // the one token that renders correctly on every platform.
    let accelerator: Option<Accelerator> = accel.and_then(|s| s.parse().ok());
    let mi = MenuItem::with_id(id, text, true, accelerator);
    actions.insert(mi.id().clone(), action);
    mi
}

fn build(actions: &mut HashMap<MenuId, MenuAction>) -> Menu {
    let menu = Menu::new();

    // ── MANIFOLD (application menu) ────────────────────────────────────────
    // The first submenu becomes the macOS application menu (titled after the
    // app automatically). Predefined items get native behaviour for free.
    let app_m = Submenu::new("MANIFOLD", true);
    let _ = app_m.append(&PredefinedMenuItem::about(Some("About MANIFOLD"), None));
    let _ = app_m.append(&PredefinedMenuItem::separator());
    let _ = app_m.append(&item(
        actions,
        "app.settings",
        MenuAction::Settings,
        "Settings…",
        Some("CmdOrCtrl+,"),
    ));
    let _ = app_m.append(&PredefinedMenuItem::separator());
    let _ = app_m.append(&PredefinedMenuItem::services(None));
    let _ = app_m.append(&PredefinedMenuItem::separator());
    let _ = app_m.append(&PredefinedMenuItem::hide(None));
    let _ = app_m.append(&PredefinedMenuItem::hide_others(None));
    let _ = app_m.append(&PredefinedMenuItem::show_all(None));
    let _ = app_m.append(&PredefinedMenuItem::separator());
    let _ = app_m.append(&PredefinedMenuItem::quit(None));

    // ── File ───────────────────────────────────────────────────────────────
    let file_m = Submenu::new("File", true);
    let _ = file_m.append(&item(actions, "file.new", MenuAction::New, "New", Some("CmdOrCtrl+N")));
    let _ = file_m.append(&item(actions, "file.open", MenuAction::Open, "Open…", Some("CmdOrCtrl+O")));
    let _ = file_m.append(&item(actions, "file.recent", MenuAction::OpenRecent, "Open Recent", None));
    let _ = file_m.append(&PredefinedMenuItem::separator());
    let _ = file_m.append(&item(actions, "file.save", MenuAction::Save, "Save", Some("CmdOrCtrl+S")));
    let _ = file_m.append(&item(
        actions,
        "file.saveas",
        MenuAction::SaveAs,
        "Save As…",
        Some("CmdOrCtrl+Shift+S"),
    ));
    let _ = file_m.append(&PredefinedMenuItem::separator());
    let _ = file_m.append(&item(
        actions,
        "file.import",
        MenuAction::ImportVideo,
        "Import Video…",
        Some("CmdOrCtrl+I"),
    ));
    let _ = file_m.append(&item(actions, "file.export", MenuAction::ExportVideo, "Export Video…", None));
    let _ = file_m.append(&item(
        actions,
        "file.exportframe",
        MenuAction::ExportFrame,
        "Export Frame…",
        None,
    ));

    // ── Edit ─────────────────────────────────────────────────────────────────
    // Undo/Redo route to the shared content-thread stack. No accelerators: the
    // existing winit handlers keep `⌘Z`/`⇧⌘Z` working with their per-window
    // redraw side effects intact. Cut/Copy/Paste are context-sensitive and join
    // here in the context-aware follow-up.
    let edit_m = Submenu::new("Edit", true);
    let _ = edit_m.append(&item(actions, "edit.undo", MenuAction::Undo, "Undo", None));
    let _ = edit_m.append(&item(actions, "edit.redo", MenuAction::Redo, "Redo", None));

    // ── View ─────────────────────────────────────────────────────────────────
    let view_m = Submenu::new("View", true);
    let _ = view_m.append(&item(actions, "view.perform", MenuAction::Perform, "Perform Mode", None));
    let _ = view_m.append(&item(actions, "view.monitor", MenuAction::Monitor, "Monitor", None));
    let _ = view_m.append(&item(actions, "view.audio", MenuAction::Audio, "Audio", None));

    let _ = menu.append(&app_m);
    let _ = menu.append(&file_m);
    let _ = menu.append(&edit_m);
    let _ = menu.append(&view_m);

    menu
}
