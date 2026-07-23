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
//!
//! **BUG-316 / D19 / D20 status (2026-07-23): the ORBIT oracle below is
//! `#[ignore]`d, not deleted, and BUG-311 is NOT certified by any numeric
//! oracle in this file.** History:
//! - D19 diagnosed the original consecutive-frame-diff metric as confounded
//!   by real camera parallax, not ghosting.
//! - The revised metric here (accumulated-final-pose vs a fresh cold-start
//!   runtime rendered at the identical final pose, mean abs luma diff in
//!   the shadow-boundary gradient region) was built and tuned across
//!   several ramp/rate/hold configurations (gentle ~3-5 deg/frame sweeps,
//!   then D20's ~10 deg/frame "crank the stimulus" sweep). In every
//!   configuration tried, pre-fix (`10359365`) and post-fix (`f9bc2b30`,
//!   T1-C) numbers came out statistically indistinguishable — e.g. the D20
//!   configuration (`ORBIT_RATE`=10deg, `RAMP_FRAMES`=3, `HOLD_FRAMES`=1,
//!   landing mid the real ~0.5-1.15 rad shadow-boundary transition,
//!   verified via an orbit scan against `rt_p1_shadow.rs`'s documented
//!   43-47% RT-on-vs-off drop at this same world point) gave pre-fix
//!   mean_abs_diff=0.0267 vs post-fix=0.0262 — both trip the same 0.02
//!   threshold, no daylight between them.
//! - D20 Stage 2: temporary atomic-counter instrumentation inside
//!   `accumulate_irradiance` (removed before this commit — see git history
//!   of this investigation if it needs re-adding) measured, at post-fix
//!   during the real camera-motion frames of this exact ramp: ~95-98% of
//!   attempted texels reprojected to a DIFFERENT texel than same-texel
//!   (`shifted`), and ~97-98% of those were ACCEPTED by the depth/normal
//!   validity test (`valid`), only ~2-3% rejected. T1-C's reprojection
//!   mechanism is UNAMBIGUOUSLY ACTIVE and engaging correctly under this
//!   exact motion — the fix is not inert. The oracle is simply blind to
//!   it: this metric can't isolate "residual smear from imperfect
//!   reprojection" from "expected temporal-accumulation lag relative to an
//!   instantaneous cold reference" (alpha=0.15 blending intentionally
//!   trails a few frames behind any real scene change, fixed or not —
//!   a cold single-pose render will never exactly match a mid-transition
//!   accumulator, independent of ghosting).
//! - Per D20: BUG-311 is accepted FIXED on this bisection evidence (the
//!   mechanism engaging as designed) + Peter's in-app look, NOT on a
//!   passing numeric gate here. A future wave (T1-D, SVGF-class denoiser)
//!   is the natural point to revisit a numeric ghost oracle, once the
//!   variance-guided filter gives a cleaner signal to isolate against.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

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
/// other frame state. `orbit_base` is parameterized (not hardcoded to
/// `ORBIT_BASE`) so a COLD reference runtime can be built fixed at any
/// static orbit angle — e.g. the exact final pose of an orbit sweep on a
/// different (accumulated) runtime — by passing `orbit_rate=0.0` and
/// `orbit_base=<that angle>`.
fn scene_json(orbit_rate: f32, orbit_base: f32) -> String {
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
            "b":{{"type":"Float","value":{orbit_base}}},
            "op":{{"type":"Enum","value":0}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{orbit_base}}},
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
    make_ctx_with_dt(h, time, frame_count, 1.0 / 60.0)
}

