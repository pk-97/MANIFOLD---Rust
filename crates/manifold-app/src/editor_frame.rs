//! The graph editor window's frame seam (UI_INVALIDATION_DESIGN.md D17, P5c):
//! its open, its planner and the composite, shared by the live App
//! (`present_graph_editor_window`) and the ui-snap script runner.
//!
//! The window has three regions, each in its own arena of its `UIRoot` tree:
//! the preview column, the card lane (the main window's inspector column,
//! hosted again) and the overlays. [`plan_editor_frame`] compares this
//! frame's [`EditorInputs`] with the last applied ones and names the regions
//! to rebuild and the live-push groups to run; equal inputs do nothing.
//! [`run_editor_frame`] applies the plan. The two panel regions paint into
//! the window's own `UICacheManager` atlas, so the composite repaints only a
//! slot whose nodes changed. The canvas is immediate-mode: it draws when its
//! inputs move ([`feed_canvas`]), every frame while live node values or
//! previews show.
//!
//! What differs between the App and the runner is input presence only: the
//! App has an offscreen, a surface-derived size and a vsync pulse; the runner
//! has no window, so no offscreen, a fixed size and a frame per step.

use std::sync::Arc;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuTexture};
use manifold_ui::graph_canvas::mapping_popover::MappingPopover;
use manifold_ui::graph_canvas::{GraphCanvas, Rect as CanvasRect};
use manifold_ui::node::{NodeId, TextAlign, UIStyle};
use manifold_ui::panels::graph_editor::GraphEditorPanel;
use manifold_ui::panels::Panel;
use manifold_ui::{Dock, MiniClip, MiniLayerLabel, MiniTimeline, Rect as UiRect, UITree};
use manifold_ui_paint::ui_cache_manager::{PanelCacheInfo, PanelSlot, UICacheManager};
use manifold_ui_paint::ui_renderer::{Depth, UIRenderer};

use crate::frame_plan::{LiveInputs, MainRegion, Projection, ProjectionMask};
use crate::frame_timer::FrameTimer;
use crate::text_input::TextInputState;
use crate::ui_root::UIRoot;
use crate::workspace::{Workspace, WorkspaceKind};
use manifold_core::work_count::{record_work, WorkKind};

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
    crate::ui_frame::project_editor_inspector(
        &mut ws.ui_root,
        project,
        active_layer,
        selection,
        &content_state.automation_latched_params,
        content_state.led_preview.as_ref(),
    );
    (ws, GraphCanvas::new())
}

/// The editor window's regions (D17), each in its own arena. The two panel
/// regions take arena ids the main window does not use; the overlays share
/// the tree's overlay arena, where an event-time mint (a dropdown opening
/// from a card) lands in either window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorRegion {
    PreviewColumn,
    CardLane,
    Overlays,
}

const _: () = assert!(
    (EditorRegion::CardLane.id() as usize) < manifold_ui::tree::DEFAULT_REGION as usize
        && EditorRegion::PreviewColumn.id() as usize >= crate::frame_plan::MAIN_REGION_COUNT
);

impl EditorRegion {
    /// The region's arena id in the editor's tree.
    pub(crate) const fn id(self) -> u8 {
        match self {
            EditorRegion::PreviewColumn => 10,
            EditorRegion::CardLane => 11,
            EditorRegion::Overlays => MainRegion::Overlays.id(),
        }
    }

    /// The region's atlas slot in the editor's cache; overlays draw outside it.
    pub(crate) fn panel_slot(self) -> Option<PanelSlot> {
        match self {
            EditorRegion::PreviewColumn => Some(PanelSlot::EditorColumn),
            EditorRegion::CardLane => Some(PanelSlot::EditorCards),
            EditorRegion::Overlays => None,
        }
    }
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

    /// What the preview column's build reads from the layout, exactly.
    fn column_key(&self) -> [u32; 8] {
        [
            self.preview_width.to_bits(),
            self.canvas.height.to_bits(),
            self.preview_x.to_bits(),
            self.preview_w.to_bits(),
            self.preview_h.to_bits(),
            self.node_title_y.to_bits(),
            self.node_img_y.to_bits(),
            self.master_title_y.to_bits(),
        ]
    }
}

