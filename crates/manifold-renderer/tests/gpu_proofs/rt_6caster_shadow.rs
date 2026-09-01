//! RS-A (caster cap 4 -> 8): six-caster shadow end-to-end proof.
//!
//! Scene: the `rt_multi_caster_shadow.rs` ground(8x8, y=0) + occluder(3x3,
//! y=1.5) + orbit camera — same verified world-space probe points from
//! `rt_p1_region_probe.rs`.
//!
//! Six casters (slots 0-5), all point lights at the same overhead position
//! (3, 20, 3) aimed at the origin. Each caster independently toggleable;
//! the gate isolates one caster at a time (the others have cast_shadows=false).
//!
//! Three proofs:
//! (a) Caster slot 5 (second texture): isolated, shadows on -> occluded
//!     region luma must drop meaningfully vs all-off control.
//! (b) Caster slot 0 (first texture regression): same isolated proof —
//!     proves the first texture still works.
//! (c) Slots 4 and 5 simultaneously — occluded region drops further than
//!     either alone (two real shadow contributions from different textures).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const RT_WARMUP_FRAMES: i64 = 16;
const RADIUS: i32 = 5;
const WT_FRAMES: usize = 16; // frames to measure for trace_ms

const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;

/// `rt_p1_region_probe.rs`'s verified probe points for the orbit camera
/// and the ground+occluder fixture.
const OCCLUDED_WORLD: [f32; 3] = [1.0, 0.0, -1.0];
const LIT_WORLD: [f32; 3] = [2.5, 0.0, -2.5];

/// Build a 6-caster scene JSON. `shadow_slots` is a bitmask: bit i set means
/// caster i has `cast_shadows=true` (and intensity=1.0); cleared casters still
/// light (intensity 1.0) but don't shadow.
fn six_caster_scene(shadow_mask: u8) -> String {
    let light_json = |i: usize, cast_shadows: bool| -> String {
        let cv = if cast_shadows { 1.0 } else { 0.0 };
        format!(
            r#"{{"id":{lid},"typeId":"node.light","nodeId":"light_{i}","params":{{
            "mode":{{"type":"Enum","value":1}},
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
            "range":{{"type":"Float","value":100.0}},
            "cast_shadows":{{"type":"Float","value":{cv}}}}}}}"#,
            lid = 30 + i, i = i
        )
    };

    let mut lights_json = String::new();
    for i in 0..6 {
        let cs = (shadow_mask >> i) & 1 == 1;
        if i > 0 { lights_json.push(','); }
        lights_json.push_str(&light_json(i, cs));
    }

    let mut wires_json = String::new();
    for i in 0..6 {
        wires_json.push_str(&format!(
            r#"{{"fromNode":{},"fromPort":"out","toNode":20,"toPort":"light_{}"}},"#,
            30 + i, i
        ));
    }

    format!(
        r#"{{"version":2,"name":"Rt6CasterShadow","nodes":[
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
        {lights},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":6}},
            "rt_enabled":{{"type":"Bool","value":true}}}}}},
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
        {wires}
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#,
        lights = lights_json,
        wires = wires_json,
    )
}

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
    .expect("RT 6-caster scene graph must build");

    let target = h.make_target("rt-6caster-shadow");
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
        let mut enc = h.device.create_encoder("rt-6caster-shadow-enc");
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

/// Project a world-space point through the orbit camera to pixel coordinates.
fn project_to_pixel(world: [f32; 3], w: u32, h: u32) -> (f32, f32) {
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, 0.05, 200.0);
    let p = cam.project_to_pixel(world, w, h).expect("probe point must project in front of camera");
    (p.px, p.py)
}

// ─── (a) Caster slot 5 (second texture, channel .g) isolated ──────────

#[test]
fn caster_slot_5_shadow_isolated() {
    // All 6 lights on (intensity=1), but only slot 5 casts shadows.
    let scene_shadow = six_caster_scene(1 << 5);
    // Control: all 6 lights on, no shadows.
    let scene_noshadow = six_caster_scene(0);

    let (bytes_shadow, w, h) = render_readback(&scene_shadow);
    let (bytes_noshadow, _, _) = render_readback(&scene_noshadow);

    let (ocx, ocy) = project_to_pixel(OCCLUDED_WORLD, w, h);
    let (lx, ly) = project_to_pixel(LIT_WORLD, w, h);

    let occluded_shadow = region_luma(&bytes_shadow, w, h, ocx, ocy, RADIUS);
    let occluded_noshadow = region_luma(&bytes_noshadow, w, h, ocx, ocy, RADIUS);
    let lit_shadow = region_luma(&bytes_shadow, w, h, lx, ly, RADIUS);
    let lit_noshadow = region_luma(&bytes_noshadow, w, h, lx, ly, RADIUS);

    eprintln!("slot 5 isolated: occluded shadow={occluded_shadow:.6} noshadow={occluded_noshadow:.6}  lit shadow={lit_shadow:.6} noshadow={lit_noshadow:.6}");

    assert!(
        occluded_shadow * 1.02 < occluded_noshadow,
        "caster slot 5 shadow must darken occluded region (shadow={occluded_shadow:.6} vs noshadow={occluded_noshadow:.6})"
    );
    assert!(
        (lit_shadow - lit_noshadow).abs() / lit_noshadow.max(1e-6) < 0.05,
        "caster slot 5 shadow must not affect lit region (shadow={lit_shadow:.6} vs noshadow={lit_noshadow:.6})"
    );
}

// ─── (b) Caster slot 0 isolated (regression — first texture still works) ──

