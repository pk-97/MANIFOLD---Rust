//! The shared per-frame graph-editor UI seam (`UI_HARNESS_UNIFICATION_DESIGN.md`
//! P3). Mirrors `ui_frame.rs`'s P1 main-window seam, but for
//! `present_graph_editor_window`: the editor is cacheless immediate-mode (D5
//! — never the `UICacheManager` atlas model), so there is no invalidation
//! seam to extract, only the tree-building + composite/render seam.
//!
//! Two functions extracted VERBATIM (moved, not rewritten) from
//! `app_render.rs`'s `present_graph_editor_window`: the left preview
//! column's tree build, and the clear/canvas/tree/dock/overlays
//! composite-and-render block. Both the live App and the headless
//! `render_graph_editor_to_png` call them — the editor window's "real path"
//! claim (D1) becomes structural (the harness paints through the exact same
//! functions the app does), replacing the old lookalike that built the
//! sidebar and the inspector as two separate scratch `UITree`s and issued
//! three separate render calls (`render.rs`'s pre-P3 `render_graph_editor_to_png`).
//!
//! App-internal module (no new crate, no new dependency, no thread-residency
//! change) — everything here is `pub(crate)`.
//!
//! ── Deviations from a literal 1:1 extraction, each argued
//! behavior-preserving for the live app:
//!
//! 1. `build_editor_preview_column` takes `panel: &GraphEditorPanel` already
//!    configured (`set_node_preview_normalize` / `set_node_inspector` called
//!    by the caller beforehand, exactly as `present_graph_editor_window` did
//!    pre-extraction at :3392/:3426) rather than the raw
//!    `node_preview_info`/`content_state` fields those calls read. Those
//!    fields live on `Application` and have no headless equivalent (no
//!    content thread, no capture bridge); `GraphEditorPanel` itself is
//!    trivially constructible (`GraphEditorPanel::new()`) and already the
//!    exact abstraction the live code funnels through, so threading the
//!    *panel* rather than the *App fields feeding it* is the non-entangled
//!    cut — the harness builds its own fresh, unconfigured panel and gets
//!    the identical "Node Output" / "Select a node" empty-state paint the
//!    live editor shows before any node is selected, through the same
//!    `render_node_inspector` call live uses, not a re-described copy of it
//!    (this is exactly what P0/P1's `render_graph_editor_to_png` was doing
//!    by hand, minus the shared function — see deviation 2).
//! 2. The lookalike's divergence was topology, not just render-path
//!    fidelity: `render_graph_editor_to_png` (pre-P3) built the preview
//!    column into a throwaway scratch `UITree` and the inspector into a
//!    *second* throwaway `UIRoot`'s tree, then issued two separate
//!    `render_tree` calls (plus a third render for the canvas). The live
//!    window builds both into ONE `UIRoot.tree` and paints it with ONE
//!    `render_tree_range` call after the canvas's immediate-mode draws.
//!    Because two separate render passes are two separate flush batches,
//!    they are not provably paint-order-identical to one merged pass even
//!    when their *content* matches — so P3 shares `build_editor_preview_column`
//!    (this module) AND repoints the harness at the live's single-`UIRoot`
//!    topology (`render.rs`), not just the composite block. This is the "if
//!    tree-building is entangled, escalate" fork in the P3 brief resolved as
//!    "not entangled, once threaded through already-computed values" —
//!    exactly the deviation-1 argument.
//! 3. `composite_editor_frame` takes `popover: &mut MappingPopover` and
//!    `text_input: &TextInputState` / `frame_timer: &FrameTimer`
//!    unconditionally rather than as `Option`s. Both types are trivially
//!    constructible with an inactive/closed default
//!    (`MappingPopover::new()` has `is_open() == false`;
//!    `TextInputState::new()` has `active: false`), so the harness passes
//!    fresh, closed instances and the guarded branches
//!    (`popover.is_open()`, `text_input.active && ...is_graph_field()`)
//!    never fire — the identical no-op the live window has on every frame
//!    neither overlay is open. This avoids threading an `Option` whose
//!    `Some`/`None` split would have to be decided by the caller anyway
//!    (mirrors P1 deviation 3's reasoning: thread the already-real,
//!    cheaply-constructed type, not a synthesized substitute).
//! 4. `composite_editor_frame` does not take a `cache: Option<&mut
//!    UICacheManager>` parameter at all (unlike `ui_frame.rs`'s
//!    `composite_main_ui_frame`) — the editor path constructs no
//!    `UICacheManager`, full stop (D5). This is not a narrowing of the P1
//!    signature; the editor was never on that model.
//!
//! ── `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1 — additional deviation found
//! at VERIFY-AT-IMPL:
//!
//! 5. `composite_editor_frame` takes `ui_root: &mut UIRoot`, not `&UIRoot` as
//!    it did through P3. The shared tree-overlay pass
//!    (`tree_passes::render_tree_overlay_passes`) performs the
//!    overlay-region dirty-clear on `ui_root.tree`, which requires `&mut`
//!    (`tree_passes.rs` module doc deviation 1) — mirrors
//!    `ui_frame.rs::composite_main_ui_frame` taking `&mut UIRoot` for the
//!    identical reason (that module's deviation 2). Every caller already
//!    held its `ui_root` behind a `mut` binding (built via `UIRoot::new()`/
//!    `sync_project_data` moments earlier, or `self.graph_editor`'s owned
//!    field), so this is source-compatible at every call site.

