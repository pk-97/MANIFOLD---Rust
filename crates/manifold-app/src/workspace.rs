//! [`Workspace`] — per-UI-window state.
//!
//! A "workspace" is a window that hosts a `UIRoot` (the manifold-ui tree,
//! input system, panels, and offscreen render target). Distinct from the
//! `Output` window, which is content-thread-owned blit-only and has no
//! UIRoot.
//!
//! The main app holds one workspace directly (the timeline/inspector
//! window). Phase 4 of the node-graph editor lands a second optional
//! workspace (`Application::graph_editor`).

use crate::ui_root::UIRoot;

/// What kind of UI a workspace is hosting. Drives input routing and
/// per-window render specialization (e.g. only `Main` blits the
/// compositor preview into its UI offscreen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceKind {
    /// The main timeline + inspector window.
    Main,
    /// The node-graph editor window. Opened via Cmd+Shift+G (and
    /// eventually the per-effect-card cog icon).
    GraphEditor,
}

/// All per-window UI state. Owns the `UIRoot`, the offscreen render
/// target, and the CVDisplayLink that drives this window's render
/// cadence.
///
/// Other per-window concerns (cursor position, modifier state) stay
/// on `Application` for now — at most one window has focus at a time,
/// so global state matches the focused window without correctness loss.
/// This keeps the Phase 1 refactor surface small.
pub struct Workspace {
    // Stored but not yet read back: today Main-vs-GraphEditor specialization
    // (e.g. the compositor-preview blit) is decided by which optional field
    // holds the workspace (`Application::graph_editor` vs the main `ws`), not
    // by matching this enum. Un-suppresses when per-window render/input
    // routing switches to matching on `kind` directly (see the module doc
    // above + docs/GRAPH_EDITOR_REDESIGN.md).
    #[allow(dead_code)]
    pub kind: WorkspaceKind,

    /// The UI tree, input system, panels, and overlay state.
    pub ui_root: UIRoot,

    /// Offscreen render target. All UI render passes write here; the
    /// drawable is acquired late and receives a single blit from this
    /// texture. `None` until GPU init completes.
    pub ui_offscreen: Option<manifold_gpu::GpuTexture>,

    /// CVDisplayLink-driven vsync signal for this window's UI thread.
    /// `None` until the window is created in `resumed()`.
    #[cfg(target_os = "macos")]
    pub ui_display_link: Option<crate::display_link::UiDisplayLink>,

    /// True when the offscreen texture needs a fresh render this frame.
    /// Set by any visual state change (content frame, dirty panels,
    /// overlay). When false, the present path just re-blits the
    /// existing offscreen.
    pub offscreen_dirty: bool,

    /// Skip drawable acquisition this frame (surface just resized —
    /// drawable pool may be reconfiguring). Offscreen render still
    /// runs; blit skipped.
    pub surface_resized_this_frame: bool,

    /// Resizable-panel layout for this window. Only the graph-editor
    /// workspace uses it today (left preview column + right card lane, and
    /// eventually a bottom mini-timeline); the main window drives its splits
    /// through `ui_root.layout` and leaves this at its default. Single source
    /// of truth for the editor's column geometry — render and input both read
    /// `dock.rects(area)`, so hit-testing can't drift from what's drawn.
    pub dock: manifold_ui::Dock,

    /// True while the user is dragging the bottom mini-timeline to scrub the
    /// playhead. Set on a press in the strip body, cleared on release; a move
    /// while set seeks the content thread.
    pub timeline_scrubbing: bool,
}

impl Workspace {
    pub fn new(kind: WorkspaceKind) -> Self {
        Self {
            kind,
            ui_root: UIRoot::new(),
            ui_offscreen: None,
            #[cfg(target_os = "macos")]
            ui_display_link: None,
            offscreen_dirty: true,
            surface_resized_this_frame: false,
            dock: manifold_ui::Dock::editor(),
            timeline_scrubbing: false,
        }
    }
}
