//! Windowless render of a built `UIRoot` to a PNG. Mirrors the proven headless
//! pattern in `manifold-renderer/tests/...` (`GpuDevice::new()` has no window).
//!
//! Pass 1 (the panel chrome — headers, ruler, lane backgrounds)
//! goes through the real cache: build a `UICacheManager`, `ensure_atlas`,
//! `invalidate_all` (this function is always a fresh device/cache/renderer per
//! call — no state persists across calls — so `invalidate_all`'s full-clear
//! path is the correct behavior, not a lesser substitute for the incremental
//! Load path, which no single-shot render can exercise anyway), then call
//! `crate::ui_frame::composite_main_ui_frame` — the SAME function
//! `present_all_windows` and `cache_path_full_render` call.
//!
//! P2 (`HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` §4 step 2): the immediate-mode
//! passes that used to continue here as this module's own `draw_immediate_
//! passes` (clip bodies, optional injected thumbnails, clip names,
//! automation lanes, top-level overlays) are now `crate::ui_frame::
//! render_main_ui_passes` — the SAME function `present_all_windows` calls,
//! not a parallel re-implementation. This closes BUG-097 by construction —
//! there is no longer a second copy that could pick
//! the wrong call. What's genuinely different here vs. the live app is INPUT, resolved
//! below and handed to the shared seam: clip bodies come from `selection`
//! (no live drag state exists headless), thumbnail atlas+quads come from
//! `thumbs::make_test_atlas`/`build_quads` (`--thumbs`-gated) instead of the
//! content-thread atlas, and `layer_bitmap_gpu`/`clip_content_gpu`/`vqt` are
//! `None` (no such renderers exist headless). See `docs/HEADLESS_UI_HARNESS.md`.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::clip_thumb_gpu::ClipThumbGpu;
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_cache_manager::UICacheManager;
use manifold_renderer::ui_renderer::UIRenderer;

use super::composite_resources::{composite_frame, CompositeResources};
use super::thumbs;
use crate::ui_root::UIRoot;

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;
/// The seam's offscreen/atlas format (`crate::ui_frame::composite_main_ui_frame`
/// and `CompositeResources` are both hard-wired to this, matching the live app).
const ATLAS_FORMAT: GpuTextureFormat = GpuTextureFormat::Bgra8Unorm;

