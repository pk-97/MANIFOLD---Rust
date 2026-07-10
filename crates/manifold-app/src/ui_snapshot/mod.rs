//! Headless UI snapshot harness (feature `ui-snapshot`). An agent-facing tool
//! to render MANIFOLD's REAL UI tree to a PNG plus a machine-readable tree dump,
//! with no winit window — so UI/UX work is see-able, measurable, and provable.
//!
//! Invoked via the `cargo xtask` alias:
//!   cargo xtask ui-snap <scene> [--dump] [--interact "select:<layer>"]
//! See `docs/HEADLESS_UI_HARNESS.md`.

mod compare;
mod composite_resources;
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
        &mut ui,
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
            &mut ui,
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

/// The data-sync half of `sync_build` (P2 split, `UI_HARNESS_UNIFICATION_
/// DESIGN.md` D3): re-derive `ui`'s selection/inspector/zoom fields from
/// `data`. Deliberately does NOT rebuild the tree — every non-script caller
/// still does that unconditionally right after (see `sync_build` below);
/// `script.rs`'s `Runner` instead gates it through
/// `crate::ui_frame::apply_ui_frame_invalidations`, matching the live App's
/// own gated rebuild instead of `sync_build`'s old unconditional `ui.build()`.
/// `zoom_ppb` is the scene's pixels-per-beat (24.0 for the 48-beat fixtures;
/// `render_ui_scene` overrides it per scene name — see the `hairlineclips`
/// far-zoom case).
fn sync_data(ui: &mut UIRoot, data: &fixtures::SceneData, zoom_ppb: f32) {
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
}

/// The push-state/reconcile tail half of `sync_build` (P2 split) — pushes
/// display-only setters and reconciles them into the (already built) tree.
/// Unconditional regardless of whether a rebuild happened this pass: mirrors
/// the live app's per-frame call (`app_render.rs`'s "6. Lightweight update"
/// after its own `push_state`). Every panel's `set_*` methods are "store
/// only; the reconcile applies them" (see `TransportPanel`'s doc comment);
/// without this the harness only ever showed each panel's `::new()`
/// hardcoded defaults, silently — every existing scene's fixture
/// `ContentState` happened to already match those defaults (paused, not
/// recording, no BPM reset/clear pending), so the gap never surfaced until
/// the `automation` scene (P4a) deliberately diverged (armed + a latch).
fn reconcile_state(ui: &mut UIRoot, data: &fixtures::SceneData) {
    let mut tcache = TransportDisplayCache::new();
    push_state(ui, &data.project, &data.content, data.active, &data.selection, false, None, &mut tcache);
    ui.update();
}

/// The real translation path: structural sync → zoom-to-fit → build → push
/// state. Unconditional full build — every caller except `script.rs`'s
/// `Runner` (P2), which gates the build decision through
/// `crate::ui_frame::apply_ui_frame_invalidations` instead; see `sync_data`/
/// `reconcile_state` above.
fn sync_build(ui: &mut UIRoot, data: &fixtures::SceneData, zoom_ppb: f32) {
    sync_data(ui, data, zoom_ppb);
    ui.build();
    reconcile_state(ui, data);
}

