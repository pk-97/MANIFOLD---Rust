//! BUG-322 — does the RT shadow FOLLOW an object that moves at performance
//! rates, or does it lag behind for the duration of the gesture?
//!
//! Peter's in-app report (2026-07-23, after BUG-320 and BUG-321 both landed
//! with green proofs and neither moved the symptom): *"during the rotation
//! RT is lost or something and the shadows go back to raster? The shadows
//! literally change shape and location during the rotation and snap back
//! once it stops."*
//!
//! Both previous fixes were certified by value-level unit proofs of a
//! mechanism that had been REASONED responsible, with no end-to-end
//! observation anywhere in the loop — and both missed. This file is that
//! missing observation: the real `node.render_scene` RT path, a real
//! occluder moving frame-to-frame on a persistent runtime (no rebuild
//! between frames, exactly like a live drag), reading back where the
//! shadow actually lands.
//!
//! **Oracle design — geometric, not perceptual.** The occluder slides along
//! +X over `MOTION_FRAMES`. Two ground probes are read on the final motion
//! frame:
//!   - `START`: directly under the occluder's INITIAL position.
//!   - `END`:   directly under the occluder's FINAL position.
//!
//! A correct renderer shadows END and not START on that frame. A renderer
//! whose acceleration structure still holds an old pose shadows START and
//! not END. That is a statement about geometry, so it is immune to the
//! accumulation-lag confound that made the D19/D20 ghost oracles
//! non-discriminating (`rt_t1a_ghost_speckle.rs`) — a stale shadow is in
//! the WRONG PLACE, not merely blurred or trailing in intensity.
//!
//! Motion is driven by port-shadowing `pos_x` from
//! `system.generator_input.time` (the same technique the T1-A orbit oracle
//! uses for the camera), so advancing `PresetContext.time` moves the object
//! on ONE persistent runtime — no rebuild, no state reset, no new
//! `Executor` and therefore no `rebuild_epoch` change.

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
const ORBIT: f32 = 0.7;

/// Same RT-D4 async-accel-build allowance the other RT node-path probes
/// use: enough frames for the request, the deferred build-enqueue, and
/// real wall-clock for the async build to complete before motion starts.
const RT_WARMUP_FRAMES: i64 = 16;

/// Occluder X positions: it starts at `START_X` and slides to `END_X`.
/// Far enough apart that the two shadow probes are unambiguously disjoint
/// (the occluder is 3x3, the slide is a full body-width).
const START_X: f32 = -1.5;
const END_X: f32 = 1.5;

/// Frames of motion and the per-frame time step that drives it — 0.5 world
/// units per frame, a brisk but entirely ordinary performance drag.
const MOTION_FRAMES: usize = 6;
const TIME_STEP: f64 = 1.0;

/// Occluder height above the ground plane.
const OCC_Y: f32 = 1.5;

/// Probe window half-width in pixels.
const PROBE_RADIUS: i32 = 3;

