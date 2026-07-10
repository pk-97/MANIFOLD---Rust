//! Headless UI snapshot harness (feature `ui-snapshot`). An agent-facing tool
//! to render MANIFOLD's REAL UI tree to a PNG plus a machine-readable tree dump,
//! with no winit window — so UI/UX work is see-able, measurable, and provable.
//!
//! Invoked via the `cargo xtask` alias:
//!   cargo xtask ui-snap <scene> [--dump] [--interact "select:<layer>"]
//! See `docs/HEADLESS_UI_HARNESS.md`.

mod compare;
mod dump;
mod fixtures;
mod interact;
mod render;
mod script;
mod thumbs;

use std::path::{Path, PathBuf};

use crate::ui_bridge::{push_state, sync_inspector_data, sync_project_data, TransportDisplayCache};
use crate::ui_root::UIRoot;

// Logical UI size = texture size (rendered 1:1; `UIRenderer::prepare`'s scale is
// a text-DPI hint, not a geometry transform). Tall enough for 7 × 140px lanes +
// ruler; `tex_w` must be a multiple of 64 for an aligned readback.
const LOGICAL_W: f32 = 1536.0;
const LOGICAL_H: f32 = 1216.0;
const SCALE: f32 = 1.0;

/// Stable output root: `target/ui-snapshots/<scene>/`.
fn out_dir(scene: &str) -> PathBuf {
    PathBuf::from("target/ui-snapshots").join(scene)
}

/// A scene name safe to use as a single path component. Only `project:<path>`
/// scenes need this (the path half contains `/`); every built-in scene name is
/// already a bare identifier, so this is a no-op for them and their output
/// paths stay byte-identical.
fn sanitize_scene_name(scene: &str) -> String {
    scene.chars().map(|c| if c == '/' || c == ':' || c == ' ' { '_' } else { c }).collect()
}

/// Entry dispatched from `main()` when `argv[1] == "ui-snap"`. `args` is the
/// argument slice starting at `"ui-snap"`.
pub fn run(args: &[String]) {
    let scene = args.get(1).map(String::as_str).unwrap_or("timeline");
    let want_dump = args.iter().any(|a| a == "--dump");
    let want_vs_mockup = args.iter().any(|a| a == "--vs-mockup");
    let want_thumbs = args.iter().any(|a| a == "--thumbs");
    let interact = arg_value(args, "--interact");
    let script_path = arg_value(args, "--script");
    // P0.0 evidence flag (`docs/TIMELINE_LAYOUT_P0_SPEC.md`): seed BOTH scroll
    // owners (`Viewport::scroll_y_px` + the header panel's `ScrollContainer`
    // offset) to the same non-zero pixel value right after the base render
    // and before any `--interact`, so a subsequent content-shrinking edit can
    // be captured mid-scroll. A flag rather than an `interact` verb because it
    // seeds state that predates the interaction being tested, not an action
    // being tested itself.
    let scroll_seed: Option<f32> = arg_value(args, "--scroll").and_then(|s| s.parse().ok());

    // `--script <file.json>` (UI_AUTOMATION_DESIGN.md §6, P2): a JSON array of
    // `AutomationAction`s executed in order. Fully owns its own build + gate
    // exit code — bypasses the `--dump`/`--interact`/mockup flags below.
    if let Some(path) = script_path {
        script::run(scene, &path);
        return;
    }

    // `all`: render every scene in one process — a full-app eyeball after a
    // change. Skips the per-scene-only flags (mockup, interact); pass those to a
    // single scene when you need them.
    if scene == "all" {
        for s in ["timeline", "states", "inspector"] {
            render_ui_scene(s, want_dump, false, want_thumbs, None, None);
        }
        run_graph_preset("Mirror");
        run_editor_preset("FluidSim2D", want_dump);
        return;
    }

    // The `graph` scene is not a UITree fixture — it renders the node-graph
    // editor canvas from a synthesized snapshot, on its own render path.
    if scene == "graph" {
        let preset = arg_value(args, "--preset").unwrap_or_else(|| "Mirror".to_string());
        run_graph_preset(&preset);
        return;
    }

    // The `editor` scene renders the FULL graph-editor window (card lane +
    // canvas + sidebar chrome), not just the bare canvas — generator presets
    // only (see `fixtures::generator_editor_fixture`).
    if scene == "editor" {
        let preset = arg_value(args, "--preset").unwrap_or_else(|| "FluidSim2D".to_string());
        run_editor_preset(&preset, want_dump);
        return;
    }

    // The `transform` scene is the UI transform-stack capability's visual
    // proof (`docs/UI_TRANSFORM_STACK_DESIGN.md`) — a bespoke `UITree` with no
    // `Project`/fixture behind it, so it doesn't go through `fixtures::build`.
    if scene == "transform" {
        let dir = out_dir("transform");
        std::fs::create_dir_all(&dir).expect("create ui-snapshots dir");
        let png = dir.join("transform.png");
        render::render_transform_proof_to_png(png.to_str().expect("utf-8 path"));
        println!("ui-snap: wrote {}", png.display());
        return;
    }

    render_ui_scene(scene, want_dump, want_vs_mockup, want_thumbs, interact, scroll_seed);
}

/// P0.3 (`docs/TIMELINE_LAYOUT_P0_SPEC.md`): `hairlineclips` needs genuine far
/// zoom (the minimum `color::ZOOM_LEVELS` entry) to make its clips
/// sub-pixel-wide; every other scene keeps the existing fixed 24px/beat so
/// their PNGs stay byte-identical across phases. Shared by `render_ui_scene`
/// and `script::run` so a script's resolved rects agree with a plain
/// `--dump`/`--interact` run of the same scene.
fn zoom_ppb_for_scene(scene: &str) -> f32 {
    if scene == "hairlineclips" { 1.0 } else { 24.0 }
}