/// Render to `<scene><suffix>.png`, and (if requested) the tree dump as JSON +
/// a terse stdout summary.
fn render_and_dump(
    ui: &mut UIRoot,
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
mod cache_path_full_render {
    //! P0+P1 (`docs/UI_HARNESS_UNIFICATION_DESIGN.md` — read the "Reframe
    //! 2026-07-10" block before this comment) — a faithful FULL-APP headless
    //! render of the main window. Unlike `footer_leak_probe` (CPU bounds
    //! walk, zero pixels) and the sibling `render` module's
    //! whole-tree-fresh-every-frame harness (structurally blind to a
    //! stale-atlas-pixel bug — see that module's own doc comment), this test
    //! builds a REAL `UICacheManager` + `UIRenderer` + atlas texture and
    //! composites through `crate::ui_frame::composite_main_ui_frame` (P1,
    //! D3) — the IDENTICAL function `present_all_windows` calls — into a
    //! real offscreen texture, then reads that back and saves it as a PNG,
    //! plus a filmstrip contact sheet of the inspector-drawer tween (D9a).
    //! The invalidation decision is likewise `crate::ui_frame::apply_ui_
    //! frame_invalidations` (P1, D3), not a hand transcription: the app and
    //! this harness now run the same code for both halves of the seam. This
    //! driver never drags, so the drag-guard branches inside that function
    //! never fire here — correctly inert, not omitted.
    //!
    //! D8: scale factor one at the fixture's logical size — layout is
    //! pixel-exact, and the raster is far cheaper than a Retina pass.
    //! `video: None` always (D8 gap #2 — no compositor output here).
    //!
    //! The BUG-060 differential/red-bracket model this module used to run is
    //! RETIRED (Reframe 2026-07-10, D2/D4a) — BUG-060 was root-caused and
    //! closed independently of this harness. There is no baseline, no
    //! byte-equality assertion, and no red/green bracket here. The only
    //! automated check is a smoke test (drew something, not blank); the real
    //! verification is a human/agent reading the saved PNGs.

    use std::time::Duration;

    use manifold_core::LayerId;
    use manifold_gpu::{GpuDevice, GpuTextureFormat};
    use manifold_renderer::ui_cache_manager::UICacheManager;
    use manifold_renderer::ui_renderer::UIRenderer;
    use manifold_ui::automation::{self, AutomationTarget, SelectorQuery};
    use manifold_ui::input::PointerAction;
    use manifold_ui::node::Vec2;

    use super::*;
    use super::composite_resources::{composite_frame as seam_composite_frame, CompositeResources};
    use crate::content_state::ContentState;
    use crate::ui_frame::{apply_ui_frame_invalidations, UiFrameSignals};
    use crate::user_prefs::UserPrefs;

    /// P1 (D3): the invalidation decision and the atlas/offscreen composite
    /// now live in `crate::ui_frame` — the app and this harness call the
    /// IDENTICAL functions (`apply_ui_frame_invalidations`,
    /// `composite_main_ui_frame`), which is the structural faithfulness
    /// proof the design's Reframe (2026-07-10) replaces the red-bracket
    /// model with. `Signals`/`apply_decision`/`composite_frame` (P0's own
    /// transcriptions of app_render.rs) are gone — see the P1 phase report.
    ///
    /// `scroll_dirty` is always `ScrollDirty::default()` here: this driver
    /// scrolls via `try_inspector_scroll` + `scrolled_in_place`, never the
    /// live app's `scroll_dirty` bitflag path, exactly as P0's `apply_
    /// decision` never called `rebuild_scroll_panels` either — omitting it
    /// changes nothing observable.
    fn apply_decision(ui: &mut UIRoot, cache: &mut UICacheManager, signals: &mut UiFrameSignals) {
        apply_ui_frame_invalidations(ui, Some(cache), signals);
    }

    // `CompositeResources` (the offscreen target + atlas/blit pipelines
    // `composite_main_ui_frame` needs) now lives in `composite_resources.rs`
    // (P2, `UI_HARNESS_UNIFICATION_DESIGN.md` D3) — shared with `render.rs`'s
    // `render_ui_to_png`, which needed the identical resources. Was a private
    // copy duplicated in this test module; see that file's module doc.

    /// D8: scale factor 1.0 always in this test — see
    /// `composite_resources::composite_frame`'s `scale_factor` param.
    fn composite_frame(
        device: &GpuDevice,
        ui_renderer: &mut UIRenderer,
        cache: &mut UICacheManager,
        ui: &mut UIRoot,
        res: &CompositeResources,
    ) {
        seam_composite_frame(device, ui_renderer, cache, ui, res, 1.0);
    }

    /// Read back the composited offscreen — the real main-window frame,
    /// through the identical function `present_all_windows` calls (P1, D3).
    fn full_frame_bytes(device: &GpuDevice, res: &CompositeResources, w: u32, h: u32) -> Vec<u8> {
        super::render::readback(device, &res.offscreen, w, h)
    }

    // `save_bgra_png`/`save_filmstrip_png` now live in `render.rs` (P2,
    // `UI_HARNESS_UNIFICATION_DESIGN.md` D3) — shared with `script.rs`'s
    // `Runner`, which needed the identical helpers for its own filmstrip/
    // snapshot artifacts. Were private copies duplicated in this test
    // module; see `render.rs`'s doc comments on both.
    use super::render::{save_bgra_png, save_filmstrip_png};

    /// The only automated assertion in this module (per the Reframe): the
    /// render drew *something* — not empty, not a single flat colour
    /// end-to-end.
    fn assert_not_blank(bgra: &[u8], label: &str) {
        assert!(!bgra.is_empty(), "{label}: readback is empty");
        let first = &bgra[0..4];
        let all_same = bgra.chunks_exact(4).all(|px| px == first);
        assert!(!all_same, "{label}: readback is a uniform single colour — drew nothing");
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

    /// Click through the REAL input path (`pointer_event` -> `process_events`
    /// -> `crate::ui_bridge::dispatch`, the same real bridge call
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

    #[test]
    fn cache_path_full_render() {
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
        let res = CompositeResources::new(&device, atlas_w, atlas_h);

        let out_dir = std::path::PathBuf::from("target/ui-snapshots/bug060heavy/full_render");
        std::fs::create_dir_all(&out_dir).expect("create full_render output dir");

        let mut signals = UiFrameSignals::default();

        // Frame 0: initial full, self-clearing composite.
        composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui, &res);

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
        let mut filmstrip: Vec<Vec<u8>> = Vec::new();

        // ── scroll to bottom ──
        let insp = ui.layout.inspector();
        let cursor_x = insp.x + insp.width * 0.5;
        assert!(
            ui.try_inspector_scroll(1_000_000.0, cursor_x),
            "sanity: inspector must report a live scroll container"
        );
        signals.scrolled_in_place = ui.inspector.take_scrolled_in_place();
        apply_decision(&mut ui, &mut cache, &mut signals);
        composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui, &res);

        // ── expand/collapse every armed drawer at once (the compact toggle
        // "cog") — a real click, real dispatch, real structural result; the
        // tween it starts is polled below exactly as app_render.rs:2942-2944
        // does. The click's own post-composite frame opens the filmstrip. ──
        let cog_text = manifold_ui::icons::Icon::Cog.text();
        let cog_pos =
            resolve_button_center(&ui, &cog_text).expect("compact-toggle (cog) button must resolve");
        if click(&mut ui, &mut data, cog_pos, clock, &mut active_layer, &content_tx, &content_state, &mut user_prefs) {
            signals.needs_structural_sync = true;
        }
        apply_decision(&mut ui, &mut cache, &mut signals);
        composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui, &res);
        filmstrip.push(full_frame_bytes(&device, &res, atlas_w, atlas_h));

        // ── poll the drawer tween to settlement, capturing a filmstrip tile
        // after every stepped frame (D9a). `InspectorCompositePanel::update`'s
        // `tick_drawers` derives dt from `Instant::now()`, not a fixed step
        // (unlike script.rs's driver clock) — a real, small wall-clock sleep
        // between polls is the honest way to advance it; see the report's
        // Shortcuts note. MOTION_MED_MS is 160ms, so ~8 ticks at 25ms plus
        // one settle frame comfortably covers it. ──
        for i in 0..16 {
            std::thread::sleep(Duration::from_millis(25));
            clock += 25.0 / 1000.0;
            ui.update();
            let still_active = ui.inspector.drawer_anim_active();
            if still_active {
                signals.needs_rebuild = true;
            }
            apply_decision(&mut ui, &mut cache, &mut signals);
            composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui, &res);
            filmstrip.push(full_frame_bytes(&device, &res, atlas_w, atlas_h));
            if !still_active && i > 0 {
                break;
            }
        }

        // ── scroll again ──
        assert!(
            ui.try_inspector_scroll(1_000_000.0, cursor_x),
            "sanity: second scroll must still hit the live container"
        );
        signals.scrolled_in_place = ui.inspector.take_scrolled_in_place();
        apply_decision(&mut ui, &mut cache, &mut signals);
        composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui, &res);

        // ── swap tab Layer -> Master -> Layer. Selector-first (orchestrator's
        // decided fork): resolve the tab strip's own "Master"/"Layer" button
        // by text; only on a resolve failure fall back to the inspector's
        // own `configure_tabs` API directly (noted in the report if it
        // fires). ──
        for tab_text in ["Master", "Layer"] {
            match resolve_button_center(&ui, tab_text) {
                Some(pos) => {
                    if click(&mut ui, &mut data, pos, clock, &mut active_layer, &content_tx, &content_state, &mut user_prefs) {
                        signals.needs_structural_sync = true;
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
                    signals.needs_structural_sync = true;
                }
            }
            apply_decision(&mut ui, &mut cache, &mut signals);
            composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui, &res);
        }

        // ── final scroll — the last step of the sequence ──
        assert!(
            ui.try_inspector_scroll(1_000_000.0, cursor_x),
            "sanity: final scroll must still hit the live container"
        );
        signals.scrolled_in_place = ui.inspector.take_scrolled_in_place();
        apply_decision(&mut ui, &mut cache, &mut signals);
        composite_frame(&device, &mut ui_renderer, &mut cache, &mut ui, &res);

        // ── save the full-app PNG (the composited main window frame —
        // through `composite_main_ui_frame`, the same function
        // `present_all_windows` calls; P1, D3) ──
        let final_bytes = full_frame_bytes(&device, &res, atlas_w, atlas_h);
        assert_not_blank(&final_bytes, "full-app render");
        let frame_png = out_dir.join("frame.png");
        save_bgra_png(&final_bytes, atlas_w, atlas_h, &frame_png);

        // ── assemble the drawer-tween filmstrip into one contact sheet ──
        let filmstrip_png = out_dir.join("drawer_filmstrip.png");
        let cols = (filmstrip.len() as u32).clamp(1, 4);
        save_filmstrip_png(&filmstrip, atlas_w, atlas_h, cols, &filmstrip_png);

        println!(
            "cache_path_full_render: wrote {} and {} ({} tile(s) in the drawer-tween filmstrip). \
             tab_swap_fallback_used={}",
            frame_png.display(),
            filmstrip_png.display(),
            filmstrip.len(),
            tab_swap_fallback_used,
        );
    }
}