//!
//! ── BUG-12iyh (UI runner drives editor window, perform mode and text
//! input), editor part: the window's open and its per-frame tree build are
//! seams here too, so the ui-snap script runner opens and drives the same
//! editor the live app does. `open_editor_workspace` is what
//! `open_graph_editor` does once a window exists; `EditorLayout::compute` and
//! `build_editor_tree` are the tree half of `present_graph_editor_window`.
//! The live app and the runner call all three. What differs is input
//! presence only: the live app passes a real offscreen and a surface-derived
//! logical size and gates the frame on its vsync pulse; the runner has no
//! window, so no offscreen, a fixed logical size and a frame per step.

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture};
use manifold_ui_paint::ui_renderer::{Depth, UIRenderer};
use manifold_ui::graph_canvas::mapping_popover::MappingPopover;
use manifold_ui::graph_canvas::{GraphCanvas, Rect as CanvasRect};
use manifold_ui::node::{NodeId, TextAlign, UIStyle};
use manifold_ui::panels::graph_editor::GraphEditorPanel;
use manifold_ui::{Dock, MiniClip, MiniLayerLabel, MiniTimeline, Rect as UiRect, UITree};

use crate::frame_timer::FrameTimer;
use crate::text_input::TextInputState;
use crate::ui_root::UIRoot;
use crate::workspace::{Workspace, WorkspaceKind};

/// A first open sizes the editor at three quarters of the primary window's
/// logical size; later opens restore where the user left it.
pub(crate) fn first_open_logical_size(primary_logical_w: f64, primary_logical_h: f64) -> (u32, u32) {
    ((primary_logical_w * 0.75) as u32, (primary_logical_h * 0.75) as u32)
}

/// The graph editor's workspace and canvas as a fresh open makes them, with
/// the inspector column already projected so it is not blank on its first
/// frame. `offscreen` is the window's render target; `None` when there is no
/// window to present to. The caller owns everything window-bound (the
/// surface, the display link).
pub(crate) fn open_editor_workspace(
    offscreen: Option<GpuTexture>,
    project: &manifold_core::project::Project,
    active_layer: Option<usize>,
    selection: &manifold_ui::UIState,
    content_state: &crate::content_state::ContentState,
) -> (Workspace, GraphCanvas) {
    let mut ws = Workspace::new(WorkspaceKind::GraphEditor);
    ws.ui_offscreen = offscreen;
    crate::ui_bridge::sync_project_data(&mut ws.ui_root, project, active_layer, selection);
    crate::ui_bridge::sync_inspector_data(
        &mut ws.ui_root,
        project,
        active_layer,
        selection,
        &content_state.automation_latched_params,
        content_state.led_preview.as_ref(),
    );
    (ws, GraphCanvas::new())
}

