//! RT_FURNACE_ORACLE: the ray tracer's first physically-closed-form
//! correctness oracle. Every existing RT test compares the renderer to
//! itself (RT-on vs RT-off, or one build vs another) — none compares to a
//! ground-truth value derived from physics. This file adds one.
//!
//! Both tests light a flat, fully-diffuse (Lambertian) albedo-1 PBR plane
//! with `node.bake_environment`'s new `uniform` mode: a CONSTANT radiance
//! `L` in every texel/direction, wired straight into `node.render_scene`'s
//! `envmap` input with zero direct lights and zero material ambient. The
//! closed-form physics: a Lambertian surface under a uniform field of
//! incident radiance `L` reflects back exactly `albedo * L` — the diffuse
//! BRDF (`albedo / pi`) integrated against `L * cos(theta)` over the
//! hemisphere is `albedo * L * pi / pi = albedo * L`. With albedo 1 and
//! `L = intensity = 1.0`, the surface must read exactly 1.0.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;

/// Same RT-D4 async-accel-build settle window `rt_bug17r3_lightless_gi.rs`
/// and `rt_p3_emissive_gi.rs` use.
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
    .expect("RT furnace scene graph must build");

    let target = h.make_target("rt-furnace-oracle");
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
        let mut enc = h.device.create_encoder("rt-furnace-oracle-enc");
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

/// Same region-average-luma probe `rt_bug17r3_lightless_gi.rs` uses.
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

/// A flat 8x8 ground plane (default `node.grid_mesh` orientation — lies in
/// the XZ plane at y=0, same convention `rt_bug17r3_lightless_gi.rs`'s
/// ground/emitter geometry uses), white albedo-1 Lambertian PBR material
/// (metallic 0, roughness 1.0 — fully diffuse, no specular lobe to
/// contaminate the reading), zero ambient, zero direct lights, lit only by
/// the uniform environment. `rt_reflections` off — roughness 1.0 makes
/// reflections physically irrelevant here, and turning them off keeps this
/// reading isolated to the diffuse IBL term under test.
fn white_surface_scene_json() -> String {
    format!(
        r#"{{"version":2,"name":"RtFurnaceWhiteSurface","nodes":[
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
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":8,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":64}},
            "height":{{"type":"Int","value":32}},
            "intensity":{{"type":"Float","value":1.0}},
            "uniform":{{"type":"Bool","value":true}}}}}},
        {{"id":4,"typeId":"node.pbr_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":0}},
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":false}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":8,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

#[test]
fn uniform_environment_white_surface_returns_the_environment_radiance() {
    let (bytes, w, h) = render_readback(&white_surface_scene_json());

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let center_px = cam
        .project_to_pixel([0.0, 0.0, 0.0], w, h)
        .expect("ground-plane centre must project in front of the camera");

    const RADIUS: i32 = 15;
    let measured = region_luma(&bytes, w, h, center_px.px, center_px.py, RADIUS);
    eprintln!("RT furnace white-surface: measured luma = {measured:.5} (physically expected 1.0)");

    const TOLERANCE: f64 = 0.10;
    assert!(
        (measured - 1.0).abs() <= TOLERANCE,
        "measured region luma {measured:.5} is not within {:.0}% of the physically-expected 1.0 \
         (a fully-diffuse albedo-1 surface under a uniform L=1.0 environment must reflect back \
         exactly L) — this is a REAL finding about the renderer's IBL diffuse term, not a tuning \
         knob on this test",
        TOLERANCE * 100.0,
    );
}

/// Same ground plane and white Lambertian material as
/// `white_surface_scene_json`, plus a second, non-emissive 3x3 occluder
/// grid hovering `0.6` world units above the ground centre (`transform_1`'s
/// `pos_y`) — mirroring `rt_bug17r3_lightless_gi.rs`'s emitter-quad
/// geometry, but opaque and unlit rather than emissive. `rt_enabled` is the
/// only thing that differs between the two renders this test compares.
fn occluded_contact_scene_json(rt_enabled: bool) -> String {
    format!(
        r#"{{"version":2,"name":"RtFurnaceOcclusion","nodes":[
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
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"occluder_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":10}},
            "resolution_y":{{"type":"Int","value":10}},
            "size_x":{{"type":"Float","value":3.0}},
            "size_y":{{"type":"Float","value":3.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"occluder_tris","params":{{
            "src_cols":{{"type":"Int","value":10}},
            "src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"occluder_xform","params":{{
            "pos_y":{{"type":"Float","value":0.6}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{DISTANCE}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":8,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":64}},
            "height":{{"type":"Int","value":32}},
            "intensity":{{"type":"Float","value":1.0}},
            "uniform":{{"type":"Bool","value":true}}}}}},
        {{"id":4,"typeId":"node.pbr_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":1.0}}}}}},
        {{"id":9,"typeId":"node.pbr_material","nodeId":"occluder_mat","params":{{
            "color_r":{{"type":"Float","value":0.5}},
            "color_g":{{"type":"Float","value":0.5}},
            "color_b":{{"type":"Float","value":0.5}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":0}},
            "rt_enabled":{{"type":"Bool","value":{rt_enabled}}},
            "rt_reflections":{{"type":"Bool","value":false}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":9,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":8,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#,
        rt_enabled = rt_enabled,
    )
}

/// EXPECTED TO FAIL — see the module doc and the assertion message below.
/// Traced occlusion today only modulates a small flat ambient term; the
/// environment diffuse term that dominates this scene is a baked-texture
/// lookup unaffected by ray-traced visibility, so the contact region barely
/// darkens even though physically it should darken substantially. This test
/// states the correct behaviour; it is the deliverable whether it passes or
/// fails.
#[test]
fn traced_occlusion_darkens_an_environment_lit_contact_region() {
    let (on_bytes, w, h) = render_readback(&occluded_contact_scene_json(true));
    let (off_bytes, _, _) = render_readback(&occluded_contact_scene_json(false));

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let contact_px = cam
        .project_to_pixel([0.0, 0.0, 0.0], w, h)
        .expect("occluder-shadow centre must project in front of the camera");

    const RADIUS: i32 = 10;
    let on = region_luma(&on_bytes, w, h, contact_px.px, contact_px.py, RADIUS);
    let off = region_luma(&off_bytes, w, h, contact_px.px, contact_px.py, RADIUS);
    eprintln!("RT furnace occlusion: rt_off={off:.5} rt_on={on:.5}");

    const DARKENING_FRACTION: f64 = 0.20;
    assert!(
        on <= off * (1.0 - DARKENING_FRACTION),
        "traced occlusion must darken the contact region under the occluder by at least \
         {:.0}% vs RT-off (that region sees far less of the environment, so ray-traced \
         occlusion must remove most of its incident radiance): rt_off={off:.5} rt_on={on:.5} \
         (ratio {:.4}) — if this fails, that is a real finding about the RT diffuse-occlusion \
         path, not a test to retune",
        DARKENING_FRACTION * 100.0,
        on / off,
    );
}
