//! `docs/REALTIME_3D_DESIGN.md` P5 gate — the editor viewport's navigate
//! (Tier 1) render path.
//!
//! Two things the isolated Rust unit tests in `viewport_camera.rs` /
//! `viewport_overlay.rs` / `viewport_render.rs` can't reach: that the
//! camera-override splice actually renders a real scene from a DIFFERENT
//! angle through a real Metal pipeline (not just that the graph-def
//! mutation is well-formed), and — the load-bearing D9 proof — that
//! rendering through the isolated viewport path leaves the SAME show
//! `PresetRuntime`'s output byte-for-byte unchanged.

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::encode_rgba8_png;
use manifold_renderer::node_graph::{
    PrimitiveRegistry, ViewportCamera, ViewportOverlayConfig, build_overlay_lines,
    composite_overlay_lines_rgba8, override_camera_def, project_lines, render_viewport_frame,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// Ground plane lit by one sun, wired to an `orbit_camera` (the SHOW
/// camera) — deliberately simple, this proof is about the viewport's
/// render-isolation and overlay compositing, not scene fidelity (that's
/// `render_scene_shadows`/`render_scene_lights`'s job).
fn scene_json() -> String {
    r#"{"version":2,"name":"ViewportNavigateProof","nodes":[
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

fn ctx(h: &harness::ParityHarness, frame_count: i64) -> PresetContext {
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
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn render_show_frame(
    runtime: &mut PresetRuntime,
    h: &harness::ParityHarness,
    target: &manifold_renderer::render_target::RenderTarget,
    frame_count: i64,
) -> Vec<u8> {
    let c = ctx(h, frame_count);
    let mut enc = h.device.create_encoder("viewport-proof-show-enc");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, &target.texture, &c, &manifold_core::params::ParamManifest::default());
    }
    enc.commit_and_wait_completed();
    h.readback(&target.texture)
}

/// The D9 proof (`docs/REALTIME_3D_DESIGN.md` D9): render the show, use the
/// isolated viewport override path (a completely separate `PresetRuntime` +
/// `MetalBackend`), then render the SAME show `PresetRuntime` again with the
/// same frame context — the two show reads must be byte-identical.
/// **Also** produces the headless PNG artifact: the viewport's own render,
/// from a DIFFERENT camera angle, with grid + camera-frustum + light-billboard
/// overlays composited on top.
#[test]
fn viewport_render_is_isolated_and_produces_overlay_png() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json();

    // ── 1. The show, rendered through its own long-lived runtime ──
    let mut show_runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("show scene graph must build");
    let show_target = h.make_target("viewport-proof-show");

    // Two warm-up frames (pipeline compilation), same convention as
    // render_scene_shadows.rs.
    render_show_frame(&mut show_runtime, h, &show_target, 0);
    let bytes_before = render_show_frame(&mut show_runtime, h, &show_target, 1);

    // ── 2. Open the viewport: isolated override render from a DIFFERENT
    //    camera angle, entirely separate PresetRuntime/backend. ──
    let def: manifold_core::effect_graph_def::EffectGraphDef =
        serde_json::from_str(&json).expect("parse scene def");
    let vp_cam = ViewportCamera {
        target: [0.0, 0.0, 0.0],
        yaw: 2.1,   // a very different angle from the show's orbit_camera
        pitch: 0.5,
        distance: 12.0,
        fov_y: 1.0,
        near: 0.05,
        far: 200.0,
    };
    let overridden = override_camera_def(&def, &manifold_core::NodeId::new("scene"), &vp_cam)
        .expect("camera splice must find the render_scene node");
    let frame_ctx = ctx(h, 0);
    let (mut viewport_rgba, vw, vh) = render_viewport_frame(
        overridden,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        &frame_ctx,
    )
    .expect("viewport override render must succeed");

    // Overlays: grid + the SHOW camera's frustum (built from the show's
    // own orbit_camera params, independent of the splice) + the light.
    let editor_cam = vp_cam.to_camera();
    let show_cam = manifold_renderer::node_graph::Camera::orbit_perspective(
        0.7, 0.6, 10.0, 0.8, 0.0, 0.0, 0.05, 200.0,
    );
    let overlay_cfg = ViewportOverlayConfig::default();
    let world_lines = build_overlay_lines(
        &overlay_cfg,
        Some((&show_cam, vw as f32 / vh as f32)),
        &[[4.0, 20.0, 3.0]],
    );
    let screen_lines = project_lines(&editor_cam, vw, vh, &world_lines);
    assert!(!screen_lines.is_empty(), "at least some overlay lines must project on-screen");
    composite_overlay_lines_rgba8(&mut viewport_rgba, vw, vh, &screen_lines);

    let png = encode_rgba8_png(&viewport_rgba, vw, vh);
    std::fs::write("/tmp/viewport_navigate_p5.png", &png)
        .expect("write /tmp/viewport_navigate_p5.png");
    eprintln!(
        "[P5 gate] wrote /tmp/viewport_navigate_p5.png ({vw}x{vh}, {} overlay lines)",
        screen_lines.len()
    );

    // ── 3. The show, rendered again on the SAME runtime, same context. ──
    let bytes_after = render_show_frame(&mut show_runtime, h, &show_target, 1);

    // D9: byte-identical. A plain `assert_eq!` on Vec<u8> already does an
    // exact machine comparison; report a diff summary if it ever fails
    // instead of dumping two full multi-KB buffers into the test log.
    if bytes_before != bytes_after {
        let first_diff = bytes_before
            .iter()
            .zip(bytes_after.iter())
            .position(|(a, b)| a != b);
        panic!(
            "D9 VIOLATION: show output changed after the viewport was used. \
             len_before={} len_after={} first_diff_at={:?}",
            bytes_before.len(),
            bytes_after.len(),
            first_diff
        );
    }
    eprintln!(
        "[P5 gate] D9 proof: show readback byte-identical before/after viewport use ({} bytes)",
        bytes_before.len()
    );
}