/// Build + render one UITree scene (`timeline` / `states` / `inspector`) through
/// the real core→UI translation path, plus an optional `--interact` "after" pass
/// and mockup composite. Unknown scene name exits 2.
fn render_ui_scene(
    scene: &str,
    want_dump: bool,
    want_vs_mockup: bool,
    want_thumbs: bool,
    interact: Option<String>,
    scroll_seed: Option<f32>,
) {
    let Some(mut data) = fixtures::build(scene) else {
        eprintln!(
            "ui-snap: unknown scene '{scene}' (known: timeline, states, inspector, scrollshrink, hairlineclips, automation, selectionclips, audiosends, empty, graph, editor, transform, all, project:<path>)"
        );
        std::process::exit(2);
    };

    let zoom_ppb: f32 = zoom_ppb_for_scene(scene);

    // `scene` itself becomes the output dir name and every dumped file's stem
    // below; a `project:<path>` scene carries `/`/`:`/` ` that would otherwise
    // land as extra directories (or, if the path is absolute, silently replace
    // `dir` entirely — `Path::join` drops the base on an absolute join). Every
    // built-in scene name is untouched by sanitization, so their output paths
    // stay byte-identical.
    let scene = &sanitize_scene_name(scene);
    let dir = out_dir(scene);
    std::fs::create_dir_all(&dir).expect("create ui-snapshots dir");

    // Build the UI through the REAL core→UI translation path, render the base.
    let mut ui = UIRoot::new();
    ui.resize(LOGICAL_W, LOGICAL_H);
    if scene == "inspector" || scene == "bug060" || scene == "paramsteps" {
        // The inspector IS the subject: keep it at a generous width and give the
        // timeline a normal split so the selected layer's cards have room.
        // `bug060` (UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md P1 gate scene) and
        // `paramsteps` (PARAM_STEP_ACTIONS P3) get the same treatment — both are
        // scrolled/inspector-subject scenes, not timeline ones.
        ui.layout.inspector_width = 600.0;
        ui.layout.timeline_split_ratio = 0.6;
    } else {
        // Make the timeline the subject: drop the inspector, let lanes fill the
        // vertical. (Both fields are read by layout.resize() inside ui.build().)
        ui.layout.inspector_width = 0.0;
        ui.layout.timeline_split_ratio = 0.93;
    }
    sync_build(&mut ui, &data, zoom_ppb);

    // P0.1: the viewport is the sole scroll owner (D2) — the header panel
    // reads `viewport.scroll_y_px()` live at draw time, so seeding it here is
    // the only seed needed (mirrors `ui_root.rs`'s settings-restore path).
    // Before P0.1 this seeded two independent copies to reproduce RC1 ("user
    // scrolled, then the content shrank"); post-fix, `rebuild_mapper_layout`
    // (called from `sync_project_data` inside the `--interact` branch below)
    // re-clamps this same value against the new content height every time
    // (D3), so RC1 no longer reproduces — see
    // `docs/evidence/timeline_p0/after/README.md`.
    // Seeded BEFORE the base render (fixed 2026-07-07): it used to apply
    // after, so a bare `--scroll` run wrote an unscrolled base PNG while
    // printing the seed message — the seed only ever showed in an
    // `--interact` after-render. The re-sync after seeding is load-bearing:
    // the header column bakes its Y offsets at BUILD time (only the lane
    // pass reads `scroll_y_px()` at draw), so rendering without a re-sync
    // draws scrolled lanes under unscrolled headers — a desync the live app
    // can't produce (it rebuilds every frame a scroll event dirties).
    if let Some(y) = scroll_seed {
        let x = ui.viewport.scroll_x_beats().as_f32();
        ui.viewport.set_scroll(x, y);
        println!("ui-snap: scroll-seed y={y} (viewport clamped to {})", ui.viewport.scroll_y_px());
        sync_build(&mut ui, &data, zoom_ppb);
    }

    render_and_dump(
        &ui,
        &data.selection,
        &data.content.automation_latched_params,
        &dir,
        scene,
        "",
        want_dump,
        want_thumbs,
    );

    // Optional: render the HTML mockup and composite app | mockup side by side.
    if want_vs_mockup {
        compare::vs_mockup(&dir, scene, &dir.join(format!("{scene}.png")));
    }

    // Optional interaction: drive a real event, re-sync, render the "after".
    if let Some(spec) = interact {
        let outcome = interact::apply(&mut ui, &mut data, &spec);
        let desc = outcome.desc;
        println!("ui-snap: interact {desc}");
        // D6 (§6 seam brief): a miss is not patched over — the outcome's
        // STRUCTURAL flag (set by every verb's Err path) fails the run
        // loudly with the dump attached, rather than rendering an "after"
        // that never actually happened. (Was a `contains("MISS: ")` grep
        // that no verb's text matched — every miss exited 0 until
        // 2026-07-07.)
        if outcome.missed {
            sync_build(&mut ui, &data, zoom_ppb);
            let fail_path = dir.join(format!("{scene}.interact-miss.tree.json"));
            script::write_fail_dump(&ui, &data, &fail_path);
            eprintln!(
                "ui-snap: interact MISS — the real input path did not resolve; dump at {}",
                fail_path.display()
            );
            std::process::exit(1);
        }
        sync_build(&mut ui, &data, zoom_ppb);
        render_and_dump(
            &ui,
            &data.selection,
            &data.content.automation_latched_params,
            &dir,
            scene,
            ".after",
            want_dump,
            want_thumbs,
        );
    }
}

/// Render the node-graph editor canvas for one preset id (effect or generator).
/// The graph snapshot is synthesized straight from the catalog —
/// `loaded_preset_view_by_id` → `snapshot_for_view` → the UI translation — so no
/// content thread or running chain is needed.
fn run_graph_preset(preset: &str) {
    let pid = manifold_core::PresetTypeId::from_string(preset.to_string());

    let Some(view) = manifold_renderer::node_graph::loaded_preset_view_by_id(&pid) else {
        eprintln!(
            "ui-snap graph: no graph view for preset '{preset}' \
             (needs a JSON preset carrying presetMetadata)"
        );
        std::process::exit(2);
    };
    let Some(rg_snap) = manifold_renderer::node_graph::snapshot_for_view(view) else {
        eprintln!("ui-snap graph: snapshot_for_view failed for '{preset}' (def failed to materialize)");
        std::process::exit(2);
    };
    let gv_snap = crate::ui_translate::graph_snapshot_to_ui(&rg_snap);

    let dir = out_dir("graph");
    std::fs::create_dir_all(&dir).expect("create ui-snapshots dir");
    let tex_w = (LOGICAL_W * SCALE) as u32;
    let tex_h = (LOGICAL_H * SCALE) as u32;
    let png = dir.join("graph.png");
    // The canonical def drives the headless graph render that produces the
    // per-node thumbnails; the snapshot drives the canvas layout.
    render::render_graph_to_png(
        &gv_snap,
        view.canonical_def,
        tex_w,
        tex_h,
        SCALE,
        png.to_str().expect("utf-8 path"),
    );
    println!("ui-snap: wrote {} ({preset})", png.display());
}

/// Render the FULL graph-editor window (preview sidebar + canvas + card lane)
/// for one generator preset. Builds a one-layer fixture `Project` carrying the
/// preset (`fixtures::generator_editor_fixture`) so the right lane's card is the
/// real `ParamCardConfig`, not synthesized — see `render::render_graph_editor_to_png`.
fn run_editor_preset(preset: &str, want_dump: bool) {
    let pid = manifold_core::PresetTypeId::from_string(preset.to_string());
    let Some(view) = manifold_renderer::node_graph::loaded_preset_view_by_id(&pid) else {
        eprintln!(
            "ui-snap editor: no graph view for preset '{preset}' \
             (needs a JSON preset carrying presetMetadata)"
        );
        std::process::exit(2);
    };
    let Some(rg_snap) = manifold_renderer::node_graph::snapshot_for_view(view) else {
        eprintln!("ui-snap editor: snapshot_for_view failed for '{preset}' (def failed to materialize)");
        std::process::exit(2);
    };
    let Some((project, target, selection)) = fixtures::generator_editor_fixture(preset) else {
        eprintln!(
            "ui-snap editor: '{preset}' isn't a generator preset \
             (the editor scene only covers GraphTarget::Generator today)"
        );
        std::process::exit(2);
    };
    let gv_snap = crate::ui_translate::graph_snapshot_to_ui(&rg_snap);

    let dir = out_dir("editor");
    std::fs::create_dir_all(&dir).expect("create ui-snapshots dir");
    let tex_w = (LOGICAL_W * SCALE) as u32;
    let tex_h = (LOGICAL_H * SCALE) as u32;
    let png = dir.join("editor.png");
    render::render_graph_editor_to_png(
        &project,
        &target,
        &selection,
        &gv_snap,
        view.canonical_def,
        tex_w,
        tex_h,
        SCALE,
        png.to_str().expect("utf-8 path"),
        want_dump.then(|| dir.join("editor.tree.json")).as_deref(),
    );
    println!("ui-snap: wrote {} ({preset})", png.display());
}

