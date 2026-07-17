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

    /// P5c (`docs/REALTIME_3D_DESIGN.md`): the persistent 3D-viewport
    /// session backing the sidebar preview pane when the previewed node is a
    /// top-level `node.render_scene` node and the viewport is toggled open
    /// (the `v` editor shortcut, `window_input.rs`). `None` when the
    /// viewport is closed, no render_scene node is previewed, or the
    /// previewed node is nested inside a group (`override_camera_def` only
    /// splices the camera into a node found in the TOP-LEVEL `def.nodes`
    /// list — a known P5 constraint, not new to P5c). Only meaningful on the
    /// graph-editor `Workspace`; the main window's never touches it.
    pub viewport_session: Option<manifold_renderer::node_graph::ViewportSession>,
    /// UI-device-local texture pane the viewport's composited RGBA8 blits
    /// through — the same `TexturePane::local` + `blit_texture_pane`
    /// pattern the audio spectrogram uses (`texture_pane.rs`), never an
    /// IOSurface bridge: the session renders and the present pass presents
    /// on the SAME (editor UI) thread, so there is no cross-thread hand-off
    /// to bridge.
    pub viewport_pane: Option<crate::texture_pane::TexturePane>,
    /// User toggle (the `v` editor shortcut): true while the viewport is
    /// requested open. The session itself only exists while this is `true`
    /// AND the previewed node qualifies (see `viewport_session` doc) —
    /// toggling this off always tears the session down immediately in the
    /// same frame, releasing its GPU resources.
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
    /// (editor state per `docs/REALTIME_3D_DESIGN.md` §5's "Forbidden"
    /// list: "viewport/gizmo state in `manifold-core`").
    pub viewport_gizmo_mode: manifold_renderer::node_graph::GizmoMode,
    /// The `node.scene_object` doc id the gizmo is attached to this session,
    /// set by a viewport object-pick (`viewport_gizmo::pick_object`) and
    /// cleared when the viewport closes or the def no longer resolves it
    /// (`gizmo_target_for` returning `None`). Also routed into the Scene
    /// Setup panel's own `SceneSelection` (`window_input.rs`) so Properties
    /// follows a viewport click — one selection store, not two.
    pub viewport_selected_object: Option<u32>,
    /// Active gizmo axis drag: armed on a press that hits a gizmo handle
    /// (`viewport_gizmo::pick_axis`), fed per-move deltas that dispatch
    /// `SetGraphNodeParamCommand` writes (P6's undo-round-trips-per-write
    /// gate), cleared on release. `transform_node_id` is resolved once at
    /// arm time — either the object's existing `node.transform_3d` (D8) or
    /// the id `AddObjectTransformCommand` just created (P6's "unwired
    /// transform_n → offers to create the atom" entry state) — so every
    /// subsequent move in the same gesture writes the SAME node without
    /// re-resolving the wire each time.
    pub viewport_gizmo_drag: Option<GizmoDrag>,
}

/// One active gizmo axis drag (P6). `transform_node_id` is resolved once at
/// arm time (see `viewport_gizmo_drag`'s field doc) and reused for every
/// `SetGraphNodeParamCommand` the gesture dispatches.
#[derive(Debug, Clone)]
pub struct GizmoDrag {
    pub axis: manifold_renderer::node_graph::GizmoAxis,
    pub object_node_id: u32,
    pub layer_id: manifold_core::LayerId,
    pub last_x: f32,
    pub last_y: f32,
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
            surface_resized_this_frame: false,
            dock: manifold_ui::Dock::editor(),
            timeline_scrubbing: false,
            viewport_session: None,
            viewport_pane: None,
            viewport_open: false,
            viewport_rect: None,
            viewport_drag: None,
            viewport_gizmo_mode: manifold_renderer::node_graph::GizmoMode::default(),
            viewport_selected_object: None,
            viewport_gizmo_drag: None,
        }
    }
}