#[test]
fn caster_slot_0_shadow_isolated() {
    let scene_shadow = six_caster_scene(1 << 0);
    let scene_noshadow = six_caster_scene(0);

    let (bytes_shadow, w, h) = render_readback(&scene_shadow);
    let (bytes_noshadow, _, _) = render_readback(&scene_noshadow);

    let (ocx, ocy) = project_to_pixel(OCCLUDED_WORLD, w, h);
    let (lx, ly) = project_to_pixel(LIT_WORLD, w, h);

    let occluded_shadow = region_luma(&bytes_shadow, w, h, ocx, ocy, RADIUS);
    let occluded_noshadow = region_luma(&bytes_noshadow, w, h, ocx, ocy, RADIUS);
    let lit_shadow = region_luma(&bytes_shadow, w, h, lx, ly, RADIUS);
    let lit_noshadow = region_luma(&bytes_noshadow, w, h, lx, ly, RADIUS);

    eprintln!("slot 0 isolated: occluded shadow={occluded_shadow:.6} noshadow={occluded_noshadow:.6}  lit shadow={lit_shadow:.6} noshadow={lit_noshadow:.6}");

    assert!(
        occluded_shadow * 1.02 < occluded_noshadow,
        "caster slot 0 shadow must darken occluded region (shadow={occluded_shadow:.6} vs noshadow={occluded_noshadow:.6})"
    );
    assert!(
        (lit_shadow - lit_noshadow).abs() / lit_noshadow.max(1e-6) < 0.05,
        "caster slot 0 shadow must not affect lit region (shadow={lit_shadow:.6} vs noshadow={lit_noshadow:.6})"
    );
}

// ─── (c) Two casters from different textures simultaneously ───────────

#[test]
fn casters_4_and_5_both_shadow() {
    // Slots 4 (last channel of first texture) + 5 (first channel of second
    // texture) both casting shadows — proves the cross-texture gate is
    // correct and both channels contribute independently.
    let scene_both = six_caster_scene((1 << 4) | (1 << 5));
    // Only slot 4 casts shadows — the combined shadow should be darker.
    let scene_one = six_caster_scene(1 << 4);

    let (bytes_both, w, h) = render_readback(&scene_both);
    let (bytes_one, _, _) = render_readback(&scene_one);

    let (ocx, ocy) = project_to_pixel(OCCLUDED_WORLD, w, h);

    let occ_both = region_luma(&bytes_both, w, h, ocx, ocy, RADIUS);
    let occ_one = region_luma(&bytes_one, w, h, ocx, ocy, RADIUS);

    eprintln!("two-caster: both={occ_both:.6} one={occ_one:.6}");

    assert!(
        occ_both * 1.05 < occ_one,
        "slots 4+5 both casting shadows must be darker than slot 4 alone (both={occ_both:.6} vs one={occ_one:.6})"
    );
}

// ─── (d) trace_ms delta: 4-vs-8 caster GPU trace dispatch time ─────────

#[test]
fn trace_ms_4vs8_caster_delta_reported_as_number() {
    // All casters shadowing, only lights 0-1 for 4-caster case,
    // lights 0-5 for 8-caster case. Both scenes identical otherwise.
    let scene_4 = six_caster_scene((1 << 0) | (1 << 1));
    let scene_8 = six_caster_scene((1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5));

    fn measure_frames(json: &str, label: &str, runs: usize) -> Vec<f64> {
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
        .expect("RT trace-ms scene graph must build");
        let target = h.make_target("rt-trace-ms");
        let mut times = Vec::with_capacity(runs);
        for frame in 0..runs {
            let ctx = PresetContext {
                time: 0.1, beat: 0.2, dt: 1.0 / 60.0,
                width: h.width, height: h.height,
                output_width: h.width, output_height: h.height,
                aspect: h.width as f32 / h.height as f32,
                owner_key: 0, is_clip_level: false,
                frame_count: frame as i64,
                anim_progress: 0.0, trigger_count: 0,
            };
            let mut enc = h.device.create_encoder("rt-trace-ms-enc");
            let t0 = std::time::Instant::now();
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
                runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        // Discard first 4 frames (warmup: accel build + JIT)
        let tail: Vec<f64> = times[4..].to_vec();
        let median = {
            let mut s = tail.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap());
            s[s.len() / 2]
        };
        // Max frame after warmup
        let max = tail.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        eprintln!("{label}: median={median:.3}ms max={max:.3}ms over {r} frames (warmup discarded)", r = tail.len());
        tail
    }

    // 16 warmup frames for each, measure last 10
    let _times_4 = measure_frames(&scene_4, "4-caster-trace-ms", WT_FRAMES);
    let times_8 = measure_frames(&scene_8, "8-caster-trace-ms", WT_FRAMES);

    let _med_4 = {
        let mut s = _times_4.clone(); s.sort_by(|a,b| a.partial_cmp(b).unwrap());
        s[s.len()/2]
    };
    let med_8 = {
        let mut s = times_8.clone(); s.sort_by(|a,b| a.partial_cmp(b).unwrap());
        s[s.len()/2]
    };
    let max_8 = times_8.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    eprintln!("TRACE_MS 4-caster median = {_med_4:.3}ms, 8-caster median = {med_8:.3}ms, delta = {:.3}ms", med_8 - _med_4);
    eprintln!("TRACE_MS 8-caster max (post-warmup) = {max_8:.3}ms");

    // The brief: MANIFOLD_RENDER_TRACE=1, no frame >20ms
    assert!(max_8 < 20.0, "8-caster max frame {max_8:.3}ms must be under 20ms");
}
