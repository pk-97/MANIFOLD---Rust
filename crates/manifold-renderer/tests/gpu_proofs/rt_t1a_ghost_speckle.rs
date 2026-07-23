//! `docs/RAYTRACING_DESIGN.md` §8 T1-A — pre-fix oracles for BUG-311
//! (motion ghosting) and BUG-312 (static-shot speckle), through the real
//! node path (`rt_p1_region_probe`'s scene + camera-math precedent).
//!
//! Both oracles reuse `rt_p1_region_probe`'s exact ground(8x8, y=0) +
//! occluder(3x3, y=1.5) + overhead sun scene, RT shadows ON (so the
//! `node.render_scene` RT path is live: `rt_ready` gates the shadow-ray
//! dispatch AND the `accumulate_irradiance` same-texel history blend —
//! see `render_scene.rs`'s `IRRADIANCE_ACCUM_ALPHA`/`accumulate_irradiance`
//! call site — the exact mechanism BUG-311/BUG-312 live in).
//!
//! Camera motion (ghost oracle only) is driven WITHOUT rebuilding the
//! runtime — rebuilding would reset the irradiance history and hide the
//! very artifact under test. Instead `node.orbit_camera`'s `orbit` input
//! is port-shadowed by a small control chain: `system.generator_input.time`
//! -> `node.math`(Multiply, ORBIT_RATE) -> `node.math`(Add, ORBIT_BASE) ->
//! `cam.orbit`. Advancing `PresetContext.time` frame-to-frame (same runtime
//! instance, same accumulation state) sweeps the orbit smoothly.
//!
//! Both probes track a WORLD point (not a fixed screen pixel) via
//! `Camera::orbit_perspective` + `project_to_pixel` — the same
//! reconstruct-and-round-trip technique `rt_p1_region_probe` uses — so the
//! ghost metric isolates the accumulation artifact from ordinary "different
//! geometry passes through a fixed screen window as the camera orbits"
//! parallax, which would otherwise swamp the signal.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
const ORBIT_BASE: f32 = 0.7;

/// Same RT-D4 async-accel-build allowance `rt_p1_region_probe` uses:
/// enough frames for the request, the deferred build-enqueue, and real
/// wall-clock time for the tiny (~900-triangle) async build to complete.
const RT_WARMUP_FRAMES: i64 = 16;