/// Render the whole UI (`ui.tree` + clip bodies + optional injected thumbnails +
/// clip names) into a `tex_w`×`tex_h` texture and save as PNG. `tex_w` must be a
/// multiple of 64 so the readback stride (`tex_w * 4`) is 256-byte aligned.
/// `ui` is `&mut` (was `&UIRoot`): `composite_main_ui_frame`'s panel-cache pass
/// (Pass 1, below) clears the dirty ranges it just painted via
/// `ui_root.tree.clear_dirty_range` — see `ui_frame.rs`'s module doc deviation
/// #2. Harmless to callers: every caller already held `ui` behind a `mut`
/// binding (it was built via `sync_build(&mut ui, ...)` moments earlier).
pub fn render_ui_to_png(
    ui: &mut UIRoot,
    selection: &manifold_ui::UIState,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    tex_w: u32,
    tex_h: u32,
    scale: f32,
    with_thumbs: bool,
    path: &str,
) {
    assert_eq!(tex_w % 64, 0, "tex_w must be a multiple of 64 for aligned readback");

    let device = GpuDevice::new();
    let mut renderer = UIRenderer::new(&device, ATLAS_FORMAT);
    let dpi = f64::from(scale);

    // Pass 1 (P2, D1/D3): the real cache path, not a full-repaint lookalike —
    // see the module doc above.
    let mut cache = UICacheManager::new(ATLAS_FORMAT, dpi);
    cache.set_scale_factor(dpi);
    cache.ensure_atlas(&device, tex_w, tex_h);
    cache.invalidate_all();
    let res = CompositeResources::new(&device, tex_w, tex_h);
    composite_frame(&device, &mut renderer, &mut cache, ui, &res, dpi);
    let target_tex = &res.offscreen;

    // Passes 4a→5 + VQT + overlay dirty-clear: the SAME
    // `crate::ui_frame::render_main_ui_passes` `present_all_windows` calls —
    // see the module doc above (closes BUG-097 by construction). Resolve the
    // simple headless inputs this caller can (clip rects/bodies from
    // `selection`, thumbnail atlas+quads from `thumbs`, automation lanes),
    // `None`/empty for everything with no headless equivalent
    // (`layer_bitmap_gpu`, `clip_content_gpu`, `vqt` — no such renderers
    // exist without a content thread or live drag state).
    let mut clip_rects = Vec::new();
    ui.viewport.visible_clip_rects(&mut clip_rects);
    let hovered_clip = ui.viewport.hovered_clip_id();
    let clip_bodies: Vec<manifold_renderer::clip_draw::ClipBody> = clip_rects
        .iter()
        .map(|cr| manifold_renderer::clip_draw::ClipBody {
            rect: cr.rect,
            base_color: cr.base_color,
            selected: selection.is_selected(&cr.clip_id),
            hovered: hovered_clip == Some(cr.clip_id.as_str()),
            muted: cr.is_muted,
            locked: cr.is_locked,
            generator: cr.is_generator,
            alpha: 1.0,
        })
        .collect();

    // Thumbnail atlas + quads (`--thumbs` only) — a labeled test fixture
    // standing in for the content-thread atlas (§3's caller test: the render
    // code (`ClipThumbGpu::render`, inside the seam) is the real, shared
    // code; only the input is synthetic). `atlas`/`thumb_gpu` must outlive
    // the seam call, so they're bound here even when unused (`with_thumbs ==
    // false` or no non-audio clips) — cheap, and keeps the `Option` borrow
    // shape simple.
    let quads = if with_thumbs { thumbs::build_quads(&clip_rects) } else { Vec::new() };
    let atlas = if with_thumbs { Some(thumbs::make_test_atlas(&device)) } else { None };
    let mut thumb_gpu = if with_thumbs { Some(ClipThumbGpu::new(&device, ATLAS_FORMAT)) } else { None };
    let thumb = match (thumb_gpu.as_mut(), atlas.as_ref()) {
        (Some(gpu), Some(atlas)) if !quads.is_empty() => {
            Some(crate::ui_frame::ThumbPass { gpu, atlas, quads: &quads })
        }
        _ => None,
    };

    let automation_lanes = ui.viewport.automation_lane_screens(automation_latched);
    let text_input = crate::text_input::TextInputState::new();
    let frame_timer = crate::frame_timer::FrameTimer::new(60.0);
    let blit_pipeline = &res.blit_pipeline;
    let blit_sampler = &res.blit_sampler;

    crate::ui_frame::render_main_ui_passes(
        &device,
        &mut renderer,
        ui,
        target_tex,
        tex_w,
        tex_h,
        dpi,
        crate::ui_frame::MainUiPassInputs {
            layer_bitmap_gpu: None,
            clip_bodies: &clip_bodies,
            clip_rects: &clip_rects,
            clip_content_gpu: None,
            thumb,
            timeline_overlays: manifold_ui::panels::viewport::TimelineOverlays::default(),
            markers: &[],
            landing_flash: None,
            automation_lanes: &automation_lanes,
            cursor_pos: manifold_ui::node::Vec2::ZERO,
            text_input: &text_input,
            frame_timer: &frame_timer,
            vqt: None,
            blit_pipeline,
            blit_sampler,
            gpu_sink: None,
        },
    );

    // `target_tex` is BGRA (the seam's atlas/offscreen format, matching the
    // live app) — swap B/R per pixel for the RGBA PNG (display-only swizzle,
    // applied only at save time, same pattern as `cache_path_full_render`'s
    // `save_bgra_png`).
    let mut bytes = readback(&device, target_tex, tex_w, tex_h);
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    image::save_buffer(path, &bytes, tex_w, tex_h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
}

/// Render a graph-editor canvas — nodes, ports, wires, and **real per-node
/// output thumbnails** — into a `tex_w`×`tex_h` texture and save as PNG. Mirrors
/// the canvas half of `Application::present_graph_editor_window`: clear to the
/// editor backdrop, the canvas paints immediate-mode through `Painter` (no
/// UITree), then each node's actual output texture is blitted over its
/// placeholder. The graph is rendered headless from `def` (the same machinery
/// the parity harness uses) with the executor's per-node dump on, so the
/// thumbnails are the live images — no content thread needed. The editor's card
/// lane / sidebar / preview monitors are intentionally omitted. `tex_w` must be
/// a multiple of 64 (aligned readback).
pub fn render_graph_to_png(
    snapshot: &manifold_ui::graph_view::GraphSnapshot,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    tex_w: u32,
    tex_h: u32,
    scale: f32,
    path: &str,
) {
    use manifold_ui::draw::Painter;
    use manifold_ui::graph_canvas::{GraphCanvas, Rect as CanvasRect};

    assert_eq!(tex_w % 64, 0, "tex_w must be a multiple of 64 for aligned readback");

    // BUG-152: `Arc<GpuDevice>` — see the `render_graph_editor_to_png` call
    // site's comment for why.
    let device = std::sync::Arc::new(GpuDevice::new());
    let mut renderer = UIRenderer::new(&device, FORMAT);
    let target = RenderTarget::new(&device, tex_w, tex_h, FORMAT, "ui-snap-graph");
    let dpi = f64::from(scale);

    // Lay the snapshot out (topological auto-layout) and frame the whole level —
    // a fresh canvas starts with `fit_pending = true`, so apply_pending_fit
    // zoom-to-fits once the nodes have positions. The canvas takes its own Rect
    // type (distinct from the UITree `manifold_ui::Rect`).
    let viewport = CanvasRect::new(0.0, 0.0, tex_w as f32 / scale, tex_h as f32 / scale);
    let mut canvas = GraphCanvas::new();
    // Show the on-node param rows (the Blender-style layout) in the PNG — a live
    // canvas starts nodes collapsed for legibility, but the snapshot is a
    // verification surface, so expand them.
    canvas.set_default_expanded(true);
    canvas.set_snapshot(snapshot);
    canvas.apply_pending_fit(viewport);

    // Render the graph headless with the per-node dump on, so each node's output
    // texture is available for the thumbnail blit below. Best-effort: a def that
    // fails to build/compile leaves `None`, and the render falls through to the
    // structure-only canvas (black placeholders) instead of failing.
    let node_textures = render_graph_node_textures(&device, def);

    // Register each visible node's real output texture and hand the canvas its
    // preview source, so the canvas paints previews INLINE at each node's depth
    // band (BUG-027) — the same inline path the live editor uses. Replaces the
    // old flat post-chrome blit that ignored node z-order.
    if let Some(nt) = node_textures.as_ref() {
        let mut src: ahash::AHashMap<
            manifold_core::NodeId,
            (manifold_ui::node::TextureHandle, [f32; 4]),
        > = ahash::AHashMap::new();
        for (node_id, _, _, _, _) in canvas.visible_node_thumbnails(viewport) {
            let Some(tex) = nt.texture_for(node_id.as_str()) else {
                continue;
            };
            // A render-target-only output can't be sampled — skip it (the live
            // atlas path guards the same way).
            if !tex.is_shader_readable() {
                continue;
            }
            let handle = manifold_ui::node::texture_handle_for_key(node_id.as_str());
            renderer.register_external_texture(handle, tex.clone());
            src.insert(node_id, (handle, [0.0, 0.0, 1.0, 1.0]));
        }
        canvas.set_node_preview_src(src);
    }

    // Clear to the editor backdrop (the live editor clears the offscreen to this
    // before the canvas paints with Load).
    {
        let mut enc = device.create_encoder("ui-snap-graph-clear");
        enc.clear_texture(&target.texture, 0.10, 0.10, 0.12, 1.0);
        enc.commit_and_wait_completed();
    }

    // Canvas: nodes, ports, wires, and the black thumbnail placeholders.
    renderer.begin_frame();
    canvas.render(&mut renderer as &mut dyn Painter, viewport);
    let drew = renderer.prepare(&device, tex_w, tex_h, dpi);
    {
        let mut enc = device.create_encoder("ui-snap-graph");
        renderer.render(&mut enc, &target.texture, GpuLoadAction::Load);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "graph canvas produced no draws (empty snapshot?)");

    // Node previews were painted INLINE by `canvas.render` above (each at its
    // node's depth band, via `set_node_preview_src`), so the old flat
    // post-chrome blit — which drew every thumbnail over the finished chrome and
    // ignored node z-order (BUG-027) — is gone.

    let bytes = readback(&device, &target.texture, tex_w, tex_h);
    image::save_buffer(path, &bytes, tex_w, tex_h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
}

/// Render the FULL graph-editor WINDOW for a generator preset: the left
/// preview sidebar's CHROME (backing panel + monitor titles + empty-state
/// hint), the center canvas (nodes/wires/thumbnails, as [`render_graph_to_png`]),
/// and the right card lane (the real `ParamCardPanel` + inner-node param list,
/// same widgets the live editor's card lane drives — docks right, same side as
/// the main timeline's inspector). The live preview-monitor IMAGES are fed by
/// content-thread commands (`SetGraphPreviewNode`/`SetNodeAtlasVisible`) and
/// can't render headless — left as the same "Select a node" hint the live
/// editor shows before a click, not faked. No node is pre-selected, so the
/// inner-node list shows its own empty state — the editor's state on open.
///
/// This builds ONE `UIRoot` (sidebar + inspector
/// merged, same as `present_graph_editor_window`'s `ws.ui_root`) and paints
/// it through `crate::editor_frame::composite_editor_frame` — the identical
/// function the live window calls. See `editor_frame.rs`'s module doc for
/// the full argument.
#[allow(clippy::too_many_arguments)]
pub fn render_graph_editor_to_png(
    project: &manifold_core::project::Project,
    target: &manifold_core::GraphTarget,
    selection: &manifold_ui::UIState,
    snapshot: &manifold_ui::graph_view::GraphSnapshot,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    tex_w: u32,
    tex_h: u32,
    scale: f32,
    path: &str,
    // UI_AUTOMATION_DESIGN.md P1: when `Some`, write the extended tree dump
    // (`super::dump::dump_tree_ex`) here — the real card-lane/inspector tree
    // (`ui_root.tree`, widget/name-bearing) plus the graph canvas'
    // `HitTargets` enumeration as a `custom_surfaces` entry. `None` skips the
    // dump entirely (unchanged PNG-only behavior).
    dump_path: Option<&std::path::Path>,
    // D6 verification (`docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2):
    // force these node ids collapsed regardless of the harness's usual
    // "expand everything" default, so a group-face "N params" chip can be
    // captured next to (or instead of) its expanded rows. Empty for every
    // caller that doesn't care (byte-identical to the pre-D6 behavior).
    force_collapsed: &[u32],
    // `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1 acceptance demo: open this
    // window's own `browser_popup` (Node mode, the same widget
    // `GraphEditCommand::OpenNodePicker` opens live) BEFORE
    // `build_overlays_for_screen` runs below, so the overlay driver records
    // its region and the shared tree-overlay pass has something to draw —
    // proves BUG-151 is fixed on the real headless path, not just by
    // inspection. `false` for every existing caller (byte-identical to the
    // pre-P1 PNGs).
    open_node_picker: bool,
    // `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P2 acceptance demo: toggle this
    // window's own `perf_hud` visible (placeholder metrics — no `ContentState`
    // exists on this headless path) BEFORE `build_overlays_for_screen` runs
    // below, same driver as `open_node_picker` — proves row 6's "whatever
    // `overlay_draw` holds" claim on a SECOND overlay type. `false` for every
    // existing caller (byte-identical to the pre-P2 PNGs).
    open_perf_hud: bool,
) {
    use manifold_ui::graph_canvas::GraphCanvas;
    use manifold_ui::panels::graph_editor::{EDITOR_CARD_LANE_WIDTH, GraphEditorPanel, SIDEBAR_WIDTH};
    use manifold_ui::Rect as UiRect;

    assert_eq!(tex_w % 64, 0, "tex_w must be a multiple of 64 for aligned readback");
    let logical_w = tex_w as f32 / scale;
    let logical_h = tex_h as f32 / scale;
    // Column + bottom-strip geometry from the same `Dock` the live editor uses,
    // so the snapshot's canvas height / strip band match the runtime.
    let dock = manifold_ui::Dock::editor();
    let dock_rects = dock.rects(UiRect::new(0.0, 0.0, logical_w, logical_h));
    let canvas_x = SIDEBAR_WIDTH;
    let canvas_width = (logical_w - SIDEBAR_WIDTH - EDITOR_CARD_LANE_WIDTH).max(0.0);
    let canvas_height = dock_rects.canvas.height;
    let card_x = canvas_x + canvas_width;

    // BUG-152: `Arc<GpuDevice>`, not a bare `GpuDevice` — `render_graph_node_
    // textures` below needs an owned `Arc` to hand `MetalBackend::new`
    // (BUG-054's constructor signature). Every other use of `device` in this
    // function keeps working unchanged via `&Arc<GpuDevice>`'s `Deref`
    // coercion to `&GpuDevice`.
    let device = std::sync::Arc::new(GpuDevice::new());
    let mut renderer = UIRenderer::new(&device, FORMAT);
    let target_tex = RenderTarget::new(&device, tex_w, tex_h, FORMAT, "ui-snap-editor");
    let dpi = f64::from(scale);

    // ONE `UIRoot` for the whole editor window — sidebar preview column AND
    // the right inspector lane merged into `ui_root.tree`, exactly as the
    // live `present_graph_editor_window` builds `ws.ui_root.tree`. This is
    // the topology fix: no more scratch `tree` + throwaway `editor_ui.tree`.
    let mut ui_root = crate::ui_root::UIRoot::new();
    let active_idx = match target {
        manifold_core::GraphTarget::Generator(lid) => {
            project.timeline.layers.iter().position(|l| &l.layer_id == lid)
        }
        manifold_core::GraphTarget::Effect(_) => None,
    };
    crate::ui_bridge::sync_project_data(&mut ui_root, project, active_idx, selection);
    // No `ContentState` exists on this path (a bare fixture `Project`, no
    // playback engine) — the graph-editor lane render never has latch data,
    // so it can only ever show the red "automated" dot, never the gray
    // overridden state. Honest empty slice, not a stopgap.
    crate::ui_bridge::sync_inspector_data(&mut ui_root, project, active_idx, selection, &[]);
    ui_root.build_inspector_in_rect(UiRect::new(
        card_x,
        0.0,
        EDITOR_CARD_LANE_WIDTH,
        canvas_height,
    ));

    // Left sidebar preview column: backing panel + the two monitor titles +
    // an empty-state hint, laid out with the same math
    // `present_graph_editor_window` uses (16:9 `monitor_aspect` default — no
    // content pipeline headless to read the real project aspect from). A
    // fresh, unconfigured `GraphEditorPanel` reproduces the live window's
    // own pre-selection empty state ("Node Output" / "Select a node")
    // through the SAME `render_node_inspector` call live uses — see
    // `editor_frame.rs` module doc deviation 1.
    let preview_pad = 8.0_f32;
    let preview_title_h = 18.0_f32;
    let monitor_aspect = 16.0_f32 / 9.0;
    let avail_w = (SIDEBAR_WIDTH - 2.0 * preview_pad).max(1.0);
    let max_body_h = ((canvas_height - 3.0 * preview_pad - 2.0 * preview_title_h) * 0.5).max(1.0);
    let width_bound_h = avail_w / monitor_aspect;
    let (preview_w, preview_h) = if width_bound_h <= max_body_h {
        (avail_w, width_bound_h)
    } else {
        (max_body_h * monitor_aspect, max_body_h)
    };
    let preview_x = (SIDEBAR_WIDTH - preview_w) * 0.5;
    let pane_block_h = 2.0 * (preview_title_h + preview_h) + preview_pad;
    let mut pane_y = ((canvas_height - pane_block_h) * 0.5).max(preview_pad);
    let node_title_y = pane_y;
    let node_img_y = node_title_y + preview_title_h;
    pane_y = node_img_y + preview_h + preview_pad;
    let master_title_y = pane_y;

    let editor_panel = GraphEditorPanel::new();
    crate::editor_frame::build_editor_preview_column(
        &mut ui_root.tree,
        &editor_panel,
        SIDEBAR_WIDTH,
        canvas_height,
        preview_x,
        preview_w,
        preview_h,
        node_title_y,
        node_img_y,
        master_title_y,
        /* show_image */ false,
    );

    // Center canvas, offset into its lane between the two side columns — the
    // same per-node-dump machinery as `render_graph_to_png`.
    let viewport = manifold_ui::graph_canvas::Rect::new(canvas_x, 0.0, canvas_width, canvas_height);
    let mut canvas = GraphCanvas::new();
    // Show the on-node param rows (the Blender-style layout) in the PNG — a live
    // canvas starts nodes collapsed for legibility, but the snapshot is a
    // verification surface, so expand them.
    canvas.set_default_expanded(true);
    canvas.set_snapshot(snapshot);
    for &id in force_collapsed {
        canvas.set_collapsed(id, true);
    }
    canvas.apply_pending_fit(viewport);
    let node_textures = render_graph_node_textures(&device, def);

    if let Some(dump_path) = dump_path {
        use manifold_ui::graph_canvas::GraphCanvasTargets;
        let canvas_targets = GraphCanvasTargets { canvas: &canvas, viewport };
        let surfaces: [&dyn manifold_ui::hit_targets::HitTargets; 1] = [&canvas_targets];
        let json = super::dump::dump_tree_ex(&ui_root.tree, &surfaces);
        std::fs::write(dump_path, serde_json::to_string_pretty(&json).expect("serialize dump"))
            .expect("write editor tree json");
        println!("ui-snap: wrote {}", dump_path.display());
    }

    // Register each visible node's output texture + preview source so the canvas
    // paints previews INLINE at each node's depth band (BUG-027), same as the
    // live editor — replacing the old flat post-chrome blit that ignored z-order.
    if let Some(nt) = node_textures.as_ref() {
        let mut src: ahash::AHashMap<
            manifold_core::NodeId,
            (manifold_ui::node::TextureHandle, [f32; 4]),
        > = ahash::AHashMap::new();
        for (node_id, _, _, _, _) in canvas.visible_node_thumbnails(viewport) {
            let Some(tex) = nt.texture_for(node_id.as_str()) else {
                continue;
            };
            if !tex.is_shader_readable() {
                continue;
            }
            let handle = manifold_ui::node::texture_handle_for_key(node_id.as_str());
            renderer.register_external_texture(handle, tex.clone());
            src.insert(node_id, (handle, [0.0, 0.0, 1.0, 1.0]));
        }
        canvas.set_node_preview_src(src);
    }

    // `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1: open the node picker (if
    // asked) and record it into the tree via the SAME driver
    // `present_graph_editor_window` calls each frame — `build_overlays_for_
    // screen`, not a parallel `begin_region`/`.build()` reimplementation.
    // Minimal `PickerItem` list from `palette_atoms()` — enough to populate
    // the grid; live app additionally threads descriptor aliases into
    // `search_text`, irrelevant to this static PNG.
    if open_node_picker {
        use manifold_ui::panels::browser_popup::{BrowserPopupMode, BrowserPopupRequest};
        use manifold_ui::panels::picker_core::PickerItem;
        let items: Vec<PickerItem> = manifold_renderer::node_graph::palette_atoms()
            .into_iter()
            .map(|a| PickerItem {
                label: a.label,
                type_id: a.type_id,
                category: None,
                search_text: None,
                badge: None,
                source: None,
                missing_from_library: false,
                thumbnail: None,
            })
            .collect();
        ui_root.browser_popup.set_screen_size(logical_w, logical_h);
        ui_root.browser_popup.open(BrowserPopupRequest {
            mode: BrowserPopupMode::Node,
            tab: manifold_ui::panels::InspectorTab::Master,
            layer_id: None,
            items,
            category_names: Vec::new(),
            spawn_graph_pos: None,
            paste_count: 0,
            screen_anchor: manifold_ui::Vec2::new(logical_w * 0.5, logical_h * 0.5),
        });
    }
    if open_perf_hud {
        ui_root.perf_hud.toggle();
        ui_root.perf_hud.set_metrics(manifold_ui::panels::perf_hud::PerfMetrics {
            ui_fps: 60.0,
            ui_frame_time_ms: 16.6,
            render_fps: 60.0,
            render_frame_time_ms: 16.6,
            gpu_fence_wait_ms: 0.0,
            render_target_fps: 60.0,
            active_clips: 0,
            preparing_clips: 0,
            current_beat: manifold_core::Beats::ZERO,
            current_time_secs: 0.0,
            bpm: manifold_core::Bpm(120.0),
            clock_source: "Internal".to_string(),
            is_playing: false,
            data_version: 0,
            profiling_active: false,
            profiling_frame_count: 0,
        });
    }
    ui_root.build_overlays_for_screen(logical_w, logical_h);
    if open_perf_hud {
        // `build_overlays_for_screen` just minted the value-label node ids
        // `push_values` writes into (`build_at_xy`'s rows start at "—" until
        // a values pass fills them in — the same two-step the live main
        // window drives via `Panel::update`/`UIRoot::update()`, which this
        // headless path never calls, so it's done explicitly here).
        ui_root.perf_hud.push_values(&mut ui_root.tree);
    }

    // Same paint order as `present_graph_editor_window` because it's the
    // SAME function: clear + canvas immediate-mode draws + the merged
    // sidebar/inspector `UITree` (ONE tree-range render call, narrowed to
    // `[0, overlay_region_start)`, D2) + dock + mini-timeline + the shared
    // tree-overlay pass (open overlays, if any) + popover/text-input +
    // prepare/render.
    let editor_area = UiRect::new(0.0, 0.0, logical_w, logical_h);
    let (mini_clips, mini_layer_labels, mini_rows, mini_total, mini_bpb, mini_readout) =
        crate::app_render::mini_timeline_data(project, 0.0);
    // Closed/inactive by construction — the guarded branches inside
    // `composite_editor_frame` never fire (see that module's doc deviation 3).
    let mut popover = manifold_ui::graph_canvas::mapping_popover::MappingPopover::new();
    let text_input = crate::text_input::TextInputState::new();
    let frame_timer = crate::frame_timer::FrameTimer::new(60.0);
    crate::editor_frame::composite_editor_frame(
        &device,
        Some(&mut renderer),
        &mut ui_root,
        &dock,
        editor_area,
        Some(&canvas),
        viewport,
        crate::editor_frame::EditorMiniTimelineInputs {
            bottom_rect: dock_rects.bottom,
            show_bottom: dock.show_bottom,
            total_beats: mini_total,
            beats_per_bar: mini_bpb,
            current_beat: 0.0,
            row_count: mini_rows,
            clips: &mini_clips,
            layer_labels: &mini_layer_labels,
            readout: &mini_readout,
            is_playing: false,
        },
        &mut popover,
        None,
        &text_input,
        &frame_timer,
        &target_tex.texture,
        tex_w,
        tex_h,
        dpi,
    );

    // Node previews were painted INLINE by `canvas.render` above (each at its
    // node's depth band, via `set_node_preview_src`) — the old flat post-chrome
    // blit that ignored node z-order (BUG-027) is gone.

    let bytes = readback(&device, &target_tex.texture, tex_w, tex_h);
    image::save_buffer(path, &bytes, tex_w, tex_h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
}

/// Visual proof for the UI transform-stack capability
/// (`docs/UI_TRANSFORM_STACK_DESIGN.md`) — a bespoke `UITree` with no
/// `Project`/fixture behind it, built directly in this function since none of
/// the existing scenes exercise a per-node `UIStyle::transform`. Three panels:
/// (a) a rotated rounded rect keeps crisp AA'd corners (the SDF runs in local
/// uv-space, so rotation is "free" — no shader change), (b) rotated text
/// glyphs rotate with their node, (c) a rect scaled about its own center
/// bulges symmetrically past an unscaled reference outline (proving the pivot
/// is the node's rect center, not a corner). `UIStyle::transform` is
/// node-local (§3 of the design doc) — the renderer pivots it about each
/// node's own bounds at draw time.
pub fn render_transform_proof_to_png(path: &str) {
    use manifold_ui::node::{Color32, TextAlign, UIFlags, UINodeType, UIStyle};
    use manifold_ui::transform2d::Affine2;
    use manifold_ui::{Rect as UiRect, UITree};

    const TEX_W: u32 = 1536;
    const TEX_H: u32 = 768;
    assert_eq!(TEX_W % 64, 0, "tex_w must be a multiple of 64 for aligned readback");

    let device = GpuDevice::new();
    let mut renderer = UIRenderer::new(&device, FORMAT);
    let target = RenderTarget::new(&device, TEX_W, TEX_H, FORMAT, "ui-snap-transform");

    let mut tree = UITree::new();
    // `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D4: a standalone scratch tree
    // local to this function, same reasoning as the sidebar wrap above —
    // still a real `UITree`, still subject to `mint`'s debug assertion.
    let proof_region = tree.begin_region(
        manifold_ui::Rect::new(0.0, 0.0, TEX_W as f32, TEX_H as f32),
        manifold_ui::ZTier::Base,
        "transform_proof",
        UIFlags::empty(),
    );
    let proof_start = tree.count();

    // Dark backdrop for contrast.
    tree.add_panel(
        None,
        0.0,
        0.0,
        TEX_W as f32,
        TEX_H as f32,
        UIStyle { bg_color: Color32::new(18, 20, 26, 255), ..UIStyle::default() },
    );

    let caption_style = UIStyle {
        text_color: Color32::new(180, 186, 196, 255),
        font_size: 16,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    };

    // (a) Rotated rounded rect — crisp AA'd corners under rotation.
    tree.add_label(None, 140.0, 140.0, 220.0, 24.0, "(a) rotate: rounded rect", caption_style);
    tree.add_panel(
        None,
        140.0,
        220.0,
        220.0,
        150.0,
        UIStyle {
            bg_color: Color32::new(64, 200, 220, 255),
            corner_radius: 28.0,
            transform: Some(Affine2::rotate(20.0_f32.to_radians())),
            ..UIStyle::default()
        },
    );

    // (b) Rotated text — the glyph quads rotate with their node; the bordered
    // panel makes the node's rotated bounding box legible alongside the text.
    tree.add_label(None, 500.0, 140.0, 300.0, 24.0, "(b) rotate: text glyphs", caption_style);
    tree.add_node(
        None,
        UiRect::new(500.0, 220.0, 300.0, 150.0),
        UINodeType::Panel,
        UIStyle {
            bg_color: Color32::new(40, 44, 54, 255),
            border_color: Color32::new(120, 128, 140, 255),
            border_width: 2.0,
            text_color: Color32::new(255, 214, 90, 255),
            font_size: 48,
            text_align: TextAlign::Center,
            transform: Some(Affine2::rotate(-15.0_f32.to_radians())),
            ..UIStyle::default()
        },
        Some("AaBb"),
        UIFlags::empty(),
    );

    // (c) Scaled about center — the filled panel is scaled 1.5x and drawn
    // first; the unscaled reference outline (same bounds, no transform) drawn
    // on top shows the scale bulging equally on all four sides, not anchored
    // to a corner.
    tree.add_label(None, 900.0, 140.0, 260.0, 24.0, "(c) scale about center", caption_style);
    let rect_c = UiRect::new(900.0, 220.0, 220.0, 150.0);
    tree.add_node(
        None,
        rect_c,
        UINodeType::Panel,
        UIStyle {
            bg_color: Color32::new(230, 130, 60, 220),
            corner_radius: 20.0,
            transform: Some(Affine2::scale(1.5, 1.5)),
            ..UIStyle::default()
        },
        None,
        UIFlags::empty(),
    );
    tree.add_node(
        None,
        rect_c,
        UINodeType::Panel,
        UIStyle {
            bg_color: Color32::TRANSPARENT,
            border_color: Color32::new(255, 255, 255, 255),
            border_width: 2.0,
            corner_radius: 20.0,
            ..UIStyle::default()
        },
        None,
        UIFlags::empty(),
    );
    tree.end_region(proof_region, proof_start);

    renderer.begin_frame();
    renderer.render_tree(&tree, None);
    let drew = renderer.prepare(&device, TEX_W, TEX_H, 1.0);
    {
        let mut enc = device.create_encoder("ui-snap-transform");
        renderer.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "transform proof produced no draws");

    let bytes = readback(&device, &target.texture, TEX_W, TEX_H);
    image::save_buffer(path, &bytes, TEX_W, TEX_H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
}

/// A headless one-frame render of `def`'s graph with the executor's per-node
/// dump enabled, holding the graph + executor alive so the dumped node textures
/// stay valid. `texture_for` resolves a stable NodeId to its output texture.
struct GraphNodeTextures {
    graph: manifold_renderer::node_graph::Graph,
    exec: manifold_renderer::node_graph::Executor,
}

impl GraphNodeTextures {
    /// The first dumped Texture2D output of the node with stable id `node_id`,
    /// or `None` if the node didn't run / produced no texture.
    fn texture_for(&self, node_id: &str) -> Option<&GpuTexture> {
        let runtime = self
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new(node_id))?;
        self.exec
            .dump_resources()
            .iter()
            .find(|(niid, _, _, tex)| *niid == runtime && tex.is_some())
            .and_then(|(_, _, _, tex)| tex.as_ref())
    }
}

/// Build `def`'s graph, run one frame with `set_dump_all`, and keep the result
/// so per-node output textures can be read back. `None` if the def can't build
/// or compile (caller falls through to a structure-only render). Effects whose
/// graph begins at a `Source` node get a neutral mid-grey fixture on that input;
/// generators self-produce and need none.
fn render_graph_node_textures(
    // BUG-152: `&Arc<GpuDevice>`, not `&GpuDevice` — `MetalBackend::new`
    // (BUG-054) takes an owned `Arc<GpuDevice>`; this is the only call in
    // this module that needs to clone one out.
    device: &std::sync::Arc<GpuDevice>,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
) -> Option<GraphNodeTextures> {
    use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_renderer::node_graph::{
        compile, EffectGraphDefExt, Executor, FrameTime, MetalBackend, PrimitiveRegistry,
        GENERATOR_INPUT_TYPE_ID, SOURCE_TYPE_ID,
    };

    // Square render dims for the node outputs; the blit stretches each into its
    // (possibly non-square) thumbnail rect with linear sampling.
    const GW: u32 = 256;
    const GH: u32 = 256;
    const GFMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

    // `with_builtin` includes the boundary-node constructors (Generator Input,
    // Final Output, Source) that a preset def references — a bare `new()` omits
    // them and `into_graph` fails with UnknownTypeId on `system.generator_input`.
    let registry = PrimitiveRegistry::with_builtin();
    let mut graph = match def.clone().into_graph(&registry) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("ui-snap graph: into_graph failed: {e:?}");
            return None;
        }
    };

    // Generators (a Generator Input boundary) need the *content-pipeline* render
    // path to produce correct node output — per-frame state, particle warmup, and
    // an HDR tonemap. A single raw-executor frame leaves particle/geometry
    // generators with wrong intermediates (HDR density clamps to white; an
    // un-warmed particle sim composites to black), which would be misleading. So
    // skip node thumbnails for generators: the canvas still shows the full
    // structure (nodes, ports, wires). Effects — texture in, texture out — render
    // correctly here. Driving generators through `GeneratorRenderer` (state +
    // warmup) is the follow-up. See docs/HEADLESS_UI_HARNESS.md.
    let is_generator = graph
        .nodes()
        .any(|inst| inst.node.type_id().as_str() == GENERATOR_INPUT_TYPE_ID);
    if is_generator {
        eprintln!(
            "ui-snap graph: generator preset — node thumbnails skipped (need the \
             content-pipeline path); rendering structure only"
        );
        return None;
    }

    let plan = match compile(&graph) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ui-snap graph: compile failed: {e:?}");
            return None;
        }
    };

    // Effects begin at a `Source` node whose `out` must be bound, or the chain
    // reads an unbound texture. Feed a neutral mid-grey fixture so the effect has
    // something to transform.
    let source_id = graph
        .nodes()
        .find(|inst| inst.node.type_id().as_str() == SOURCE_TYPE_ID)
        .map(|inst| inst.id);
    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), GW, GH, GFMT);
    if let Some(sid) = source_id
        && let Some(res) = resource_for_output(&plan, sid, "out")
    {
        // A UV gradient (R=u, G=v, B=(u+v)/2) rather than a flat fill, so spatial
        // effects — mirror, distort, blur, displace — visibly act on real content
        // in the thumbnails instead of grey-in/grey-out.
        let fixture = RenderTarget::new(device, GW, GH, GFMT, "ui-snap-graph-source");
        let gradient = make_gradient_pipeline(device, GFMT);
        let mut fenc = device.create_encoder("ui-snap-graph-source-fill");
        fenc.draw_fullscreen(&gradient, &fixture.texture, &[], true, true, "Source Fixture Gradient");
        fenc.commit_and_wait_completed();
        backend.pre_bind_texture_2d(res, fixture);
    }

    let mut exec = Executor::new(Box::new(backend));
    exec.set_dump_all(true);

    // Deterministic clock so any time-dependent node is reproducible run to run.
    let frame_time = FrameTime {
        beats: manifold_core::Beats(2.0),
        seconds: manifold_core::Seconds(1.0),
        delta: manifold_core::Seconds(1.0 / 60.0),
        frame_count: 0,
    };

    let mut enc = device.create_encoder("ui-snap-graph-exec");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_gpu(&mut graph, &plan, frame_time, &mut gpu);
    }
    enc.commit_and_wait_completed();

    Some(GraphNodeTextures { graph, exec })
}

