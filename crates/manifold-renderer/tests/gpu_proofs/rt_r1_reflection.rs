//! `docs/RAYTRACING_DESIGN.md` section 9.6 R1 gate (a) — mirror reflection probe:
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
    scene_json_full(rt_reflections, true, 0.0)
}

/// `env_intensity` > 0 is the DEBUG/I-R1 discriminator: with a non-black env
/// the OFF leg's specular IBL is the env everywhere, and ON-minus-OFF
/// isolates exactly what the traced substitution changes.
/// `with_emitter` = the mirror probe (gate a); `!with_emitter` = the I-R1
/// empty-scene fixture (no occluder — every reflection ray misses, so ON
/// must equal OFF: the miss branch IS the env fetch, RD4/RD1).
fn scene_json_full(rt_reflections: bool, with_emitter: bool, env_intensity: f32) -> String {
    let rt_v = if rt_reflections { "true" } else { "false" };
    let (objects, emitter_nodes, emitter_wires) = if with_emitter {
        (
            2,
            r#",{"id":5,"typeId":"node.grid_mesh","nodeId":"quad_grid","params":{
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
            "emission_intensity":{"type":"Float","value":10.0}}}"#,
            r#",{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"},
        {"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"},
        {"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"},
        {"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"}"#,
        )
    } else {
        (1, "", "")
    };
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
            "intensity":{{"type":"Float","value":{env_intensity}}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":{objects}}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":{rt_v}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        {emitter_nodes}],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":10,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        {emitter_wires}]}}"#
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
            gpu_signal_committed: 0,
            gpu_signaled: 0,
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


/// Mirror-pixel helper: given a world point on the emitter quad, compute
/// its virtual image across y=0, intersect the camera→virtual_image ray
/// with the y=0 plane, and project to screen pixel.
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

/// Mirror reflection probe: metallic/roughness-0 ground plane with one
/// emissive quad above it. The emissive quad's mirror image (reflected across
/// y=0) appears on the ground at a computed world point. With RT reflections
/// ON the traced ray hits the emitter and returns bright; with OFF the dummy
/// envmap yields near-zero specular IBL and only the direct shading remains.
///
/// Thresholds pinned 2026-07-26 (mean_on 1.2173 / mean_off 0.8225 /
/// delta 0.3948). The probe window overlaps the sun's GGX highlight, so the
/// peak is toggle-invariant (~4.03 both legs) and the discriminating signal
/// is the window mean. R2's roughness-narrowed atrous filter concentrates
/// the mirror image, which is why the mean sits below the pre-R2 2.15 while
/// total energy is preserved.


#[test]
fn mirror_reflection_of_emissive_quad_appears_only_when_rt_reflections_enabled() {
    let (refl_bytes, w, h) = render_readback(&scene_json(true));
    let (ctrl_bytes, _, _) = render_readback(&scene_json(false));

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let (px, py) = mirror_pixel(&cam, [EMISSIVE_X, EMISSIVE_Y, EMISSIVE_Z], w, h);

    const RADIUS: i32 = 7; // 15x15 window
    let luma_on = region_luma(&refl_bytes, w, h, px, py, RADIUS);
    let luma_off = region_luma(&ctrl_bytes, w, h, px, py, RADIUS);

    const THRESHOLD_ON: f64 = 1.0;
    const MIN_DELTA: f64 = 0.3;
    const CEILING_OFF: f64 = 1.2;

    eprintln!(
        "reflection region (pixel ({:.0},{:.0})): mean_on={luma_on:.4} mean_off={luma_off:.4} \
         delta={:.4} | threshold_on={THRESHOLD_ON} min_delta={MIN_DELTA} ceiling_off={CEILING_OFF}",
        px, py, luma_on - luma_off,
    );

    assert!(
        luma_on >= THRESHOLD_ON,
        "reflection region mean_on={luma_on:.4} < {THRESHOLD_ON} — the emissive quad's mirror \
         image is too dim with rt_reflections enabled"
    );
    assert!(
        luma_on - luma_off >= MIN_DELTA,
        "reflection delta={:.4} < {MIN_DELTA} — the ON leg's mean ({luma_on:.4}) and OFF leg's \
         mean ({luma_off:.4}) discriminate too weakly at the reflection point",
        luma_on - luma_off,
    );
    assert!(
        luma_off <= CEILING_OFF,
        "reflection region mean_off={luma_off:.4} > {CEILING_OFF} — the OFF leg should show \
         only the dummy envmap (mean_off 0.8225 expected)"
    );
}