/// Scene: ground grid + a 3x3 occluder floating at `OCC_Y`, overhead sun,
/// RT shadows ON. The occluder transform's `pos_x` is port-shadowed by
/// `time * rate + base`, so `PresetContext.time` alone slides the object
/// with no rebuild.
fn scene_json(motion_rate: f32, base_x: f32) -> String {
    format!(
        r#"{{"version":2,"name":"RtObjectMotionShadow","nodes":[
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
        {{"id":40,"typeId":"node.math","nodeId":"motion_rate_mul","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":{motion_rate}}},
            "op":{{"type":"Enum","value":2}}}}}},
        {{"id":41,"typeId":"node.math","nodeId":"motion_add_base","params":{{
            "a":{{"type":"Float","value":0.0}},
            "b":{{"type":"Float","value":{base_x}}},
            "op":{{"type":"Enum","value":0}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"occ_xform","params":{{
            "pos_x":{{"type":"Float","value":{base_x}}},
            "pos_y":{{"type":"Float","value":{OCC_Y}}}}}}},
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
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":20.0}},
            "pos_z":{{"type":"Float","value":0.0}},
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
        {{"fromNode":0,"fromPort":"time","toNode":40,"toPort":"a"}},
        {{"fromNode":40,"fromPort":"out","toNode":41,"toPort":"a"}},
        {{"fromNode":41,"fromPort":"out","toNode":7,"toPort":"pos_x"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

fn make_ctx(h: &harness::ParityHarness, time: f64, frame_count: i64, dt: f32) -> PresetContext {
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

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// Mean luminance of a square window around a pixel.
fn region_mean_luma(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32, radius: i32) -> f64 {
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
    assert!(!out.is_empty(), "probe window is entirely off-screen");
    mean(&out)
}

/// Ground-plane point (x, 0, 0) -> screen pixel, via the same camera math
/// the scene's `node.orbit_camera` builds.
fn ground_pixel(h: &harness::ParityHarness, x: f32) -> (f32, f32) {
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let p = cam
        .project_to_pixel([x, 0.0, 0.0], h.width, h.height)
        .expect("ground probe point must project in front of the camera");
    (p.px, p.py)
}


/// Where along X the ground is darkest — the shadow's centroid, in world
/// units. Scanning and locating the minimum is what makes this oracle
/// GEOMETRIC: a stale acceleration structure puts the shadow at the
/// object's OLD x, which this reports directly, rather than hiding inside
/// an intensity average that AO/GI differences would swamp.
fn darkest_ground_x(h: &harness::ParityHarness, bytes: &[u8]) -> (f32, f64) {
    let mut best_x = f32::NAN;
    let mut best_l = f64::INFINITY;
    let mut x = SCAN_MIN;
    while x <= SCAN_MAX + 1e-3 {
        let (px, py) = ground_pixel(h, x);
        let l = region_mean_luma(bytes, h.width, h.height, px, py, PROBE_RADIUS);
        if l < best_l {
            best_l = l;
            best_x = x;
        }
        x += SCAN_STEP;
    }
    (best_x, best_l)
}

const SCAN_MIN: f32 = -3.0;
const SCAN_MAX: f32 = 3.0;
const SCAN_STEP: f32 = 0.25;

/// How far apart the RT and raster shadow centroids may sit, in world
/// units. The two techniques legitimately differ at the edges (soft RT
/// shadow vs hard map, plus RT's AO/GI darkening), so this tolerates a
/// sample step or two — but a shadow left behind at the object's starting
/// position would be 3.0 units out, far outside this.
const SHADOW_AGREEMENT_TOLERANCE: f32 = 0.75;

/// Renders the warmup + motion sequence and returns
/// `(parked_darkest_x, moved_darkest_x)`.
fn run_motion_sequence(h: &harness::ParityHarness, rt_on: bool) -> (f32, f32) {
    let registry = PrimitiveRegistry::with_builtin();
    let motion_rate = (END_X - START_X) / (MOTION_FRAMES as f64 * TIME_STEP) as f32;
    let json = scene_json(motion_rate, START_X).replace(
        "\"rt_enabled\":{\"type\":\"Bool\",\"value\":true}",
        &format!("\"rt_enabled\":{{\"type\":\"Bool\",\"value\":{rt_on}}}"),
    );
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("RT object-motion scene graph must build");
    let target = h.make_target("rt-objmotion");

    let render_one = |runtime: &mut PresetRuntime, ctx: &PresetContext| {
        let mut enc = h.device.create_encoder("rt-objmotion-enc");
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

    // Warmup at time 0 (occluder parked at START_X) so the async RT accel
    // build completes before motion begins — otherwise this would measure
    // RT-D4 startup, not motion behavior.
    for frame in 0..RT_WARMUP_FRAMES {
        render_one(&mut runtime, &make_ctx(h, 0.0, frame, 1.0 / 60.0));
    }
    let parked = h.readback(&target.texture);
    let (parked_x, parked_l) = darkest_ground_x(h, &parked);

    // Motion on ONE persistent runtime — no rebuild, exactly like a live drag.
    let mut time = 0.0f64;
    for frame in 0..MOTION_FRAMES {
        time += TIME_STEP;
        render_one(
            &mut runtime,
            &make_ctx(h, time, RT_WARMUP_FRAMES + frame as i64, TIME_STEP as f32),
        );
    }
    let moved = h.readback(&target.texture);
    let (moved_x, moved_l) = darkest_ground_x(h, &moved);

    eprintln!(
        "[BUG-322] rt={rt_on}: shadow centroid parked x={parked_x:+.2} (luma {parked_l:.3}) \
         -> after motion x={moved_x:+.2} (luma {moved_l:.3})"
    );
    (parked_x, moved_x)
}

/// BUG-322's gate: **the RT shadow must track a moving object to the same
/// place the raster shadow does.**
///
/// Both techniques render the identical motion sequence; the assertion is
/// that their shadow centroids agree after the move. This is deliberately
/// a RELATIVE oracle. An absolute "the shadow must be at END_X" gate would
/// depend on my own model of the scene's sun/plane geometry, and an early
/// version of this file did exactly that, mispredicted the shadow's
/// position, and produced a red test that looked like Peter's bug but was
/// my arithmetic — the RT-vs-raster leg is what caught it. Raster is the
/// independent reference: it has no acceleration structure and therefore
/// cannot go stale, so if RT agrees with it the RT geometry is current.
///
/// What this catches: an acceleration structure holding an old transform
/// (RT's shadow stays behind while raster's moves), and a mid-gesture
/// fallback to the raster path *that renders differently from raster* .
/// What it does NOT catch, and what BUG-322 may still be: motion that
/// changes VERTEX CONTENT rather than the model matrix (skinned/deforming
/// meshes — the accel key hashes buffer identity and triangle count, not
/// bytes), or instanced objects. This scene moves a rigid object through
/// `node.transform_3d`, the same path glTF import wires for rigid objects.
#[test]
fn rt_shadow_tracks_a_moving_object_to_the_same_place_raster_does() {
    let h = harness::shared();

    let (rt_parked, rt_moved) = run_motion_sequence(h, true);
    let (raster_parked, raster_moved) = run_motion_sequence(h, false);

    // Guard: the shadow must actually MOVE, or the comparison below is
    // vacuous (two frozen shadows also "agree").
    assert!(
        (rt_moved - rt_parked).abs() > SHADOW_AGREEMENT_TOLERANCE,
        "the shadow did not move at all between parked ({rt_parked:+.2}) and moved \
         ({rt_moved:+.2}) with RT on — the fixture is not exercising object motion, so this \
         oracle proves nothing"
    );

    assert!(
        (rt_moved - raster_moved).abs() <= SHADOW_AGREEMENT_TOLERANCE,
        "BUG-322: after {MOTION_FRAMES} frames of object motion the RT shadow sits at \
         x={rt_moved:+.2} but the raster shadow — which cannot go stale — sits at \
         x={raster_moved:+.2}. RT is tracing geometry at a different position than the object \
         actually occupies, i.e. the acceleration structure is holding a stale transform. \
         (Parked reference: RT {rt_parked:+.2}, raster {raster_parked:+.2}.)"
    );
}
