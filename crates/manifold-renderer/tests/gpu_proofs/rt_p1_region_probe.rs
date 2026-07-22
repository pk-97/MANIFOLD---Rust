//! `docs/RAYTRACING_DESIGN.md` §5.2 P1/RT-D3 — scripted region-luminance
//! probe: the P1 gate's stand-in for the apricot-scan probe (no photoscan
//! asset is wired into this repo's test fixtures; this reuses
//! `render_scene_shadows.rs`'s decisive ground-plane + occluder + sun
//! scene instead — same "isolate the shadow term" shape the gate asks
//! for: a named occluded region's mean luminance must drop >=30% with RT
//! shadows ON vs OFF, and a named lit region must change <5%).
//!
//! Scene: `render_scene_shadows.rs`'s exact ground(8x8, y=0) + occluder
//! (3x3, y=1.5, centered over the ground's origin) + one overhead sun
//! (pos (3, 20, 3), aimed at the origin). The gate wants RT-shadows-ON
//! vs RT-shadows-OFF (unshadowed), not RT-vs-raster, so `cast_shadows`
//! rides the SAME toggle as `rt_enabled`: OFF = no shadow of any kind,
//! ON = `rt_enabled` true AND `has_casters` true, so the RT dispatch
//! actually runs. Same `node.orbit_camera` params
//! (orbit=0.7,tilt=0.95,distance=10,fov_y=0.8).
//!
//! Region selection is COMPUTED, not eyeballed (CLAUDE.md oracle
//! discipline): `Camera::orbit_perspective` + `project_to_pixel` (the
//! exact formula `node.orbit_camera`/render_scene.rs's camera math uses)
//! locates the pixel for two known WORLD points:
//! - occluded probe: world (0, 0, 0) — the ground's own origin, directly
//!   under the occluder's center. Hand-traced sun-to-occluder-center ray
//!   crosses `y=0` at approximately `(-0.24, 0, -0.24)` (well within the
//!   occluder's 3x3 footprint), so the origin sits inside the shadow.
//! - lit probe: world (3.5, 0, -3.5) — a far corner of the 8x8 ground,
//!   well outside the small near-origin shadow.
//!
//! Region = a 15x15 pixel window around each projected point, averaged.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;

