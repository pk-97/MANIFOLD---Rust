//! Windowless render of a built `UIRoot` to a PNG. Mirrors the proven headless
//! pattern in `manifold-renderer/tests/...` (`GpuDevice::new()` has no window),
//! plus the clip passes from `app_render`: clip bodies, optional injected
//! thumbnails, and clip names are immediate-mode passes (NOT `UITree` nodes),
//! drawn on top of the tree in order (bodies → thumbs → names) with `Load`.
//! See `docs/HEADLESS_UI_HARNESS.md`.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuBinding, GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::clip_draw::{emit_clip_names, emit_clips, ClipBody};
use manifold_renderer::clip_thumb_gpu::ClipThumbGpu;
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;

use super::thumbs;
use crate::ui_root::UIRoot;

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

/// Render the whole UI (`ui.tree` + clip bodies + optional injected thumbnails +
/// clip names) into a `tex_w`×`tex_h` texture and save as PNG. `tex_w` must be a
/// multiple of 64 so the readback stride (`tex_w * 4`) is 256-byte aligned.
pub fn render_ui_to_png(
    ui: &UIRoot,
    selection: &manifold_ui::UIState,
    tex_w: u32,
    tex_h: u32,
    scale: f32,
    with_thumbs: bool,
    path: &str,
) {
    assert_eq!(tex_w % 64, 0, "tex_w must be a multiple of 64 for aligned readback");

    let device = GpuDevice::new();
    let mut renderer = UIRenderer::new(&device, FORMAT);
    let target = RenderTarget::new(&device, tex_w, tex_h, FORMAT, "ui-snap");
    let dpi = f64::from(scale);

    // Pass 1: the UITree — headers, ruler, lane backgrounds, playhead, markers.
    renderer.begin_frame();
    renderer.render_tree(&ui.tree, None);
    let drew = renderer.prepare(&device, tex_w, tex_h, dpi);
    {
        let mut enc = device.create_encoder("ui-snap-tree");
        renderer.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "prepare() reported no UI content to draw");

    // Clips are immediate-mode passes, not tree nodes (same emit path as
    // app_render 4b/5). Resolve the visible clips once, reused across passes.
    let mut clip_rects = Vec::new();
    ui.viewport.visible_clip_rects(&mut clip_rects);
    if !clip_rects.is_empty() {
        let tracks = ui.viewport.get_tracks_rect();

        // Pass 2: GPU clip bodies (Load).
        // Resolve per-clip selected/hovered from real state, exactly as
        // app_render does — never pin them false (a hardcode would misrepresent
        // clip selection once a `select:clip` scene exists).
        let hovered_clip = ui.viewport.hovered_clip_id();
        let bodies: Vec<ClipBody> = clip_rects
            .iter()
            .map(|cr| ClipBody {
                rect: cr.rect,
                base_color: cr.base_color,
                selected: selection.is_selected(&cr.clip_id),
                hovered: hovered_clip == Some(cr.clip_id.as_str()),
                muted: cr.is_muted,
                locked: cr.is_locked,
                generator: cr.is_generator,
            })
            .collect();
        renderer.begin_frame();
        renderer.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
        emit_clips(&mut renderer, &bodies);
        renderer.pop_immediate_clip();
        if renderer.prepare(&device, tex_w, tex_h, dpi) {
            let mut enc = device.create_encoder("ui-snap-clips");
            renderer.render(&mut enc, &target.texture, GpuLoadAction::Load);
            enc.commit_and_wait_completed();
        }

        // Pass 3: injected test thumbnails (Load), through the real ClipThumbGpu.
        if with_thumbs {
            let atlas = thumbs::make_test_atlas(&device);
            let quads = thumbs::build_quads(&clip_rects);
            if !quads.is_empty() {
                let mut thumb = ClipThumbGpu::new(&device, FORMAT);
                let mut enc = device.create_encoder("ui-snap-thumbs");
                thumb.render(
                    &device,
                    &mut enc,
                    &target.texture,
                    tex_w,
                    tex_h,
                    scale,
                    tracks,
                    &atlas,
                    &quads,
                );
                enc.commit_and_wait_completed();
            }
        }

        // Pass 4: clip names on top (Load).
        renderer.begin_frame();
        emit_clip_names(&mut renderer, &clip_rects, tracks);
        if renderer.prepare(&device, tex_w, tex_h, dpi) {
            let mut enc = device.create_encoder("ui-snap-names");
            renderer.render(&mut enc, &target.texture, GpuLoadAction::Load);
            enc.commit_and_wait_completed();
        }
    }

    // Pass 5: top-level overlays (modals, dropdowns, perf HUD) on top of
    // everything — mirrors the live app drawing the overlay region at
    // `Depth::OVERLAY`. The headless passes are painter's-order, so a final Load
    // pass over the overlay node ranges lifts them above the immediate-mode clip
    // passes; without it an open modal would be occluded by the clips.
    if !ui.overlay_draw.is_empty() {
        renderer.begin_frame();
        for &(start, end) in &ui.overlay_draw {
            renderer.render_tree_range(&ui.tree, start, end);
        }
        if renderer.prepare(&device, tex_w, tex_h, dpi) {
            let mut enc = device.create_encoder("ui-snap-overlays");
            renderer.render(&mut enc, &target.texture, GpuLoadAction::Load);
            enc.commit_and_wait_completed();
        }
    }

    let bytes = readback(&device, &target.texture, tex_w, tex_h);
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

    let device = GpuDevice::new();
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

    // Composite each node's real output texture over its placeholder. The canvas
    // reports a screen rect per image-emitting node (`visible_node_thumbnails`),
    // keyed by the same stable NodeId the dump uses.
    if let Some(nt) = node_textures.as_ref() {
        let blit = make_blit_pipeline(&device);
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            ..Default::default()
        });
        let mut enc = device.create_encoder("ui-snap-graph-thumbs");
        for (node_id, x, y, w, h) in canvas.visible_node_thumbnails(viewport) {
            let Some(tex) = nt.texture_for(node_id.as_str()) else {
                continue;
            };
            // A render-target-only output can't be sampled — binding it crashes
            // AGX (the live atlas path guards the same way). Skip its cell.
            if !tex.is_shader_readable() {
                continue;
            }
            enc.draw_fullscreen_viewport(
                &blit,
                &target.texture,
                &[
                    GpuBinding::Texture { binding: 0, texture: tex },
                    GpuBinding::Sampler { binding: 1, sampler: &sampler },
                ],
                (x, y, w, h),
                GpuLoadAction::Load,
                "Node Thumbnail Blit",
            );
        }
        enc.commit_and_wait_completed();
    }

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
) {
    use manifold_ui::draw::Painter;
    use manifold_ui::graph_canvas::{GraphCanvas, Rect as CanvasRect};
    use manifold_ui::node::{TextAlign, UIStyle};
    use manifold_ui::panels::graph_editor::{EDITOR_CARD_LANE_WIDTH, SIDEBAR_WIDTH};
    use manifold_ui::{Rect as UiRect, UITree};

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

    let device = GpuDevice::new();
    let mut renderer = UIRenderer::new(&device, FORMAT);
    let target_tex = RenderTarget::new(&device, tex_w, tex_h, FORMAT, "ui-snap-editor");
    let dpi = f64::from(scale);

    let mut tree = UITree::new();

    // Right lane: the WHOLE inspector column, driven exactly like the live editor
    // (`present_graph_editor_window`) — a throwaway `UIRoot` synced from the
    // fixture project, its inspector built into the lane rect and rendered from
    // its own tree alongside the sidebar `tree` below. Matches the live editor's
    // full-inspector column (master/layer tabs, cards, chrome, macros).
    let mut editor_ui = crate::ui_root::UIRoot::new();
    let active_idx = match target {
        manifold_core::GraphTarget::Generator(lid) => {
            project.timeline.layers.iter().position(|l| &l.layer_id == lid)
        }
        manifold_core::GraphTarget::Effect(_) => None,
    };
    crate::ui_bridge::sync_project_data(&mut editor_ui, project, active_idx, selection);
    crate::ui_bridge::sync_inspector_data(&mut editor_ui, project, active_idx, selection);
    editor_ui.build_inspector_in_rect(UiRect::new(
        card_x,
        0.0,
        EDITOR_CARD_LANE_WIDTH,
        canvas_height,
    ));

    // Left sidebar: backing panel + the two monitor titles + an empty-state
    // hint, laid out with the same math `present_graph_editor_window` uses (16:9
    // monitor_aspect default — no content pipeline headless to read the real
    // project aspect from).
    {
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

        let title_style = UIStyle {
            text_color: manifold_ui::color::TEXT_WHITE_C32,
            font_size: 14,
            text_align: TextAlign::Left,
            ..UIStyle::default()
        };
        tree.add_panel(
            None,
            0.0,
            0.0,
            SIDEBAR_WIDTH,
            canvas_height,
            UIStyle { bg_color: manifold_ui::color::EFFECT_CARD_INNER_BG_C32, ..UIStyle::default() },
        );
        tree.add_label(None, preview_x, node_title_y, preview_w, preview_title_h, "Node Output", title_style);
        tree.add_label(
            None,
            preview_x,
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
        tree.add_label(None, preview_x, master_title_y, preview_w, preview_title_h, "Master Out", title_style);
    }

    // Center canvas, offset into its lane between the two side columns — the
    // same per-node-dump machinery as `render_graph_to_png`.
    let viewport = CanvasRect::new(canvas_x, 0.0, canvas_width, canvas_height);
    let mut canvas = GraphCanvas::new();
    // Show the on-node param rows (the Blender-style layout) in the PNG — a live
    // canvas starts nodes collapsed for legibility, but the snapshot is a
    // verification surface, so expand them.
    canvas.set_default_expanded(true);
    canvas.set_snapshot(snapshot);
    canvas.apply_pending_fit(viewport);
    let node_textures = render_graph_node_textures(&device, def);

    {
        let mut enc = device.create_encoder("ui-snap-editor-clear");
        enc.clear_texture(&target_tex.texture, 0.10, 0.10, 0.12, 1.0);
        enc.commit_and_wait_completed();
    }

    // Same paint order as `present_graph_editor_window`: canvas immediate-mode
    // draws first, then the lane/sidebar UITree layered on top in the same batch.
    renderer.begin_frame();
    canvas.render(&mut renderer as &mut dyn Painter, viewport);
    renderer.render_tree(&tree, None);
    // The inspector column lives in its own tree (built via a throwaway UIRoot).
    renderer.render_tree(&editor_ui.tree, None);
    // Column dividers, same as the runtime present pass — default widths (this
    // headless path has no interactive drag state).
    let editor_area = UiRect::new(0.0, 0.0, logical_w, logical_h);
    dock.draw(editor_area, &mut renderer as &mut dyn Painter);
    // Bottom mini-timeline, built from the fixture project (playhead at beat 0),
    // same view-model the live present pass draws.
    let (mini_clips, mini_rows, mini_total, mini_bpb, mini_readout) =
        crate::app_render::mini_timeline_data(project, 0.0);
    manifold_ui::MiniTimeline::draw(
        dock_rects.bottom,
        mini_total,
        mini_bpb,
        0.0,
        mini_rows,
        &mini_clips,
        &mini_readout,
        false,
        &mut renderer as &mut dyn Painter,
    );
    let drew = renderer.prepare(&device, tex_w, tex_h, dpi);
    {
        let mut enc = device.create_encoder("ui-snap-editor");
        renderer.render(&mut enc, &target_tex.texture, GpuLoadAction::Load);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "graph editor window produced no draws (empty snapshot?)");

    if let Some(nt) = node_textures.as_ref() {
        let blit = make_blit_pipeline(&device);
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            ..Default::default()
        });
        let mut enc = device.create_encoder("ui-snap-editor-thumbs");
        for (node_id, x, y, w, h) in canvas.visible_node_thumbnails(viewport) {
            let Some(tex) = nt.texture_for(node_id.as_str()) else {
                continue;
            };
            if !tex.is_shader_readable() {
                continue;
            }
            enc.draw_fullscreen_viewport(
                &blit,
                &target_tex.texture,
                &[
                    GpuBinding::Texture { binding: 0, texture: tex },
                    GpuBinding::Sampler { binding: 1, sampler: &sampler },
                ],
                (x, y, w, h),
                GpuLoadAction::Load,
                "Node Thumbnail Blit",
            );
        }
        enc.commit_and_wait_completed();
    }

    let bytes = readback(&device, &target_tex.texture, tex_w, tex_h);
    image::save_buffer(path, &bytes, tex_w, tex_h, image::ExtendedColorType::Rgba8)
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
    device: &GpuDevice,
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
    let mut backend = MetalBackend::new(device, GW, GH, GFMT);
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

/// A fullscreen-triangle texture blit pipeline targeting the snapshot format —
/// the same raw blit the live workspace/atlas preview uses, to composite a node
/// output texture into a thumbnail viewport.
fn make_blit_pipeline(device: &GpuDevice) -> manifold_gpu::GpuRenderPipeline {
    let shader = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
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
    return textureSample(t_source, s_source, in.uv);
}
"#;
    device.create_render_pipeline(shader, "vs_main", "fs_main", FORMAT, None, "Node Thumbnail Blit")
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

fn readback(device: &GpuDevice, texture: &GpuTexture, w: u32, h: u32) -> Vec<u8> {
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
