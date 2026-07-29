//! `docs/RAYTRACING_DESIGN.md` section 9.6 R2 gate (Step 4) — specular temporal
//! accumulation blend-engagement + cut-reset gate.
//!
//! RULE: The reprojected specular history is variance-clipped (BUG-dx6w) to
//! mean ± RT_REFL_CLAMP_GAMMA·stddev of the CURRENT frame's 3x3 `hi_refl`
//! neighborhood, then blended 10% per frame (0.9 * clamped history +
//! 0.1 * current raw). An owner-key change (cut) resets the accumulator to
//! the raw trace immediately, same as before.
//!
//! Fixture: mirror plane (roughness 0.01, metallic 1.0) at y=0, one emissive
//! quad at (0, 0.8, 2.0) whose emission_intensity is time-driven via a
//! node.math chain from system.generator_input.time:
//!
//!   clamp((time - STEP_T) * 1e6, 0, 1) * 10 + 10
//!
//! → emission = 10 before STEP_T, 20 at/after. Static camera. Dummy env
//! (intensity 0).
//!
//! Three measurements per run:
//! - B = baseline (emission 10, converged after 16 warmup + 6 motion frames)
//! - a_nc = step+1 frame, NO owner change
//! - a_c = step+1 frame, owner_key 1 (accumulation resets, raw 2*B)
//!
//! Theory (post-clamp): at the mirror interior the current frame's 3x3
//! `hi_refl` neighborhood around the step+1 raw trace (≈2*B) is NOT
//! perfectly zero-variance in practice — real texel-level dithering/AA
//! gradient gives the box nonzero width — so the stale ≈B history clamps
//! partway toward ≈2*B rather than landing on it exactly. Measured 2026-07-29:
//! a_nc/B ≈ 1.67, clearly above the pre-clamp ≈1.1 (the clamp is engaging
//! and moving the result well past plain 10% blend) and below the cut
//! leg's ≈1.94, which never touches the blend/clamp path at all (raw
//! trace reset).
//!
//! GATE-MUST-FAIL discipline: if a_nc/B drops back to ≈1.1, that WAS the
//! pre-clamp pass value (2026-07-26) — it now means the clamp is not
//! engaging (stale history survived the variance box unclamped, sweep
//! trails are back). If a_c/B ≈ 1.1, the cut didn't reset (reset path
//! dead).
//! Measured values (2026-07-29): B=2.859054, A_nc=4.760989, A_c=5.538713.

use half::f16;
use manifold_core::params::ParamManifest;
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

// Emitter quad world position.
const EMISSIVE_X: f32 = 0.0;
const EMISSIVE_Y: f32 = 0.8;
const EMISSIVE_Z: f32 = 2.0;

// Step-function timing. Warmup at WARMUP_TIME converges emission=10.
// After warmup, motion frames advance by 1/60.
// Frame 5 at time=0.1+6/60=0.2=STEP_T: emission still 10 (baseline B).
// Frame 6 at time=0.1+7/60≈0.2167: emission jumps to 20 (step+1).
const WARMUP_TIME: f64 = 0.1;
const STEP_T: f64 = 0.2;
const WARMUP_FRAMES: i64 = 16;
// Indices within the motion-frame loop (0-based).
const BASELINE_FRAME: i64 = 5;
const STEP_PLUS_ONE: i64 = 6;

