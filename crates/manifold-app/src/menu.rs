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
use std::path::PathBuf;

use muda::accelerator::Accelerator;
use muda::{Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// Logical action a menu item triggers, resolved from its `MenuId` on drain.
/// Decoupled from `PanelAction` so the menu module has no UI-crate dependency;
/// the app maps these onto `PanelAction`s / methods in one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    // File
    New,
    Open,
    /// Open a specific project from the dynamic "Open Recent" submenu.
    OpenRecentPath(PathBuf),
    /// Empty the recent-projects list.
    ClearRecentProjects,
    Save,
    SaveAs,
    /// Restore a history snapshot (by manifest hash) into the current
    /// project, in place. From the dynamic "Revert to Snapshot" submenu.
    RestoreSnapshot(String),
    /// Open a history snapshot (by manifest hash) as a detached copy —
    /// untitled, so the first save asks where to put it.
    OpenSnapshotCopy(String),
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
    /// Toggle the Scene Setup dock — mirrors [`Self::Audio`]
    /// (`SCENE_SETUP_PANEL_DESIGN.md` D2: the two utility docks are the
    /// same View-menu mechanism, mutually exclusive).
    Scene,
    // App menu
    Settings,
}

/// Id of the "Clear Recent Projects" item appended to the recent submenu.
/// Stable across rebuilds (string-keyed `MenuId`), so one entry in `actions`
/// covers every rebuild of the submenu.
const RECENT_CLEAR_ID: &str = "file.recent.clear";

/// One row of the dynamic "Revert to Snapshot" submenu: manifest hash plus
/// the human-readable line shown in the menu (timestamp — label/kind).
pub struct HistoryMenuEntry {
    pub hash: String,
    pub display: String,
}

/// Owns the native menu and the id→action map. Must be kept alive for the
/// process lifetime — dropping it tears the menu down.
pub struct AppMenu {
    // Held so the native menu (and its key equivalents) stay registered.
    _menu: Menu,
    actions: HashMap<MenuId, MenuAction>,
    /// The dynamic "Open Recent" submenu, rebuilt by [`set_recent_projects`].
    recent_submenu: Submenu,
    /// Maps each currently-shown recent item's id to the project it opens.
    /// Rebuilt in lockstep with `recent_submenu`.
    recent_actions: HashMap<MenuId, PathBuf>,
    /// The dynamic "Revert to Snapshot" submenu (the archive's `history/`
    /// browser), rebuilt by [`set_history_snapshots`].
    history_submenu: Submenu,
    /// Maps each currently-shown snapshot item's id to its action.
    /// Rebuilt in lockstep with `history_submenu`.
    history_actions: HashMap<MenuId, MenuAction>,
}

impl AppMenu {
    /// Build the menu tree. Call once, on the main thread.
    pub fn new() -> Self {
        let mut actions = HashMap::new();
        let (menu, recent_submenu, history_submenu) = build(&mut actions);
        Self {
            _menu: menu,
            actions,
            recent_submenu,
            recent_actions: HashMap::new(),
            history_submenu,
            history_actions: HashMap::new(),
        }
    }

    /// Rebuild the "Open Recent" submenu from `paths` (most-recent first), one
    /// clickable item per project plus a "Clear Recent Projects" footer. Call on
    /// the main thread at startup and after any project open / save. An empty
    /// list shows a single disabled placeholder.
    pub fn set_recent_projects(&mut self, paths: &[PathBuf]) {
        // Drop every existing child, then the stale id→path mappings.
        while self.recent_submenu.remove_at(0).is_some() {}
        self.recent_actions.clear();

        if paths.is_empty() {
            let placeholder = MenuItem::new("No Recent Projects", false, None);
            let _ = self.recent_submenu.append(&placeholder);
            return;
        }

        for (i, path) in paths.iter().enumerate() {
            // Show the project name (file stem); fall back to the full path.
            let label = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let id = format!("file.recent.{i}");
            let mi = MenuItem::with_id(&id, &label, true, None);
            self.recent_actions.insert(mi.id().clone(), path.clone());
            let _ = self.recent_submenu.append(&mi);
        }

        let _ = self.recent_submenu.append(&PredefinedMenuItem::separator());
        let clear = MenuItem::with_id(RECENT_CLEAR_ID, "Clear Recent Projects", true, None);
        self.actions
            .insert(clear.id().clone(), MenuAction::ClearRecentProjects);
        let _ = self.recent_submenu.append(&clear);
    }

