//! P5c demo/evidence (`docs/REALTIME_3D_DESIGN.md`): drives the ACTUAL
//! `viewport_input` classification layer (`classify_mouse_drag` + `apply`,
//! the exact call `window_input.rs`'s `editor_mouse_input`/
//! `editor_cursor_moved` make) against a real `ViewportSession`, then dumps
//! before/after headless PNGs and asserts the pixels actually changed —
//! the L2 acceptance-demo bar (`docs/DESIGN_DOC_STANDARD.md` §10).
//!
//! **Why this lives here instead of a `scripts/ui-flows/*.json` L3 script**
//! (the preferred bar): the flow driver (`ui_snapshot::script::run`) only
//! drives the MAIN window's `UIRoot` through `apply_ui_frame_invalidations`/
//! `composite_main_ui_frame` — it has no `graph_editor` workspace, no
//! `Application::window_event` routing, and no knowledge of a second window
//! at all, so it structurally cannot reach `editor_mouse_input`/
//! `editor_cursor_moved` or the graph-editor window the viewport docks into.
//! Reaching L3 needs a flow driver extension that can open/target the
//! editor window — out of scope for this plumbing phase; named as the gap
//! for a follow-up click-script. This test is the best available L2
//! evidence: it exercises production code (`viewport_input`'s classifiers +
//! `apply`, `ViewportSession`) end to end, just without the winit event
//! loop and window dispatch around it.
//!
//! `#![cfg(test)]` on the whole module (the `journey_proof.rs`/
//! `bug035_verify.rs` convention) — invisible outside `cargo test`, so it
//! never needs a `#[allow(dead_code)]`.
#![cfg(test)]

use std::sync::Arc;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::GpuDevice;
use manifold_renderer::headless_readback::encode_rgba8_png;
use manifold_renderer::node_graph::{PrimitiveRegistry, ViewportOverlayConfig, ViewportSession};
use manifold_renderer::preset_context::PresetContext;

use crate::viewport_input::{ViewportInputSensitivity, apply, classify_mouse_drag};

/// Ground plane + one light + a `render_scene` node, wired to a SHOW
/// `orbit_camera` the viewport never touches (D9's isolation is proven
/// elsewhere, `scene_viewport_navigate.rs` — this fixture is deliberately
/// the same shape so a reviewer can compare the two directly). Real visible
/// geometry (a tilted grid), so an orbit actually moves recognizable
/// pixels — an empty scene wouldn't demonstrate anything.
fn scene_json() -> String {
    r#"{"version":2,"name":"ViewportP5cDemo","nodes":[
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":8192},
            "resolution_x":{"type":"Int","value":20},
            "resolution_y":{"type":"Int","value":20},
            "size_x":{"type":"Float","value":8.0},
            "size_y":{"type":"Float","value":8.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":20},
            "src_rows":{"type":"Int","value":20}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"show_cam","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.6},
            "distance":{"type":"Float","value":10.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":0.8},
            "color_g":{"type":"Float","value":0.8},
            "color_b":{"type":"Float","value":0.9},
            "ambient":{"type":"Float","value":0.1}}},
        {"id":5,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":4.0},
            "pos_y":{"type":"Float","value":20.0},
            "pos_z":{"type":"Float","value":3.0},
            "aim_x":{"type":"Float","value":0.0},
            "aim_y":{"type":"Float","value":0.0},
            "aim_z":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":0.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
    ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":5,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
    ]}"#
        .to_string()
}

fn ctx(width: u32, height: u32, frame_count: i64) -> PresetContext {
    PresetContext {
        time: 0.1,
        beat: 0.2,
        dt: 1.0 / 60.0,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

/// Drives a left-drag orbit through the SAME `classify_mouse_drag` + `apply`
/// call `window_input.rs`'s `editor_cursor_moved` makes on every mouse-move
/// while a viewport drag is armed, then asserts the rendered framing
/// actually changed — the visible proof that the input→camera wiring, not
/// just `ViewportSession`'s own mechanics (already proven by
/// `scene_viewport_session.rs`'s gpu-proofs), moves pixels.
#[test]
fn viewport_input_orbit_drag_changes_framing() {
    let device = Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json();
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse scene def");
    let (width, height) = (320_u32, 240_u32);

    let mut session = ViewportSession::open(
        &def,
        &NodeId::new("scene"),
        &registry,
        Arc::clone(&device),
        width,
        height,
        &ctx(width, height, 0),
    )
    .expect("viewport session must open against a top-level render_scene node");

    let overlay_cfg = ViewportOverlayConfig::default();
    let before = session
        .render_if_dirty(&ctx(width, height, 1), &overlay_cfg, None, &[])
        .to_vec();
    std::fs::write(
        "/tmp/viewport_p5c_before.png",
        encode_rgba8_png(&before, width, height),
    )
    .expect("write /tmp/viewport_p5c_before.png");

    // The exact production call site: a left-drag with no shift held
    // classifies as Orbit, then `apply` forwards it to
    // `ViewportSession::orbit` at the wired-in sensitivity default.
    let sens = ViewportInputSensitivity::default();
    let gesture = classify_mouse_drag(winit::event::MouseButton::Left, false, 220.0, 90.0)
        .expect("a bare left-drag must classify as an Orbit gesture");
    apply(&mut session, gesture, &sens);
    assert!(session.is_dirty(), "apply(Orbit) must mark the session dirty");

    let after = session
        .render_if_dirty(&ctx(width, height, 2), &overlay_cfg, None, &[])
        .to_vec();
    std::fs::write(
        "/tmp/viewport_p5c_after.png",
        encode_rgba8_png(&after, width, height),
    )
    .expect("write /tmp/viewport_p5c_after.png");

    assert_eq!(before.len(), after.len(), "readback dimensions must be stable across an orbit");
    let diff_count = before.iter().zip(after.iter()).filter(|(a, b)| a != b).count();
    assert!(
        diff_count > (before.len() / 20),
        "orbiting the viewport camera via viewport_input::apply must change a visible \
         fraction of pixels (only {diff_count}/{} bytes differed) — see \
         /tmp/viewport_p5c_before.png and /tmp/viewport_p5c_after.png",
        before.len()
    );
    eprintln!(
        "[P5c demo] wrote /tmp/viewport_p5c_before.png + /tmp/viewport_p5c_after.png \
         ({width}x{height}, {diff_count}/{} bytes differed after an orbit drag)",
        before.len()
    );
}