#[cfg(test)]
mod editor_window_harness {
    //! P3 (`docs/UI_HARNESS_UNIFICATION_DESIGN.md`) — the graph-editor
    //! window's OWN structural invariant. Per D5 the editor is cacheless
    //! immediate-mode (no `UICacheManager`, never the atlas differential);
    //! its invariant is geometric, not pixel-staleness: a node the fixture
    //! places must actually paint at the screen rect the canvas itself
    //! reports for it.
    //!
    //! Builds the real `fixtures::generator_editor_fixture` + the SAME merged
    //! `UIRoot` topology `render_graph_editor_to_png` builds (sidebar preview
    //! column + inspector in ONE tree — P3's topology fix), renders through
    //! `crate::editor_frame::composite_editor_frame` — the IDENTICAL function
    //! `present_graph_editor_window` calls — then reads back the node's
    //! declared screen rect from `GraphCanvasTargets` (the same hit-target
    //! enumeration `render_graph_editor_to_png`'s `--dump` uses) and samples
    //! its center pixel against the window's clear color. A node whose
    //! declared rect doesn't actually paint — wrong topology, a dropped
    //! render pass, a camera/viewport mismatch — fails this test; the atlas
    //! differential (D4/D5) is deliberately not asked here, because there is
    //! no cache path to have a stale pixel in.

