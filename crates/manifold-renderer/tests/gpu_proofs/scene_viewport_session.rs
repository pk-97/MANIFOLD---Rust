//! `docs/REALTIME_3D_DESIGN.md` P5 gate — the persistent [`ViewportSession`]
//! that backs live drag-navigation (the follow-up to
//! `scene_viewport_navigate.rs`'s one-shot `render_viewport_frame` proof).
//!
//! Three things a session must prove that a one-shot render can't:
//! 1. Camera moves are cheap — `orbit`/`pan`/`dolly` never rebuild the
//!    `PresetRuntime` (only the FIRST `render_if_dirty` after `open()` pays
//!    the two-frame warm-up; subsequent camera-only moves render once).
//! 2. A def change (the performer edits the graph while the viewport is
//!    open) is detected and rebuilt via `sync_def`, carrying the camera
//!    forward rather than resetting it.
//! 3. `render_if_dirty` is a real debounce: calling it again with no camera
//!    move and no def change returns the SAME cached bytes without another
//!    GPU dispatch (proven by content, not by a mock — a stale-camera bug
//!    would silently pass a "did it not crash" check).
//!
//! Also produces the PNG evidence the P5b task asks for: three frames at
//! different orbit angles reached purely by driving `ViewportSession`'s
//! input methods, the same surface a real mouse-drag handler calls.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_renderer::headless_readback::encode_rgba8_png;
use manifold_renderer::node_graph::{PrimitiveRegistry, ViewportOverlayConfig, ViewportSession};
use manifold_renderer::preset_context::PresetContext;

use crate::harness;

/// Bounds and handles must redraw over the cached scene while a setup drag
/// is still a UI draft. No fluid solver or graph rebuild should run for it.
#[test]
fn viewport_session_fluid_domain_draft_redraws_without_rebuilding() {
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_renderer::node_graph::{GizmoMode, gizmo_lines, gizmo_target_for};
    use manifold_renderer::node_graph::scene_vm::{SceneObjectVm, SceneVm};
    use manifold_renderer::node_graph::viewport_overlay::fluid_domain_lines;

    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let def: EffectGraphDef = serde_json::from_str(&scene_json()).unwrap();
    let mut frame_ctx = ctx(h);
    frame_ctx.width = 640;
    frame_ctx.height = 400;
    frame_ctx.output_width = 640;
    frame_ctx.output_height = 400;
    frame_ctx.aspect = 1.6;
    let mut session = ViewportSession::open(
        &def, &NodeId::new("scene"), &registry, std::sync::Arc::clone(&h.device),
        frame_ctx.width, frame_ctx.height, &frame_ctx,
    ).unwrap();
    session.pan(-20.0, 25.0, 0.01);
    session.dolly(-2.5, 0.3);
    let cfg = ViewportOverlayConfig::default();
    let clean = session.render_if_dirty(&frame_ctx, &cfg, None, &[], &[]);
    let mut fluid: EffectGraphDef = serde_json::from_str(r#"{
        "version":2,"nodes":[
            {"id":1,"nodeId":"domain","typeId":"node.transform_3d","params":{
                "pos_y":{"type":"Float","value":2.0},
                "scale_x":{"type":"Float","value":4.0},
                "scale_y":{"type":"Float","value":4.0},
                "scale_z":{"type":"Float","value":4.0}}},
            {"id":2,"nodeId":"water","typeId":"node.fluid_surface"},
            {"id":3,"nodeId":"surface","typeId":"node.scene_object"},
            {"id":4,"nodeId":"scene","typeId":"node.render_scene","params":{
                "objects":{"type":"Int","value":1}}},
            {"id":5,"nodeId":"out","typeId":"system.final_output"}
        ],"wires":[
            {"fromNode":1,"fromPort":"transform","toNode":2,"toPort":"domain"},
            {"fromNode":2,"fromPort":"vertices","toNode":3,"toPort":"vertices"},
            {"fromNode":3,"fromPort":"object","toNode":4,"toPort":"object_0"},
            {"fromNode":4,"fromPort":"color","toNode":5,"toPort":"in"}
        ]}"#).unwrap();
    let mut frames = Vec::new();
    for (label, mode, edit) in [
        ("original", GizmoMode::Move, None),
        ("moved", GizmoMode::Move, Some(("pos_x", 2.0))),
        ("resized", GizmoMode::Scale, Some(("scale_x", 6.0))),
    ] {
        if let Some((param, value)) = edit {
            fluid.nodes[0].params.insert(param.into(), SerializedParamValue::Float { value });
        }
        let scene = SceneVm::from_def(&fluid).expect("fluid authoring scene");
        let target = gizmo_target_for(&scene, 3).expect("editable fluid domain");
        let domain = scene.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.object_node_id == 3 => row.fluid_domain,
            _ => None,
        }).unwrap();
        let mut lines = fluid_domain_lines(domain).to_vec();
        lines.extend(gizmo_lines(mode, &target));
        let frame = session.render_if_dirty(&frame_ctx, &cfg, None, &[], &lines);
        assert_ne!(frame, clean, "{label} bounds must be visible");
        assert!(!session.is_dirty(), "overlay drafts must not rebuild or simulate");
        std::fs::write(format!("/tmp/fluid_domain_draft_{label}.png"),
            encode_rgba8_png(&frame, frame_ctx.width, frame_ctx.height)).unwrap();
        frames.push(frame);
    }
    assert_ne!(frames[0], frames[1], "moving the bounds must redraw");
    assert_ne!(frames[1], frames[2], "resizing the bounds must redraw");
    assert_eq!(clean, session.render_if_dirty(&frame_ctx, &cfg, None, &[], &[]),
        "cancelling a draft restores the unchanged cached scene");
}

