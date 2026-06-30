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

    // The `graph` scene is not a UITree fixture — it renders the node-graph
    // editor canvas from a synthesized snapshot, on its own render path.
    if scene == "graph" {
        run_graph(args);
        return;
    }

    let Some(mut data) = fixtures::build(scene) else {
        eprintln!("ui-snap: unknown scene '{scene}' (known: timeline, states, inspector, graph)");
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

/// Render the node-graph editor canvas for one preset. The graph snapshot is
/// synthesized straight from the catalog — `loaded_preset_view_by_id` →
/// `snapshot_for_view` → the UI translation — so no content thread or running
/// chain is needed. Pick the preset with `--preset <TypeId>` (effect or
/// generator); defaults to `Mirror`.
fn run_graph(args: &[String]) {
    let preset = arg_value(args, "--preset").unwrap_or_else(|| "Mirror".to_string());
    let pid = manifold_core::PresetTypeId::from_string(preset.clone());

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
    render::render_graph_to_png(&gv_snap, tex_w, tex_h, SCALE, png.to_str().expect("utf-8 path"));
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