/// The real translation path: structural sync → zoom-to-fit → build → push state.
/// `zoom_ppb` is the scene's pixels-per-beat (24.0 for the 48-beat fixtures;
/// `render_ui_scene` overrides it per scene name — see the `hairlineclips`
/// far-zoom case).
fn sync_build(ui: &mut UIRoot, data: &fixtures::SceneData, zoom_ppb: f32) {
    sync_project_data(ui, &data.project, data.active, &data.selection);
    // Configure the inspector (tabs + the active layer's effect/gen cards) from
    // the selection — the live app calls this whenever the active layer changes.
    // Without it the inspector stays on its default Master view, so the selected
    // layer's chain never appears.
    sync_inspector_data(
        ui,
        &data.project,
        data.active,
        &data.selection,
        &data.content.automation_latched_params,
    );
    // Zoom so the fixture's clips fit the lane width (set before build so the
    // ruler ticks and the clip rects agree on px/beat).
    ui.viewport.set_zoom(zoom_ppb);
    ui.build();
    let mut tcache = TransportDisplayCache::new();
    push_state(ui, &data.project, &data.content, data.active, &data.selection, false, None, &mut tcache);
    // Reconcile the `push_state` setters into the tree — mirrors the live
    // app's per-frame call (`app_render.rs`'s "6. Lightweight update" after its
    // own `push_state`). Every panel's `set_*` methods are "store only; the
    // reconcile applies them" (see `TransportPanel`'s doc comment); without
    // this the harness only ever showed each panel's `::new()` hardcoded
    // defaults, silently — every existing scene's fixture `ContentState`
    // happened to already match those defaults (paused, not recording, no BPM
    // reset/clear pending), so the gap never surfaced until the `automation`
    // scene (P4a) deliberately diverged (armed + a latch).
    ui.update();
}

/// Render to `<scene><suffix>.png`, and (if requested) the tree dump as JSON +
/// a terse stdout summary.
fn render_and_dump(
    ui: &UIRoot,
    selection: &manifold_ui::UIState,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    dir: &Path,
    scene: &str,
    suffix: &str,
    want_dump: bool,
    with_thumbs: bool,
) {
    let tex_w = (LOGICAL_W * SCALE) as u32;
    let tex_h = (LOGICAL_H * SCALE) as u32;
    let png = dir.join(format!("{scene}{suffix}.png"));
    render::render_ui_to_png(
        ui,
        selection,
        automation_latched,
        tex_w,
        tex_h,
        SCALE,
        with_thumbs,
        png.to_str().expect("utf-8 path"),
    );
    println!("ui-snap: wrote {}", png.display());

    if want_dump {
        // Custom-surface targets (UI_AUTOMATION_DESIGN.md D5/§5): the same
        // live geometry `render_ui_to_png` paints from and `ClipHitTester` /
        // `hit_test_automation` hit-test against — read once here so the dump
        // can never disagree with what's on screen or clickable.
        let mut clip_rects = Vec::new();
        ui.viewport.visible_clip_rects(&mut clip_rects);
        let clip_targets = manifold_ui::clip_hit_tester::ClipHitTargets(&clip_rects);
        let automation_lanes = ui.viewport.automation_lane_screens(automation_latched);
        let automation_targets = manifold_ui::automation_hit_tester::AutomationHitTargets(&automation_lanes);
        let surfaces: Vec<&dyn manifold_ui::hit_targets::HitTargets> =
            vec![&clip_targets, &automation_targets];

        let json = dump::dump_tree_ex(&ui.tree, &surfaces);
        let json_path = dir.join(format!("{scene}{suffix}.tree.json"));
        std::fs::write(&json_path, serde_json::to_string_pretty(&json).expect("serialize dump"))
            .expect("write tree json");
        println!("ui-snap: wrote {}", json_path.display());
        print!("{}", dump::terse(&ui.tree));
    }
}

/// Value following `flag` in `args`, if present (`--interact <value>`).
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

#[cfg(test)]
mod footer_leak_probe {
    //! BUG-060 investigation: observe the LIVE cache-path traversal (the one the
    //! main window renders through — `render_dirty_panels` →
    //! `UIRenderer::render_sub_region` → `UITree::traverse_flat_range`), NOT the
    //! headless `traverse()` path the P1 gate PNG used. Builds the real `bug060`
    //! scene through the real `UIRoot::build()` region wrap, scrolls the inspector
    //! to the bottom exactly like the live app, then walks the inspector panel's
    //! node range the way the cache manager does and reports every node whose
    //! visible (clipped) paint reaches BELOW the footer's top edge.
    use super::*;
    use manifold_renderer::ui_cache_manager::PanelSlot;
    use manifold_ui::node::Rect;
    use manifold_ui::tree::TraversalEvent;
    use manifold_ui::UIFlags;

    fn intersect(a: Rect, b: Rect) -> Rect {
        let x0 = a.x.max(b.x);
        let y0 = a.y.max(b.y);
        let x1 = (a.x + a.width).min(b.x + b.width);
        let y1 = (a.y + a.height).min(b.y + b.height);
        Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }

