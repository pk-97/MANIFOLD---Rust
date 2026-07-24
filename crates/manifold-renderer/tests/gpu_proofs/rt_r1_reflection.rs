//! `docs/RAYTRACING_DESIGN.md` §9.6 R1 gate (a) — mirror reflection probe:
//! metallic/roughness-0 ground plane with one emissive quad at a known world
//! position above it. The reflection leg (`rt_reflections`=TRUE) must show the
//! emissive quad's mirror image above a threshold; the control leg
//! (`rt_reflections`=FALSE) must show only the dummy envmap (below a floor).
//!
//! Scene: 8x8 ground plane at y=0 with PBR material (metallic=1.0,
//! roughness=0.01 = effective mirror — the roughness 0.0 floor is 0.01 per
//! `pbr_material.rs`'s GGX clamp), one small emissive quad at a known world
//! position above the ground, one overhead light for PBR material validity,
//! and a dummy (intensity=0) envmap so the control leg yields near-zero IBL
//! contribution.
//!
//! The quad's mirror image appears on the ground at the intersection of y=0
//! with the line from the camera position through the virtual image
//! `(emissive_x, -emissive_y, emissive_z)`. `Camera::orbit_perspective` +
//! `project_to_pixel` locates this on-screen per the same computed-pixel
//! discipline as `rt_p1_region_probe.rs`.

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

// LEAD: expectation math — the emissive quad world position. These coords
// determine where the mirror image appears on the ground plane.
const EMISSIVE_X: f32 = 0.0;
const EMISSIVE_Y: f32 = 0.8;
const EMISSIVE_Z: f32 = 2.0;