    /// Rebuild the "Revert to Snapshot" submenu from the project archive's
    /// history entries (newest first). Each snapshot gets a nested submenu
    /// with Restore / Open a Copy. Call on the main thread after any project
    /// open or save (manual or auto) — same cadence as `set_recent_projects`.
    /// An empty list shows a single disabled placeholder.
    pub fn set_history_snapshots(&mut self, entries: &[HistoryMenuEntry]) {
        while self.history_submenu.remove_at(0).is_some() {}
        self.history_actions.clear();

        if entries.is_empty() {
            let placeholder = MenuItem::new("No Snapshots", false, None);
            let _ = self.history_submenu.append(&placeholder);
            return;
        }

        for (i, entry) in entries.iter().enumerate() {
            let snap_m = Submenu::new(&entry.display, true);

            let restore =
                MenuItem::with_id(format!("file.history.{i}.restore"), "Restore", true, None);
            self.history_actions.insert(
                restore.id().clone(),
                MenuAction::RestoreSnapshot(entry.hash.clone()),
            );
            let _ = snap_m.append(&restore);

            let copy =
                MenuItem::with_id(format!("file.history.{i}.copy"), "Open a Copy", true, None);
            self.history_actions.insert(
                copy.id().clone(),
                MenuAction::OpenSnapshotCopy(entry.hash.clone()),
            );
            let _ = snap_m.append(&copy);

            let _ = self.history_submenu.append(&snap_m);
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
                out.push(action.clone());
            } else if let Some(path) = self.recent_actions.get(&ev.id) {
                out.push(MenuAction::OpenRecentPath(path.clone()));
            } else if let Some(action) = self.history_actions.get(&ev.id) {
                out.push(action.clone());
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

/// Build the menu tree. Returns the root menu plus the two (initially empty)
/// dynamic submenus: "Open Recent" (populated via
/// [`AppMenu::set_recent_projects`]) and "Revert to Snapshot" (populated via
/// [`AppMenu::set_history_snapshots`]).
fn build(actions: &mut HashMap<MenuId, MenuAction>) -> (Menu, Submenu, Submenu) {
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
    // "Open Recent" is a submenu populated at runtime from the recent-projects
    // list (see `AppMenu::set_recent_projects`). Returned to the caller so it can
    // refresh it after each project open / save.
    let recent_m = Submenu::with_id("file.recent", "Open Recent", true);
    let _ = file_m.append(&recent_m);
    let _ = file_m.append(&PredefinedMenuItem::separator());
    let _ = file_m.append(&item(actions, "file.save", MenuAction::Save, "Save", Some("CmdOrCtrl+S")));
    let _ = file_m.append(&item(
        actions,
        "file.saveas",
        MenuAction::SaveAs,
        "Save As…",
        Some("CmdOrCtrl+Shift+S"),
    ));
    // "Revert to Snapshot" browses the archive's history/ entries (versioned
    // autosave + manual saves — GIG_RESILIENCE_DESIGN §6). Populated at
    // runtime via `AppMenu::set_history_snapshots`.
    let history_m = Submenu::with_id("file.history", "Revert to Snapshot", true);
    let _ = file_m.append(&history_m);
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
    let _ = view_m.append(&item(actions, "view.scene", MenuAction::Scene, "Scene", None));

    let _ = menu.append(&app_m);
    let _ = menu.append(&file_m);
    let _ = menu.append(&edit_m);
    let _ = menu.append(&view_m);

    (menu, recent_m, history_m)
}