/// The `ResourceId` of `node`'s named output port in `plan`, or `None`. Mirrors
/// the parity harness's `resource_for_output`.
fn resource_for_output(
    plan: &manifold_renderer::node_graph::ExecutionPlan,
    node: manifold_renderer::node_graph::NodeInstanceId,
    port: &str,
) -> Option<manifold_renderer::node_graph::ResourceId> {
    plan.steps()
        .iter()
        .find(|s| s.node == node)
        .and_then(|s| s.outputs.iter().find(|(name, _)| *name == port).map(|(_, id)| *id))
}

/// A fullscreen-triangle pipeline that fills the target with a UV gradient
/// (R=u, G=v, B=(u+v)/2) — the neutral source fixture for effect graphs, so a
/// spatial effect's output is legible in its node thumbnail.
fn make_gradient_pipeline(
    device: &GpuDevice,
    format: GpuTextureFormat,
) -> manifold_gpu::GpuRenderPipeline {
    let shader = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(in.uv.x, in.uv.y, (in.uv.x + in.uv.y) * 0.5, 1.0);
}
"#;
    device.create_render_pipeline(shader, "vs_main", "fs_main", format, None, "Source Fixture Gradient")
}

/// `pub(super)` (not just `fn`): `ui_harness_p0`'s
/// `cache_path_full_render` (a sibling module of `render`, both children of
/// `ui_snapshot`) reuses this exact readback helper to pull real pixels off
/// the live `UICacheManager`'s atlas — `pub(super)` is visible to
/// `ui_snapshot` and all its descendants, which covers the sibling test
/// module without making this a public crate API.
pub(super) fn readback(device: &GpuDevice, texture: &GpuTexture, w: u32, h: u32) -> Vec<u8> {
    let bytes_per_row = w * 4;
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("ui-snap-readback");
    enc.copy_texture_to_buffer(texture, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}

/// Save BGRA8 readback bytes as an RGBA8 PNG — swap B/R per pixel (a
/// display-only swizzle; the seam's atlas/offscreen format is BGRA8,
/// matching the live app — `CompositeResources`/`crate::ui_frame`). `pub(super)`
/// for the same reason as [`readback`]: shared by `render_ui_to_png`,
/// `script.rs`'s `Runner` (P2), and `mod.rs`'s `cache_path_full_render`.
pub(super) fn save_bgra_png(bgra: &[u8], w: u32, h: u32, path: &std::path::Path) {
    let mut rgba = bgra.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    image::save_buffer(path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {}: {e}", path.display()));
}

/// Assemble filmstrip tiles (each `tile_w`x`tile_h`, raw BGRA8 bytes) into
/// ONE contact-sheet PNG, `cols` tiles per row (D9a; trailing cells on an
/// incomplete last row stay black). `pub(super)`, same sharing rationale as
/// [`save_bgra_png`].
pub(super) fn save_filmstrip_png(
    tiles: &[Vec<u8>],
    tile_w: u32,
    tile_h: u32,
    cols: u32,
    path: &std::path::Path,
) {
    assert!(!tiles.is_empty(), "filmstrip must have at least one tile");
    let n = tiles.len() as u32;
    let rows = n.div_ceil(cols);
    let sheet_w = tile_w * cols;
    let sheet_h = tile_h * rows;
    let mut sheet = vec![0u8; (sheet_w * sheet_h * 4) as usize];
    let row_bytes = (tile_w * 4) as usize;
    for (i, tile) in tiles.iter().enumerate() {
        let i = i as u32;
        let (col, row) = (i % cols, i / cols);
        let (ox, oy) = (col * tile_w, row * tile_h);
        for y in 0..tile_h {
            let src_off = y as usize * row_bytes;
            let dst_off = (((oy + y) * sheet_w + ox) as usize) * 4;
            sheet[dst_off..dst_off + row_bytes].copy_from_slice(&tile[src_off..src_off + row_bytes]);
        }
    }
    save_bgra_png(&sheet, sheet_w, sheet_h, path);
}