/// Scene JSON: `rt_p1_region_probe`'s ground+occluder+sun scene, RT
/// shadows always ON (the "on" case is the only one either T1-A oracle
/// needs — no RT-vs-raster comparison here, unlike the P1 shadow-drop
/// gate). `orbit` is port-shadowed by a `system.generator_input.time` ->
/// Multiply(rate) -> Add(base) chain instead of a static param, so
/// `PresetContext.time` alone drives the camera without touching any
/// other frame state.
fn scene_json(orbit_rate: f32) -> String {
    format!(
        r#"{{"version":2,"name":"RtT1AGhostSpeckle","nodes":[
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
        {{"id":40,"typeId":"node.math","nodeId":"orbit_rate_mul","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":{orbit_rate}}},
            "op":{{"type":"Enum","value":2}}}}}},
        {{"id":41,"typeId":"node.math","nodeId":"orbit_add_base","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":{ORBIT_BASE}}},
            "op":{{"type":"Enum","value":0}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT_BASE}}},
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
            "cast_shadows":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":true}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":0,"fromPort":"time","toNode":40,"toPort":"a"}},
        {{"fromNode":40,"fromPort":"out","toNode":41,"toPort":"a"}},
        {{"fromNode":41,"fromPort":"out","toNode":3,"toPort":"orbit"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

fn make_ctx(h: &harness::ParityHarness, time: f64, frame_count: i64) -> PresetContext {
    PresetContext {
        time,
        beat: time * 0.5,
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

/// Mean luminance over a `(2*radius+1)^2` window, AND the per-pixel
/// luminance values (row-major within the window) so callers needing
/// spatial/temporal variance don't have to re-decode.
fn region_luma_values(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32, radius: i32) -> Vec<f64> {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let mut out = Vec::new();
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
            out.push((0.2126 * r + 0.7152 * g + 0.0722 * b) as f64);
        }
    }
    assert!(!out.is_empty(), "region window is entirely off-screen");
    out
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

fn variance(v: &[f64]) -> f64 {
    let m = mean(v);
    v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64
}

/// ORBIT/GHOST oracle. Threshold picked from the observed pre-fix number
/// (see the `#[ignore]` note below) with generous headroom above zero —
/// a correctly motion-reprojected accumulator tracking a WORLD point under
/// a fixed sun should show near-zero frame-to-frame luminance change here
/// (Lambertian diffuse shading is view-angle-independent), so any real
/// margin above ~0 is the ghost signal, not noise.
const GHOST_MEAN_ABS_DIFF_THRESHOLD: f64 = 0.02;

/// Small per-frame orbit sweep (rad per time-unit) — enough motion to
/// expose same-texel history drift without walking the tracked world
/// point off-screen across the sweep.
const ORBIT_RATE: f32 = 0.03;
const GHOST_FRAMES: usize = 12;
const GHOST_TIME_STEP: f64 = 1.0;

/// PRE-FIX BASELINE (recorded 2026-07-23, `accumulate_irradiance` same-
/// texel blend, no reprojection): mean abs consecutive-frame luminance
/// diff at the shadow-boundary world point, orbiting at `ORBIT_RATE` for
/// `GHOST_FRAMES` frames = 0.0444 (>> the 0.02 threshold this test
/// asserts; the flat far-corner probe `rt_p1_region_probe` uses for its
/// own lit-region gate reads <0.002 here — its irradiance is too spatially
/// uniform to expose the same-texel artifact, which is why this oracle
/// tracks the shadow boundary instead).
///
/// FIXED by T1-C (RAYTRACING_DESIGN.md §8 Tier-1 item 1, BUG-311):
/// `accumulate_irradiance` now reprojects history through `prev_view_proj`
/// before blending and rejects a depth/normal mismatch — this oracle is
/// flipped live (no longer `#[ignore]`d) and now asserts the metric is
/// BELOW threshold, the inverse of the pre-fix assertion above.
#[test]
fn rt_ghost_orbit_consecutive_frame_luma_diff_exceeds_threshold() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &scene_json(ORBIT_RATE),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT ghost-probe scene graph must build");
    let target = h.make_target("rt-t1a-ghost");

    let render_one = |runtime: &mut PresetRuntime, ctx: &PresetContext| {
        let mut enc = h.device.create_encoder("rt-t1a-ghost-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    };

    // Settle: warm up the accel structure + irradiance history at a FIXED
    // orbit (time=0 => orbit=ORBIT_BASE, same as rt_p1_region_probe).
    for frame in 0..RT_WARMUP_FRAMES {
        render_one(&mut runtime, &make_ctx(h, 0.0, frame));
    }

    // Tracked world point: `rt_p1_region_probe`'s "occluded region" world
    // point, sitting on the SHADOW BOUNDARY (a steep local ambient-
    // occlusion/GI gradient — the accumulated `irradiance` channel BUG-311
    // lives in) rather than the flat far-corner lit patch (near-uniform
    // irradiance there hides the same-texel history artifact almost
    // entirely — verified: swept and measured <0.002 there pre-fix).
    let ghost_world = [1.0_f32, 0.0, -1.0];

    let mut lumas = Vec::with_capacity(GHOST_FRAMES);
    for i in 0..GHOST_FRAMES {
        let time = i as f64 * GHOST_TIME_STEP;
        let orbit = ORBIT_BASE + ORBIT_RATE * time as f32;
        let cam = Camera::orbit_perspective(orbit, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
        let px = cam
            .project_to_pixel(ghost_world, h.width, h.height)
            .unwrap_or_else(|| panic!("lit probe point must stay on-screen through the sweep at frame {i} (orbit={orbit})"));

        render_one(&mut runtime, &make_ctx(h, time, RT_WARMUP_FRAMES + i as i64));
        let bytes = h.readback(&target.texture);
        let values = region_luma_values(&bytes, h.width, h.height, px.px, px.py, 7);
        lumas.push(mean(&values));
    }

    let diffs: Vec<f64> = lumas.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
    let mean_abs_diff = mean(&diffs);
    eprintln!(
        "ghost probe: lumas={lumas:?} consecutive-frame mean_abs_diff={mean_abs_diff:.4} (threshold={GHOST_MEAN_ABS_DIFF_THRESHOLD:.4})"
    );

    assert!(
        mean_abs_diff <= GHOST_MEAN_ABS_DIFF_THRESHOLD,
        "T1-C's motion-reprojected accumulation should keep consecutive-frame \
         luminance change at the tracked world point at or below \
         {GHOST_MEAN_ABS_DIFF_THRESHOLD} under camera orbit, got {mean_abs_diff:.4} \
         (pre-fix baseline was 0.0444) — lumas={lumas:?}"
    );
}

/// STILL/SPECKLE oracle. Static camera (orbit_rate=0), lighting settled —
/// measures (1) per-pixel TEMPORAL variance across `SPECKLE_FRAMES`
/// identical-camera frames in a flat lit region (should be ~0 for a static
/// scene with a converged accumulator) and (2) SPATIAL variance within
/// that same region on the last frame (a flat Lambertian patch under one
/// directional sun should read visually uniform — high spatial variance
/// is per-pixel speckle noise).
const SPECKLE_TEMPORAL_VARIANCE_THRESHOLD: f64 = 1e-5;
const SPECKLE_SPATIAL_VARIANCE_THRESHOLD: f64 = 7e-5;
const SPECKLE_FRAMES: usize = 8;

/// PRE-FIX BASELINE (recorded 2026-07-23, current `main`, depth-only
/// bilateral upsample + finite-difference normals, no SVGF filter, no
/// blue-noise ray sampling): temporal_variance = 1.1e-8 (well UNDER the
/// 1e-5 threshold — this scene/build has no per-frame ray-direction
/// jitter yet, so a static camera is already bit-stable frame-to-frame;
/// this metric is forward-looking for when T1-D adds blue-noise sampling
/// and needs a temporal filter to keep it converged). spatial_variance on
/// the last frame = 1.076e-4 (>> the 7e-5 threshold — the metric that
/// actually trips today: per-pixel AO/GI noise across a nominally flat lit
/// patch). The assertion is an OR across both metrics, so either one
/// tripping demonstrates the bug. Run without `#[ignore]` to reproduce;
/// T1-D (variance-guided SVGF-class spatial+temporal filter) is the fix
/// this gates.
#[test]
#[ignore = "BUG-312 pre-fix baseline — trips until T1-D lands the SVGF-class denoiser; see this test's doc comment for the recorded numbers"]
fn rt_still_frame_speckle_variance_exceeds_thresholds() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &scene_json(0.0),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT speckle-probe scene graph must build");
    let target = h.make_target("rt-t1a-speckle");

    let render_one = |runtime: &mut PresetRuntime, ctx: &PresetContext| {
        let mut enc = h.device.create_encoder("rt-t1a-speckle-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    };

    // Settle at the static orbit (time=0 throughout — orbit_rate=0.0 makes
    // this a no-op driver anyway, but keep the same warmup shape as the
    // ghost test for consistency).
    for frame in 0..RT_WARMUP_FRAMES {
        render_one(&mut runtime, &make_ctx(h, 0.0, frame));
    }

    let cam = Camera::orbit_perspective(ORBIT_BASE, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let lit_world = [2.5_f32, 0.0, -2.5];
    let px = cam
        .project_to_pixel(lit_world, h.width, h.height)
        .expect("lit probe point must project on-screen at the static orbit");

    let mut per_frame_region_values: Vec<Vec<f64>> = Vec::with_capacity(SPECKLE_FRAMES);
    for frame in 0..SPECKLE_FRAMES {
        render_one(
            &mut runtime,
            &make_ctx(h, 0.0, RT_WARMUP_FRAMES + frame as i64),
        );
        let bytes = h.readback(&target.texture);
        per_frame_region_values.push(region_luma_values(&bytes, h.width, h.height, px.px, px.py, 7));
    }

    let n_pixels = per_frame_region_values[0].len();
    let temporal_variances: Vec<f64> = (0..n_pixels)
        .map(|pixel_idx| {
            let series: Vec<f64> = per_frame_region_values.iter().map(|f| f[pixel_idx]).collect();
            variance(&series)
        })
        .collect();
    let temporal_variance = mean(&temporal_variances);

    let last_frame = per_frame_region_values.last().expect("at least one frame rendered");
    let spatial_variance = variance(last_frame);

    eprintln!(
        "speckle probe: temporal_variance={temporal_variance:.9} (threshold={SPECKLE_TEMPORAL_VARIANCE_THRESHOLD:.9}) \
         spatial_variance={spatial_variance:.9} (threshold={SPECKLE_SPATIAL_VARIANCE_THRESHOLD:.9})"
    );

    assert!(
        temporal_variance > SPECKLE_TEMPORAL_VARIANCE_THRESHOLD
            || spatial_variance > SPECKLE_SPATIAL_VARIANCE_THRESHOLD,
        "expected pre-fix speckle to trip at least one metric: \
         temporal_variance={temporal_variance:.6} (threshold={SPECKLE_TEMPORAL_VARIANCE_THRESHOLD}), \
         spatial_variance={spatial_variance:.6} (threshold={SPECKLE_SPATIAL_VARIANCE_THRESHOLD})"
    );
}