/// Everything the editor window's tree depends on, as versions and exact
/// geometry (D17). `Copy`, integers only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditorInputs {
    /// The live push's keys, folded from the main window's frame inputs.
    pub live: LiveInputs,
    /// Moves when the inspector projection rewrote this window's column.
    pub inspector_projected: u64,
    /// The editor column's own view state (drawers, collapse, tabs).
    pub inspector_view: u64,
    pub card_rect: [u32; 4],
    pub column: [u32; 8],
    /// What the node-output pane shows (`GraphEditorPanel::view_version`).
    pub panel: u64,
    pub show_image: bool,
    pub overlays: u64,
    pub screen: [u32; 2],
}

/// One editor frame's work, decided from inputs alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditorPlan {
    pub cards: bool,
    pub column: bool,
    pub overlays: bool,
    /// The live-push groups (`LiveValues`, `LiveShapes`, `LiveRanges`).
    pub live: ProjectionMask,
}

impl EditorPlan {
    /// The first frame, and `AssertFresh`'s reference frame: every region
    /// rebuilt and every live group pushed.
    fn everything() -> Self {
        let mut live = ProjectionMask::default();
        for p in [Projection::LiveValues, Projection::LiveShapes, Projection::LiveRanges] {
            live.insert(p);
        }
        Self { cards: true, column: true, overlays: true, live }
    }

    fn any_rebuild(&self) -> bool {
        self.cards || self.column || self.overlays
    }
}

/// Decide the editor frame's work from what moved (D6, D17). Pure; equal
/// inputs give an empty plan. The card lane rebuilds when the inspector
/// projection ran, its view changed or its rect moved; values, shapes and
/// range bands reach it through the live push on the main window's keys.
pub(crate) fn plan_editor_frame(last: &EditorInputs, next: &EditorInputs) -> EditorPlan {
    let mut plan = EditorPlan::default();
    if last == next {
        return plan;
    }
    plan.cards = last.inspector_projected != next.inspector_projected
        || last.inspector_view != next.inspector_view
        || last.card_rect != next.card_rect;
    plan.column = last.column != next.column || last.panel != next.panel || last.show_image != next.show_image;
    plan.overlays = last.overlays != next.overlays || last.screen != next.screen;
    crate::frame_plan::plan_live(&last.live, &next.live, &mut plan.live);
    plan
}

/// The editor planner's state and the window's atlas, owned by its
/// `Workspace`.
#[derive(Default)]
pub(crate) struct EditorFrameState {
    /// The inputs the last applied plan saw; `None` before the first frame.
    last: Option<EditorInputs>,
    /// The preview column's smart-preview toggle, as its last build drew it.
    smart_preview_toggle: Option<NodeId>,
    /// What the canvas was last fed.
    pub(crate) canvas: CanvasFeed,
    /// The window's atlas; created by the first composite with a device.
    pub(crate) cache: Option<UICacheManager>,
    /// What the last composite drew outside the tree and the canvas: the
    /// window size and the mini-timeline playhead.
    pub(crate) last_composite: Option<[u32; 3]>,
}

impl EditorFrameState {
    fn invalidate(&mut self, region: EditorRegion) {
        if let (Some(cm), Some(slot)) = (self.cache.as_mut(), region.panel_slot()) {
            cm.invalidate_slot(slot);
        }
    }
}

/// What one editor frame reads besides the workspace itself.
pub(crate) struct EditorFrameInputs<'a> {
    pub project: &'a manifold_core::project::Project,
    pub content_state: &'a crate::content_state::ContentState,
    pub active_layer: Option<usize>,
    /// The main window's live-push keys this frame (`FrameInputs::live`).
    pub live: LiveInputs,
    pub dt: f32,
    /// The node monitor shows an image this frame (the selected node has one,
    /// or the 3D viewport is displaying a frame).
    pub show_image: bool,
}

