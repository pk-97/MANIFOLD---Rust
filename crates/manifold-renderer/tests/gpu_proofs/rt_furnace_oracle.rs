//! RT_FURNACE_ORACLE: the ray tracer's first physically-closed-form
//! correctness oracle. Every existing RT test compares the renderer to
//! itself (RT-on vs RT-off, or one build vs another) — none compares to a
//! ground-truth value derived from physics. This file adds one.
//!
//! Every scene lights a flat, fully-diffuse (Lambertian) albedo-1 PBR plane
//! with `node.bake_environment`'s new `uniform` mode: a CONSTANT radiance
//! `L` in every texel/direction, wired straight into `node.render_scene`'s
//! `envmap` input with zero direct lights and zero emission. The
//! closed-form physics: a Lambertian surface under a uniform field of
//! incident radiance `L` reflects back exactly `albedo * L` — the diffuse
//! BRDF (`albedo / pi`) integrated against `L * cos(theta)` over the
//! hemisphere is `albedo * L * pi / pi = albedo * L`. With albedo 1 and
//! `L = intensity = 1.0`, the surface must read exactly 1.0.
//!
//! The occlusion tests below probe two regions of ONE render rather than
//! toggling `rt_enabled` between two renders — `furnace_occlusion_scene_json`'s
//! doc comment states why an on/off toggle is the wrong comparison here (it
//! is self-defeating: with the ground material's `ambient` at zero, the RT
//! irradiance term is algebraically zero either way). Every RT-on render in
//! this file confirms the RT kernel actually dispatched
//! (`harness::assert_rt_dispatched`) rather than trusting a pixel diff to
//! rule out "RT never ran at all" (BUG-1l7f's failure class).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

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

fn build_runtime(json: &str) -> (PresetRuntime, RenderTarget) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let runtime = PresetRuntime::from_json_str_with_device(
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
    (runtime, target)
}

fn render_frame(runtime: &mut PresetRuntime, target: &RenderTarget, frame_count: i64) {
    let h = harness::shared();
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
        frame_count,
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

/// Build, settle `RT_WARMUP_FRAMES`, read back, then CONFIRM the RT kernel
/// actually dispatched on that settled state (`harness::assert_rt_dispatched`
/// — `render_scene` only pushes capture slots from inside its `rt_enabled &&
/// rt_ready` branch, which no pixel comparison can substitute for). BUG-1l7f
/// names the exact failure mode this guards: a scene that never traces at
/// all still produces SOME pixels, and a bare byte comparison cannot tell
/// "RT ran and did nothing new" apart from "RT never ran". The readback
/// happens before the confirmation frame, so the extra frame this runs
/// (frame `RT_WARMUP_FRAMES`) never contaminates the measured bytes — on
/// these static scenes it would render identically anyway.
fn render_readback_confirmed(json: &str, context: &str) -> (Vec<u8>, u32, u32) {
    let h = harness::shared();
    let (mut runtime, target) = build_runtime(json);
    for frame in 0..RT_WARMUP_FRAMES {
        render_frame(&mut runtime, &target, frame);
    }
    let bytes = h.readback(&target.texture);
    harness::assert_rt_dispatched(|| render_frame(&mut runtime, &target, RT_WARMUP_FRAMES), context);
    (bytes, h.width, h.height)
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

/// The occluder carries the SAME albedo-1 white material as the ground, on
/// purpose. An earlier revision painted it 0.5 grey, and this gate passed at
/// `ratio 0.5077` — reading the plate's own albedo as if it were shade. The
/// probe under the plate sees the PLATE, not the floor, so a darker occluder
/// manufactures the exact number the test is looking for. Matching the albedo
/// removes that confound: with both surfaces white the ratio is `1.0010`, i.e.
/// no darkening anywhere in the frame. Never reintroduce a colour difference
/// between these two materials.
///
/// World-space probe points on `furnace_occlusion_scene_json`'s ground
/// plane: `SHADED_WORLD` sits directly under the occluder's centre,
/// `OPEN_WORLD` sits well clear of it (the occluder is a 3x3 plate centred
/// on the origin — 2.2 world units out on both axes clears it with margin)
/// while staying inside the camera's visible footprint on the plane.
const SHADED_WORLD: [f32; 3] = [0.0, 0.0, 0.0];
const OPEN_WORLD: [f32; 3] = [2.2, 0.0, 2.2];

/// Same ground plane and white Lambertian material as
/// `white_surface_scene_json`, plus a second, non-emissive 3x3 occluder
/// grid hovering `0.6` world units above the ground centre (`transform_1`'s
/// `pos_y`) — mirroring `rt_bug17r3_lightless_gi.rs`'s emitter-quad
/// geometry, but opaque and unlit rather than emissive. `rt_enabled` is
/// always true — this is ONE render, probed at two screen regions, not an
/// RT-on/RT-off toggle (an RT-off comparison here would be self-defeating:
/// with `ground_ambient` at 0.0, no lights, and no emission, the RT
/// irradiance term `ambient_color * ao + gi` is algebraically zero either
/// way, so toggling `rt_enabled` changes nothing by construction and
/// proves nothing about whether occlusion traced correctly).
/// `ground_ambient` is exposed so the diagnostic test below can compare the
/// shaded-region reading at ambient 0.0 vs 1.0.
fn furnace_occlusion_scene_json(ground_ambient: f32) -> String {
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
            "ambient":{{"type":"Float","value":{ground_ambient}}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":1.0}}}}}},
        {{"id":9,"typeId":"node.pbr_material","nodeId":"occluder_mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":0}},
            "rt_enabled":{{"type":"Bool","value":true}},
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
        ]}}"#
    )
}