/// The editor window's geometry for one frame: the dock's columns, the two
/// stacked monitors in the left column and the card lane on the right. One
/// computation feeds the tree build, the composite and the monitor blits.
#[derive(Clone, Copy)]
pub(crate) struct EditorLayout {
    pub logical_w: u32,
    pub logical_h: u32,
    pub area: UiRect,
    pub preview_width: f32,
    pub canvas: UiRect,
    pub bottom: UiRect,
    pub card_viewport: UiRect,
    pub preview_x: f32,
    pub preview_w: f32,
    pub preview_h: f32,
    pub node_title_y: f32,
    pub node_img_y: f32,
    pub master_title_y: f32,
    pub master_img_y: f32,
    /// Width over height of the monitors and the node atlas cells.
    pub monitor_aspect: f32,
}

impl EditorLayout {
    /// Monitors take the output aspect (a portrait show gets portrait
    /// monitors), fit to the column width and clamp in height so both panes
    /// fit above the mini-timeline strip; the pair is centred vertically.
    /// `monitor_aspect` is the content pipeline's output aspect; 16:9 when
    /// there is no pipeline.
    pub(crate) fn compute(dock: &Dock, logical_w: u32, logical_h: u32, monitor_aspect: Option<f32>) -> Self {
        let monitor_aspect = monitor_aspect.unwrap_or(16.0 / 9.0);
        let area = UiRect::new(0.0, 0.0, logical_w as f32, logical_h as f32);
        let rects = dock.rects(area);
        let preview_width = rects.left.width;
        let canvas_height = rects.canvas.height;
        let preview_pad = 8.0_f32;
        let preview_title_h = 18.0_f32;
        let avail_w = (preview_width - 2.0 * preview_pad).max(1.0);
        let max_body_h =
            ((canvas_height - 3.0 * preview_pad - 2.0 * preview_title_h) * 0.5).max(1.0);
        let width_bound_h = avail_w / monitor_aspect;
        let (preview_w, preview_h) = if width_bound_h <= max_body_h {
            (avail_w, width_bound_h)
        } else {
            (max_body_h * monitor_aspect, max_body_h)
        };
        let preview_x = (preview_width - preview_w) * 0.5;
        let pane_block_h = 2.0 * (preview_title_h + preview_h) + preview_pad;
        let node_title_y = ((canvas_height - pane_block_h) * 0.5).max(preview_pad);
        let node_img_y = node_title_y + preview_title_h;
        let master_title_y = node_img_y + preview_h + preview_pad;
        let master_img_y = master_title_y + preview_title_h;
        Self {
            logical_w,
            logical_h,
            area,
            preview_width,
            canvas: rects.canvas,
            bottom: rects.bottom,
            card_viewport: UiRect::new(rects.right.x, 0.0, rects.right.width, canvas_height),
            preview_x,
            preview_w,
            preview_h,
            node_title_y,
            node_img_y,
            master_title_y,
            master_img_y,
            monitor_aspect,
        }
    }

    /// The canvas viewport `canvas.render` and the pointer handlers use.
    pub(crate) fn canvas_rect(&self) -> CanvasRect {
        CanvasRect::new(self.canvas.x, 0.0, self.canvas.width, self.canvas.height)
    }
}

/// What one editor tree build reads besides the workspace itself.
pub(crate) struct EditorTreeInputs<'a> {
    pub project: &'a manifold_core::project::Project,
    pub active_layer: Option<usize>,
    pub fire_meters: &'a manifold_core::audio_trigger::FireMeterCapture,
    pub dt: f32,
    /// The node monitor shows an image this frame (the selected node has one,
    /// or the 3D viewport is displaying a frame).
    pub show_image: bool,
}