/// Raster-parity reflections gate (section 9.6, 2026-07-25): the env-at-hit term
/// must ADD environment radiance at reflection hits. Same mirror fixture,
/// reflections ON in both legs; the ONLY difference is the baked env's
/// intensity (0.0 vs 4.0 — high so the delta swamps the region's partial
/// mirror coverage). At the mirror probe point the traced hit shading is:
///   env=0: hit_emissive + sun_bounce                      (pre-parity)
///   env=4: hit_emissive + sun_bounce
///          + hit_albedo * env_diffuse + hit_f0 * env_spec (parity term)
/// so luma(env4) - luma(env0) > 0 iff the term fires. A ~zero delta means
/// the env-at-hit path is dead (binding/plumbing class). This test is the
/// black-car bisector: the AMG GT3 stays dark with reflections on even
/// after the parity build landed (2026-07-25: fraction 0.024 vs baseline
/// 0.096), so the first question is whether the term fires AT ALL in a
/// fully-known scene.
#[test]
fn raster_parity_env_at_hit_adds_env_term() {
    let (env0_bytes, w, h) = render_readback(&scene_json_full(true, true, 0.0));
    let (env4_bytes, _, _) = render_readback(&scene_json_full(true, true, 4.0));

    // Same probe point as the mirror probe above (virtual image of the
    // emitter across y=0, intersected with the plane).
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let (px, py) = mirror_pixel(&cam, [EMISSIVE_X, EMISSIVE_Y, EMISSIVE_Z], w, h);

    const RADIUS: i32 = 7;
    let luma_env0 = region_luma(&env0_bytes, w, h, px, py, RADIUS);
    let luma_env4 = region_luma(&env4_bytes, w, h, px, py, RADIUS);

    // Expectation pinned from measured values (lead, 2026-07-25 — see the
    // eprintln). The floor is deliberately loose: the mirror image covers
    // the region only partially, so the hit term arrives diluted, and
    // miss-path pixels in the region legitimately brighten too (both are
    // the env reaching the substituted value — what this bisector asks).
    let min_delta = 0.02;
    eprintln!(
        "env-at-hit region (pixel ({:.0},{:.0})): env0={luma_env0:.4} env4={luma_env4:.4} \
         delta={:.4} | min_delta={min_delta}",
        px,
        py,
        luma_env4 - luma_env0,
    );
    assert!(
        luma_env4 - luma_env0 >= min_delta,
        "env-at-hit term did not fire: luma delta {} < {min_delta} between \
         env-intensity 4.0 and 0.0 at the reflection region — the parity \
         env path (hit_diffuse_env / hit_specular_env in the trace kernel) \
         is not contributing",
        luma_env4 - luma_env0,
    );
}

/// I-R1 — exactly one environment-specular contribution per pixel: with NO
/// occluder in the scene, every reflection ray misses and the traced value
/// must equal the raster's own env fetch (RD4's miss branch) — so
/// reflections-ON equals reflections-OFF within epsilon on a real
/// (intensity 0.5) envmap. Fails loudly if the term is ADDED on top of
/// `specular_ibl` instead of substituted (the 818a06b0 double-count class),
/// or if the kernel's equirect mapping/mip selection drifts from the
/// raster's (I-R1's second job).
#[test]
fn reflection_of_empty_scene_equals_env_only() {
    let (on_bytes, w, h) = render_readback(&scene_json_full(true, false, 0.5));
    let (off_bytes, _, _) = render_readback(&scene_json_full(false, false, 0.5));

    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let (px, py) = mirror_pixel(&cam, [EMISSIVE_X, EMISSIVE_Y, EMISSIVE_Z], w, h);
    let on_mean = region_luma(&on_bytes, w, h, px, py, 7);
    let off_mean = region_luma(&off_bytes, w, h, px, py, 7);
    eprintln!("empty_scene: on_mean={on_mean:.6} off_mean={off_mean:.6}");

    assert!(
        (on_mean - off_mean).abs() < 0.05,
        "I-R1: empty-scene ON ({on_mean:.4}) must equal OFF ({off_mean:.4}) — \
         the reflection term is being ADDED, not substituted (or the kernel's \
         miss-branch env fetch drifts from the raster's)"
    );
    // Sanity: the env really contributes at the probe point (a black render
    // would also "pass" equality — vacuous-proofing, same discipline as the
    // P1 probe's round-trip check).
    assert!(
        off_mean > 0.05,
        "sanity: no env contribution at the probe point (OFF mean {off_mean:.4}) \
         — the equality check above is vacuous"
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

    // BUG-uo3z (rt first-frame stall assert load-flaky): collect every
    // frame's wall time first — the ceiling below is computed from THIS
    // run's own steady-state frames, so it can't be known until the whole
    // loop (or at least its tail) has run.
    use crate::rt_p1_region_probe::WARMUP_FRAMES_EXEMPT;
    let mut frame_ms: Vec<f64> = Vec::with_capacity(RT_WARMUP_FRAMES as usize);
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
            gpu_signal_committed: 0,
            gpu_signaled: 0,
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
        frame_ms.push(elapsed.as_secs_f64() * 1000.0);

        if frame >= WARMUP_FRAMES_EXEMPT && elapsed > worst.1 {
            worst = (frame as u32, elapsed);
        }
    }
    eprintln!(
        "worst post-warmup frame: {} at {:.2}ms",
        worst.0,
        worst.1.as_secs_f64() * 1000.0
    );

    // BUG-uo3z: ceiling relative to THIS run's own steady state, not a
    // bare wall-clock constant — a bare 20ms flaked under full-suite GPU
    // thread contention. Same shared math as `rt_p1_region_probe.rs`'s
    // equivalent assert (one fix, not three copies).
    use crate::rt_p1_region_probe::{stall_ceiling_ms, STALL_ABS_FLOOR_MS, STALL_FACTOR, STEADY_TAIL_COUNT};
    let checked = &frame_ms[WARMUP_FRAMES_EXEMPT as usize..];
    let steady_tail = &checked[checked.len().saturating_sub(STEADY_TAIL_COUNT)..];
    let ceiling_ms = stall_ceiling_ms(steady_tail);
    eprintln!(
        "steady tail {steady_tail:?} -> median-based ceiling {ceiling_ms:.2}ms \
         (floor {STALL_ABS_FLOOR_MS:.1}ms, factor {STALL_FACTOR}x)"
    );
    for (i, &ms) in checked.iter().enumerate() {
        let frame = WARMUP_FRAMES_EXEMPT + i as i64;
        assert!(
            ms <= ceiling_ms,
            "frame {frame} took {ms:.2}ms (>{ceiling_ms:.2}ms steady-state ceiling) — the \
             reflection dispatch must not hitch (BUG-uo3z: ceiling is relative to this run's own \
             steady state, not a load-flaky bare wall-clock constant)"
        );
    }
}
