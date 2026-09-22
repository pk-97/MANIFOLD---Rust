//! End-to-end proof for the eight-slot RT caster contract.
//!
//! The first four casters have zero radiance but remain enabled, so lights 4,
//! 5, 6, and 7 are genuine compacted RT slots. RT AO, GI, and reflections are
//! disabled in this fixture so the probes isolate direct shadow visibility.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::{ParamValue, PrimitiveRegistry};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const RT_WARMUP_FRAMES: i64 = 16;
const RADIUS: i32 = 5;
const ORBIT: f32 = 0.7;
const TILT: f32 = 0.95;
const DISTANCE: f32 = 10.0;
const FOV_Y: f32 = 0.8;
const OCCLUDED_WORLD: [f32; 3] = [1.0, 0.0, -1.0];
const LIT_WORLD: [f32; 3] = [2.5, 0.0, -2.5];

fn light_json(i: usize, intensity: f32, cast_shadows: f32) -> String {
    format!(
        r#"{{"id":{id},"typeId":"node.light","nodeId":"light_{i}","params":{{
        "mode":{{"type":"Enum","value":1}},"pos_x":{{"type":"Float","value":{pos_x}}},"pos_y":{{"type":"Float","value":20.0}},"pos_z":{{"type":"Float","value":3.0}},
        "aim_x":{{"type":"Float","value":0.0}},"aim_y":{{"type":"Float","value":0.0}},"aim_z":{{"type":"Float","value":0.0}},
        "color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},
        "intensity":{{"type":"Float","value":{intensity}}},"range":{{"type":"Float","value":100.0}},"cast_shadows":{{"type":"Float","value":{cast_shadows}}}}}}}"#,
        id = 30 + i,
        // Dark filler casters see the probe unoccluded; sampling a wrong
        // preceding mask channel cannot impersonate the target shadow.
        pos_x = if intensity == 0.0 { 30.0 } else { 3.0 },
    )
}