/// The real gate, one render, two regions. The physics is unarguable: the
/// region directly under the occluder sees a small fraction of the uniform
/// environment, the open region on the same plane sees essentially all of
/// it, so the shaded region must read substantially darker — no RT-on/
/// RT-off toggle needed or wanted (see `furnace_occlusion_scene_json`'s
/// doc comment for why that comparison is self-defeating here).
/// `assert_rt_dispatched` (inside `render_readback_confirmed`) rules out
/// the vacuous case where this whole scene rendered pure raster.
///
/// FAILS TODAY, measured: `open=0.97266 shaded=0.97362 ratio=1.0010` — no
/// darkening anywhere. A frame dump of this exact scene is flat white: a
/// plate hovering over a floor under a uniform sky, RT on, and no contact
/// shadow at all. Cause is architectural, not probe placement. The kernel
/// writes `irradiance = ambient_color * ao + gi`, and here `ambient_color`
/// is zero (material ambient 0.0) and `gi` is zero (no lights, so no sun
/// casters; no emissives), so traced occlusion has nothing to multiply.
/// Meanwhile the term that actually lights this scene, `diffuse_ibl`, is
/// gated only by the material's BAKED occlusion texture and never sees the
/// traced result. No probe placement rescues this.
///
/// Un-ignore with BUG-yq1d (traced-ao-never-darkens-environment-diffuse).
#[test]
#[ignore = "BUG-yq1d (traced-ao-never-darkens-environment-diffuse): traced occlusion multiplies a term that is zero in an environment-lit scene"]
fn traced_occlusion_darkens_the_shaded_region_relative_to_the_open_plane() {
    let json = furnace_occlusion_scene_json(0.0);
    let (bytes, w, h) = render_readback_confirmed(&json, "furnace occlusion scene, ambient=0.0, RT enabled");

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let shaded_px = cam
        .project_to_pixel(SHADED_WORLD, w, h)
        .expect("shaded probe must project in front of the camera");
    let open_px = cam
        .project_to_pixel(OPEN_WORLD, w, h)
        .expect("open probe must project in front of the camera");

    const RADIUS: i32 = 8;
    let shaded = region_luma(&bytes, w, h, shaded_px.px, shaded_px.py, RADIUS);
    let open = region_luma(&bytes, w, h, open_px.px, open_px.py, RADIUS);
    eprintln!(
        "RT furnace occlusion (single render): open={open:.5} shaded={shaded:.5} ratio(shaded/open)={:.4}",
        shaded / open
    );

    const DARKENING_FRACTION: f64 = 0.20;
    assert!(
        shaded <= open * (1.0 - DARKENING_FRACTION),
        "the region directly under the occluder must read at least {:.0}% darker than the open \
         region on the SAME plane in the SAME render (it sees a small fraction of the uniform \
         environment; the open region sees essentially all of it): open={open:.5} \
         shaded={shaded:.5} ratio={:.4} — if this fails, that is a real finding about the RT \
         diffuse-occlusion path (or the raster environment-diffuse term it doesn't reach), not a \
         test to retune",
        DARKENING_FRACTION * 100.0,
        shaded / open,
    );
}

/// DIAGNOSTIC, not a gate — no assertion on which reading is "correct".
/// `furnace_occlusion_scene_json`'s doc comment names the mechanism: with
/// the ground material's `ambient` at 0.0 and no lights/emission, the RT
/// irradiance term (`ambient_color * ao + gi`) is algebraically zero
/// regardless of whether occlusion traced correctly, because there is
/// nothing for it to modulate. This records whether lifting `ambient` off
/// zero is what makes traced occlusion visible at the shaded probe at all
/// — evidence for or against the "occlusion only reaches a flat ambient
/// term, never the baked environment-diffuse term" hypothesis, without
/// this test taking a position on it.
#[test]
fn diagnostic_shaded_region_luma_with_ambient_zero_vs_ambient_one() {
    let zero_json = furnace_occlusion_scene_json(0.0);
    let one_json = furnace_occlusion_scene_json(1.0);
    let (zero_bytes, w, h) =
        render_readback_confirmed(&zero_json, "furnace occlusion scene, ambient=0.0, RT enabled");
    let (one_bytes, _, _) =
        render_readback_confirmed(&one_json, "furnace occlusion scene, ambient=1.0, RT enabled");

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let shaded_px = cam
        .project_to_pixel(SHADED_WORLD, w, h)
        .expect("shaded probe must project in front of the camera");

    const RADIUS: i32 = 8;
    let ambient_zero = region_luma(&zero_bytes, w, h, shaded_px.px, shaded_px.py, RADIUS);
    let ambient_one = region_luma(&one_bytes, w, h, shaded_px.px, shaded_px.py, RADIUS);
    eprintln!(
        "RT furnace occlusion diagnostic: shaded region luma at ambient=0.0 -> {ambient_zero:.5}, \
         at ambient=1.0 -> {ambient_one:.5}"
    );
}