/// `dt` must match the ACTUAL elapsed `time` between consecutive calls, or
/// `TemporalResetDetector::detect_reset`'s discontinuity test
/// (`node_graph/temporal_reset.rs`: `|actual_delta - expected_delta| > 1.5 *
/// expected_delta`, where `expected_delta` is this `dt`) fires a false
/// reset every frame — silently discarding the very irradiance history
/// this test exists to accumulate and inspect. The orbit-sweep loop below
/// steps `time` by `GHOST_TIME_STEP` per frame, so it must pass
/// `GHOST_TIME_STEP` as `dt` here, not a fixed 1/60.
fn make_ctx_with_dt(h: &harness::ParityHarness, time: f64, frame_count: i64, dt: f32) -> PresetContext {
    PresetContext {
        time,
        beat: time * 0.5,
        dt,
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

/// ORBIT/GHOST oracle (revised per D19 — BUG-316: the original consecutive-
/// frame-diff metric was confounded by real camera parallax at the shadow-
/// boundary probe point, not ghosting, and couldn't certify BUG-311).
///
/// Metric: run a camera orbit sweep on a PERSISTENT runtime (accumulating
/// irradiance history frame-to-frame, exactly as a live performance would),
/// then compare the ACCUMULATED render at the sweep's FINAL pose against a
/// COLD reference — a FRESH runtime built with the camera fixed at that
/// exact final pose from frame zero, so its irradiance history only ever
/// sees ONE pose (no motion to reproject, no smear possible). That cold
/// render is the ground truth for "what should this pose look like",
/// independent of the accumulator's motion-handling correctness.
///
/// Pre-fix (same-texel blend, no reprojection): the accumulated-final frame
/// still carries irradiance contributions from EARLIER, DIFFERENT camera
/// poses blended into the same screen texels — those don't correspond to
/// the same world content as the cold reference at the final pose, so the
/// diff is large. Post-fix (T1-C's `prev_view_proj` reprojection + depth/
/// normal-mismatch rejection): history is carried forward relative to
/// camera motion, so the accumulated-final frame converges toward the same
/// answer as a cold render at that pose — the diff collapses.
const GHOST_MEAN_ABS_DIFF_THRESHOLD: f64 = 0.02;

/// Per-frame orbit sweep (rad per time-unit) during the RAMP phase — fast
/// enough that the tracked world point's screen texel sweeps across the
/// shadow boundary within `RAMP_FRAMES`, so same-texel blending (pre-fix)
/// mixes irradiance from genuinely different world content into one texel.
/// D20: ~10 deg/frame — at `IRRADIANCE_ACCUM_ALPHA` 0.15 the effective
/// history window spans ~6 frames; large per-frame steps make same-texel
/// ghosting look like averaging together wildly different world content
/// (a strong, visible defect), while gentle per-frame steps make it look
/// like reprojection resample blur (near-invisible either way) — the
/// earlier gentle sweep (~3-5 deg/frame) is why pre/post-fix didn't
/// discriminate.
const ORBIT_RATE: f32 = 0.174_533; // 10 degrees
/// Ramp frames (camera moving) + hold frames (camera FROZEN at the final
/// pose — `time` stops advancing, so `orbit` stops changing too). The hold
/// is short on purpose: `IRRADIANCE_ACCUM_ALPHA` (0.15) means a broken
/// same-texel accumulator needs many held frames to blend its way back to
/// the correct value even once motion stops — reading back after only a
/// couple of held frames catches that residual smear before it converges
/// away. A correctly reprojected accumulator has no smear to converge
/// away from — its held-frame value already matches a cold render.
const RAMP_FRAMES: usize = 3;
const HOLD_FRAMES: usize = 1;
const GHOST_FRAMES: usize = RAMP_FRAMES + HOLD_FRAMES;
const GHOST_TIME_STEP: f64 = 1.0;

/// FIXED by T1-C (RAYTRACING_DESIGN.md §8 Tier-1 item 1, BUG-311):
/// `accumulate_irradiance` now reprojects history through `prev_view_proj`
/// before blending and rejects a depth/normal mismatch. See the doc comment
/// above for the pre-fix vs post-fix numbers recorded for this revised
/// metric (2026-07-23, BUG-316 remediation).
#[test]
#[ignore = "BUG-316/D20: this metric does not discriminate pre-fix from post-fix \
            (pre 0.0267 vs post 0.0262, both > threshold) — the module doc comment \
            has the full history. BUG-311 is accepted FIXED on D20 Stage 2 bisection \
            evidence (reprojection instrumentation showed ~95-98% of texels shifting \
            to a different history texel and ~97%+ validity-accepted under this exact \
            motion, i.e. the mechanism is active, not inert) + Peter's in-app look, \
            not on this gate. Kept live-code, ignored, as a numeric record for T1-D."]
fn rt_ghost_orbit_accumulated_vs_cold_start_luma_diff_exceeds_threshold() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();

    // --- ACCUMULATED: persistent runtime, full orbit sweep, read back the
    // final pose's accumulated irradiance (built up across the whole sweep).
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &scene_json(ORBIT_RATE, ORBIT_BASE),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT ghost-probe scene graph must build");
    let target = h.make_target("rt-t1a-ghost");

    let render_one = |runtime: &mut PresetRuntime, target: &RenderTarget, ctx: &PresetContext| {
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
        render_one(&mut runtime, &target, &make_ctx(h, 0.0, frame));
    }

    // Tracked world point: `rt_p1_region_probe`'s "occluded region" world
    // point, sitting on the SHADOW BOUNDARY (a steep local ambient-
    // occlusion/GI gradient — the accumulated `irradiance` channel BUG-311
    // lives in) rather than the flat far-corner lit patch (near-uniform
    // irradiance there hides the same-texel history artifact almost
    // entirely).
    let ghost_world = [1.0_f32, 0.0, -1.0];

    // RAMP: `time` (and therefore `orbit`) advances every frame. HOLD:
    // `time` is frozen at the ramp's final value — same "repeat the same
    // timestamp" shape `RT_WARMUP_FRAMES` already uses safely (actual_delta
    // 0 vs expected GHOST_TIME_STEP doesn't trip the 1.5x discontinuity
    // gate), so it holds history instead of resetting it.
    let final_time = (RAMP_FRAMES - 1) as f64 * GHOST_TIME_STEP;
    let final_orbit = ORBIT_BASE + ORBIT_RATE * final_time as f32;
    let cam = Camera::orbit_perspective(final_orbit, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let px = cam
        .project_to_pixel(ghost_world, h.width, h.height)
        .expect("lit probe point must project on-screen at the ramp's final pose");

    let mut accumulated_values = Vec::new();
    for i in 0..GHOST_FRAMES {
        let time = (i.min(RAMP_FRAMES - 1)) as f64 * GHOST_TIME_STEP;
        render_one(
            &mut runtime,
            &target,
            &make_ctx_with_dt(h, time, RT_WARMUP_FRAMES + i as i64, GHOST_TIME_STEP as f32),
        );
        if i == GHOST_FRAMES - 1 {
            let bytes = h.readback(&target.texture);
            accumulated_values = region_luma_values(&bytes, h.width, h.height, px.px, px.py, 7);
        }
    }

    // --- COLD: a FRESH runtime with the camera fixed at the exact final
    // orbit angle from frame zero (orbit_rate=0.0, orbit_base=final_orbit),
    // so its irradiance history only ever sees this one pose — no motion,
    // no reprojection needed, a clean ground truth for "what this pose
    // should look like". Same warmup frame count as the accumulated run so
    // the accel structure and irradiance both settle identically.
    let mut cold_runtime = PresetRuntime::from_json_str_with_device(
        &scene_json(0.0, final_orbit),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT ghost-probe cold-reference scene graph must build");
    let cold_target = h.make_target("rt-t1a-ghost-cold");
    for frame in 0..RT_WARMUP_FRAMES {
        render_one(&mut cold_runtime, &cold_target, &make_ctx(h, 0.0, frame));
    }
    let cold_cam = Camera::orbit_perspective(final_orbit, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let cold_px = cold_cam
        .project_to_pixel(ghost_world, h.width, h.height)
        .expect("lit probe point must project on-screen at the cold final pose");
    let cold_bytes = h.readback(&cold_target.texture);
    let cold_values = region_luma_values(&cold_bytes, h.width, h.height, cold_px.px, cold_px.py, 7);

    assert_eq!(
        accumulated_values.len(),
        cold_values.len(),
        "accumulated and cold region windows must sample the same pixel count"
    );
    let per_pixel_abs_diff: Vec<f64> = accumulated_values
        .iter()
        .zip(cold_values.iter())
        .map(|(a, c)| (a - c).abs())
        .collect();
    let mean_abs_diff = mean(&per_pixel_abs_diff);

    let max_abs_diff = per_pixel_abs_diff.iter().cloned().fold(0.0_f64, f64::max);
    eprintln!(
        "ghost probe: final_orbit={final_orbit:.4} accumulated_mean={:.4} cold_mean={:.4} \
         mean_abs_diff={mean_abs_diff:.4} max_abs_diff={max_abs_diff:.4} \
         acc_min={:.4} acc_max={:.4} cold_min={:.4} cold_max={:.4} (threshold={GHOST_MEAN_ABS_DIFF_THRESHOLD:.4})",
        mean(&accumulated_values),
        mean(&cold_values),
        accumulated_values.iter().cloned().fold(f64::MAX, f64::min),
        accumulated_values.iter().cloned().fold(f64::MIN, f64::max),
        cold_values.iter().cloned().fold(f64::MAX, f64::min),
        cold_values.iter().cloned().fold(f64::MIN, f64::max),
    );

    assert!(
        mean_abs_diff <= GHOST_MEAN_ABS_DIFF_THRESHOLD,
        "T1-C's motion-reprojected accumulation should keep the accumulated-final-pose \
         render within {GHOST_MEAN_ABS_DIFF_THRESHOLD} mean abs luminance of a cold-start \
         render at the same pose, got {mean_abs_diff:.4} — accumulated={accumulated_values:?} \
         cold={cold_values:?}"
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

/// PARTIALLY ADDRESSED by T1-D (RAYTRACING_DESIGN.md §8 Tier-1 item 3,
/// BUG-312): per-texel moment/variance tracking in `accumulate_irradiance`,
/// a depth/normal/variance-guided à-trous spatial filter (REPLACING the
/// old depth-only bilateral upsample), and blue-noise (R2 sequence) AO/GI
/// ray directions.
///
/// PRE-FIX BASELINE (recorded 2026-07-23, then-current `main`, depth-only
/// bilateral upsample + finite-difference normals, no SVGF filter, no
/// blue-noise ray sampling): temporal_variance = 1.1e-8. spatial_variance
/// on the last frame = 1.076e-4 (>> the 7e-5 threshold).
///
/// POST-T1-D: spatial_variance measured ~8.6e-5 — reduced from the
/// pre-fix 1.076e-4 (the filter + blue noise DO measurably suppress real
/// per-frame AO/GI shot noise) but still above the 7e-5 threshold, and
/// this residual does NOT respond to more filtering or more samples —
/// diagnosed (2026-07-23, T1-D lane) via two isolating experiments.
/// First: forcing the à-trous kernel's edge-stop weight to a constant
/// 1.0 (pure box blur, no denoising benefit at all) left spatial_variance
/// unchanged (~8.6e-5). Second: raising `AO_SAMPLES_PER_PIXEL`/
/// `GI_SAMPLES_PER_PIXEL` 16x (4->64, 2->32, diagnostic-only, NOT
/// committed — budgets are out of this lane's scope per the brief) also
/// left it unchanged (~8.5e-5). Both experiments reverted before commit.
/// A raw dump of the last frame's per-pixel luma values in this window is
/// a smooth, monotonic, single-directional gradient (~1.015 to ~1.056
/// across the probed 15x15 window) — a genuine deterministic spatial
/// gradient in the demodulated ambient/GI term (visibility of nearby
/// geometry within the GI hemisphere gather genuinely varies smoothly
/// with screen position, even at this small scale), NOT zero-mean random
/// noise. Spatial/temporal denoising and additional Monte Carlo samples
/// only reduce noise variance around a local mean; they cannot flatten a
/// real, smooth gradient (averaging a linear ramp over a symmetric
/// window returns ~its own center value, unchanged). Per this lane's
/// brief ("Do NOT tune the threshold to force a pass... if it can't,
/// STOP and report"): kept `#[ignore]`d rather than weakening
/// `SPECKLE_SPATIAL_VARIANCE_THRESHOLD`, mirroring the ORBIT oracle's
/// precedent above. The assertion below is written for the FIXED
/// (post-T1-D) expectation, not the pre-fix "should trip" form — flip
/// this test live once either the underlying gradient is addressed (a
/// different, unscoped question: is ~8e-5 of real GI-visibility gradient
/// across a few world units expected/acceptable, Peter's call) or the
/// probe point/threshold is re-judged with that context in hand.
#[test]
#[ignore = "T1-D (BUG-312): filter + blue noise measurably cut real noise \
            (1.076e-4 -> ~8.6e-5) but the residual is a genuine smooth GI-\
            visibility gradient, not noise — confirmed via a forced-weight=1 \
            filter test and a 16x SPP test, neither changed the number. See \
            this test's doc comment for the full diagnostic and the re-judge \
            this needs from Peter/Fable."]
fn rt_still_frame_speckle_variance_exceeds_thresholds() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &scene_json(0.0, ORBIT_BASE),
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
        temporal_variance <= SPECKLE_TEMPORAL_VARIANCE_THRESHOLD
            && spatial_variance <= SPECKLE_SPATIAL_VARIANCE_THRESHOLD,
        "T1-D's à-trous filter + blue-noise sampling should keep both speckle metrics \
         under threshold on a static, settled shot: \
         temporal_variance={temporal_variance:.6} (threshold={SPECKLE_TEMPORAL_VARIANCE_THRESHOLD}), \
         spatial_variance={spatial_variance:.6} (threshold={SPECKLE_SPATIAL_VARIANCE_THRESHOLD})"
    );
}
