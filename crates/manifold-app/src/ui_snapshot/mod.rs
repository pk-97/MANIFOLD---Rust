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

/// Entry dispatched from `main()` when `argv[1] == "ui-snap"`. `args` is the
/// argument slice starting at `"ui-snap"`.
pub fn run(args: &[String]) {
    let scene = args.get(1).map(String::as_str).unwrap_or("timeline");
    let want_dump = args.iter().any(|a| a == "--dump");
    let want_vs_mockup = args.iter().any(|a| a == "--vs-mockup");
    let want_thumbs = args.iter().any(|a| a == "--thumbs");
    let interact = arg_value(args, "--interact");
    // P0.0 evidence flag (`docs/TIMELINE_LAYOUT_P0_SPEC.md`): seed BOTH scroll
    // owners (`Viewport::scroll_y_px` + the header panel's `ScrollContainer`
    // offset) to the same non-zero pixel value right after the base render
    // and before any `--interact`, so a subsequent content-shrinking edit can
    // be captured mid-scroll. A flag rather than an `interact` verb because it
    // seeds state that predates the interaction being tested, not an action
    // being tested itself.
    let scroll_seed: Option<f32> = arg_value(args, "--scroll").and_then(|s| s.parse().ok());

    // `all`: render every scene in one process — a full-app eyeball after a
    // change. Skips the per-scene-only flags (mockup, interact); pass those to a
    // single scene when you need them.
    if scene == "all" {
        for s in ["timeline", "states", "inspector"] {
            render_ui_scene(s, want_dump, false, want_thumbs, None, None);
        }
        run_graph_preset("Mirror");
        run_editor_preset("FluidSim2D");
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
        run_editor_preset(&preset);
        return;
    }

    render_ui_scene(scene, want_dump, want_vs_mockup, want_thumbs, interact, scroll_seed);
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
            "ui-snap: unknown scene '{scene}' (known: timeline, states, inspector, scrollshrink, graph, editor, all)"
        );
        std::process::exit(2);
    };

    let dir = out_dir(scene);
    std::fs::create_dir_all(&dir).expect("create ui-snapshots dir");

    // Build the UI through the REAL core→UI translation path, render the base.
    let mut ui = UIRoot::new();
    ui.resize(LOGICAL_W, LOGICAL_H);
    if scene == "inspector" {
        // The inspector IS the subject: keep it at a generous width and give the
        // timeline a normal split so the selected layer's cards have room.
        ui.layout.inspector_width = 600.0;
        ui.layout.timeline_split_ratio = 0.6;
    } else {
        // Make the timeline the subject: drop the inspector, let lanes fill the
        // vertical. (Both fields are read by layout.resize() inside ui.build().)
        ui.layout.inspector_width = 0.0;
        ui.layout.timeline_split_ratio = 0.93;
    }
    sync_build(&mut ui, &data);
    render_and_dump(&ui, &data.selection, &dir, scene, "", want_dump, want_thumbs);

    // P0.1: the viewport is the sole scroll owner (D2) — the header panel
    // reads `viewport.scroll_y_px()` live at draw time, so seeding it here is
    // the only seed needed (mirrors `ui_root.rs`'s settings-restore path).
    // Before P0.1 this seeded two independent copies to reproduce RC1 ("user
    // scrolled, then the content shrank"); post-fix, `rebuild_mapper_layout`
    // (called from `sync_project_data` inside the `--interact` branch below)
    // re-clamps this same value against the new content height every time
    // (D3), so RC1 no longer reproduces — see
    // `docs/evidence/timeline_p0/after/README.md`.
    if let Some(y) = scroll_seed {
        let x = ui.viewport.scroll_x_beats().as_f32();
        ui.viewport.set_scroll(x, y);
        println!("ui-snap: scroll-seed y={y} (viewport clamped to {})", ui.viewport.scroll_y_px());
    }

    // Optional: render the HTML mockup and composite app | mockup side by side.
    if want_vs_mockup {
        compare::vs_mockup(&dir, scene, &dir.join(format!("{scene}.png")));
    }

    // Optional interaction: drive a real event, re-sync, render the "after".
    if let Some(spec) = interact {
        let desc = interact::apply(&mut ui, &mut data, &spec);
        println!("ui-snap: interact {desc}");
        sync_build(&mut ui, &data);
        render_and_dump(&ui, &data.selection, &dir, scene, ".after", want_dump, want_thumbs);
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
fn run_editor_preset(preset: &str) {
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
    );
    println!("ui-snap: wrote {} ({preset})", png.display());
}

/// The real translation path: structural sync → zoom-to-fit → build → push state.
fn sync_build(ui: &mut UIRoot, data: &fixtures::SceneData) {
    sync_project_data(ui, &data.project, data.active, &data.selection);
    // Configure the inspector (tabs + the active layer's effect/gen cards) from
    // the selection — the live app calls this whenever the active layer changes.
    // Without it the inspector stays on its default Master view, so the selected
    // layer's chain never appears.
    sync_inspector_data(ui, &data.project, data.active, &data.selection);
    // Zoom out so the 48-beat fixture clips fit the lane width (set before build
    // so the ruler ticks and the clip rects agree on px/beat).
    ui.viewport.set_zoom(24.0);
    ui.build();
    let mut tcache = TransportDisplayCache::new();
    push_state(ui, &data.project, &data.content, data.active, &data.selection, false, None, &mut tcache);
}

/// Render to `<scene><suffix>.png`, and (if requested) the tree dump as JSON +
/// a terse stdout summary.
fn render_and_dump(
    ui: &UIRoot,
    selection: &manifold_ui::UIState,
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
        tex_w,
        tex_h,
        SCALE,
        with_thumbs,
        png.to_str().expect("utf-8 path"),
    );
    println!("ui-snap: wrote {}", png.display());

    if want_dump {
        let json = dump::dump_tree(&ui.tree);
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
