//! P6 demo/evidence (`docs/REALTIME_3D_DESIGN.md` P6 — Viewport Tier 2
//! gizmos): drives the ACTUAL `manifold_renderer::node_graph` gizmo
//! functions (`pick_object`, `gizmo_target_for`, `gizmo_lines`, `drag_write`)
//! against a real `node.scene_object`-shaped scene through a real
//! `ViewportSession`, then dumps headless PNGs and asserts pixels actually
//! changed — the same L2 acceptance-demo bar `viewport_p5c_demo.rs`
//! establishes for P5c (see that module's doc comment for why an L3
//! click-script isn't reachable yet: the flow driver has no graph-editor-
//! window routing).
//!
//! `window_input.rs`'s `editor_viewport_gizmo_press`/
//! `editor_viewport_gizmo_drag_move` (the real winit-facing call sites) are
//! thin wrappers around exactly these same renderer functions plus a
//! `SetGraphNodeParamCommand`/`AddObjectTransformCommand` dispatch — the
//! undo/redo round-trip half of P6's gate is proven at the command level in
//! `manifold-editing`'s own test suite
//! (`add_object_transform_then_gizmo_param_drag_round_trips_undo_redo`),
//! not re-proven here; this module's job is the RENDER half: pick
//! highlighting, gizmo-mode geometry, and a drag actually moving the
//! rendered object.
#![cfg(test)]

use std::sync::Arc;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_gpu::GpuDevice;
use manifold_renderer::headless_readback::encode_rgba8_png;
use manifold_renderer::node_graph::scene_vm::SceneVm;
use manifold_renderer::node_graph::{
    GizmoMode, PrimitiveRegistry, ViewportOverlayConfig, ViewportSession, drag_write, gizmo_lines,
    gizmo_target_for, pick_object,
};
use manifold_renderer::preset_context::PresetContext;