    #[test]
    fn cache_path_inspector_does_not_paint_below_footer_top() {
        let data = fixtures::build("bug060").expect("bug060 scene");
        let mut ui = UIRoot::new();
        ui.resize(LOGICAL_W, LOGICAL_H);
        ui.layout.inspector_width = 600.0;
        ui.layout.timeline_split_ratio = 0.6;
        sync_build(&mut ui, &data, 24.0);

        // Scroll the inspector hard to the bottom — the live gesture path.
        let insp = ui.layout.inspector();
        let cursor_x = insp.x + insp.width * 0.5;
        let scrolled = ui.try_inspector_scroll(1_000_000.0, cursor_x);
        assert!(scrolled, "sanity: inspector must report a live scroll container");

        let footer_top = ui.layout.footer().y;
        assert!(footer_top > 0.0);

        // The inspector panel's node range, EXACTLY as `panel_cache_info` reports
        // it to the cache manager.
        let infos = ui.panel_cache_info();
        let insp_info = infos
            .iter()
            .find(|i| i.slot == PanelSlot::Inspector)
            .expect("inspector panel in cache info");
        let (start, end) = (insp_info.node_start, insp_info.node_end);
        let eps = 0.5_f32;

        // Sanity: the scroll must have pushed content so that some node's RAW
        // bounds straddle the footer edge — otherwise the leak check below is
        // vacuous (nothing to clip). Mirrors the P1 tab-strip test's `any_above`.
        let straddling = (start..end)
            .filter(|&i| {
                let n = ui.tree.get_node(ui.tree.id_at(i)).unwrap();
                n.flags.contains(UIFlags::VISIBLE)
                    && n.bounds.width > 0.0
                    && n.bounds.height > 0.0
                    && n.bounds.y < footer_top
                    && n.bounds.y_max() > footer_top + eps
            })
            .count();
        assert!(
            straddling > 0,
            "sanity: scrolling must leave some node straddling footer_top ({footer_top}) \
             or this test proves nothing"
        );

        // Walk the range the way `render_sub_region` does: `traverse_flat_range`
        // pre-pushes ancestor clips (the region container + scroll-column clips),
        // then emits nodes. Reconstruct the effective clip per node and flag any
        // whose clipped paint crosses below the footer's top edge.
        let mut clip_stack: Vec<Rect> = Vec::new();
        let mut leaks: Vec<(usize, Rect, Option<Rect>, String)> = Vec::new();
        ui.tree.traverse_flat_range(start, end, false, |ev| match ev {
            TraversalEvent::PushClip(r) => {
                let clipped = clip_stack.last().map(|c| intersect(*c, r)).unwrap_or(r);
                clip_stack.push(clipped);
            }
            TraversalEvent::PopClip => {
                clip_stack.pop();
            }
            TraversalEvent::Node(n) => {
                let b = n.bounds;
                if !n.flags.contains(UIFlags::VISIBLE) || b.width <= 0.0 || b.height <= 0.0 {
                    return;
                }
                // The GPU cull (`draw_node`) discards a node fully outside the
                // clip; replicate it so we don't count nodes that never paint.
                if let Some(c) = clip_stack.last()
                    && (b.x >= c.x_max()
                        || b.x_max() <= c.x
                        || b.y >= c.y_max()
                        || b.y_max() <= c.y)
                {
                    return;
                }
                // Effective painted bottom = clipped bottom (or raw if unclipped).
                let painted_bottom = match clip_stack.last() {
                    Some(c) => b.y_max().min(c.y_max()),
                    None => b.y_max(),
                };
                if painted_bottom > footer_top + eps {
                    let text = n.text.clone().unwrap_or_default();
                    leaks.push((n.id.index(), b, clip_stack.last().copied(), text));
                }
            }
        });

        if !leaks.is_empty() {
            eprintln!(
                "\n=== BUG-060 cache-path leak: {} inspector node(s) paint below footer_top={footer_top} ===",
                leaks.len()
            );
            for (idx, b, clip, text) in &leaks {
                eprintln!(
                    "  node[{idx}] bounds=({:.1},{:.1} {:.1}x{:.1}) y_max={:.1}  clip={:?}  text={:?}",
                    b.x, b.y, b.width, b.height, b.y_max(), clip, text
                );
            }
            eprintln!("=== end leak report ===\n");
        }
        assert!(
            leaks.is_empty(),
            "{} inspector node(s) paint below the footer top edge on the LIVE cache path",
            leaks.len()
        );
    }
}

#[cfg(test)]
mod cache_path_footer_differential {
    //! P0 (`docs/UI_HARNESS_UNIFICATION_DESIGN.md`, D1/D2/D7/D8/D9c): the
    //! reliability-critical BUG-060 assertion. Unlike `footer_leak_probe`
    //! (CPU bounds walk, zero pixels) and the sibling `render` module's
    //! whole-tree-fresh-every-frame harness (FORBIDDEN here, structurally
    //! blind to a stale-atlas-pixel bug — see that module's own doc comment),
    //! this test builds a REAL `UICacheManager` + `UIRenderer` + atlas texture and
    //! renders through `render_dirty_panels` exactly as `present_all_windows`
    //! does — the mirrored call order (`panel_cache_info` →
    //! `set_scale_factor` → `ensure_atlas` → `render_dirty_panels` →
    //! `atlas_texture()`, app_render.rs:3890-3904/4017) — then reads back
    //! real footer-band pixels and diffs them across a scroll + drawer-tween
    //! + tab-swap repro sequence (§5).
    //!
    //! D8: scale factor one at the fixture's logical size — layout is
    //! pixel-exact, and the raster is far cheaper than a Retina pass.
    //!
    //! P0 is the beachhead (D6): the invalidation decision below is
    //! TRANSCRIBED from `app_render.rs:955-965` (scroll-in-place) and
    //! `2819-2852` (rebuild/structural), not yet the shared
    //! `apply_ui_frame_invalidations` seam (P1, D3) — this driver never
    //! drags, so the drag-guard branches app_render.rs has are correctly
    //! omitted here, not silently dropped.

    use std::time::Duration;

    use manifold_core::LayerId;
    use manifold_gpu::{GpuDevice, GpuTextureFormat};
    use manifold_renderer::ui_cache_manager::{PanelSlot, UICacheManager};
    use manifold_renderer::ui_renderer::UIRenderer;
    use manifold_ui::automation::{self, AutomationTarget, SelectorQuery};
    use manifold_ui::input::PointerAction;
    use manifold_ui::node::Vec2;

    use super::*;
    use crate::content_state::ContentState;
    use crate::user_prefs::UserPrefs;

    /// The per-frame decision flags `apply_ui_frame_invalidations` will own
    /// at P1 (D3/D12) — carried by hand here, exactly transcribing
    /// app_render.rs's persistence (a flag set THIS frame's tween-poll is
    /// consumed by NEXT frame's decision block, never immediately — see
    /// `app_render.rs:2942` vs `:2821`).
    #[derive(Default)]
    struct Signals {
        needs_rebuild: bool,
        needs_structural_sync: bool,
    }

    /// Running state across the repro sequence — deliberately NOT holding
    /// the GPU device/renderer/cache/tree by reference (see report), so
    /// gestures between steps can still borrow those directly.
    #[derive(Default)]
    struct RunState {
        signals: Signals,
        ref_bytes: Vec<u8>,
        assert_points: usize,
        changed_rows_total: usize,
        changed_pixels_total: usize,
        last_diff_bytes: Option<Vec<u8>>,
    }

    /// Mirrors app_render.rs:2819-2845 minus the inspector/layer drag guards
    /// (this driver never drags — the guards would never fire, so omitting
    /// them changes nothing observable).
    fn apply_decision(ui: &mut UIRoot, cache: &mut UICacheManager, signals: &mut Signals) {
        if signals.needs_rebuild || signals.needs_structural_sync {
            signals.needs_rebuild = false;
            signals.needs_structural_sync = false;
            ui.build();
            ui.inspector.apply_selection_visuals(&mut ui.tree);
            cache.invalidate_all();
        }
    }

    /// Mirrors app_render.rs:3890-3904 — up to, and not past, reading
    /// `atlas_texture()` (the seam this test asserts against; the offscreen
    /// clear/blit/video-band steps at 4011-4064 are winit-presentation-only
    /// and irrelevant to atlas-pixel staleness). Returns the FULL panel
    /// ranges rendered this frame — the same value `present_all_windows`
    /// uses to clear dirty flags (3912-3914), replicated here so dirty
    /// tracking doesn't drift from the live contract across steps.
    fn composite_frame(
        device: &GpuDevice,
        ui_renderer: &mut UIRenderer,
        cache: &mut UICacheManager,
        ui: &mut UIRoot,
    ) -> Vec<(usize, usize)> {
        let panel_infos = ui.panel_cache_info();
        let (_, rendered_ranges) =
            cache.render_dirty_panels(device, ui_renderer, &ui.tree, &panel_infos);
        for (start, end) in &rendered_ranges {
            ui.tree.clear_dirty_range(*start, *end);
        }
        rendered_ranges
    }