fn scene_json() -> String {
    format!(
        r#"{{"version":2,"name":"RtR2AccumGate","nodes":[
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
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":0.8}},
            "pos_z":{{"type":"Float","value":2.0}}}}}},
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
        {{"id":10,"typeId":"node.bake_environment","nodeId":"env","params":{{
            "width":{{"type":"Int","value":16}},
            "height":{{"type":"Int","value":8}},
            "intensity":{{"type":"Float","value":0.0}}}}}},
        {{"id":40,"typeId":"node.math","nodeId":"step_sub","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":{STEP_T}}},
            "op":{{"type":"Enum","value":1}}}}}},
        {{"id":41,"typeId":"node.math","nodeId":"step_mul_1e6","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":1000000.0}},
            "op":{{"type":"Enum","value":2}}}}}},
        {{"id":42,"typeId":"node.math","nodeId":"step_max_0","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":0.0}},
            "op":{{"type":"Enum","value":5}}}}}},
        {{"id":43,"typeId":"node.math","nodeId":"step_min_1","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":1.0}},
            "op":{{"type":"Enum","value":4}}}}}},
        {{"id":44,"typeId":"node.math","nodeId":"step_mul_10","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":10.0}},
            "op":{{"type":"Enum","value":2}}}}}},
        {{"id":45,"typeId":"node.math","nodeId":"step_add_10","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":10.0}},
            "op":{{"type":"Enum","value":0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":true}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":0,"fromPort":"time","toNode":40,"toPort":"a"}},
        {{"fromNode":40,"fromPort":"out","toNode":41,"toPort":"a"}},
        {{"fromNode":41,"fromPort":"out","toNode":42,"toPort":"a"}},
        {{"fromNode":42,"fromPort":"out","toNode":43,"toPort":"a"}},
        {{"fromNode":43,"fromPort":"out","toNode":44,"toPort":"a"}},
        {{"fromNode":44,"fromPort":"out","toNode":45,"toPort":"a"}},
        {{"fromNode":45,"fromPort":"out","toNode":8,"toPort":"emission_intensity"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":10,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Render warmup + motion frames and read back two frames:
/// - baseline: the last frame before the emission step (emission 10, converged)
/// - step_plus_one: first frame with emission 20 (owner_key=1 on this
///   frame if `reset_on_target`)
///
/// Returns (baseline_bytes, step_plus_one_bytes, width, height).
fn render_sequence(
    h: &harness::ParityHarness,
    json: &str,
    reset_on_target: bool,
) -> (Vec<u8>, Vec<u8>, u32, u32) {
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
    .expect("R2 accumulation gate scene must build");

    let target = h.make_target("rt-r2-accum-gate");

    // Warmup: all frames at WARMUP_TIME, emission 10.
    for frame in 0..WARMUP_FRAMES {
        let ctx = PresetContext {
            time: WARMUP_TIME,
            beat: WARMUP_TIME * 2.0,
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
        let mut enc = h.device.create_encoder("rt-r2-accum-gate-warmup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }

    // Motion frames up to and including BASELINE_FRAME (time 0.2, emission 10).
    for frame in 0..=BASELINE_FRAME {
        let t = WARMUP_TIME + (frame as f64 + 1.0) / 60.0;
        let ctx = PresetContext {
            time: t,
            beat: t * 2.0,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: WARMUP_FRAMES + frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("rt-r2-accum-gate-baseline");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }

    let baseline_bytes = h.readback(&target.texture);

    // Remaining motion frames: only STEP_PLUS_ONE (time 0.2167, emission 20).
    // If reset_on_target, the owner_key flips to 1 on this frame.
    {
        let t = WARMUP_TIME + (STEP_PLUS_ONE as f64 + 1.0) / 60.0;
        let owner = if reset_on_target { 1 } else { 0 };
        let ctx = PresetContext {
            time: t,
            beat: t * 2.0,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: owner,
            is_clip_level: false,
            frame_count: WARMUP_FRAMES + STEP_PLUS_ONE,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("rt-r2-accum-gate-step");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }

    let step_bytes = h.readback(&target.texture);
    (baseline_bytes, step_bytes, h.width, h.height)
}

/// Mean luminance over a `(2*radius+1)^2` pixel window centered at
/// `(cx, cy)`.
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

/// Mirror-pixel: virtual image of `world` across y=0, intersect
/// camera->virtual_image ray with y=0 plane, project to screen.
fn mirror_pixel(cam: &Camera, world: [f32; 3], w: u32, h: u32) -> (f32, f32) {
    let c = cam.pos;
    let virtual_image = [world[0], -world[1], world[2]];
    let t = c[1] / (c[1] - virtual_image[1]);
    let reflection_world = [
        c[0] + t * (virtual_image[0] - c[0]),
        0.0,
        c[2] + t * (virtual_image[2] - c[2]),
    ];
    let px = cam
        .project_to_pixel(reflection_world, w, h)
        .expect("mirror probe point must project in front of the camera");
    (px.px, px.py)
}

/// Specular temporal accumulation: variance-clip engagement (no-cut clamps
/// stale history into the current-frame neighborhood box before blending)
/// and cut reset (owner-key change discards history immediately).
///
/// Theory (BUG-dx6w clamp):
/// - B (converged at emission 10): baseline luminance
/// - a_nc (step+1, no cut): the stale ≈B history gets variance-clipped
///   toward the current frame's 3x3 `hi_refl` neighborhood (≈2*B, nonzero
///   width in practice) before the 0.9/0.1 blend, landing well above the
///   pre-clamp ≈1.1*B but below the cut leg's raw ≈2*B.
/// - a_c (step+1, cut via owner_key=1): raw trace ≈ 2 * B (unchanged, never
///   touches the blend/clamp path)
///
/// Bands (pinned from measured values 2026-07-29: B=2.859054,
/// A_nc=4.760989, A_c=5.538713 — a_nc/B≈1.665, a_c/B≈1.937):
/// - a_nc/B: [1.4, 2.2] — clearly separates the clamp-engaged result from
///   the pre-clamp ≈1.1 MUST-FAIL signature.
/// - a_c/B: [1.8, 2.2] — unchanged cut-reset band.
#[test]
fn specular_history_blends_without_cut_and_resets_on_cut() {
    let h = crate::harness::shared();
    let json = scene_json();

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let w = h.width;
    let hh = h.height;

    let (px, py) = mirror_pixel(&cam, [EMISSIVE_X, EMISSIVE_Y, EMISSIVE_Z], w, hh);

    const RADIUS: i32 = 7;

    // No-cut run: baseline B and step+1 a_nc (both owner_key 0).
    let (b_bytes, nc_bytes, _, _) = render_sequence(h, &json, false);
    let b = region_luma(&b_bytes, w, hh, px, py, RADIUS);
    let a_nc = region_luma(&nc_bytes, w, hh, px, py, RADIUS);

    // Cut run: step+1 a_c with owner_key 1 on the target frame.
    let (_, c_bytes, _, _) = render_sequence(h, &json, true);
    let a_c = region_luma(&c_bytes, w, hh, px, py, RADIUS);

    // Neighbor pixel 30 texels right of the mirror pixel (for sanity).
    let neighbor = region_luma(&b_bytes, w, hh, px + 30.0, py, RADIUS);

    let ratio_nc = a_nc / b;
    let ratio_c = a_c / b;

    eprintln!("R2 STEP 4 ACCUM GATE — measured values (2026-07-29, post-clamp BUG-dx6w)");
    eprintln!("  B={b:.6}  A_nc={a_nc:.6}  A_c={a_c:.6}");
    eprintln!("  a_nc/B={ratio_nc:.6}  a_c/B={ratio_c:.6}");
    eprintln!("  neighbor={neighbor:.6}  B/neighbor={:.4}", b / neighbor);
    eprintln!(
        "  mirror_pixel=({px:.0},{py:.0})  w={w} h={hh}"
    );

    // Sanity: emitter reflection is visible over the background.
    assert!(
        b > neighbor * 2.0,
        "Sanity fail: emitter not visible in reflection. B={b:.6} neighbor={neighbor:.6}",
    );

    // Clamp (BUG-dx6w): no-cut must land clearly above the pre-clamp ≈1.1
    // signature because the stale converged history gets variance-clipped
    // toward the current frame's neighborhood before the blend.
    assert!(
        ratio_nc > 1.4 && ratio_nc < 2.2,
        "Clamp fail: a_nc/B={ratio_nc:.6}. Expected ≈1.4-2.2 (clamp engaged, \
         measured ≈1.67 on 2026-07-29). If ≈1.1, the clamp is not engaging \
         — that was the pre-clamp pass value (2026-07-26), meaning stale \
         history is surviving unclamped (gate-must-fail, the sweep trail is \
         back).",
    );

    // Cut: owner change must reset to raw trace (~2x B).
    assert!(
        ratio_c > 1.8 && ratio_c < 2.2,
        "Cut reset fail: a_c/B={ratio_c:.6}. Expected ≈2.0 (raw trace). \
         If ≈1.1, cut path dead (gate-must-fail).",
    );
}

// =========================================================================
// Peter's R2 motion-quality artifact (D-61): the mirror scene under a fast
// camera sweep, specular accumulation active.
// =========================================================================

/// Warmup time — constant for all warmup frames.
const SWEEP_WARMUP_TIME: f64 = 0.1;

/// Number of warmup frames (convergence + acceleration).
const SWEEP_WARMUP_FRAMES: i64 = 16;

/// Number of sweep frames.
const SWEEP_FRAMES: i64 = 24;

/// Scene: mirror plane (roughness 0.01 mirror) at y=0, emissive quad at
/// (0, 0.8, 2.0), sun, dummy env, rt_enabled + rt_reflections.
/// Camera orbit is time-driven via two node.math nodes wired from
/// system.generator_input.time: orbit = 0.7 + time * 0.6.
fn sweep_scene_json() -> String {
    r#"{"version":2,"name":"RtR2SweepDump","nodes":[
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
        {"id":5,"typeId":"node.grid_mesh","nodeId":"quad_grid","params":{
            "max_capacity":{"type":"Int","value":8192},
            "resolution_x":{"type":"Int","value":4},
            "resolution_y":{"type":"Int","value":4},
            "size_x":{"type":"Float","value":1.0},
            "size_y":{"type":"Float","value":1.0}}},
        {"id":6,"typeId":"node.make_triangles","nodeId":"quad_tris","params":{
            "src_cols":{"type":"Int","value":4},
            "src_rows":{"type":"Int","value":4}}},
        {"id":7,"typeId":"node.transform_3d","nodeId":"quad_xform","params":{
            "pos_x":{"type":"Float","value":0.0},
            "pos_y":{"type":"Float","value":0.8},
            "pos_z":{"type":"Float","value":2.0}}},
        {"id":8,"typeId":"node.pbr_material","nodeId":"quad_mat","params":{
            "color_r":{"type":"Float","value":0.5},
            "color_g":{"type":"Float","value":0.5},
            "color_b":{"type":"Float","value":0.5},
            "ambient":{"type":"Float","value":0.0},
            "metallic":{"type":"Float","value":0.0},
            "roughness":{"type":"Float","value":0.5},
            "emission_r":{"type":"Float","value":1.0},
            "emission_g":{"type":"Float","value":0.2},
            "emission_b":{"type":"Float","value":0.1},
            "emission_intensity":{"type":"Float","value":10.0}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.95},
            "distance":{"type":"Float","value":10.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":30,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":3.0},
            "pos_y":{"type":"Float","value":20.0},
            "pos_z":{"type":"Float","value":3.0},
            "aim_x":{"type":"Float","value":0.0},
            "aim_y":{"type":"Float","value":0.0},
            "aim_z":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":1.0}}},
        {"id":4,"typeId":"node.pbr_material","nodeId":"ground_mat","params":{
            "color_r":{"type":"Float","value":0.8},
            "color_g":{"type":"Float","value":0.8},
            "color_b":{"type":"Float","value":0.8},
            "ambient":{"type":"Float","value":0.0},
            "metallic":{"type":"Float","value":1.0},
            "roughness":{"type":"Float","value":0.01}}},
        {"id":10,"typeId":"node.bake_environment","nodeId":"env","params":{
            "width":{"type":"Int","value":16},
            "height":{"type":"Int","value":8},
            "intensity":{"type":"Float","value":0.0}}},
        {"id":40,"typeId":"node.math","nodeId":"orbit_rate","params":{
            "a":{"type":"Float","value":0.0},
            "b":{"type":"Float","value":0.6},
            "op":{"type":"Enum","value":2}}},
        {"id":41,"typeId":"node.math","nodeId":"orbit_base","params":{
            "a":{"type":"Float","value":0.0},
            "b":{"type":"Float","value":0.7},
            "op":{"type":"Enum","value":0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":2},
            "lights":{"type":"Int","value":1},
            "rt_enabled":{"type":"Bool","value":true},
            "rt_reflections":{"type":"Bool","value":true}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
        ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"},
        {"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"},
        {"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"},
        {"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"},
        {"fromNode":0,"fromPort":"time","toNode":40,"toPort":"a"},
        {"fromNode":40,"fromPort":"out","toNode":41,"toPort":"a"},
        {"fromNode":41,"fromPort":"out","toNode":3,"toPort":"orbit"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":10,"fromPort":"envmap","toNode":20,"toPort":"envmap"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
        ]}"#
    .to_string()
}

/// Peter's R2 motion-quality artifact (D-61): the mirror scene under a fast
/// camera sweep, specular accumulation active. Ignored — run deliberately:
/// cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs rt_r2_sweep_dump -- --ignored --nocapture
#[test]
#[ignore = "demo dump for Peter, not a gate"]
fn rt_r2_sweep_dump() {
    let h = crate::harness::shared();
    let json = sweep_scene_json();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("R2 sweep dump scene must build");

    let target = h.make_target("rt-r2-sweep-dump");

    // 16 warmup frames at time 0.1 (accel + convergence).
    for frame in 0..SWEEP_WARMUP_FRAMES {
        let ctx = PresetContext {
            time: SWEEP_WARMUP_TIME,
            beat: SWEEP_WARMUP_TIME * 2.0,
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
        let mut enc = h.device.create_encoder("rt-r2-sweep-warmup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }

    // 24 sweep frames advancing time by 1/60 each.
    for frame in 0..SWEEP_FRAMES {
        let t = SWEEP_WARMUP_TIME + (frame as f64 + 1.0) / 60.0;
        let ctx = PresetContext {
            time: t,
            beat: t * 2.0,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: SWEEP_WARMUP_FRAMES + frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("rt-r2-sweep-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();

        // Readback, tonemap, encode PNG, write.
        let rgba = manifold_renderer::headless_readback::readback_tonemapped_rgba8(
            &h.device,
            &target.texture,
            h.width,
            h.height,
        );
        let png = manifold_renderer::headless_readback::encode_rgba8_png(
            &rgba,
            h.width,
            h.height,
        );
        let path = format!("/tmp/r2_sweep_frame_{:02}.png", frame);
        std::fs::write(&path, &png)
            .unwrap_or_else(|e| panic!("write {path}: {e}"));
        eprintln!("Wrote {path}");
    }
}