    use manifold_gpu::{GpuDevice, GpuTextureFormat};
    use manifold_renderer::render_target::RenderTarget;
    use manifold_renderer::ui_renderer::UIRenderer;
    use manifold_ui::graph_canvas::{GraphCanvas, GraphCanvasTargets, Rect as CanvasRect};
    use manifold_ui::hit_targets::HitTargets;
    use manifold_ui::panels::graph_editor::{
        GraphEditorPanel, EDITOR_CARD_LANE_WIDTH, SIDEBAR_WIDTH,
    };
    use manifold_ui::Rect as UiRect;

    use super::*;
    use crate::editor_frame::{
        build_editor_preview_column, composite_editor_frame, EditorMiniTimelineInputs,
    };

    const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

    #[test]
    fn node_the_fixture_places_renders_at_its_declared_screen_rect() {
        let preset = "FluidSim2D";
        let pid = manifold_core::PresetTypeId::from_string(preset.to_string());
        let view = manifold_renderer::node_graph::loaded_preset_view_by_id(&pid)
            .expect("FluidSim2D preset must be loadable");
        let rg_snap = manifold_renderer::node_graph::snapshot_for_view(view)
            .expect("FluidSim2D snapshot must materialize");
        let (project, target, selection) = fixtures::generator_editor_fixture(preset)
            .expect("FluidSim2D is a generator preset");
        let gv_snap = crate::ui_translate::graph_snapshot_to_ui(&rg_snap);

        let tex_w = LOGICAL_W as u32;
        let tex_h = LOGICAL_H as u32;
        let logical_w = LOGICAL_W;
        let logical_h = LOGICAL_H;

        // Same geometry `render_graph_editor_to_png` computes.
        let dock = manifold_ui::Dock::editor();
        let dock_rects = dock.rects(UiRect::new(0.0, 0.0, logical_w, logical_h));
        let canvas_x = SIDEBAR_WIDTH;
        let canvas_width = (logical_w - SIDEBAR_WIDTH - EDITOR_CARD_LANE_WIDTH).max(0.0);
        let canvas_height = dock_rects.canvas.height;
        let card_x = canvas_x + canvas_width;

        let device = GpuDevice::new();
        let mut renderer = UIRenderer::new(&device, FORMAT);
        let target_tex = RenderTarget::new(&device, tex_w, tex_h, FORMAT, "ui-snap-editor-harness");

        // ONE merged `UIRoot` — sidebar preview column + inspector lane —
        // exactly the topology `present_graph_editor_window` and (post-P3)
        // `render_graph_editor_to_png` both build.
        let mut ui_root = UIRoot::new();
        let active_idx = match &target {
            manifold_core::GraphTarget::Generator(lid) => {
                project.timeline.layers.iter().position(|l| &l.layer_id == lid)
            }
            manifold_core::GraphTarget::Effect(_) => None,
        };
        sync_project_data(&mut ui_root, &project, active_idx, &selection);
        sync_inspector_data(&mut ui_root, &project, active_idx, &selection, &[]);
        ui_root.build_inspector_in_rect(UiRect::new(
            card_x,
            0.0,
            EDITOR_CARD_LANE_WIDTH,
            canvas_height,
        ));

        let preview_pad = 8.0_f32;
        let preview_title_h = 18.0_f32;
        let monitor_aspect = 16.0_f32 / 9.0;
        let avail_w = (SIDEBAR_WIDTH - 2.0 * preview_pad).max(1.0);
        let max_body_h =
            ((canvas_height - 3.0 * preview_pad - 2.0 * preview_title_h) * 0.5).max(1.0);
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
        build_editor_preview_column(
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
            false,
        );

        let viewport = CanvasRect::new(canvas_x, 0.0, canvas_width, canvas_height);
        let mut canvas = GraphCanvas::new();
        canvas.set_default_expanded(true);
        canvas.set_snapshot(&gv_snap);
        canvas.apply_pending_fit(viewport);

        // The expected screen rect for a real node the fixture placed — the
        // SAME hit-target enumeration `render_graph_editor_to_png`'s `--dump`
        // uses (`custom_surfaces` / `"graph_canvas"`), read BEFORE the render
        // call so this assertion is against the canvas's own declared
        // geometry, not a value derived from the pixels it's checking.
        let targets_surface = GraphCanvasTargets { canvas: &canvas, viewport };
        let mut entries = Vec::new();
        targets_surface.enumerate(&mut entries);
        let node_entry = entries
            .iter()
            .find(|e| e.kind == "node")
            .expect("FluidSim2D graph must have at least one node");
        let (ex, ey, ew, eh) = (
            node_entry.rect.x,
            node_entry.rect.y,
            node_entry.rect.width,
            node_entry.rect.height,
        );

        let editor_area = UiRect::new(0.0, 0.0, logical_w, logical_h);
        let (mini_clips, mini_layer_labels, mini_rows, mini_total, mini_bpb, mini_readout) =
            crate::app_render::mini_timeline_data(&project, 0.0);
        let mut popover = manifold_ui::graph_canvas::mapping_popover::MappingPopover::new();
        let text_input = crate::text_input::TextInputState::new();
        let frame_timer = crate::frame_timer::FrameTimer::new(60.0);

        composite_editor_frame(
            &device,
            Some(&mut renderer),
            &ui_root,
            &dock,
            editor_area,
            Some(&canvas),
            viewport,
            EditorMiniTimelineInputs {
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
            1.0,
        );

        let bytes = super::render::readback(&device, &target_tex.texture, tex_w, tex_h);

        // Structural check, self-contained (no hardcoded clear color, no
        // external background reference — this dark theme's canvas grid and
        // a node's mostly-empty body fill land at nearly the SAME near-black
        // shade, so a single center-pixel-vs-clear-color check is unreliable
        // here; verified against the saved PNG before writing this). A node
        // that genuinely rendered — header text, param-row labels, port
        // dots, border — has RICH internal color variety; empty canvas space
        // of the same size is perfectly flat (exactly one distinct color).
        // Count distinct colors inside the node's declared rect and assert
        // it's well above "flat" — proof the node the canvas says is at
        // (ex,ey,ew,eh) actually painted structure there, not that the rect
        // is empty canvas the topology mis-declared as a node.
        let x0 = ex.max(0.0) as u32;
        let y0 = ey.max(0.0) as u32;
        let x1 = (ex + ew).min(tex_w as f32) as u32;
        let y1 = (ey + eh).min(tex_h as f32) as u32;
        assert!(
            x1 > x0 && y1 > y0,
            "node '{}' declared rect ({ex},{ey},{ew}x{eh}) is off the {tex_w}x{tex_h} canvas",
            node_entry.label,
        );
        let mut distinct: std::collections::HashSet<[u8; 3]> = std::collections::HashSet::new();
        for y in y0..y1 {
            for x in x0..x1 {
                let idx = ((y * tex_w + x) * 4) as usize;
                distinct.insert([bytes[idx], bytes[idx + 1], bytes[idx + 2]]);
            }
        }
        assert!(
            distinct.len() > 20,
            "node '{}' at declared rect ({ex},{ey},{ew}x{eh}) is flat ({} distinct color(s)) — \
             the node did not paint where the canvas says it is",
            node_entry.label,
            distinct.len(),
        );
    }
}

#[cfg(test)]
mod overlay_fidelity_proof {
    //! BUG-097 — the permanent RED→GREEN regression proof for the overlay
    //! pass, closed by construction in
    //! `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` §4 step 2.
    //!
    //! `UIRoot::build_overlays` mints each open overlay as its own region and
    //! records `(start, end)` with `start = tree.count()` taken AFTER
    //! `begin_region` — so the region's own root sits at `start - 1`,
    //! deliberately OUTSIDE the recorded range (the shadow-peek heuristic in
    //! the overlay pass depends on `start` being the first REAL node). That
    //! makes the render-call choice load-bearing:
    //!
    //! - `render_tree_range(start, end)` is a ROOT scan (`traverse_range`): it
    //!   finds no region root inside `[start, end)` and renders NOTHING — the
    //!   old harness's bug (BUG-097). Every open overlay — dropdown, modal,
    //!   perf HUD — would render blank in a harness PNG.
    //! - `render_sub_region(start, end, false)` is an ancestor-aware FLAT scan
    //!   (`traverse_flat_range`): it walks the parent chain from `start` and
    //!   picks up the region's `CLIPS_CHILDREN` regardless — it DRAWS the
    //!   overlay. This is the call `render_main_ui_passes` (the shared seam)
    //!   makes; there is no longer a second, divergent copy to pick the wrong
    //!   one.
    //!
    //! The test proves both halves on the SAME real overlay range, comparing
    //! one offscreen before/after each render call (never two separate
    //! composites — that sidesteps the ~172px of font/GPU nondeterminism a
    //! fresh composite carries). Keep the GREEN assertion: reverting the seam
    //! to `render_tree_range` makes `sub_region_drew` false and this fails.