    /// The footer's own (node_start, node_end) THIS frame, from a fresh
    /// `panel_cache_info()` call — panel ranges can shift across a rebuild,
    /// so this must never be cached across steps.
    fn footer_range(ui: &UIRoot) -> (usize, usize) {
        let infos = ui.panel_cache_info();
        let info = infos.iter().find(|i| i.slot == PanelSlot::Footer).expect("footer panel");
        (info.node_start, info.node_end)
    }

    /// Read back the WHOLE atlas — `render.rs:841`'s `copy_texture_to_buffer`
    /// only supports a (0,0)-anchored copy, no sub-rect origin — and slice
    /// the footer's rows out. Rows are contiguous in the returned buffer, so
    /// this is a plain byte-range slice, not a second GPU copy.
    fn footer_band_bytes(
        device: &GpuDevice,
        cache: &UICacheManager,
        atlas_w: u32,
        atlas_h: u32,
        footer_top: u32,
        footer_h: u32,
    ) -> Vec<u8> {
        let atlas = cache.atlas_texture().expect("atlas texture must exist by frame 0");
        let bytes = super::render::readback(device, atlas, atlas_w, atlas_h);
        let row_bytes = (atlas_w * 4) as usize;
        let top = (footer_top as usize).min(atlas_h as usize);
        let bottom = ((footer_top + footer_h) as usize).min(atlas_h as usize);
        bytes[top * row_bytes..bottom * row_bytes].to_vec()
    }