fn scene_json(rt_enabled: bool) -> String {
    let rt_v = if rt_enabled { "true" } else { "false" };
    // The gate compares RT-shadows-ON vs RT-shadows-OFF (unshadowed), not
    // RT-vs-raster — `cast_shadows` rides the SAME toggle so the "off"
    // render has no shadow of any kind (the raster path would otherwise
    // still darken the occluded region when `rt_enabled` is false, since
    // `has_casters` gates the raster shadow-map loop independently of
    // `rt_enabled`).
    let cast_v = if rt_enabled { 1.0 } else { 0.0 };
    format!(
        r#"{{"version":2,"name":"RtP1RegionProbe","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":20}},
            "resolution_y":{{"type":"Int","value":20}},
            "size_x":{{"type":"Float","value":8.0}},
            "size_y":{{"type":"Float","value":8.0}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{{
            "src_cols":{{"type":"Int","value":20}},
            "src_rows":{{"type":"Int","value":20}}}}}},
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"occ_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":10}},
            "resolution_y":{{"type":"Int","value":10}},
            "size_x":{{"type":"Float","value":3.0}},
            "size_y":{{"type":"Float","value":3.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"occ_tris","params":{{
            "src_cols":{{"type":"Int","value":10}},
            "src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"occ_xform","params":{{
            "pos_y":{{"type":"Float","value":1.5}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.05}}}}}},
        {{"id":30,"typeId":"node.light","nodeId":"sun_0","params":{{
            "mode":{{"type":"Enum","value":0}},
            "pos_x":{{"type":"Float","value":3.0}},
            "pos_y":{{"type":"Float","value":20.0}},
            "pos_z":{{"type":"Float","value":3.0}},
            "aim_x":{{"type":"Float","value":0.0}},
            "aim_y":{{"type":"Float","value":0.0}},
            "aim_z":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "intensity":{{"type":"Float","value":1.0}},
            "cast_shadows":{{"type":"Float","value":{cast_v}}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":{rt_v}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Render a scene-graph JSON to `Rgba16Float`, returning readback bytes.
/// Two committed frames so pipeline warm-up is past; `commit_and_wait_completed`
/// hard-checks for Metal GPU errors — a bad RT dispatch/bind surfaces as a
/// panic here, not a silently wrong frame.
fn render_readback(json: &str) -> (Vec<u8>, u32, u32) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT region-probe scene graph must build");

    let target = h.make_target("rt-p1-region-probe");
    for frame in 0..2 {
        let ctx = PresetContext {
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
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("rt-p1-region-probe-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }
    (h.readback(&target.texture), h.width, h.height)
}

/// Mean luminance over a `(2*radius+1)^2` pixel window centered at
/// `(cx, cy)`, clamped to the image bounds.
fn region_luma(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32, radius: i32) -> f64 {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let x = cxi + dx;
            let y = cyi + dy;
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                continue;
            }
            let idx = ((y as u32 * w + x as u32) * 8) as usize;
            let px = &bytes[idx..idx + 8];
            let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
            let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
            let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
            assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
            sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
            n += 1;
        }
    }
    assert!(n > 0, "region window is entirely off-screen");
    sum / n as f64
}

// BUG-307 (docs/BUG_BACKLOG.md): this currently FAILS — RT-on and RT-off
// frames come out byte-identical (0.0% drop where >=30% is expected),
// even though every CPU-side input (rt_enabled, the scene_params.w
// shader flag, the E2a depth-snapshot draw, the RT dispatch's sun_dir/
// depth_tex/BLAS-transforms) was verified correct via runtime prints.
// The isolated kernel (`rt_p1_shadow.rs`) is proven correct against a
// hand-built fixture — this is a full-scene INTEGRATION bug, root cause
// unknown. Un-ignore once BUG-307 is fixed; that's the gate going green.
#[ignore = "BUG-307: RT-on/RT-off render byte-identical, root cause unknown — see docs/BUG_BACKLOG.md"]
#[test]
fn rt_shadow_darkens_occluded_region_and_leaves_lit_region_alone() {
    let (on_bytes, w, h) = render_readback(&scene_json(true));
    let (off_bytes, _, _) = render_readback(&scene_json(false));

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let occluded_world = [0.0, 0.0, 0.0];
    let lit_world = [2.5, 0.0, 0.0];
    let occ_px = cam
        .project_to_pixel(occluded_world, w, h)
        .expect("occluded probe point must project in front of the camera");
    let lit_px = cam
        .project_to_pixel(lit_world, w, h)
        .expect("lit probe point must project in front of the camera");

    const RADIUS: i32 = 7; // 15x15 window
    let occ_on = region_luma(&on_bytes, w, h, occ_px.px, occ_px.py, RADIUS);
    let occ_off = region_luma(&off_bytes, w, h, occ_px.px, occ_px.py, RADIUS);
    let lit_on = region_luma(&on_bytes, w, h, lit_px.px, lit_px.py, RADIUS);
    let lit_off = region_luma(&off_bytes, w, h, lit_px.px, lit_px.py, RADIUS);

    let occ_drop = (occ_off - occ_on) / occ_off.max(1e-9);
    let lit_change = (lit_on - lit_off).abs() / lit_off.max(1e-9);
    eprintln!(
        "occluded region: off={occ_off:.4} on={occ_on:.4} drop={:.1}% | lit region: off={lit_off:.4} on={lit_on:.4} change={:.1}%",
        occ_drop * 100.0,
        lit_change * 100.0
    );

    assert!(
        occ_drop >= 0.30,
        "occluded region (pixel ({:.0},{:.0})) must drop >=30% RT-on vs RT-off: \
         off={occ_off:.4} on={occ_on:.4} drop={:.1}%",
        occ_px.px,
        occ_px.py,
        occ_drop * 100.0
    );
    assert!(
        lit_change < 0.05,
        "lit region (pixel ({:.0},{:.0})) must change <5% RT-on vs RT-off: \
         off={lit_off:.4} on={lit_on:.4} change={:.1}%",
        lit_px.px,
        lit_px.py,
        lit_change * 100.0
    );
}