/// Ground plane lit by one sun, wired to an `orbit_camera` (the SHOW
/// camera) — identical scene to `scene_viewport_navigate.rs`'s proof scene,
/// deliberately: this test is about session lifecycle/dirty-tracking, not
/// scene fidelity.
fn scene_json() -> String {
    r#"{"version":2,"name":"ViewportSessionProof","nodes":[
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

fn ctx(h: &harness::ParityHarness) -> PresetContext {
    PresetContext {
        time: 0.1,
        beat: 0.2,
        dt: 1.0 / 60.0,
        width: h.width,
        height: h.height,
        output_width: h.width,
        output_height: h.height,
        aspect: h.width as f32 / h.height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

/// Drive a `ViewportSession` with synthetic input events exactly the shape a
/// real mouse-drag handler would produce (pixel deltas + a sensitivity
/// constant), and prove: (1) navigation actually moves the camera and the
/// rendered pixels change with it — three PNGs at three orbit angles; (2) a
/// no-op `render_if_dirty` call (nothing moved) is a cache hit, not a
/// re-render — proven by content equality on consecutive calls with zero
/// camera delta in between; (3) `sync_def` on an unchanged def is a no-op
/// (doesn't reset the camera / force a spurious rebuild-driven redraw).
#[test]
fn viewport_session_navigates_and_debounces() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json();
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse scene def");
    let frame_ctx = ctx(h);

    let mut session = ViewportSession::open(
        &def,
        &NodeId::new("scene"),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        &frame_ctx,
    )
    .expect("viewport session must open");

    let overlay_cfg = ViewportOverlayConfig::default();

    // ── Frame 1: default framing. ──
    assert!(session.is_dirty(), "a freshly-opened session must render its first frame");
    let frame1 = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert!(!session.is_dirty(), "render_if_dirty must clear the dirty flag");

    // ── Debounce proof: call again with NO input in between — must be a
    //    cache hit (identical bytes, no new dispatch needed to prove it,
    //    since a stale/rebuilt render would still often look "similar" —
    //    what actually matters is `is_dirty()` staying false). ──
    let frame1_again = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert_eq!(frame1, frame1_again, "no camera/def change ⇒ identical cached bytes");
    assert!(!session.is_dirty(), "no-op render_if_dirty must not mark dirty");

    // ── Frame 2: orbit — LMB-drag equivalent. ──
    session.orbit(220.0, 40.0, 0.01);
    assert!(session.is_dirty(), "orbit() must mark the session dirty");
    let frame2 = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert_ne!(frame1, frame2, "orbiting the camera must change the rendered pixels");

    // ── Frame 3: dolly in — scroll-wheel equivalent, from the orbited pose. ──
    session.dolly(1.0, 0.3);
    assert!(session.is_dirty(), "dolly() must mark the session dirty");
    let frame3 = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert_ne!(frame2, frame3, "dollying must change the rendered pixels");

    // ── sync_def on the SAME def is a no-op: doesn't reset the camera or
    //    force a redraw. ──
    session.sync_def(&def, &registry, &frame_ctx).expect("sync_def on unchanged def must succeed");
    assert!(!session.is_dirty(), "sync_def on an unchanged def must not mark dirty");
    assert!(
        (session.camera().yaw - 0.6 - 220.0 * 0.01).abs() < 1e-4,
        "sync_def on an unchanged def must not reset the navigated camera"
    );

    let (vw, vh) = session.dimensions();
    for (label, frame) in [("open", &frame1), ("orbit", &frame2), ("dolly", &frame3)] {
        let png = encode_rgba8_png(frame, vw, vh);
        let path = format!("/tmp/viewport_session_{label}.png");
        std::fs::write(&path, &png).unwrap_or_else(|e| panic!("write {path}: {e}"));
        eprintln!("[P5b gate] wrote {path} ({vw}x{vh})");
    }
}

/// `sync_def` on a genuinely CHANGED def (a param edit, simulating the
/// performer tweaking the scene while the viewport is open) rebuilds and
/// re-renders — proven by pixel change with the camera held fixed.
#[test]
fn viewport_session_rebuilds_on_def_change() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json();
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse scene def");
    let frame_ctx = ctx(h);

    let mut session = ViewportSession::open(
        &def,
        &NodeId::new("scene"),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        &frame_ctx,
    )
    .expect("viewport session must open");
    let overlay_cfg = ViewportOverlayConfig::default();
    let before = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);

    // Edit the material color — a real "performer changed the graph" event.
    let mut edited = def.clone();
    let mat = edited
        .nodes
        .iter_mut()
        .find(|n| n.node_id.as_str() == "mat")
        .expect("mat node present");
    mat.params.insert(
        "color_r".to_string(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.05 },
    );

    session.sync_def(&edited, &registry, &frame_ctx).expect("sync_def must rebuild on a real change");
    assert!(session.is_dirty(), "a real def change must mark the session dirty");
    let after = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert_ne!(before, after, "a material color edit must change the rendered pixels");
}