    /// Save a footer-band byte slice as a PNG. The atlas is `Bgra8Unorm`;
    /// `image::save_buffer` only has an RGBA8 color type, so swap B/R per
    /// pixel — a display-only swizzle, never applied to the bytes the
    /// assertion compares.
    fn save_band_png(bytes: &[u8], w: u32, h: u32, path: &std::path::Path) {
        let mut rgba = bytes.to_vec();
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        image::save_buffer(path, &rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {}: {e}", path.display()));
    }

    /// Resolve `text` (exact node-text match, `type: "Button"`) to its
    /// screen-space center — selector-first, per the brief's decided default
    /// for the tab swap (and reused for the compact-toggle "cog").
    fn resolve_button_center(ui: &UIRoot, text: &str) -> Option<Vec2> {
        let target = AutomationTarget::Query(SelectorQuery {
            text: Some(text.to_string()),
            node_type: Some("Button".to_string()),
            ..Default::default()
        });
        automation::resolve(&ui.tree, &[], &target)
            .ok()
            .map(|r| Vec2::new(r.rect.x + r.rect.width * 0.5, r.rect.y + r.rect.height * 0.5))
    }

    /// Click through the REAL input path (`pointer_event` → `process_events`
    /// → `crate::ui_bridge::dispatch`, the same real bridge call
    /// `script.rs`'s Runner uses) and report whether any dispatched action
    /// was structural — the ONLY signal this driver reads to decide
    /// `needs_structural_sync`, never a hand-set flag. Per-gesture drag/undo
    /// snapshots are scratch-local: this harness only ever clicks, never
    /// drags, so they never carry state between calls.
    #[allow(clippy::too_many_arguments)]
    fn click(
        ui: &mut UIRoot,
        data: &mut fixtures::SceneData,
        pos: Vec2,
        clock: f32,
        active_layer: &mut Option<LayerId>,
        content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
        content_state: &ContentState,
        user_prefs: &mut UserPrefs,
    ) -> bool {
        ui.pointer_event(pos, PointerAction::Down, clock);
        ui.pointer_event(pos, PointerAction::Up, clock);
        let actions = ui.process_events();
        let mut structural = false;
        let mut drag_snapshot: Option<f32> = None;
        let mut trim_snapshot: Option<(f32, f32)> = None;
        let mut target_snapshot: Option<f32> = None;
        let mut decay_snapshot: Option<f32> = None;
        let mut audio_shape_snapshot: Option<manifold_core::audio_mod::AudioModShape> = None;
        let mut audio_action_snapshot: Option<manifold_core::audio_mod::TriggerAction> = None;
        let mut audio_crossover_snapshot: Option<(f32, f32)> = None;
        let mut audio_send_gain_drag_snapshot: Option<f32> = None;
        let mut audio_send_sensitivity_drag_snapshot: Option<
            Vec<manifold_core::audio_trigger::TriggerRoute>,
        > = None;
        let mut active_inspector_drag: Option<crate::app::ActiveInspectorDrag> = None;
        for action in &actions {
            let result = crate::ui_bridge::dispatch(
                action,
                &mut data.project,
                content_tx,
                content_state,
                ui,
                &mut data.selection,
                active_layer,
                &mut drag_snapshot,
                &mut trim_snapshot,
                &mut target_snapshot,
                &mut decay_snapshot,
                &mut audio_shape_snapshot,
                &mut audio_action_snapshot,
                &mut audio_crossover_snapshot,
                &mut audio_send_gain_drag_snapshot,
                &mut audio_send_sensitivity_drag_snapshot,
                user_prefs,
                &mut active_inspector_drag,
                None,
            );
            structural |= result.structural_change;
        }
        structural
    }

    /// Decide+composite+assert-or-rebaseline for one repro-sequence step —
    /// the recipe in design §5, transcribed verbatim: if the footer's own
    /// range was among the panels `render_dirty_panels` actually rendered
    /// this frame, the footer legitimately repainted, so re-baseline;
    /// otherwise the cache says it skipped the footer, so its pixels MUST be
    /// byte-identical to the last baseline.
    #[allow(clippy::too_many_arguments)]
    fn run_step(
        label: &str,
        device: &GpuDevice,
        ui_renderer: &mut UIRenderer,
        cache: &mut UICacheManager,
        ui: &mut UIRoot,
        atlas_w: u32,
        atlas_h: u32,
        footer_top: u32,
        footer_h: u32,
        state: &mut RunState,
    ) {
        apply_decision(ui, cache, &mut state.signals);
        let rendered = composite_frame(device, ui_renderer, cache, ui);
        let fr = footer_range(ui);
        let bytes = footer_band_bytes(device, cache, atlas_w, atlas_h, footer_top, footer_h);
        if rendered.contains(&fr) {
            state.ref_bytes = bytes;
            return;
        }
        state.assert_points += 1;
        if bytes != state.ref_bytes {
            let row_bytes = atlas_w as usize * 4;
            let changed_rows = bytes
                .chunks_exact(row_bytes)
                .zip(state.ref_bytes.chunks_exact(row_bytes))
                .filter(|(a, b)| a != b)
                .count();
            let changed_pixels = bytes
                .chunks_exact(4)
                .zip(state.ref_bytes.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count();
            state.changed_rows_total += changed_rows;
            state.changed_pixels_total += changed_pixels;
            state.last_diff_bytes = Some(bytes);
            eprintln!(
                "cache_path_footer_differential: RED at step '{label}' — \
                 {changed_rows} row(s) / {changed_pixels} px changed vs. last baseline"
            );
        }
    }

    #[test]
    fn cache_path_footer_differential() {
        let mut data = fixtures::build("bug060heavy").expect("bug060heavy scene");
        let mut ui = UIRoot::new();
        ui.resize(LOGICAL_W, LOGICAL_H);
        ui.layout.inspector_width = 600.0;
        ui.layout.timeline_split_ratio = 0.6;
        sync_build(&mut ui, &data, 24.0);

        let device = GpuDevice::new();
        let mut ui_renderer = UIRenderer::new(&device, GpuTextureFormat::Bgra8Unorm);
        // D8: scale factor 1.0 always, at the fixture's logical size — layout
        // is a function of logical size, never shrink the window for speed.
        let mut cache = UICacheManager::new(GpuTextureFormat::Bgra8Unorm, 1.0);
        cache.set_scale_factor(1.0);
        let atlas_w = LOGICAL_W as u32;
        let atlas_h = LOGICAL_H as u32;
        cache.ensure_atlas(&device, atlas_w, atlas_h);
        cache.invalidate_all();

        let footer_rect = ui.layout.footer();
        let footer_top = footer_rect.y.round() as u32;
        let footer_h = footer_rect.height.round() as u32;
        assert!(footer_h > 0, "sanity: footer must have nonzero height or this test proves nothing");

        let out_dir = std::path::PathBuf::from("target/ui-snapshots/bug060heavy/differential");
        std::fs::create_dir_all(&out_dir).expect("create differential output dir");

        // Frame 0: the initial full, self-clearing composite — the baseline
        // every re-baseline in the loop below replaces.
        composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui);
        let mut state = RunState {
            ref_bytes: footer_band_bytes(&device, &cache, atlas_w, atlas_h, footer_top, footer_h),
            ..Default::default()
        };
        save_band_png(&state.ref_bytes, atlas_w, footer_h, &out_dir.join("00_baseline.png"));

        let (content_tx, _content_rx) =
            crossbeam_channel::bounded::<crate::content_command::ContentCommand>(64);
        let content_state = ContentState::default();
        let mut user_prefs = UserPrefs::in_memory();
        let mut active_layer: Option<LayerId> = data
            .active
            .and_then(|i| data.project.timeline.layers.get(i))
            .map(|l| l.layer_id.clone());
        let mut clock = 0.0_f32;
        let mut tab_swap_fallback_used = false;

        // ── scroll to bottom ──
        let insp = ui.layout.inspector();
        let cursor_x = insp.x + insp.width * 0.5;
        assert!(
            ui.try_inspector_scroll(1_000_000.0, cursor_x),
            "sanity: inspector must report a live scroll container"
        );
        if ui.inspector.take_scrolled_in_place() {
            cache.invalidate_inspector();
        }
        run_step("scroll-1", &device, &mut ui_renderer, &mut cache, &mut ui, atlas_w, atlas_h, footer_top, footer_h, &mut state);

        // ── expand/collapse every armed drawer at once (the §6b compact
        // toggle "cog") — a real click, real dispatch, real structural
        // result; the tween it starts is polled below exactly as
        // app_render.rs:2942-2944 does. ──
        let cog_text = manifold_ui::icons::Icon::Cog.text();
        let cog_pos =
            resolve_button_center(&ui, &cog_text).expect("compact-toggle (cog) button must resolve");
        if click(&mut ui, &mut data, cog_pos, clock, &mut active_layer, &content_tx, &content_state, &mut user_prefs) {
            state.signals.needs_structural_sync = true;
        }
        run_step("drawer-toggle-click", &device, &mut ui_renderer, &mut cache, &mut ui, atlas_w, atlas_h, footer_top, footer_h, &mut state);

        // ── poll the drawer tween to settlement. `InspectorCompositePanel::
        // update`'s `tick_drawers` derives dt from `Instant::now()`, not a
        // fixed step (unlike script.rs's driver clock) — a real, small
        // wall-clock sleep between polls is the honest way to advance it;
        // see the report's Shortcuts note. MOTION_MED_MS is 160ms, so ~8
        // ticks at 25ms plus one settle frame comfortably covers it. ──
        for i in 0..16 {
            std::thread::sleep(Duration::from_millis(25));
            clock += 25.0 / 1000.0;
            ui.update();
            let still_active = ui.inspector.drawer_anim_active();
            if still_active {
                state.signals.needs_rebuild = true;
            }
            run_step("drawer-tween-poll", &device, &mut ui_renderer, &mut cache, &mut ui, atlas_w, atlas_h, footer_top, footer_h, &mut state);
            if !still_active && i > 0 {
                break;
            }
        }

        // ── scroll again ──
        assert!(
            ui.try_inspector_scroll(1_000_000.0, cursor_x),
            "sanity: second scroll must still hit the live container"
        );
        if ui.inspector.take_scrolled_in_place() {
            cache.invalidate_inspector();
        }
        run_step("scroll-2", &device, &mut ui_renderer, &mut cache, &mut ui, atlas_w, atlas_h, footer_top, footer_h, &mut state);

        // ── swap tab Layer -> Master -> Layer. Selector-first (orchestrator's
        // decided fork): resolve the tab strip's own "Master"/"Layer" button
        // by text; only on a resolve failure fall back to the inspector's
        // own `configure_tabs` API directly (noted in the report if it
        // fires). ──
        for tab_text in ["Master", "Layer"] {
            match resolve_button_center(&ui, tab_text) {
                Some(pos) => {
                    if click(&mut ui, &mut data, pos, clock, &mut active_layer, &content_tx, &content_state, &mut user_prefs) {
                        state.signals.needs_structural_sync = true;
                    }
                }
                None => {
                    tab_swap_fallback_used = true;
                    let tab = if tab_text == "Master" {
                        manifold_ui::InspectorTab::Master
                    } else {
                        manifold_ui::InspectorTab::Layer
                    };
                    ui.inspector.configure_tabs(
                        &[manifold_ui::InspectorTab::Layer, manifold_ui::InspectorTab::Master],
                        tab,
                    );
                    state.signals.needs_structural_sync = true;
                }
            }
            run_step("tab-swap", &device, &mut ui_renderer, &mut cache, &mut ui, atlas_w, atlas_h, footer_top, footer_h, &mut state);
        }

        // ── final scroll — the last assertable step ──
        assert!(
            ui.try_inspector_scroll(1_000_000.0, cursor_x),
            "sanity: final scroll must still hit the live container"
        );
        if ui.inspector.take_scrolled_in_place() {
            cache.invalidate_inspector();
        }
        run_step("scroll-3", &device, &mut ui_renderer, &mut cache, &mut ui, atlas_w, atlas_h, footer_top, footer_h, &mut state);

        assert!(
            state.assert_points > 0,
            "sanity: at least one step must be assert-eligible (footer not re-baselined) \
             or this test proves nothing"
        );

        let after_bytes = state.last_diff_bytes.clone().unwrap_or_else(|| state.ref_bytes.clone());
        save_band_png(&after_bytes, atlas_w, footer_h, &out_dir.join("01_after.png"));

        println!(
            "cache_path_footer_differential: {} assert-eligible step(s), {} changed row(s), \
             {} changed pixel(s) total across the repro sequence. tab_swap_fallback_used={}. \
             Bands: {}/00_baseline.png, {}/01_after.png",
            state.assert_points,
            state.changed_rows_total,
            state.changed_pixels_total,
            tab_swap_fallback_used,
            out_dir.display(),
            out_dir.display(),
        );

        assert_eq!(
            state.changed_pixels_total, 0,
            "{} footer-band pixel(s) changed across the repro sequence on a frame the cache \
             said it skipped the footer for — the BUG-060 red bracket (D2). If this is 0, re-read \
             the report's escalation-branch note before treating it as a clean pass.",
            state.changed_pixels_total
        );
    }

    // ── Iteration 2: audio-modulation-driven INCREMENTAL-path repro ──────────
    //
    // The clue that reframed the hunt: BUG-060 repros even while the transport
    // is PAUSED. That rules out an LFO/transport-time driver (a paused LFO is
    // frozen) and points at AUDIO MODULATION — live audio keeps flowing into
    // the analyzer with the transport stopped, so an audio-modulated slider
    // keeps re-writing its effective value (and thus its slider node) every
    // frame regardless of play state.
    //
    // The first test's gestures each forced a FULL repaint (scroll →
    // invalidate_inspector sets panel_valid=false; drawer/tab → invalidate_all),
    // so it never entered the incremental sub-region Load path at
    // ui_cache_manager.rs:213 — the exact path BUG-060 is suspected to live in.
    // That path only fires when panel_valid[Inspector] stays TRUE and a single
    // card's sub-region goes dirty. This test reproduces exactly that: after an
    // initial full render, it mutates the armed effects' param VALUES and
    // re-pushes them in place (`push_state`, which never rebuilds or
    // invalidates), so one-or-few card sub-regions dirty while the panel stays
    // valid — and it classifies the cache path each frame from the same public
    // predicates render_dirty_panels branches on, so we can SEE whether the
    // incremental path was actually entered.

    /// The path `render_dirty_panels` takes for the Inspector this frame,
    /// computed from the SAME public predicates the cache branches on
    /// (`has_dirty_in_range`, ui_cache_manager.rs:202; `has_dirty_outside_ranges`,
    /// :213/:318), read on the real tree at the instant the cache reads them.
    /// `panel_valid[Inspector]` is KNOWN true throughout the loop (established by
    /// the initial full render, never invalidated after), and `extents_unchanged`
    /// holds because nothing rebuilds (a card's first-node frame never moves on an
    /// in-place value write) — so the classification is faithful observation of
    /// the real branch inputs, not derivation from a snippet. (One residual the
    /// private guard also checks, dirty-sub-region count ≤ INCREMENTAL_THRESHOLD
    /// = 16: this loop dirties ≤ 6 cards, well under it.)
    #[derive(Debug, PartialEq, Clone, Copy)]
    enum CachePath {
        /// Panel skipped entirely — valid, no dirty in range (line 202).
        Skipped,
        /// Incremental sub-region Load — valid, dirt only inside sub-regions (line 213).
        Incremental,
        /// Full self-clearing render — dirt outside sub-regions forced the fallback.
        Full,
    }

    fn classify_inspector_path(ui: &UIRoot) -> CachePath {
        let infos = ui.panel_cache_info();
        let info = infos.iter().find(|i| i.slot == PanelSlot::Inspector).expect("inspector panel");
        let (start, end) = (info.node_start, info.node_end);
        let subs = info.sub_regions.clone().unwrap_or_default();
        if !ui.tree.has_dirty_in_range(start, end) {
            CachePath::Skipped
        } else if ui.tree.has_dirty_outside_ranges(start, end, &subs) {
            CachePath::Full
        } else {
            CachePath::Incremental
        }
    }

    /// Non-vacuousness guard: is there a sub-region that is BOTH dirty (an
    /// incremental Load will repaint it this frame) AND whose rendered extent
    /// straddles `footer_top` (some node in its range crosses the footer's top
    /// edge)? `render_sub_region` repaints EVERY node in a dirty sub-region's
    /// range, so a node straddling footer_top is exactly the content whose Load
    /// could leak into the footer band. If no dirty sub-region reaches the
    /// footer edge, a byte-stable footer proves nothing about the clip — the
    /// test would be vacuous. Raw (pre-clip) bounds on purpose: the whole
    /// question is whether the clip holds, so there must be something to clip.
    fn dirty_subregion_reaches_footer(
        ui: &UIRoot,
        sub_regions: &[(usize, usize)],
        footer_top: f32,
    ) -> bool {
        let n = ui.tree.nodes().len();
        sub_regions.iter().any(|&(s, e)| {
            let range = s..e.min(n);
            let dirty = range
                .clone()
                .any(|i| ui.tree.nodes()[i].flags.contains(manifold_ui::UIFlags::DIRTY));
            let straddles = range.clone().any(|i| {
                let b = &ui.tree.nodes()[i].bounds;
                b.width > 0.0 && b.height > 0.0 && b.y < footer_top && b.y_max() > footer_top
            });
            dirty && straddles
        })
    }

    #[test]
    fn incremental_path_modulation_differential() {
        let mut data = fixtures::build("bug060heavy").expect("bug060heavy scene");
        let mut ui = UIRoot::new();
        ui.resize(LOGICAL_W, LOGICAL_H);
        ui.layout.inspector_width = 600.0;
        ui.layout.timeline_split_ratio = 0.6;
        sync_build(&mut ui, &data, 24.0);

        let device = GpuDevice::new();
        let mut ui_renderer = UIRenderer::new(&device, GpuTextureFormat::Bgra8Unorm);
        let mut cache = UICacheManager::new(GpuTextureFormat::Bgra8Unorm, 1.0);
        cache.set_scale_factor(1.0);
        let atlas_w = LOGICAL_W as u32;
        let atlas_h = LOGICAL_H as u32;
        cache.ensure_atlas(&device, atlas_w, atlas_h);
        cache.invalidate_all();

        let footer_rect = ui.layout.footer();
        let footer_top = footer_rect.y.round() as u32;
        let footer_h = footer_rect.height.round() as u32;
        assert!(footer_h > 0, "sanity: footer must have nonzero height");

        let out_dir = std::path::PathBuf::from("target/ui-snapshots/bug060heavy/modulation");
        std::fs::create_dir_all(&out_dir).expect("create modulation output dir");

        // Frame 0: full self-clearing render (unscrolled).
        composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui);

        // Scroll to bottom so the last heavy cards straddle the footer band —
        // the geometric precondition for a modulated card's incremental Load to
        // reach the footer at all (unscrolled, every card sits well above
        // footer_top, so no modulation there could ever touch the footer).
        // This scroll is a ONE-TIME positioning step, NOT part of the measured
        // loop: it invalidates the inspector, the next composite does a FULL
        // repaint (panel_valid[Inspector] back to true, last_sub_regions recorded
        // at the scrolled positions), and only THEN does the pure-modulation loop
        // begin — every loop frame keeps the panel valid and never scrolls again.
        // Deviation from the coordinator's step-1 (which omitted the scroll),
        // made deliberately: without it the footer overlap is geometrically
        // impossible and a no-scroll loop would trivially read 0, proving nothing.
        let insp = ui.layout.inspector();
        let cursor_x = insp.x + insp.width * 0.5;
        assert!(
            ui.try_inspector_scroll(1_000_000.0, cursor_x),
            "sanity: inspector must report a live scroll container"
        );
        if ui.inspector.take_scrolled_in_place() {
            cache.invalidate_inspector();
        }
        composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui);