/// One editor frame's tree work: tick the column's tweens and fire meters,
/// plan, rebuild the planned regions alone, run the live push, and lay out
/// the canvas's pending focus and fit. `panel` is configured by the caller.
/// `fresh` forces the full reference frame (`AssertFresh`). Returns the
/// smart-preview toggle's id.
pub(crate) fn run_editor_frame(
    ui_root: &mut UIRoot,
    state: &mut EditorFrameState,
    panel: &GraphEditorPanel,
    canvas: Option<&mut GraphCanvas>,
    layout: &EditorLayout,
    frame: EditorFrameInputs<'_>,
    fresh: bool,
) -> Option<NodeId> {
    // Drawer tweens and the value flash move their nodes in place; a
    // structural tween moves the column's view version, read below.
    ui_root.tick_inspector();
    ui_root.update_fire_meters(&frame.content_state.fire_meters, frame.dt);

    let quantize = |r: UiRect| [r.x.to_bits(), r.y.to_bits(), r.width.to_bits(), r.height.to_bits()];
    let next = EditorInputs {
        live: frame.live,
        inspector_projected: ui_root.inspector_projection_version(),
        inspector_view: ui_root.inspector.view_version(),
        card_rect: quantize(layout.card_viewport),
        column: layout.column_key(),
        panel: panel.view_version(),
        show_image: frame.show_image,
        overlays: ui_root.overlays_version(),
        screen: [layout.logical_w, layout.logical_h],
    };
    let last = state.last.replace(next);
    // A moved or resized panel leaves pixels that belong to no slot; the
    // atlas clears and repaints whole, as the main window's does.
    if last.is_none_or(|l| l.card_rect != next.card_rect || l.column != next.column || l.screen != next.screen)
        && let Some(cm) = state.cache.as_mut()
    {
        cm.invalidate_all();
    }
    let plan = match last {
        Some(last) if !fresh => plan_editor_frame(&last, &next),
        _ => EditorPlan::everything(),
    };

    if plan.cards {
        record_work(WorkKind::EditorTreeBuild);
        ui_root.input.invalidate_hover();
        ui_root.tree.clear_region(EditorRegion::CardLane.id());
        ui_root.build_inspector_in(EditorRegion::CardLane.id(), layout.card_viewport);
        // A rebuilt column recreates its cards unselected and undimmed.
        ui_root.inspector.apply_selection_visuals(&mut ui_root.tree);
        state.invalidate(EditorRegion::CardLane);
    }
    if plan.column {
        record_work(WorkKind::EditorTreeBuild);
        ui_root.tree.clear_region(EditorRegion::PreviewColumn.id());
        state.smart_preview_toggle = build_editor_preview_column(
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
            frame.show_image,
        );
        state.invalidate(EditorRegion::PreviewColumn);
    }
    if plan.overlays {
        record_work(WorkKind::EditorTreeBuild);
        ui_root.tree.clear_region(EditorRegion::Overlays.id());
        ui_root.build_overlays_for_screen(layout.logical_w as f32, layout.logical_h as f32);
    }
    crate::ui_frame::settle_ghosts(ui_root);
    if plan.any_rebuild() {
        // A rebuild cleared its nodes' flags; restore hover and press from
        // the input system's durable widget state.
        ui_root.input.apply_interaction_flags(&mut ui_root.tree);
    }

    // The editor's live push (D22): the main window's keys, plus a rebuilt
    // card lane, which reads its first values from the last projection.
    let live = crate::ui_bridge::LiveGroups {
        values: plan.cards || plan.live.contains(Projection::LiveValues),
        shapes: plan.cards
            || plan.live.contains(Projection::LiveShapes)
            || plan.live.contains(Projection::LiveRanges),
    };
    if live.any() {
        crate::ui_bridge::push_live(ui_root, frame.project, frame.content_state, frame.active_layer, live);
    }
    ui_root.tick_editor_perf_hud();

    // An in-place scroll offset the cards without marking them all; repaint
    // the lane's slot. Cards that moved in place slid under a still cursor.
    let scrolled = ui_root.inspector.take_scrolled_in_place();
    if scrolled {
        state.invalidate(EditorRegion::CardLane);
    }
    if scrolled | ui_root.inspector.take_moved_in_place() {
        ui_root.input.refresh_hover(&mut ui_root.tree);
    }

    if let Some(canvas) = canvas {
        let vp = layout.canvas_rect();
        canvas.resolve_pending_focus(vp);
        canvas.apply_pending_fit(vp);
        // Wire-to-port magnetize needs port screen positions, so it ticks
        // here beside the draw rather than in the main canvas tick.
        canvas.tick_wire_magnet(vp);
    }
    state.smart_preview_toggle
}

