//! Headless UI snapshot harness (feature `ui-snapshot`). An agent-facing tool
//! to render MANIFOLD's REAL UI tree to a PNG plus a machine-readable tree dump,
//! with no winit window — so UI/UX work is see-able, measurable, and provable.
//!
//! Invoked via the `cargo xtask` alias: `cargo xtask ui-snap <scene> [--dump]`.
//! See `docs/HEADLESS_UI_HARNESS.md`.

mod dump;
mod fixtures;
mod render;

use std::path::PathBuf;

use crate::ui_bridge::{push_state, sync_project_data, TransportDisplayCache};
use crate::ui_root::UIRoot;

// Logical UI size = texture size (rendered 1:1; `UIRenderer::prepare`'s scale
// is a text-DPI hint, not a geometry transform). Tall enough for 7 × 140px
// lanes + ruler; `tex_w` must be a multiple of 64 for an aligned readback.
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

    let Some(data) = fixtures::build(scene) else {
        eprintln!("ui-snap: unknown scene '{scene}' (known: timeline)");
        std::process::exit(2);
    };

    // Build the UI through the REAL core→UI translation path.
    let mut ui = UIRoot::new();
    ui.resize(LOGICAL_W, LOGICAL_H);
    // Make the timeline the subject: drop the inspector, let lanes fill the
    // vertical. (Both fields are read by layout.resize() inside ui.build().)
    ui.layout.inspector_width = 0.0;
    ui.layout.timeline_split_ratio = 0.93;
    sync_project_data(&mut ui, &data.project, data.active, &data.selection);
    ui.build();
    let mut tcache = TransportDisplayCache::new();
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

    let dir = out_dir(scene);
    std::fs::create_dir_all(&dir).expect("create ui-snapshots dir");

    let tex_w = (LOGICAL_W * SCALE) as u32;
    let tex_h = (LOGICAL_H * SCALE) as u32;
    let png = dir.join(format!("{scene}.png"));
    render::render_tree_to_png(&ui.tree, tex_w, tex_h, SCALE, png.to_str().expect("utf-8 path"));
    println!("ui-snap: wrote {}", png.display());

    if want_dump {
        let json = dump::dump_tree(&ui.tree);
        let json_path = dir.join(format!("{scene}.tree.json"));
        std::fs::write(&json_path, serde_json::to_string_pretty(&json).expect("serialize dump"))
            .expect("write tree json");
        println!("ui-snap: wrote {}", json_path.display());
        // Terse summary to stdout so the values are visible inline.
        print!("{}", dump::terse(&ui.tree));
    }
}