/// `shadow_mask` controls caster flags while `radiance_mask` controls
/// intensity. This separation leaves leading compacted slots real while their
/// direct contribution is zero. The ninth light is an unshadowed overflow
/// light and must continue contributing when the eight caster table is full.
fn caster_scene(
    shadow_mask: u16,
    radiance_mask: u16,
    rt_enabled: bool,
    rt_shadows: bool,
) -> String {
    let mut lights = String::new();
    let mut wires = String::new();
    for i in 0..9 {
        if i > 0 {
            lights.push(',');
        }
        let intensity = if radiance_mask & (1 << i) != 0 { 1.0 } else { 0.0 };
        let cast_shadows = if shadow_mask & (1 << i) != 0 { 1.0 } else { 0.0 };
        lights.push_str(&light_json(i, intensity, cast_shadows));
        wires.push_str(&format!(
            r#"{{"fromNode":{},"fromPort":"out","toNode":20,"toPort":"light_{}"}},"#,
            30 + i,
            i,
        ));
    }
    let rt_enabled = if rt_enabled { "true" } else { "false" };
    let rt_shadows = if rt_shadows { "true" } else { "false" };
    format!(
        r#"{{"version":2,"name":"Rt8CasterShadow","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":20}},"resolution_y":{{"type":"Int","value":20}},"size_x":{{"type":"Float","value":8.0}},"size_y":{{"type":"Float","value":8.0}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{{"src_cols":{{"type":"Int","value":20}},"src_rows":{{"type":"Int","value":20}}}}}},
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"occ_grid","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":10}},"resolution_y":{{"type":"Int","value":10}},"size_x":{{"type":"Float","value":3.0}},"size_y":{{"type":"Float","value":3.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"occ_tris","params":{{"src_cols":{{"type":"Int","value":10}},"src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"occ_xform","params":{{"pos_y":{{"type":"Float","value":1.5}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{"orbit":{{"type":"Float","value":{ORBIT}}},"tilt":{{"type":"Float","value":{TILT}}},"distance":{{"type":"Float","value":{DISTANCE}}},"fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{{"color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},"ambient":{{"type":"Float","value":0.05}}}}}},
        {lights},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{"objects":{{"type":"Int","value":2}},"lights":{{"type":"Int","value":9}},"rt_enabled":{{"type":"Bool","value":{rt_enabled}}},"rt_shadows":{{"type":"Bool","value":{rt_shadows}}},"rt_ao":{{"type":"Bool","value":false}},"rt_gi":{{"type":"Bool","value":false}},"rt_reflections":{{"type":"Bool","value":false}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},{{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},{{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},{{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},{{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {wires}{{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#,
        lights = lights,
        wires = wires,
    )
}

fn context(frame: i64, h: &harness::ParityHarness) -> PresetContext {
    PresetContext {
        time: frame as f64 / 60.0,
        beat: frame as f64 / 30.0,
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
    }
}

fn render_frame(
    runtime: &mut PresetRuntime,
    target: &manifold_renderer::render_target::RenderTarget,
    frame: i64,
) -> (Vec<u8>, usize, FrameRenderStatus) {
    let h = harness::shared();
    let mut status = FrameRenderStatus::Complete;
    let captures = harness::capture_rt_channels(|| {
        let mut enc = h.device.create_encoder("rt-8caster-shadow");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &context(frame, h),
                &manifold_core::params::ParamManifest::default(),
            );
            status = gpu.frame_status();
        }
        enc.commit_and_wait_completed();
    });
    (h.readback(&target.texture), captures.len(), status)
}

fn render_readback(json: &str) -> (Vec<u8>, u32, u32, usize) {
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
    .expect("RT 8-caster scene graph must build");
    let target = h.make_target("rt-8caster-shadow");
    let mut captures = 0;
    for frame in 0..RT_WARMUP_FRAMES {
        captures = render_frame(&mut runtime, &target, frame).1;
    }
    (h.readback(&target.texture), h.width, h.height, captures)
}

fn region_luma(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32) -> f64 {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let mut sum = 0.0;
    let mut n = 0u64;
    for dy in -RADIUS..=RADIUS {
        for dx in -RADIUS..=RADIUS {
            let x = cxi + dx;
            let y = cyi + dy;
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                continue;
            }
            let px = &bytes[((y as u32 * w + x as u32) * 8) as usize..];
            let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
            let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
            let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
            assert!(r.is_finite() && g.is_finite() && b.is_finite());
            sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
            n += 1;
        }
    }
    assert!(n > 0);
    sum / n as f64
}

fn project_to_pixel(world: [f32; 3], w: u32, h: u32) -> (f32, f32) {
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, 0.05, 200.0);
    let p = cam
        .project_to_pixel(world, w, h)
        .expect("probe must project");
    (p.px, p.py)
}

fn slot_probe(target_slot: usize) -> (f64, f64, f64, f64, usize) {
    let shadow_mask = (1u16 << (target_slot + 1)) - 1;
    let radiance = (1u16 << target_slot) | (1u16 << 8);
    let (shadow_bytes, w, h, captures) =
        render_readback(&caster_scene(shadow_mask, radiance, true, true));
    let (control_bytes, _, _, _) = render_readback(&caster_scene(0, radiance, true, false));
    let (ocx, ocy) = project_to_pixel(OCCLUDED_WORLD, w, h);
    let (lx, ly) = project_to_pixel(LIT_WORLD, w, h);
    (
        region_luma(&shadow_bytes, w, h, ocx, ocy),
        region_luma(&control_bytes, w, h, ocx, ocy),
        region_luma(&shadow_bytes, w, h, lx, ly),
        region_luma(&control_bytes, w, h, lx, ly),
        captures,
    )
}

#[test]
fn caster_contract_compacted_slots_4_through_7_shadow() {
    for slot in [0, 3, 4, 5, 6, 7] {
        let (occluded, control, lit, lit_control, captures) = slot_probe(slot);
        eprintln!("caster slot {slot}: shadow={occluded:.6} control={control:.6} lit={lit:.6} lit_control={lit_control:.6} captures={captures}");
        assert!(captures > 0, "slot {slot} must dispatch RT");
        assert!(
            occluded * 1.02 < control,
            "compacted slot {slot} must darken the occluded region"
        );
        assert!(
            (lit - lit_control).abs() / lit_control.max(1e-6) < 0.08,
            "slot {slot} must preserve the lit region"
        );
    }
}

#[test]
fn caster_contract_overflow_and_sparse_light_indices() {
    // All nine request shadows. The ninth must illuminate unshadowed because
    // the preceding eight still occupy the RT budget despite zero radiance.
    let (overflow, w, h, captures) = render_readback(&caster_scene(0x1ff, 1 << 8, true, true));
    let (control, _, _, _) = render_readback(&caster_scene(0, 1 << 8, true, false));
    let (ocx, ocy) = project_to_pixel(OCCLUDED_WORLD, w, h);
    let value = region_luma(&overflow, w, h, ocx, ocy);
    let expected = region_luma(&control, w, h, ocx, ocy);
    assert!(captures > 0 && expected > 0.1);
    assert!((value - expected).abs() / expected < 0.02, "ninth caster must illuminate without a shadow: {value} vs {expected}");

    // A hole in wired-light indices is not a hole in the caster list: the
    // ninth light becomes slot 4 when only four earlier lights cast shadows.
    let (sparse, _, _, _) = render_readback(&caster_scene(0b1_0000_1111, 1 << 8, true, true));
    let shadowed = region_luma(&sparse, w, h, ocx, ocy);
    assert!(shadowed * 1.02 < expected, "sparse ninth light must reach compacted slot 4");
}

#[test]
fn caster_contract_live_rt_and_shadow_toggles() {
    use manifold_core::NodeId;
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = caster_scene(0xff, (1u16 << 7) | (1u16 << 8), true, true);
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("live caster scene must build");
    let scene = runtime
        .graph
        .instance_by_node_id(&NodeId::new("scene"))
        .unwrap();
    let target = h.make_target("rt-8caster-live-toggle");
    for frame in 0..RT_WARMUP_FRAMES {
        let _ = render_frame(&mut runtime, &target, frame);
    }
    let mut observations = Vec::new();
    for (frame, (rt_enabled, rt_shadows)) in [
        (true, true),
        (false, false),
        (true, true),
        (true, false),
        (true, true),
    ]
    .into_iter()
    .enumerate()
    {
        runtime
            .graph
            .set_param(scene, "rt_enabled", ParamValue::Bool(rt_enabled))
            .unwrap();
        runtime
            .graph
            .set_param(scene, "rt_shadows", ParamValue::Bool(rt_shadows))
            .unwrap();
        let (bytes, captures, status) =
            render_frame(&mut runtime, &target, RT_WARMUP_FRAMES + frame as i64);
        let (ocx, ocy) = project_to_pixel(OCCLUDED_WORLD, h.width, h.height);
        observations.push((
            region_luma(&bytes, h.width, h.height, ocx, ocy),
            captures,
            status,
        ));
    }
    assert!(observations.iter().all(|o| o.2 == FrameRenderStatus::Complete));
    assert!(
        observations[0].1 > 0 && observations[1].1 == 0 && observations[2].1 > 0 && observations[3].1 > 0 && observations[4].1 > 0,
        "RT dispatch must follow on/off/on"
    );
    assert!(
        observations[0].0 < observations[1].0 && observations[2].0 * 1.02 < observations[3].0 && observations[4].0 * 1.02 < observations[3].0,
        "shadow term must follow live rt_shadows toggles"
    );
    assert!(
        (observations[0].0 - observations[2].0).abs() / observations[0].0.max(1e-6) < 0.25,
        "RT on/off/on must recover the same shadow behaviour"
    );
}

#[test]
fn trace_ms_2vs6_caster_delta_reported_as_number() {
    // All casters shadowing, only lights 0-1 for 2-caster case,
    // lights 0-5 for 6-caster case. Both scenes identical otherwise.
    let scene_2 = caster_scene(0b11, 0x3f, true, true);
    let scene_6 = caster_scene(0x3f, 0x3f, true, true);

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
    let _times_2 = measure_frames(&scene_2, "2-caster-trace-ms", 16);
    let times_6 = measure_frames(&scene_6, "6-caster-trace-ms", 16);

    let _med_2 = {
        let mut s = _times_2.clone(); s.sort_by(|a,b| a.partial_cmp(b).unwrap());
        s[s.len()/2]
    };
    let med_6 = {
        let mut s = times_6.clone(); s.sort_by(|a,b| a.partial_cmp(b).unwrap());
        s[s.len()/2]
    };
    let max_6 = times_6.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    eprintln!("TRACE_MS 2-caster median = {_med_2:.3}ms, 6-caster median = {med_6:.3}ms, delta = {:.3}ms", med_6 - _med_2);
    eprintln!("TRACE_MS 6-caster max (post-warmup) = {max_6:.3}ms");

    // The brief: MANIFOLD_RENDER_TRACE=1, no frame >20ms
    assert!(max_6 < 20.0, "6-caster max frame {max_6:.3}ms must be under 20ms");
}