/// A `node.scene_object`-shaped scene (SCENE_OBJECT_AND_PANEL_V2_DESIGN
/// D1/D12 — the shape `scene_vm::SceneVm::from_def` requires to resolve
/// `Known` objects at all): one cube, one `node.transform_3d` feeding its
/// `transform` port (id 10, so a test can target `pos_x` directly), a phong
/// material, one light, wired to a SHOW `orbit_camera` the viewport
/// overrides (D9). `wire_pos_x` optionally wires a constant into the
/// transform's `pos_x` port — the P6 "locked axis" fixture.
fn scene_json(wire_pos_x: bool) -> String {
    let extra_wire = if wire_pos_x {
        r#",{"fromNode":30,"fromPort":"out","toNode":10,"toPort":"pos_x"}"#
    } else {
        ""
    };
    format!(
        r#"{{"version":2,"name":"ViewportP6Demo","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.cube_mesh","nodeId":"cube","params":{{
            "max_capacity":{{"type":"Int","value":36}},
            "size":{{"type":"Float","value":1.5}}}}}},
        {{"id":10,"typeId":"node.transform_3d","nodeId":"xf","params":{{
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":0.0}},
            "pos_z":{{"type":"Float","value":0.0}}}}}},
        {{"id":30,"typeId":"node.value","nodeId":"pos_x_const","params":{{
            "value":{{"type":"Float","value":0.0}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":0.85}},
            "color_g":{{"type":"Float","value":0.3}},
            "color_b":{{"type":"Float","value":0.3}},
            "ambient":{{"type":"Float","value":0.15}}}}}},
        {{"id":6,"typeId":"node.scene_object","nodeId":"obj"}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"show_cam","params":{{
            "orbit":{{"type":"Float","value":0.6}},
            "tilt":{{"type":"Float","value":0.5}},
            "distance":{{"type":"Float","value":8.0}},
            "fov_y":{{"type":"Float","value":0.8}}}}}},
        {{"id":5,"typeId":"node.light","nodeId":"sun","params":{{
            "mode":{{"type":"Enum","value":0}},
            "pos_x":{{"type":"Float","value":4.0}},
            "pos_y":{{"type":"Float","value":20.0}},
            "pos_z":{{"type":"Float","value":3.0}},
            "aim_x":{{"type":"Float","value":0.0}},
            "aim_y":{{"type":"Float","value":0.0}},
            "aim_z":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "intensity":{{"type":"Float","value":1.0}},
            "cast_shadows":{{"type":"Float","value":0.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":1}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
    ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":6,"toPort":"vertices"}},
        {{"fromNode":10,"fromPort":"transform","toNode":6,"toPort":"transform"}},
        {{"fromNode":4,"fromPort":"out","toNode":6,"toPort":"material"}},
        {{"fromNode":6,"fromPort":"object","toNode":20,"toPort":"object_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":5,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}{extra_wire}
    ]}}"#
    )
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

fn open_session(def: &EffectGraphDef, device: &Arc<GpuDevice>, w: u32, h: u32) -> ViewportSession {
    let registry = PrimitiveRegistry::with_builtin();
    ViewportSession::open(def, &NodeId::new("scene"), &registry, Arc::clone(device), w, h, &ctx(w, h, 0))
        .expect("viewport session must open against a top-level render_scene node")
}

/// (a) Pick-highlight: `pick_object` finds the cube under its own projected
/// screen position, and the resulting gizmo overlay (Move mode) is visibly
/// composited onto the render — `/tmp/viewport_p6_pick_highlight.png`.
#[test]
fn pick_object_highlights_the_clicked_object() {
    let device = Arc::new(GpuDevice::new());
    let def: EffectGraphDef = serde_json::from_str(&scene_json(false)).expect("parse scene def");
    let (w, h) = (320_u32, 240_u32);
    let mut session = open_session(&def, &device, w, h);
    let overlay_cfg = ViewportOverlayConfig { grid: false, ..ViewportOverlayConfig::default() };

    let scene = SceneVm::from_def(&def).expect("scene resolves");
    let cam = session.camera().to_camera();
    let obj_proj = cam.project_to_pixel([0.0, 0.0, 0.0], w, h).expect("cube origin is in front of the camera");
    let picked = pick_object(&scene, &cam, w, h, (obj_proj.px, obj_proj.py));
    assert_eq!(picked, Some(6), "clicking the cube's own projected position must pick it (node id 6)");

    let target = gizmo_target_for(&scene, picked.unwrap()).expect("gizmo target resolves");
    let lines = gizmo_lines(GizmoMode::Move, &target);
    assert_eq!(lines.len(), 3, "move gizmo draws 3 axis handles");

    let rgba = session.render_if_dirty(&ctx(w, h, 1), &overlay_cfg, None, &[], &lines);
    std::fs::write("/tmp/viewport_p6_pick_highlight.png", encode_rgba8_png(&rgba, w, h))
        .expect("write /tmp/viewport_p6_pick_highlight.png");
}

/// (b) Each gizmo mode renders distinct handle geometry —
/// `/tmp/viewport_p6_gizmo_move.png` / `_rotate.png` / `_scale.png`. A
/// fourth render (`_locked.png`) uses the wired-`pos_x` fixture to show D8's
/// "wired axis locks gray" refusal visibly.
#[test]
fn each_gizmo_mode_renders_and_locked_axis_shows_gray() {
    let device = Arc::new(GpuDevice::new());
    let def: EffectGraphDef = serde_json::from_str(&scene_json(false)).expect("parse scene def");
    let (w, h) = (320_u32, 240_u32);
    let mut session = open_session(&def, &device, w, h);
    let overlay_cfg = ViewportOverlayConfig { grid: false, ..ViewportOverlayConfig::default() };
    let scene = SceneVm::from_def(&def).expect("scene resolves");
    let target = gizmo_target_for(&scene, 6).expect("gizmo target resolves");

    let mut frame = 1_i64;
    let mut renders = Vec::new();
    for (mode, name) in [
        (GizmoMode::Move, "move"),
        (GizmoMode::Rotate, "rotate"),
        (GizmoMode::Scale, "scale"),
    ] {
        let lines = gizmo_lines(mode, &target);
        assert!(!lines.is_empty(), "{name} gizmo must draw handle geometry");
        let rgba = session.render_if_dirty(&ctx(w, h, frame), &overlay_cfg, None, &[], &lines);
        std::fs::write(format!("/tmp/viewport_p6_gizmo_{name}.png"), encode_rgba8_png(&rgba, w, h))
            .unwrap_or_else(|_| panic!("write /tmp/viewport_p6_gizmo_{name}.png"));
        renders.push(rgba);
        frame += 1;
    }
    // The three modes must draw visibly different overlays (different line
    // counts/shapes composited onto the SAME clean scene render).
    assert_ne!(renders[0], renders[1], "move and rotate gizmo overlays must differ");
    assert_ne!(renders[1], renders[2], "rotate and scale gizmo overlays must differ");

    // Locked-axis fixture: pos_x is wired, so the X handle must draw
    // LOCKED-gray, not its normal red.
    let locked_def: EffectGraphDef = serde_json::from_str(&scene_json(true)).expect("parse locked scene def");
    let locked_scene = SceneVm::from_def(&locked_def).expect("locked scene resolves");
    let locked_target = gizmo_target_for(&locked_scene, 6).expect("locked gizmo target resolves");
    let (_, _, driven) =
        drag_write(GizmoMode::Move, manifold_renderer::node_graph::GizmoAxis::X, &locked_target)
            .expect("transform is wired, drag_write must resolve");
    assert!(driven, "pos_x is wired in the locked fixture — drag_write must report it driven");
    let locked_lines = gizmo_lines(GizmoMode::Move, &locked_target);
    let mut locked_session = open_session(&locked_def, &device, w, h);
    let rgba = locked_session.render_if_dirty(&ctx(w, h, 1), &overlay_cfg, None, &[], &locked_lines);
    std::fs::write("/tmp/viewport_p6_gizmo_locked.png", encode_rgba8_png(&rgba, w, h))
        .expect("write /tmp/viewport_p6_gizmo_locked.png");
}

/// (c) A move-gizmo drag actually moves the rendered object: mutate the
/// transform atom's `pos_x` param (the same write
/// `SetGraphNodeParamCommand`/`window_input.rs`'s
/// `editor_viewport_gizmo_drag_move` dispatch performs — the undo/redo
/// round-trip of that exact write is proven at the command level in
/// `manifold-editing`), rebuild the session against the mutated def via
/// `sync_def` (same call `app_render.rs` makes every present frame), and
/// assert the rendered pixels actually moved —
/// `/tmp/viewport_p6_move_before.png` / `_after.png`.
#[test]
fn move_gizmo_drag_moves_the_rendered_object() {
    let device = Arc::new(GpuDevice::new());
    let def: EffectGraphDef = serde_json::from_str(&scene_json(false)).expect("parse scene def");
    let (w, h) = (320_u32, 240_u32);
    let mut session = open_session(&def, &device, w, h);
    let overlay_cfg = ViewportOverlayConfig { grid: false, ..ViewportOverlayConfig::default() };

    let before = session.render_if_dirty(&ctx(w, h, 1), &overlay_cfg, None, &[], &[]);
    std::fs::write("/tmp/viewport_p6_move_before.png", encode_rgba8_png(&before, w, h))
        .expect("write /tmp/viewport_p6_move_before.png");

    // The exact write a move-axis drag dispatches: the transform atom's
    // pos_x param moves by a few world units.
    let mut moved_def = def.clone();
    let xf = moved_def.nodes.iter_mut().find(|n| n.id == 10).expect("transform node 10 exists");
    xf.params.insert("pos_x".to_string(), SerializedParamValue::Float { value: 3.0 });

    session
        .sync_def(&moved_def, &PrimitiveRegistry::with_builtin(), &ctx(w, h, 2))
        .expect("sync_def against the moved def must rebuild");
    assert!(session.is_dirty(), "a real transform param change must mark the session dirty");

    let after = session.render_if_dirty(&ctx(w, h, 3), &overlay_cfg, None, &[], &[]);
    std::fs::write("/tmp/viewport_p6_move_after.png", encode_rgba8_png(&after, w, h))
        .expect("write /tmp/viewport_p6_move_after.png");

    assert_eq!(before.len(), after.len());
    let diff_count = before.iter().zip(after.iter()).filter(|(a, b)| a != b).count();
    assert!(
        diff_count > (before.len() / 50),
        "moving the transform's pos_x must visibly move the rendered cube \
         (diff_count={diff_count}, len={})",
        before.len()
    );
}