/// What the canvas was last fed, kept so a frame feeds only what moved.
#[derive(Default)]
pub(crate) struct CanvasFeed {
    /// The snapshot last pushed, retained so a new one is a pointer compare.
    graph: Option<Arc<manifold_ui::graph_view::GraphSnapshot>>,
    /// The scope it was resolved at.
    scope: Vec<u32>,
    aspect_bits: Option<u32>,
    has_graph_mod: Option<bool>,
}

/// Feeds the watched graph to the canvas for one frame: the preview aspect
/// first, so a level's first layout takes the right node-face heights; the
/// canvas's own tweens (marquee fade, connect pop, error shake); the snapshot
/// when it or the canvas scope changed; this frame's live (modulated) node
/// values over it, so a driver or card slider is seen moving each knob; and
/// the reset pill. True when the canvas must draw again.
pub(crate) fn feed_canvas(
    canvas: &mut GraphCanvas,
    feed: &mut CanvasFeed,
    graph: &Arc<manifold_ui::graph_view::GraphSnapshot>,
    preview_aspect: Option<f32>,
    dt_ms: f32,
    live: &manifold_ui::graph_view::LiveNodeParams,
    has_graph_mod: bool,
) -> bool {
    let mut moved = false;
    if let Some(aspect) = preview_aspect
        && feed.aspect_bits != Some(aspect.to_bits())
    {
        feed.aspect_bits = Some(aspect.to_bits());
        canvas.set_preview_aspect(aspect);
        moved = true;
    }
    moved |= canvas.tick(dt_ms);
    let same_graph = feed.graph.as_ref().is_some_and(|g| Arc::ptr_eq(g, graph));
    if !same_graph || feed.scope.as_slice() != canvas.scope_path() {
        canvas.set_snapshot(graph);
        feed.graph = Some(Arc::clone(graph));
        feed.scope.clear();
        feed.scope.extend_from_slice(canvas.scope_path());
        moved = true;
    }
    // Live values arrive every content frame while a graph is watched and
    // feed the node sparklines, so they are a per-frame pass while present.
    // Empty when nothing is modulated or watched, and then free.
    if !live.is_empty() {
        canvas.apply_live_values(live);
        moved = true;
    }
    if feed.has_graph_mod != Some(has_graph_mod) {
        feed.has_graph_mod = Some(has_graph_mod);
        canvas.set_has_graph_mod(has_graph_mod);
        moved = true;
    }
    moved
}

/// Builds the editor sidebar's left preview column — backing panel,
/// node-output pane (image, or `panel`'s value-inspector text, or a "Select
/// a node" hint), master-out title — into its own arena, which the caller
/// has cleared. `panel` is configured by the caller
/// (`set_node_preview_normalize` / `set_node_inspector`). Returns the
/// smart-preview toggle's `NodeId` when drawn.
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

    let preview_region = tree.begin_region_in(
        EditorRegion::PreviewColumn.id(),
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

/// One entry per editor panel region with nodes, for its `UICacheManager`:
/// each region's whole arena after its root, at its screen rect. The card
/// lane carries the inspector's card sub-regions for incremental repaints.
pub(crate) fn editor_panel_cache_info(ui_root: &UIRoot, layout: &EditorLayout) -> Vec<PanelCacheInfo> {
    let mut info = Vec::with_capacity(2);
    let column = EditorRegion::PreviewColumn.id();
    let column_len = ui_root.tree.region_len(column);
    if column_len > 1 {
        info.push(PanelCacheInfo {
            slot: PanelSlot::EditorColumn,
            region: column,
            start: 1,
            end: column_len,
            rect: UiRect::new(0.0, 0.0, layout.preview_width, layout.canvas.height),
            sub_regions: None,
        });
    }
    let cards = EditorRegion::CardLane.id();
    let cards_len = ui_root.tree.region_len(cards);
    let first = ui_root.inspector.first_node();
    if first < cards_len {
        info.push(PanelCacheInfo {
            slot: PanelSlot::EditorCards,
            region: cards,
            start: first,
            end: cards_len,
            rect: layout.card_viewport,
            sub_regions: Some(ui_root.inspector.sub_region_ranges()),
        });
    }
    info
}

/// Whether any editor panel region holds a node the atlas has not painted.
pub(crate) fn editor_atlas_dirty(ui_root: &UIRoot) -> bool {
    [EditorRegion::PreviewColumn, EditorRegion::CardLane]
        .into_iter()
        .any(|r| ui_root.tree.has_dirty_in_region(r.id()))
}

/// Whether the editor's offscreen must composite this frame: `live` is the
/// caller's per-frame reasons (a dirty mark from input or the canvas feed,
/// node previews or the 3D viewport on screen, the mapping popover or an
/// editor text field open, playback moving the mini-timeline); otherwise an
/// unpainted panel node, a dirty overlay, a resize or a moved playhead. An
/// idle editor composites nothing and re-presents its last frame.
pub(crate) fn needs_composite(
    state: &mut EditorFrameState,
    ui_root: &UIRoot,
    live: bool,
    logical: [u32; 2],
    playhead: f32,
) -> bool {
    let key = [logical[0], logical[1], playhead.to_bits()];
    let moved = state.last_composite != Some(key);
    state.last_composite = Some(key);
    live || moved
        || editor_atlas_dirty(ui_root)
        || ui_root.tree.has_dirty_in_region(MainRegion::Overlays.id())
        || ui_root.tree.has_dirty_in_region(MainRegion::Ghosts.id())
}

/// Precomputed mini-timeline draw inputs (live: `mini_timeline_data` +
/// `ws.dock`'s bottom rect / `show_bottom`; harness: the same
/// `mini_timeline_data` free function over the fixture `Project` at beat 0).
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

/// The editor window's atlas and the pipeline that blits it.
pub(crate) struct EditorAtlas<'a> {
    pub cache: &'a mut UICacheManager,
    pub pipeline: &'a GpuRenderPipeline,
    pub sampler: &'a GpuSampler,
    pub scale_factor: f64,
}