/// Builds the editor window's tree for one frame: ticks the inspector's own
/// tweens and fire meters, rebuilds the card lane, builds the preview column
/// and the overlays, re-applies hover and press flags, and lays out the
/// canvas's pending focus and fit. `panel` is configured by the caller as for
/// [`build_editor_preview_column`]. Returns the smart-preview toggle's id.
pub(crate) fn build_editor_tree(
    ui_root: &mut UIRoot,
    panel: &GraphEditorPanel,
    canvas: Option<&mut GraphCanvas>,
    layout: &EditorLayout,
    inputs: EditorTreeInputs<'_>,
) -> Option<NodeId> {
    // The editor root never sets `built`, so its `update()` early-returns;
    // tick the inspector's drawer tweens, value flash and fire meters here,
    // before the rebuild reads them, or the card frame sits at a stale height
    // under rows built at their true size (BUG-160 (editor card frame stale
    // height)).
    ui_root.tick_inspector();
    ui_root.update_fire_meters(inputs.fire_meters, inputs.dt);
    crate::editor_bridge::rebuild_editor_inspector(
        ui_root,
        inputs.project,
        inputs.active_layer,
        layout.card_viewport,
    );
    let toggle_id = build_editor_preview_column(
        &mut ui_root.tree,
        panel,
        layout.preview_width,
        layout.canvas.height,
        layout.preview_x,
        layout.preview_w,
        layout.preview_h,
        layout.node_title_y,
        layout.node_img_y,
        layout.master_title_y,
        inputs.show_image,
    );
    // Overlays build last, through the main window's driver, so they sit on
    // top in the Overlays arena and record `overlay_draw`.
    ui_root.build_overlays_for_screen(layout.logical_w as f32, layout.logical_h as f32);
    ui_root.tick_editor_perf_hud();
    // The rebuild cleared every node's flags; restore hover and press from the
    // input system's durable widget state.
    ui_root.input.apply_interaction_flags(&mut ui_root.tree);
    if let Some(canvas) = canvas {
        let vp = layout.canvas_rect();
        canvas.resolve_pending_focus(vp);
        canvas.apply_pending_fit(vp);
        // Wire-to-port magnetize needs port screen positions, so it ticks
        // here beside the draw rather than in the main canvas tick.
        canvas.tick_wire_magnet(vp);
    }
    toggle_id
}

/// Pushes the watched graph into the canvas for one frame. The preview
/// aspect goes first, so a level's first layout takes the right node-face
/// heights; then the canvas's own tweens (marquee fade, connect pop, error
/// shake), the snapshot, this frame's live (modulated) node values over it so
/// a driver or card slider is seen moving each knob, and the reset pill.
pub(crate) fn feed_canvas(
    canvas: &mut GraphCanvas,
    graph: &manifold_ui::graph_view::GraphSnapshot,
    preview_aspect: Option<f32>,
    dt_ms: f32,
    live: &manifold_ui::graph_view::LiveNodeParams,
    has_graph_mod: bool,
) {
    if let Some(aspect) = preview_aspect {
        canvas.set_preview_aspect(aspect);
    }
    canvas.tick(dt_ms);
    canvas.set_snapshot(graph);
    canvas.apply_live_values(live);
    canvas.set_has_graph_mod(has_graph_mod);
}