    use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTextureFormat};
    use manifold_renderer::ui_cache_manager::UICacheManager;
    use manifold_renderer::ui_renderer::UIRenderer;

    use super::composite_resources::{composite_frame, CompositeResources};
    use super::render::readback;
    use super::{fixtures, sync_build};
    use crate::ui_frame::{render_main_ui_passes, MainUiPassInputs};
    use crate::ui_root::UIRoot;

    /// The minimal `MainUiPassInputs` a headless overlay-only frame needs —
    /// every timeline/clip/VQT input absent (`None`/empty), exactly as
    /// `render_ui_to_png` fills them (§3: input presence, not caller
    /// identity). Only the overlay pass, which reads off `ui_root`, produces
    /// pixels here.
    fn overlay_only_inputs<'a>(
        res: &'a CompositeResources,
        text_input: &'a crate::text_input::TextInputState,
        frame_timer: &'a crate::frame_timer::FrameTimer,
    ) -> MainUiPassInputs<'a> {
        MainUiPassInputs {
            layer_bitmap_gpu: None,
            clip_bodies: &[],
            clip_rects: &[],
            clip_content_gpu: None,
            thumb: None,
            timeline_overlays: manifold_ui::panels::viewport::TimelineOverlays::default(),
            markers: &[],
            landing_flash: None,
            automation_lanes: &[],
            cursor_pos: manifold_ui::node::Vec2::ZERO,
            text_input,
            frame_timer,
            vqt: None,
            blit_pipeline: &res.blit_pipeline,
            blit_sampler: &res.blit_sampler,
            gpu_sink: None,
        }
    }

    #[test]
    fn bug097_render_sub_region_draws_root_excluding_overlay_that_render_tree_range_blanks() {
        let w = super::LOGICAL_W as u32;
        let h = super::LOGICAL_H as u32;
        let data = fixtures::build("bug060heavy").expect("bug060heavy scene");

        let mut ui = UIRoot::new();
        ui.resize(super::LOGICAL_W, super::LOGICAL_H);
        // Open the Audio Setup panel (an `OverlayId::AudioSetup`, Modeless —
        // no full-screen scrim, so its range is pure panel content: the sharp
        // BUG-097 case) BEFORE the build, so `build_overlays` records its
        // root-excluding range this frame.
        ui.audio_setup_panel.open();
        sync_build(&mut ui, &data, 24.0);

        assert!(
            !ui.overlay_draw.is_empty(),
            "opening Audio Setup must record an overlay range (build_overlays)"
        );
        let (start, end) = ui.overlay_draw[0];
        assert!(end > start, "overlay range must be non-empty ({start}..{end})");
        // The mechanism under test: the region root is at `start - 1`, outside
        // the recorded range. If this ever stops holding, the whole premise of
        // BUG-097 is gone and this test should be revisited, not silently pass.
        assert!(start >= 1, "overlay range must exclude its region root at start-1");

        let device = GpuDevice::new();
        let mut renderer = UIRenderer::new(&device, GpuTextureFormat::Bgra8Unorm);
        let mut cache = UICacheManager::new(GpuTextureFormat::Bgra8Unorm, 1.0);
        cache.set_scale_factor(1.0);
        cache.ensure_atlas(&device, w, h);
        let res = CompositeResources::new(&device, w, h);

        // Helper: paint just the overlay range through `f` onto a FRESH
        // composite of the same panels, and return (base_before, after).
        // Comparing before/after the SAME offscreen keeps the assertion immune
        // to the font/GPU nondeterminism a second composite would introduce.
        let mut paint_range = |renderer: &mut UIRenderer,
                               cache: &mut UICacheManager,
                               ui: &mut UIRoot,
                               sub_region: bool|
         -> (Vec<u8>, Vec<u8>) {
            cache.invalidate_all();
            composite_frame(&device, renderer, cache, ui, &res, 1.0);
            let before = readback(&device, &res.offscreen, w, h);
            renderer.begin_frame();
            if sub_region {
                renderer.render_sub_region(&ui.tree, start, end, false);
            } else {
                renderer.render_tree_range(&ui.tree, start, end);
            }
            if renderer.prepare(&device, w, h, 1.0) {
                let mut enc = device.create_encoder("bug097-overlay-range");
                renderer.render(&mut enc, &res.offscreen, GpuLoadAction::Load);
                enc.commit_and_wait_completed();
            }
            let after = readback(&device, &res.offscreen, w, h);
            (before, after)
        };

        // RED — the old harness call: render_tree_range renders NOTHING for a
        // root-excluding overlay range. The offscreen is byte-identical
        // before/after: the overlay is blank.
        let (red_before, red_after) = paint_range(&mut renderer, &mut cache, &mut ui, false);
        assert_eq!(
            red_before, red_after,
            "render_tree_range drew pixels for a root-excluding overlay range — \
             the BUG-097 premise no longer holds; revisit this test"
        );

        // GREEN — the seam's call: render_sub_region DRAWS the overlay. The
        // offscreen changes.
        let (green_before, green_after) = paint_range(&mut renderer, &mut cache, &mut ui, true);
        let sub_region_drew = green_before != green_after;
        assert!(
            sub_region_drew,
            "render_sub_region drew nothing for the open overlay — the seam's \
             overlay pass is broken (BUG-097 regressed)"
        );

        // Tie to the production path: `render_main_ui_passes` (the seam the
        // live app + harness both call) makes the render_sub_region choice
        // above internally. Run it end-to-end with the overlay open and
        // confirm it draws (changes the composited base) without panicking —
        // a smoke check that the real seam executes the overlay pass, not a
        // hand-rolled render_sub_region.
        cache.invalidate_all();
        composite_frame(&device, &mut renderer, &mut cache, &mut ui, &res, 1.0);
        let seam_before = readback(&device, &res.offscreen, w, h);
        let text_input = crate::text_input::TextInputState::new();
        let frame_timer = crate::frame_timer::FrameTimer::new(60.0);
        render_main_ui_passes(
            &device,
            &mut renderer,
            &mut ui,
            &res.offscreen,
            w,
            h,
            1.0,
            overlay_only_inputs(&res, &text_input, &frame_timer),
        );
        let seam_after = readback(&device, &res.offscreen, w, h);
        assert_ne!(
            seam_before, seam_after,
            "render_main_ui_passes drew nothing with an overlay open — the \
             production seam is not executing the overlay pass"
        );
    }
}
