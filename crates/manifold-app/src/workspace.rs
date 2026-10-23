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
use std::sync::Arc;

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

    /// The offscreen's physical size at the main window's last composite;
    /// `None` while the target holds nothing (just created). The main
    /// window composites whenever it differs from the target's size, so a
    /// resize or scale change needs no repaint call. The editor keys its
    /// target through `editor_frame.last_composite` instead.
    pub composited_size: Option<[u32; 2]>,

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

    /// The graph editor viewport's navigation camera and the last
    /// successfully displayed scene frame. It and the output preview's
    /// navigation (`preview_navigation`) own the one content request in
    /// turn (D7a). Content owns simulation and rendering.
    pub viewport_session: Option<crate::scene_viewport::SceneViewportNavigation>,
    /// Owner and render node of the cached scene; identical graphs on two
    /// owners must still have separate simulation and parameter histories.
    pub viewport_target: Option<(manifold_core::GraphTarget, manifold_core::NodeId)>,
    /// Request most recently sent to the content thread. The redraw path
    /// replaces this when the target or requested camera changes.
    pub viewport_request_sent: Option<Arc<crate::scene_viewport::SceneViewportRequest>>,
    /// User toggle (the `v` editor shortcut): true while the viewport is open.
    pub viewport_open: bool,
    /// The viewport's screen rect in logical window pixels, as last computed
    /// by the present pass — `None` when the viewport isn't showing this
    /// frame. Input handlers (`window_input.rs`) hit-test against this to
    /// route mouse events to `viewport_input::classify_*`/`apply` instead of
    /// the canvas's own pan/zoom.
    pub viewport_rect: Option<manifold_ui::Rect>,
    /// Active viewport navigation drag: `(button, last_logical_x,
    /// last_logical_y)`, set on a press inside `viewport_rect` while a
    /// session is open, cleared on release. Drives the per-move delta fed to
    /// `viewport_input::classify_mouse_drag`.
    pub viewport_drag: Option<(winit::event::MouseButton, f32, f32)>,
    /// P6 (`docs/REALTIME_3D_DESIGN.md` D7 Tier 2): the currently active
    /// gizmo mode (move/rotate/scale) — a plain toggle, not project state
    /// (editor state per `docs/REALTIME_3D_DESIGN.md` section 5's "Forbidden"
    /// list: "viewport/gizmo state in `manifold-core`").
    pub viewport_gizmo_mode: manifold_nodes_scene::node_graph::viewport_gizmo::GizmoMode,
    /// The main output preview's gizmo host (main window only).
    pub preview_gizmo: crate::physics_overlay::PreviewGizmo,
    /// The main output preview's preview-only navigation (main window
    /// only).
    pub preview_navigation: crate::preview_navigation::PreviewNavigation,
    /// The Scene panel's layer last sent to the content thread
    /// (`SetPhysicsOverlay`): it reports that layer's overlay frames, with
    /// Show gizmos on or off, so a click on its picture can pick.
    pub gizmo_layer_sent: Option<manifold_core::LayerId>,
    /// The Scene panel's selection drawn over the viewport, rebuilt when
    /// its scene or the selection changes.
    pub viewport_selection: Option<crate::viewport_selection::ViewportSelection>,
    /// The gizmo drag in progress, a knob scrub ended on release or
    /// cancellation.
    pub viewport_gizmo_drag: Option<crate::scene_gizmo::GizmoGesture>,
    /// Render slots of the editor viewport's selected object, for its
    /// outline (D-102).
    pub viewport_outline: Vec<u32>,
    /// The outline slots last sent to the content thread (output, viewport);
    /// main window only.
    pub outline_sent: (Option<crate::selection_outline::OutlineSlots>, Option<crate::selection_outline::OutlineSlots>),
    /// The gizmo overlay last sent to the content thread; main window only.
    pub gizmo_sent: crate::gizmo_overlay::GizmoOverlay,
    /// The graph editor's frame planner state and atlas (P5c); unused by
    /// the main window.
    pub(crate) editor_frame: crate::editor_frame::EditorFrameState,
}

impl Workspace {
    pub fn new(kind: WorkspaceKind) -> Self {
        let mut ui_root = UIRoot::new();
        if kind == WorkspaceKind::GraphEditor {
            // BUG-121 root fix: the editor window's inspector column is the
            // authoring surface (right-lane cards + mapping drawer), never
            // the perform-surface's own cards — set once here so every card
            // this instance builds (`InspectorCompositePanel::reconcile_cards`
            // / `configure_gen_params`) draws the mapping-drawer chevron
            // instead of the cog/perform chrome.
            ui_root.inspector.set_card_context(
                manifold_ui::panels::param_card::CardContext::Author,
            );
        }
        Self {
            kind,
            ui_root,
            ui_offscreen: None,
            #[cfg(target_os = "macos")]
            ui_display_link: None,
            offscreen_dirty: true,
            composited_size: None,
            surface_resized_this_frame: false,
            dock: manifold_ui::Dock::editor(),
            timeline_scrubbing: false,
            viewport_session: None,
            viewport_target: None,
            viewport_request_sent: None,
            viewport_open: false,
            viewport_rect: None,
            viewport_drag: None,
            viewport_gizmo_mode: manifold_nodes_scene::node_graph::viewport_gizmo::GizmoMode::default(),
            // The grid, a domain box and the largest handle set fit without
            // a display-frame reallocation.
            preview_gizmo: Default::default(),
            preview_navigation: Default::default(),
            gizmo_layer_sent: None,
            viewport_selection: None,
            viewport_gizmo_drag: None,
            viewport_outline: Vec::new(),
            outline_sent: (None, None),
            gizmo_sent: Default::default(),
            editor_frame: Default::default(),
        }
    }

    /// End the editor viewport's navigation (D7a): the session, its drag
    /// and its record of the request sent. True when it owned the content
    /// request.
    pub fn end_viewport_navigation(&mut self) -> bool {
        self.viewport_session = None;
        self.viewport_target = None;
        self.viewport_drag = None;
        self.viewport_request_sent.take().is_some()
    }
}