/// Builds the editor sidebar's left preview column — backing panel,
/// node-output pane (image, or `panel`'s value-inspector text, or a "Select
/// a node" hint), master-out title — into `tree` at the given layout rect.
/// `panel` must already be configured by the caller
/// (`set_node_preview_normalize` / `set_node_inspector`), exactly as
/// `present_graph_editor_window` configured `self.graph_editor_panel` before
/// this block ran pre-extraction. Returns the smart-preview toggle's
/// `NodeId` when drawn (the live caller wires it to input handling; the
/// harness ignores it — no interactive input reaches a single-shot render).
/// Precedent: app_render.rs (pre-extraction) :3449-3562, moved not rewritten.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_editor_preview_column(
    tree: &mut UITree,
    panel: &GraphEditorPanel,
    preview_width: f32,
    canvas_height: f32,
    preview_x: f32,
    preview_w: f32,
    preview_h: f32,
    node_title_y: f32,
    node_img_y: f32,
    master_title_y: f32,
    show_image: bool,
) -> Option<NodeId> {
    let title_style = UIStyle {
        text_color: manifold_ui::color::TEXT_WHITE_C32,
        font_size: 14,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    };
    let title_x = preview_x;
    let preview_title_h = 18.0_f32;

    // Protective wrap only (see the live call site's comment) — not the
    // editor window's P2 region migration.
    let preview_region = tree.begin_region(
        manifold_ui::Rect::new(0.0, 0.0, preview_width, canvas_height),
        manifold_ui::ZTier::Base,
        "editor_preview_column",
        manifold_ui::UIFlags::empty(),
    );
    let preview_region_start = tree.count();
    tree.add_panel(
        None,
        0.0,
        0.0,
        preview_width,
        canvas_height,
        UIStyle {
            bg_color: manifold_ui::color::EFFECT_CARD_INNER_BG_C32,
            ..UIStyle::default()
        },
    );

    let node_region = manifold_ui::Rect::new(
        title_x,
        node_title_y,
        preview_w,
        preview_title_h + preview_h,
    );
    let inspector_drawn = panel.render_node_inspector(tree, node_region);

    let mut toggle_id = None;
    if !inspector_drawn {
        tree.add_label(
            None,
            title_x,
            node_title_y,
            preview_w,
            preview_title_h,
            "Node Output",
            title_style,
        );
        if show_image {
            let toggle_region = manifold_ui::Rect::new(
                title_x + preview_w * 0.42,
                node_title_y,
                preview_w * 0.58,
                preview_title_h,
            );
            toggle_id = Some(panel.render_smart_preview_toggle(tree, toggle_region));
        }
        if !show_image {
            tree.add_label(
                None,
                title_x,
                node_img_y + preview_h * 0.5 - 8.0,
                preview_w,
                16.0,
                "Select a node",
                UIStyle {
                    text_color: manifold_ui::color::TEXT_DIMMED_C32,
                    font_size: 12,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
        }
    }
    tree.add_label(
        None,
        title_x,
        master_title_y,
        preview_w,
        preview_title_h,
        "Master Out",
        title_style,
    );
    tree.end_region(preview_region, preview_region_start);
    toggle_id
}

/// Precomputed mini-timeline draw inputs (section live: `mini_timeline_data` +
/// `ws.dock`'s bottom rect / `show_bottom`; harness: the same
/// `mini_timeline_data` free function over the fixture `Project` at beat 0).
/// Bundled into a struct, not individual params, to keep
/// `composite_editor_frame`'s signature from ballooning further — mirrors
/// `ui_frame.rs`'s `UiFrameSignals` grouping precedent.
pub(crate) struct EditorMiniTimelineInputs<'a> {
    pub bottom_rect: UiRect,
    pub show_bottom: bool,
    pub total_beats: f32,
    pub beats_per_bar: f32,
    pub current_beat: f32,
    pub row_count: usize,
    pub clips: &'a [MiniClip],
    pub layer_labels: &'a [MiniLayerLabel],
    pub readout: &'a str,
    pub is_playing: bool,
}

/// Composites the graph-editor window for one frame into `offscreen`: clear,
/// canvas immediate-mode draws, the merged sidebar/inspector `UITree`
/// (the default arena only: overlays mint into their own arena, D2 and
/// UI_INVALIDATION_DESIGN.md D7), dock dividers, mini-timeline, the shared tree-overlay pass
/// (`tree_passes::render_tree_overlay_passes` —
/// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` D1, P1), the mapping popover, and
/// `prepare`/`render`. Does NOT acquire or present a drawable, and does
/// NOT draw the node-output preview monitor blits (those are
/// drawable-targeted, macOS-only, live-input-fed — stay in
/// `present_graph_editor_window`, unchanged, on their own encoder after this
/// call returns, exactly as `composite_main_ui_frame` leaves the video-band
/// blit's sibling passes to its caller). The clear + commit happen
/// unconditionally, matching the live pre-extraction code exactly (the
/// clear ran even when `ui_renderer`/`canvas` were `None`, since a later
/// pass always blits `offscreen` onto the drawable) — `ui_renderer` is
/// `Option<&mut UIRenderer>`, not `&mut`, for the same reason
/// `ui_frame::apply_ui_frame_invalidations`'s `cache` is `Option`: the live
/// tuple guard was `if let (Some(ui), Some(canvas)) = (&mut
/// self.ui_renderer, self.graph_canvas.as_ref())` — both sides genuinely
/// optional, not just one. `popover` / `text_input` / `frame_timer` are
/// always-real, always-cheap values — see module doc deviation 3.
/// `ui_root` is `&mut` (was `&`, P3): the shared tree-overlay pass performs
/// the overlay-region dirty-clear on `ui_root.tree`, which needs `&mut`
/// (`tree_passes.rs` module doc deviation 1) — mirrors `ui_frame.rs`'s
/// `composite_main_ui_frame` taking `ui_root: &mut UIRoot` for the same
/// reason (module doc deviation 2).
/// Precedent: `present_graph_editor_window` (pre-extraction) :3694-3751
/// minus the drawable-tail's node-preview blits; caller order per
/// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` section 3 (popover moves AFTER the tree-
/// overlay pass — depth sorting, not enqueue order, governs stacking, so
/// this is not a visual change: POPOVER(300) still paints above
/// OVERLAY(200) and below TOOLTIP(400) regardless of enqueue order).
#[allow(clippy::too_many_arguments)]
pub(crate) fn composite_editor_frame(
    device: &GpuDevice,
    ui_renderer: Option<&mut UIRenderer>,
    ui_root: &mut UIRoot,
    dock: &Dock,
    editor_area: UiRect,
    canvas: Option<&GraphCanvas>,
    canvas_rect: CanvasRect,
    mini: EditorMiniTimelineInputs<'_>,
    popover: &mut MappingPopover,
    popover_live_value: Option<f32>,
    text_input: &TextInputState,
    frame_timer: &FrameTimer,
    offscreen: &GpuTexture,
    logical_w: u32,
    logical_h: u32,
    scale: f64,
    viewport: Option<crate::scene_viewport::SceneViewportPaint<'_>>,
) {
    let mut encoder = device.create_encoder("Graph Editor Frame");
    encoder.clear_texture(offscreen, 0.10, 0.10, 0.12, 1.0);

    if let (Some(ui_renderer), Some(canvas)) = (ui_renderer, canvas) {
        ui_renderer.begin_frame();
        canvas.render(ui_renderer, canvas_rect);
        // Layer the sidebar/inspector UITree on top of the canvas's
        // immediate-mode draws (the flush protocol covers them with their
        // own batches) — ONE tree, ONE call, matching the live paint order.
        // The editor builds its own panels into the default arena; open
        // overlays (the node browser) and the card-drag ghost live in their
        // own arenas, drawn region-aware at OVERLAY depth by the shared
        // tree-overlay pass below, never by this root scan (D2).
        ui_renderer.render_tree_range(&ui_root.tree, manifold_ui::tree::DEFAULT_REGION, 0, usize::MAX);
        if let Some(viewport) = viewport {
            viewport.draw(ui_renderer);
        }
        // Column dividers: a thin seam always, a highlight band on
        // hover/drag. Drawn after the panels so the seam reads on top of
        // both the canvas and the sidebar backgrounds.
        dock.draw(editor_area, ui_renderer);
        if mini.show_bottom {
            MiniTimeline::draw(
                mini.bottom_rect,
                mini.total_beats,
                mini.beats_per_bar,
                mini.current_beat,
                mini.row_count,
                mini.clips,
                mini.layer_labels,
                mini.readout,
                mini.is_playing,
                ui_renderer,
            );
        }
        // Shared tree-overlay pass (D1): overlay_draw loop + TOOLTIP tier +
        // browser-popup thumbnails + overlay-region dirty-clear — the SAME
        // function the main window calls. `text_input` is `Some` only when
        // the active session belongs to THIS window (D4).
        crate::tree_passes::render_tree_overlay_passes(
            device,
            ui_renderer,
            ui_root,
            logical_w,
            logical_h,
            crate::tree_passes::TreeOverlayInputs {
                text_input: text_input.is_owned_by_editor().then_some(text_input),
                frame_timer,
            },
        );
        // The mapping drawer floats over the composited canvas + sidebar:
        // it draws inline at POPOVER depth (above the CONTENT-depth nodes),
        // unclipped. D5(doc): immediate-mode content, exempt from
        // `overlay_draw` — depth constants (OVERLAY 200 < POPOVER 300 <
        // TOOLTIP 400) order it correctly regardless of enqueue order.
        if popover.is_open() {
            ui_renderer.push_depth(Depth::POPOVER);
            popover.set_live_value(popover_live_value);
            popover.render(ui_renderer);
            ui_renderer.pop_depth();
        }
        if ui_renderer.prepare(device, logical_w, logical_h, scale) {
            ui_renderer.render(&mut encoder, offscreen, GpuLoadAction::Load);
        }
    }

    encoder.commit();
}