/// Build the scene JSON with `rt_reflections` toggled. Both legs share this
/// single fixture builder — only the Bool param differs between the two
/// render calls.
fn scene_json(rt_reflections: bool) -> String {
    let rt_v = if rt_reflections { "true" } else { "false" };
    format!(
        r#"{{"version":2,"name":"RtR1ReflectionProbe","nodes":[
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
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"quad_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":4}},
            "resolution_y":{{"type":"Int","value":4}},
            "size_x":{{"type":"Float","value":1.0}},
            "size_y":{{"type":"Float","value":1.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"quad_tris","params":{{
            "src_cols":{{"type":"Int","value":4}},
            "src_rows":{{"type":"Int","value":4}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"quad_xform","params":{{
            "pos_x":{{"type":"Float","value":{EMISSIVE_X}}},
            "pos_y":{{"type":"Float","value":{EMISSIVE_Y}}},
            "pos_z":{{"type":"Float","value":{EMISSIVE_Z}}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":30,"typeId":"node.light","nodeId":"sun","params":{{
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
            "cast_shadows":{{"type":"Float","value":1.0}}}}}},
        {{"id":4,"typeId":"node.pbr_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":0.8}},
            "color_g":{{"type":"Float","value":0.8}},
            "color_b":{{"type":"Float","value":0.8}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":1.0}},
            "roughness":{{"type":"Float","value":0.01}}}}}},
        {{"id":8,"typeId":"node.pbr_material","nodeId":"quad_mat","params":{{
            "color_r":{{"type":"Float","value":0.5}},
            "color_g":{{"type":"Float","value":0.5}},
            "color_b":{{"type":"Float","value":0.5}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":0.5}},
            "emission_r":{{"type":"Float","value":1.0}},
            "emission_g":{{"type":"Float","value":0.2}},
            "emission_b":{{"type":"Float","value":0.1}},
            "emission_intensity":{{"type":"Float","value":10.0}}}}}},
        {{"id":10,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":16}},
            "height":{{"type":"Int","value":8}},
            "intensity":{{"type":"Float","value":0.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":{rt_v}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":10,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Render a scene-graph JSON to `Rgba16Float`, returning readback bytes.
/// Shares the `RT_WARMUP_FRAMES` / async-accel pattern from
/// `rt_p1_region_probe.rs` — the accel build is async and deferred one frame,
/// so we commit enough frames for: (1) the request frame, (2) the deferred
/// build-enqueue frame, and (3) real wall-clock time for the tiny async build
/// to complete before readback.
const RT_WARMUP_FRAMES: i64 = 16;

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
    .expect("RT R1 reflection scene graph must build");

    let target = h.make_target("rt-r1-reflection-probe");
    for frame in 0..RT_WARMUP_FRAMES {
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
        let mut enc = h.device.create_encoder("rt-r1-reflection-enc");
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

/// Mirror reflection probe: metallic/roughness-0 ground plane with one
/// emissive quad above it. The emissive quad's mirror image (reflected across
/// y=0) appears on the ground at a computed world point. With RT reflections
/// ON the traced ray hits the emitter and returns bright; with OFF the dummy
/// envmap yields near-zero luminance.
///
/// LEAD: expectation math — fill in:
/// - `reflection_world`: the ground-plane intersection point of the line from
///   the camera position through the virtual image `(EMISSIVE_X, -EMISSIVE_Y,
///   EMISSIVE_Z)`. Solve `camera_pos + t * (virtual_image - camera_pos)` at
///   y=0.
/// - `threshold_on`: minimum mean luminance in the 15x15 probe window when
///   `rt_reflections` is true (the traced reflection of the emissive quad).
/// - `ceiling_off`: maximum mean luminance when `rt_reflections` is false
///   (the dummy envmap + ambient base).
#[test]
fn mirror_reflection_of_emissive_quad_appears_only_when_rt_reflections_enabled() {
    let (refl_bytes, w, h) = render_readback(&scene_json(true));
    let (ctrl_bytes, _, _) = render_readback(&scene_json(false));

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);

    // Expectation math (lead): the emitter's virtual image across the y=0
    // plane is (EMISSIVE_X, -EMISSIVE_Y, EMISSIVE_Z); its mirror image
    // appears where the segment camera → virtual image crosses y=0.
    // Computed from the camera's own public `pos` — no hand-derived
    // constants to drift from the scene above.
    let c = cam.pos;
    let virtual_image = [EMISSIVE_X, -EMISSIVE_Y, EMISSIVE_Z];
    let t = c[1] / (c[1] - virtual_image[1]);
    let reflection_world = [
        c[0] + t * (virtual_image[0] - c[0]),
        0.0,
        c[2] + t * (virtual_image[2] - c[2]),
    ];

    let rfl_px = cam
        .project_to_pixel(reflection_world, w, h)
        .expect("reflection probe point must project in front of the camera");

    const RADIUS: i32 = 7; // 15x15 window
    let luma_on = region_luma(&refl_bytes, w, h, rfl_px.px, rfl_px.py, RADIUS);
    let luma_off = region_luma(&ctrl_bytes, w, h, rfl_px.px, rfl_px.py, RADIUS);

    // LEAD: expectation math — fill in these thresholds
    let threshold_on = 0.1;  // LEAD: minimum luminance with RT reflections ON
    let ceiling_off = 0.01;  // LEAD: maximum luminance with RT reflections OFF

    eprintln!(
        "reflection region (pixel ({:.0},{:.0})): on={luma_on:.4} off={luma_off:.4} | \
         threshold_on={threshold_on} ceiling_off={ceiling_off}",
        rfl_px.px, rfl_px.py,
    );

    assert!(
        luma_on >= threshold_on,
        "reflection region (pixel ({:.0},{:.0})) must be >={threshold_on} with \
         RT reflections ON: got {luma_on:.4}",
        rfl_px.px,
        rfl_px.py,
    );
    assert!(
        luma_off <= ceiling_off,
        "reflection region (pixel ({:.0},{:.0})) must be <={ceiling_off} with \
         RT reflections OFF: got {luma_off:.4}",
        rfl_px.px,
        rfl_px.py,
    );
}

/// R1 gate (d): frame-time discipline — the reflection dispatch adds rays
/// to the existing trace kernel; the per-frame budget must not exceed 20ms
/// (no frame hitch). Same `RT_WARMUP_FRAMES` and async-accel pattern as
/// the region-probe frame-time gate.
///
/// LEAD: no expectation math needed here — the 20ms/hitch assertion is the
/// standing budget from P1's gate; the measured `trace_ms` delta (reflections
/// on vs off) is reported in the phase report.
#[test]
fn rt_reflections_dispatch_never_stalls_past_20ms() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &scene_json(true),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT R1 scene graph must build");
    let target = h.make_target("rt-r1-frame-time");

    let mut worst: (u32, std::time::Duration) = (0, std::time::Duration::ZERO);
    for frame in 0..RT_WARMUP_FRAMES {
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
        let start = std::time::Instant::now();
        let mut enc = h.device.create_encoder("rt-r1-frame-time-enc");
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
        let elapsed = start.elapsed();
        eprintln!("frame {frame}: {:.2}ms", elapsed.as_secs_f64() * 1000.0);

        const WARMUP_FRAMES_EXEMPT: i64 = 2;
        if frame >= WARMUP_FRAMES_EXEMPT && elapsed > worst.1 {
            worst = (frame as u32, elapsed);
        }
        assert!(
            frame < WARMUP_FRAMES_EXEMPT || elapsed.as_secs_f64() * 1000.0 <= 20.0,
            "frame {frame} took {:.2}ms (>20ms budget) — the reflection dispatch must not hitch",
            elapsed.as_secs_f64() * 1000.0
        );
    }
    eprintln!(
        "worst post-warmup frame: {} at {:.2}ms",
        worst.0,
        worst.1.as_secs_f64() * 1000.0
    );
}