/// Composites the graph-editor window for one frame into `offscreen`: the
/// dirty panel slots painted into the window's atlas first, then on one
/// encoder: clear, the canvas's immediate-mode draws, the atlas blit (the
/// preview column and card lane), the 3D viewport, dock dividers, the
/// mini-timeline, the shared tree-overlay pass (`tree_passes`, the same
/// function the main window calls), and the mapping popover. Does not
/// acquire or present a drawable, and does not draw the node-output monitor
/// blits, which target the drawable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn composite_editor_frame(
    device: &GpuDevice,
    ui_renderer: Option<&mut UIRenderer>,
    ui_root: &mut UIRoot,
    layout: &EditorLayout,
    dock: &Dock,
    canvas: Option<&GraphCanvas>,
    atlas: EditorAtlas<'_>,
    mini: EditorMiniTimelineInputs<'_>,
    popover: &mut MappingPopover,
    popover_live_value: Option<f32>,
    text_input: &TextInputState,
    frame_timer: &FrameTimer,
    offscreen: &GpuTexture,
    scale: f64,
    viewport: Option<crate::scene_viewport::SceneViewportPaint<'_>>,
) {
    let (logical_w, logical_h) = (layout.logical_w, layout.logical_h);
    let EditorAtlas { cache, pipeline: atlas_pipeline, sampler: atlas_sampler, scale_factor } = atlas;
    let Some(ui_renderer) = ui_renderer else {
        let mut encoder = device.create_encoder("Graph Editor Frame");
        encoder.clear_texture(offscreen, 0.10, 0.10, 0.12, 1.0);
        encoder.commit();
        return;
    };

    // The panel regions paint into the atlas only where their nodes changed.
    cache.set_scale_factor(scale_factor);
    cache.ensure_atlas(device, logical_w, logical_h);
    let panels = editor_panel_cache_info(ui_root, layout);
    let (_, painted) = cache.render_dirty_panels(device, ui_renderer, &ui_root.tree, &panels);
    crate::ui_frame::clear_painted_dirt(ui_root, &painted);

    let mut encoder = device.create_encoder("Graph Editor Frame");
    encoder.clear_texture(offscreen, 0.10, 0.10, 0.12, 1.0);
    if let Some(canvas) = canvas {
        ui_renderer.begin_frame();
        canvas.render(ui_renderer, layout.canvas_rect());
        if ui_renderer.prepare(device, logical_w, logical_h, scale) {
            ui_renderer.render(&mut encoder, offscreen, GpuLoadAction::Load);
        }
    }
    // The panels over the canvas: the atlas is transparent outside them.
    if let Some(atlas_tex) = cache.atlas_texture() {
        encoder.draw_fullscreen(
            atlas_pipeline,
            offscreen,
            &[
                manifold_gpu::GpuBinding::Texture { binding: 0, texture: atlas_tex },
                manifold_gpu::GpuBinding::Sampler { binding: 1, sampler: atlas_sampler },
            ],
            false,
            true,
            "Editor Atlas Blit",
        );
    }

    ui_renderer.begin_frame();
    if let Some(viewport) = viewport {
        viewport.draw(ui_renderer);
    }
    // Column dividers: a thin seam always, a highlight band on hover/drag,
    // over both the canvas and the sidebar backgrounds.
    dock.draw(layout.area, ui_renderer);
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
    // Shared tree-overlay pass: the overlay arena, the tooltip tier and the
    // overlay dirty-clear. `text_input` is `Some` only when the active session
    // belongs to this window.
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
    // The mapping drawer floats over everything at POPOVER depth (above the
    // overlay tier, below tooltips), unclipped.
    if popover.is_open() {
        ui_renderer.push_depth(Depth::POPOVER);
        popover.set_live_value(popover_live_value);
        popover.render(ui_renderer);
        ui_renderer.pop_depth();
    }
    if ui_renderer.prepare(device, logical_w, logical_h, scale) {
        ui_renderer.render(&mut encoder, offscreen, GpuLoadAction::Load);
    }
    encoder.commit();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> EditorInputs {
        EditorInputs { card_rect: [1, 2, 3, 4], screen: [800, 600], ..Default::default() }
    }

    #[test]
    fn equal_inputs_give_an_empty_plan() {
        assert_eq!(plan_editor_frame(&base(), &base()), EditorPlan::default());
    }

    #[test]
    fn values_push_in_place_and_never_rebuild_the_card_lane() {
        let mut next = base();
        next.live.values += 1;
        let plan = plan_editor_frame(&base(), &next);
        assert!(plan.live.contains(Projection::LiveValues));
        assert!(!plan.cards && !plan.column && !plan.overlays);
        let mut next = base();
        next.live.parts.bump(manifold_core::change::ChangeSet::of(&[manifold_core::change::Part::Shapes]));
        let plan = plan_editor_frame(&base(), &next);
        assert!(plan.live.contains(Projection::LiveShapes) && !plan.cards);
        let mut next = base();
        next.live.parts.bump(manifold_core::change::ChangeSet::of(&[manifold_core::change::Part::MappingRanges]));
        let plan = plan_editor_frame(&base(), &next);
        assert!(plan.live.contains(Projection::LiveRanges) && !plan.cards);
    }

    #[test]
    fn the_card_lane_rebuilds_on_the_projection_its_view_and_its_rect() {
        for change in [
            |i: &mut EditorInputs| i.inspector_projected += 1,
            |i: &mut EditorInputs| i.inspector_view += 1,
            |i: &mut EditorInputs| i.card_rect[2] += 1,
        ] {
            let mut next = base();
            change(&mut next);
            let plan = plan_editor_frame(&base(), &next);
            assert!(plan.cards && !plan.column && !plan.overlays, "{plan:?}");
        }
    }

    #[test]
    fn the_preview_column_rebuilds_on_its_panel_image_and_geometry() {
        for change in [
            |i: &mut EditorInputs| i.panel += 1,
            |i: &mut EditorInputs| i.show_image = true,
            |i: &mut EditorInputs| i.column[0] += 1,
        ] {
            let mut next = base();
            change(&mut next);
            let plan = plan_editor_frame(&base(), &next);
            assert!(plan.column && !plan.cards && !plan.overlays, "{plan:?}");
        }
    }

    #[test]
    fn overlays_rebuild_on_their_version_and_the_window_size() {
        for change in [|i: &mut EditorInputs| i.overlays += 1, |i: &mut EditorInputs| i.screen[0] += 1] {
            let mut next = base();
            change(&mut next);
            let plan = plan_editor_frame(&base(), &next);
            assert!(plan.overlays && !plan.cards && !plan.column, "{plan:?}");
        }
    }
}