        // REF after the scrolled full repaint — the footer legitimately painted
        // here; every subsequent skipped-footer frame is compared against it.
        let mut ref_bytes =
            footer_band_bytes(&device, &cache, atlas_w, atlas_h, footer_top, footer_h);
        save_band_png(&ref_bytes, atlas_w, footer_h, &out_dir.join("00_baseline.png"));

        let mut tcache = TransportDisplayCache::new();
        let mut path_log: Vec<CachePath> = Vec::new();
        let mut assert_points = 0usize;
        let mut changed_rows_total = 0usize;
        let mut changed_pixels_total = 0usize;
        let mut last_diff: Option<Vec<u8>> = None;
        let mut incremental_frames = 0usize;
        let mut straddle_frames = 0usize;

        for frame in 0..24usize {
            // Audio modulation writes a new effective value every frame even
            // while paused (the clue). Emulate by writing EVERY effect's first
            // param `.value` to a frame-varying, in-range number — distinct every
            // frame, or update_value no-ops and the node never dirties. Modulating
            // all cards (not a subset) guarantees the footer-straddling card at
            // the bottom of the scroll is among the dirtied ones, and matches the
            // "heavy modulation" repro (many sliders redrawing at once). Cards
            // whose first param is a toggle/trigger simply don't dirty a slider —
            // harmless.
            {
                let layer = &mut data.project.timeline.layers[0];
                if let Some(effects) = layer.effects.as_mut() {
                    for (idx, fx) in effects.iter_mut().enumerate() {
                        if let Some(p) = fx.params.iter_mut().next() {
                            let span = (p.spec.max - p.spec.min).max(1e-3);
                            let t = ((frame * 7 + idx * 13) % 17) as f32 / 17.0;
                            p.value = p.spec.min + span * t;
                        }
                    }
                }
            }

            // In-place value sync: push_state mutates slider nodes only — no
            // build, no invalidate — so panel_valid[Inspector] stays true and
            // only the armed cards' sub-regions go dirty.
            push_state(
                &mut ui,
                &data.project,
                &data.content,
                data.active,
                &data.selection,
                false,
                None,
                &mut tcache,
            );

            // Non-vacuousness: did a dirtied card actually straddle the footer
            // this frame, giving the incremental Load a chance to leak into it?
            {
                let infos = ui.panel_cache_info();
                if let Some(info) = infos.iter().find(|i| i.slot == PanelSlot::Inspector) {
                    let subs = info.sub_regions.clone().unwrap_or_default();
                    if dirty_subregion_reaches_footer(&ui, &subs, footer_top as f32) {
                        straddle_frames += 1;
                    }
                }
            }

            // Classify the path the cache is about to take, from the real tree.
            let path = classify_inspector_path(&ui);
            if path == CachePath::Incremental {
                incremental_frames += 1;
            }
            path_log.push(path);

            // Real render_dirty_panels + dirty-flag clear (as present_all_windows).
            let rendered = composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui);
            let fr = footer_range(&ui);
            let bytes = footer_band_bytes(&device, &cache, atlas_w, atlas_h, footer_top, footer_h);
            if rendered.contains(&fr) {
                ref_bytes = bytes; // footer legitimately repainted → re-baseline
                continue;
            }
            assert_points += 1;
            if bytes != ref_bytes {
                let row_bytes = atlas_w as usize * 4;
                let rows = bytes
                    .chunks_exact(row_bytes)
                    .zip(ref_bytes.chunks_exact(row_bytes))
                    .filter(|(a, b)| a != b)
                    .count();
                let px = bytes
                    .chunks_exact(4)
                    .zip(ref_bytes.chunks_exact(4))
                    .filter(|(a, b)| a != b)
                    .count();
                changed_rows_total += rows;
                changed_pixels_total += px;
                last_diff = Some(bytes);
                eprintln!(
                    "incremental_path_modulation_differential: RED at frame {frame} (path {path:?}) — \
                     {rows} row(s) / {px} px changed vs. last baseline"
                );
            }
        }

        let after = last_diff.clone().unwrap_or_else(|| ref_bytes.clone());
        save_band_png(&after, atlas_w, footer_h, &out_dir.join("01_after.png"));

        println!("incremental_path_modulation_differential: per-frame inspector cache path:");
        for (i, p) in path_log.iter().enumerate() {
            println!("  frame {i:>2}: {p:?}");
        }
        let verdict = if incremental_frames == 0 {
            "WALL — modulation never entered the incremental path headlessly"
        } else if changed_pixels_total == 0 {
            "CLEAN — incremental path entered, footer byte-stable (no BUG-060 via this mechanism)"
        } else {
            "RED — incremental path entered AND footer went stale (BUG-060 bracket)"
        };
        println!(
            "  → verdict: {verdict}. {incremental_frames}/{} frame(s) incremental, \
             {straddle_frames}/{} with a dirtied card straddling footer_top, \
             {assert_points} assert-eligible, {changed_rows_total} changed row(s), \
             {changed_pixels_total} changed pixel(s). Bands: {}/00_baseline.png, {}/01_after.png",
            path_log.len(),
            path_log.len(),
            out_dir.display(),
            out_dir.display(),
        );

        // Non-vacuousness: at least one measured frame must have had a DIRTIED
        // card straddling footer_top — otherwise the incremental Load never had
        // a chance to touch the footer and a byte-stable result proves nothing
        // about the clip. If this fails, the finding is "the fixture/scroll
        // didn't put a modulated card at the footer edge", not "footer clean".
        assert!(
            straddle_frames > 0,
            "vacuous: no measured frame had a dirtied card straddling footer_top ({footer_top}) — \
             the scroll didn't position a modulated card at the footer edge, so the CLEAN result \
             would be meaningless. Fix the scroll/fixture before trusting a byte-stable footer."
        );

        // The footer must stay byte-identical on every frame the cache skipped
        // it — a diff there is the BUG-060 RED bracket (D2). The per-frame path
        // log above answers the diagnostic question (did modulation enter the
        // incremental path); this asserts the reliability invariant. A WALL
        // outcome (incremental_frames == 0) is reported in the log/verdict, not
        // forced into a diff.
        assert_eq!(
            changed_pixels_total, 0,
            "{} footer-band pixel(s) changed across the modulation loop on frames the cache \
             skipped the footer — the BUG-060 RED bracket. Read the band PNGs to confirm the \
             differing pixels are stale UI chrome.",
            changed_pixels_total
        );
    }
}