/// Same Reinhard+gamma tonemap `render_scene_shadows.rs`'s `write_png` uses.
fn write_png(bytes: &[u8], w: u32, h: u32, path: &str) {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in bytes.chunks_exact(8) {
        for c in 0..4 {
            let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
            let mapped = (v / (1.0 + v)).clamp(0.0, 1.0);
            out.push((mapped.powf(1.0 / 2.2) * 255.0).round() as u8);
        }
    }
    image::save_buffer(path, &out, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
}

/// Raw (untonemapped) linear RGB of one pixel — the value `region_luma`
/// averages over a window, read at a single texel instead.
fn raw_pixel_rgb(bytes: &[u8], w: u32, px: f32, py: f32) -> [f32; 3] {
    let x = px.round() as u32;
    let y = py.round() as u32;
    let idx = ((y * w + x) * 8) as usize;
    let p = &bytes[idx..idx + 8];
    [
        f16::from_le_bytes([p[0], p[1]]).to_f32(),
        f16::from_le_bytes([p[2], p[3]]).to_f32(),
        f16::from_le_bytes([p[4], p[5]]).to_f32(),
    ]
}

/// DIAGNOSTIC — establishes what surface the "shaded" probe in
/// `traced_occlusion_darkens_the_shaded_region_relative_to_the_open_plane`
/// is actually reading, per the lead's kernel read: `crates/manifold-gpu/
/// src/metal/raytrace.rs`'s `gi` only accumulates from `hit_emissive` and a
/// sun-caster (`kind == 0`) bounce term; this scene has zero lights (no
/// casters) and a non-emissive occluder, so `gi` is exactly zero, and with
/// `ambient` at 0.0 `ambient_color` is zero too — the RT irradiance term
/// `ambient_color * ao + gi` this scene produces is 0 by construction,
/// regardless of whether occlusion traced correctly. The leading
/// alternative explanation: `SHADED_WORLD` is the point directly under the
/// occluder's centre, and the occluder hovers only 0.6 units above it — a
/// camera ray to that ground point at this file's oblique (not straight-
/// down) framing may pass through the occluder plate itself before
/// reaching the ground, in which case the "shaded" reading is the
/// occluder's OWN albedo reflecting the same uniform environment directly
/// (the same closed-form `albedo * L` this file's white-surface test
/// checks), not an occlusion effect on the ground at all. If the occluder
/// material's own albedo matches the ground's, this alternative predicts
/// the shaded reading should be close to the OPEN reading and to the
/// ground-only measurement, not darker than either.
///
/// Renders the two-object scene (occluder present) and the ground-only
/// scene (`white_surface_scene_json`, occluder absent) at the SAME camera,
/// probes the identical `SHADED_WORLD` pixel in both, dumps the two-object
/// framebuffer to PNG, and prints the raw per-channel RGB (not the
/// region-averaged luma) at that one texel for both scenes. No assertion —
/// this is "find out", not a gate.
#[test]
fn diagnose_what_the_shaded_probe_pixel_actually_shows() {
    let occluder_json = furnace_occlusion_scene_json(0.0);
    let ground_only_json = white_surface_scene_json();

    let (occluder_bytes, w, h) =
        render_readback_confirmed(&occluder_json, "furnace occlusion scene (occluder present), RT enabled");
    let (ground_only_bytes, _, _) =
        render_readback_confirmed(&ground_only_json, "furnace ground-only scene (occluder absent), RT enabled");

    let png_path = "/tmp/rt_furnace_occluder_present.png";
    write_png(&occluder_bytes, w, h, png_path);

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let shaded_px = cam
        .project_to_pixel(SHADED_WORLD, w, h)
        .expect("shaded probe must project in front of the camera");

    const RADIUS: i32 = 8;
    let occluder_region = region_luma(&occluder_bytes, w, h, shaded_px.px, shaded_px.py, RADIUS);
    let ground_only_region = region_luma(&ground_only_bytes, w, h, shaded_px.px, shaded_px.py, RADIUS);
    let occluder_pixel = raw_pixel_rgb(&occluder_bytes, w, shaded_px.px, shaded_px.py);
    let ground_only_pixel = raw_pixel_rgb(&ground_only_bytes, w, shaded_px.px, shaded_px.py);

    eprintln!(
        "RT furnace shaded-probe diagnosis: pixel=({:.0},{:.0}) \
         occluder-present region_luma={occluder_region:.5} raw_rgb={occluder_pixel:?} | \
         occluder-absent (ground-only) region_luma={ground_only_region:.5} raw_rgb={ground_only_pixel:?} | \
         PNG={png_path}",
        shaded_px.px, shaded_px.py,
    );
}
