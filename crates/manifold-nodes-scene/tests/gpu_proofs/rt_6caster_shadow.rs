//! End-to-end proof that every shadow-casting light casts a shadow, raster and
//! ray traced, however many there are.
//!
//! Leading casters have zero radiance but stay enabled, so later lights are
//! genuine compacted slots, including slot 8, the first in the second
//! visibility layer. RT AO, GI, and reflections are
//! disabled in this fixture so the probes isolate direct shadow visibility.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::scene::camera::Camera;
use manifold_node_engine::{parameters::ParamValue, persistence::PrimitiveRegistry};

use manifold_nodes_scene::testkit::gpu_harness as harness;

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
/// direct contribution is zero.
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
        let intensity = if radiance_mask & (1 << i) != 0 {
            1.0
        } else {
            0.0
        };
        let cast_shadows = if shadow_mask & (1 << i) != 0 {
            1.0
        } else {
            0.0
        };
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
        {{"id":4,"typeId":"node.cel_material","nodeId":"mat","params":{{"color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},"band_low":{{"type":"Float","value":0.05}}}}}},
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

fn context(
    frame: i64,
    h: &manifold_node_engine::testkit::gpu_harness::ParityHarness,
) -> PresetContext {
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
    target: &manifold_node_engine::gpu::render_target::RenderTarget,
    frame: i64,
) -> (Vec<u8>, usize, FrameRenderStatus) {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
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
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &manifold_nodes_scene::testkit::scene_objects::bundle_scene_objects(json),
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
    region_luma_r(bytes, w, h, cx, cy, RADIUS)
}

fn region_luma_r(bytes: &[u8], w: u32, h: u32, cx: f32, cy: f32, radius: i32) -> f64 {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let mut sum = 0.0;
    let mut n = 0u64;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
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
fn caster_contract_compacted_slots_shadow() {
    for slot in [0, 3, 4, 7] {
        let (occluded, control, lit, lit_control, captures) = slot_probe(slot);
        eprintln!(
            "caster slot {slot}: shadow={occluded:.6} control={control:.6} lit={lit:.6} lit_control={lit_control:.6} captures={captures}"
        );
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
fn caster_contract_ninth_caster_and_sparse_light_indices() {
    // All nine request shadows and only the ninth is lit: it must shadow.
    let (ninth, w, h, captures) = render_readback(&caster_scene(0x1ff, 1 << 8, true, true));
    let (control, _, _, _) = render_readback(&caster_scene(0, 1 << 8, true, false));
    let (ocx, ocy) = project_to_pixel(OCCLUDED_WORLD, w, h);
    let value = region_luma(&ninth, w, h, ocx, ocy);
    let expected = region_luma(&control, w, h, ocx, ocy);
    assert!(captures > 0 && expected > 0.1);
    assert!(
        value * 1.02 < expected,
        "ninth caster must shadow: {value} vs {expected}"
    );

    // A hole in wired-light indices is not a hole in the caster list: the
    // ninth light becomes slot 4 when only four earlier lights cast shadows.
    let (sparse, _, _, _) = render_readback(&caster_scene(0b1_0000_1111, 1 << 8, true, true));
    let shadowed = region_luma(&sparse, w, h, ocx, ocy);
    assert!(
        shadowed * 1.02 < expected,
        "sparse ninth light must reach compacted slot 4"
    );
}

#[test]
fn caster_contract_live_rt_and_shadow_toggles() {
    use manifold_core::NodeId;
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = caster_scene(0xff, (1u16 << 7) | (1u16 << 8), true, true);
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &manifold_nodes_scene::testkit::scene_objects::bundle_scene_objects(&json),
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
    let occluder = runtime
        .graph
        .instance_by_node_id(&NodeId::new("occ_xform"))
        .unwrap();
    let target = h.make_target("rt-8caster-live-toggle");
    for frame in 0..RT_WARMUP_FRAMES {
        let _ = render_frame(&mut runtime, &target, frame);
    }
    let mut observations = Vec::new();
    // On the RT-shadows-off frame the occluder moves away. A live shadow map
    // follows it and lights the probe; a read gate that kept sampling the
    // accumulated RT visibility would still show the old shadow.
    for (frame, (rt_enabled, rt_shadows, occluder_x)) in [
        (true, true, 0.0),
        (false, false, 0.0),
        (true, true, 0.0),
        (true, false, 6.0),
        (true, true, 0.0),
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
        runtime
            .graph
            .set_param(occluder, "pos_x", ParamValue::Float(occluder_x))
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
    assert!(
        observations
            .iter()
            .all(|o| o.2 == FrameRenderStatus::Complete)
    );
    assert!(
        observations[0].1 > 0
            && observations[1].1 == 0
            && observations[2].1 > 0
            && observations[3].1 > 0
            && observations[4].1 > 0,
        "RT dispatch must follow on/off/on"
    );
    // Ray traced or raster, the same lights shadow: with RT switched off
    // live, slot 7 falls back to its shadow map and stays dark.
    let (control, _, _, _) =
        render_readback(&caster_scene(0, (1u16 << 7) | (1u16 << 8), true, false));
    let (ocx, ocy) = project_to_pixel(OCCLUDED_WORLD, h.width, h.height);
    let unshadowed = region_luma(&control, h.width, h.height, ocx, ocy);
    let lumas: Vec<f64> = observations.iter().map(|o| o.0).collect();
    eprintln!("live toggles: {lumas:?} unshadowed={unshadowed:.6}");
    assert!(
        [0, 1, 2, 4].iter().all(|&i| lumas[i] * 1.02 < unshadowed),
        "every live toggle state must keep slot 7 shadowed: {lumas:?} vs {unshadowed}"
    );
    assert!(
        (lumas[3] - unshadowed).abs() / unshadowed < 0.05,
        "with RT shadows off the probe must follow the moved occluder, not stale RT visibility: {lumas:?} vs {unshadowed}"
    );
    assert!(
        (observations[0].0 - observations[2].0).abs() / observations[0].0.max(1e-6) < 0.25,
        "RT on/off/on must recover the same shadow behaviour"
    );
}

const MIXED_LIGHTS: usize = 10;
const MIXED_OCC_SIZE: f32 = 1.6;
const MIXED_OCC_Y: f32 = 1.0;
const MIXED_SHADOW_DIST: f32 = 2.6;
const MIXED_PROBE_RADIUS: i32 = 2;

struct MixedLight {
    /// 0 sun, 1 point, 2 spot.
    mode: u32,
    pos: [f32; 3],
    shadow_resolution: f32,
    /// Where the occluder centre's shadow lands on the ground.
    shadow: [f32; 3],
}

/// Ten lights of all three kinds spread around one occluder. Every light sits
/// on the line from its own ground spot through the occluder centre, so each
/// drops its shadow on a different spot of the ground. Shadow resolutions
/// differ so the shadow-map array holds layers of different sizes.
fn mixed_lights() -> Vec<MixedLight> {
    (0..MIXED_LIGHTS)
        .map(|i| {
            let theta = (i as f32 + 0.5) * std::f32::consts::TAU / MIXED_LIGHTS as f32;
            let s = [
                MIXED_SHADOW_DIST * theta.cos(),
                MIXED_SHADOW_DIST * theta.sin(),
            ];
            let mode = (i % 3) as u32;
            let height = if mode == 0 { 6.0 } else { 4.0 };
            // From the ground spot through the occluder centre up to `height`.
            let k = 1.0 - height / MIXED_OCC_Y;
            MixedLight {
                mode,
                pos: [s[0] * k, height, s[1] * k],
                shadow_resolution: [512.0, 1024.0, 2048.0][i % 3],
                shadow: [s[0], 0.0, s[1]],
            }
        })
        .collect()
}

fn mixed_scene(cast_mask: u16, radiance_mask: u16, rt_enabled: bool) -> String {
    let mut lights = String::new();
    let mut wires = String::new();
    for (i, l) in mixed_lights().iter().enumerate() {
        // The lit lights share one budget so the all-lights frame does not
        // saturate and each shadow stays visible in it.
        let lit = radiance_mask.count_ones().max(1) as f32;
        let intensity = if radiance_mask & (1 << i) != 0 {
            1.5 / lit
        } else {
            0.0
        };
        let cast = if cast_mask & (1 << i) != 0 { 1.0 } else { 0.0 };
        // A sun's range is its shadow box half-extent; points and spots use it
        // as their reach.
        let range = if l.mode == 0 { 8.0 } else { 100.0 };
        lights.push_str(&format!(
            r#",{{"id":{id},"typeId":"node.light","nodeId":"mixed_{i}","params":{{
            "mode":{{"type":"Enum","value":{mode}}},"pos_x":{{"type":"Float","value":{px}}},"pos_y":{{"type":"Float","value":{py}}},"pos_z":{{"type":"Float","value":{pz}}},
            "aim_x":{{"type":"Float","value":0.0}},"aim_y":{{"type":"Float","value":{occ_y}}},"aim_z":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},
            "intensity":{{"type":"Float","value":{intensity}}},"range":{{"type":"Float","value":{range}}},
            "shadow_resolution":{{"type":"Float","value":{res}}},"cast_shadows":{{"type":"Float","value":{cast}}}}}}}"#,
            id = 30 + i,
            mode = l.mode,
            px = l.pos[0],
            py = l.pos[1],
            pz = l.pos[2],
            occ_y = MIXED_OCC_Y,
            res = l.shadow_resolution,
        ));
        wires.push_str(&format!(
            r#"{{"fromNode":{},"fromPort":"out","toNode":20,"toPort":"light_{}"}},"#,
            30 + i,
            i,
        ));
    }
    let rt = if rt_enabled { "true" } else { "false" };
    format!(
        r#"{{"version":2,"name":"MixedTenLightShadow","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":20}},"resolution_y":{{"type":"Int","value":20}},"size_x":{{"type":"Float","value":8.0}},"size_y":{{"type":"Float","value":8.0}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{{"src_cols":{{"type":"Int","value":20}},"src_rows":{{"type":"Int","value":20}}}}}},
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"occ_grid","params":{{"max_capacity":{{"type":"Int","value":8192}},"resolution_x":{{"type":"Int","value":10}},"resolution_y":{{"type":"Int","value":10}},"size_x":{{"type":"Float","value":{occ}}},"size_y":{{"type":"Float","value":{occ}}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"occ_tris","params":{{"src_cols":{{"type":"Int","value":10}},"src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"occ_xform","params":{{"pos_y":{{"type":"Float","value":{occ_y}}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{"orbit":{{"type":"Float","value":{ORBIT}}},"tilt":{{"type":"Float","value":{TILT}}},"distance":{{"type":"Float","value":{DISTANCE}}},"fov_y":{{"type":"Float","value":{FOV_Y}}}}}}},
        {{"id":4,"typeId":"node.cel_material","nodeId":"mat","params":{{"color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},"color_b":{{"type":"Float","value":1.0}},"band_low":{{"type":"Float","value":0.05}}}}}}
        {lights},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{"objects":{{"type":"Int","value":2}},"lights":{{"type":"Int","value":{n}}},"rt_enabled":{{"type":"Bool","value":{rt}}},"rt_shadows":{{"type":"Bool","value":{rt}}},"rt_ao":{{"type":"Bool","value":false}},"rt_gi":{{"type":"Bool","value":false}},"rt_reflections":{{"type":"Bool","value":false}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},{{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},{{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},{{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},{{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {wires}{{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#,
        occ = MIXED_OCC_SIZE,
        occ_y = MIXED_OCC_Y,
        n = MIXED_LIGHTS,
    )
}

fn tonemapped_rgba8(bytes: &[u8]) -> Vec<u8> {
    bytes
        .chunks_exact(8)
        .flat_map(|px| {
            (0..4).map(move |c| {
                if c == 3 {
                    return 255;
                }
                let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
                ((v / (1.0 + v)).clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8
            })
        })
        .collect()
}

/// Ten shadow-casting lights, three kinds, three shadow resolutions. Each light
/// is lit alone (the other nine still cast, at zero radiance, so it sits at
/// its real slot) and compared with itself not casting. Raster and ray traced
/// must both shadow every light: the same set, all ten. Writes
/// `/tmp/mixed_ten_light_shadows.png`: raster on the top row, ray traced below,
/// light 0 on the left; then the all-lights frames beside it.
#[test]
fn ten_mixed_lights_all_shadow_raster_and_ray_traced() {
    let all: u16 = (1 << MIXED_LIGHTS) - 1;
    let lights = mixed_lights();
    let mut shadowed = [Vec::new(), Vec::new()];
    let mut tiles: [Vec<Vec<u8>>; 2] = [Vec::new(), Vec::new()];
    let (mut w, mut h) = (0, 0);
    for (renderer, rt) in [false, true].into_iter().enumerate() {
        for (i, light) in lights.iter().enumerate() {
            let only = 1u16 << i;
            let (cast_bytes, bw, bh, _) = render_readback(&mixed_scene(all, only, rt));
            let (control_bytes, _, _, _) = render_readback(&mixed_scene(all & !only, only, rt));
            (w, h) = (bw, bh);
            let (sx, sy) = project_to_pixel(light.shadow, w, h);
            let lit = region_luma_r(&cast_bytes, w, h, sx, sy, MIXED_PROBE_RADIUS);
            let control = region_luma_r(&control_bytes, w, h, sx, sy, MIXED_PROBE_RADIUS);
            let name = if rt { "rt" } else { "raster" };
            eprintln!(
                "{name} light {i} (mode {}): shadow={lit:.4} control={control:.4}",
                light.mode
            );
            assert!(
                control > 0.05,
                "{name} light {i} must light its probe: {control}"
            );
            if lit < control * 0.8 {
                shadowed[renderer].push(i);
            }
            tiles[renderer].push(tonemapped_rgba8(&cast_bytes));
        }
    }
    let (all_raster, _, _, _) = render_readback(&mixed_scene(all, all, false));
    let (all_rt, _, _, _) = render_readback(&mixed_scene(all, all, true));
    tiles[0].push(tonemapped_rgba8(&all_raster));
    tiles[1].push(tonemapped_rgba8(&all_rt));

    let cols = (MIXED_LIGHTS + 1) as u32;
    let mut sheet = vec![0u8; (cols * w * 2 * h * 4) as usize];
    for (row, row_tiles) in tiles.iter().enumerate() {
        for (col, tile) in row_tiles.iter().enumerate() {
            for y in 0..h {
                let src = (y * w * 4) as usize;
                let dst = (((row as u32 * h + y) * cols * w + col as u32 * w) * 4) as usize;
                sheet[dst..dst + (w * 4) as usize]
                    .copy_from_slice(&tile[src..src + (w * 4) as usize]);
            }
        }
    }
    image::save_buffer(
        "/tmp/mixed_ten_light_shadows.png",
        &sheet,
        cols * w,
        2 * h,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write contact sheet");

    let every: Vec<usize> = (0..MIXED_LIGHTS).collect();
    assert_eq!(
        shadowed[0], shadowed[1],
        "raster and ray traced must shadow the same lights"
    );
    assert_eq!(
        shadowed[0], every,
        "every shadow-casting light must cast a shadow"
    );
}

/// Wall-clock, so `rt-perf-proofs` only (run with `--test-threads=1`): in
/// the parallel correctness binary a frame time measures the other tests.
#[cfg(feature = "rt-perf-proofs")]
#[test]
fn trace_ms_2vs6_caster_delta_reported_as_number() {
    // All casters shadowing, only lights 0-1 for 2-caster case,
    // lights 0-5 for 6-caster case. Both scenes identical otherwise.
    let scene_2 = caster_scene(0b11, 0x3f, true, true);
    let scene_6 = caster_scene(0x3f, 0x3f, true, true);

    fn measure_frames(json: &str, label: &str, runs: usize) -> Vec<f64> {
        let h = manifold_node_engine::testkit::gpu_harness::shared();
        let registry = PrimitiveRegistry::with_builtin();
        let mut runtime = PresetRuntime::from_json_str_with_device(
            &manifold_nodes_scene::testkit::scene_objects::bundle_scene_objects(json),
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
                frame_count: frame as i64,
                anim_progress: 0.0,
                trigger_count: 0,
            };
            let mut enc = h.device.create_encoder("rt-trace-ms-enc");
            let t0 = std::time::Instant::now();
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
        eprintln!(
            "{label}: median={median:.3}ms max={max:.3}ms over {r} frames (warmup discarded)",
            r = tail.len()
        );
        tail
    }

    // 16 warmup frames for each, measure last 10
    let _times_2 = measure_frames(&scene_2, "2-caster-trace-ms", 16);
    let times_6 = measure_frames(&scene_6, "6-caster-trace-ms", 16);

    let _med_2 = {
        let mut s = _times_2.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s[s.len() / 2]
    };
    let med_6 = {
        let mut s = times_6.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s[s.len() / 2]
    };
    let max_6 = times_6.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    eprintln!(
        "TRACE_MS 2-caster median = {_med_2:.3}ms, 6-caster median = {med_6:.3}ms, delta = {:.3}ms",
        med_6 - _med_2
    );
    eprintln!("TRACE_MS 6-caster max (post-warmup) = {max_6:.3}ms");

    // The brief: MANIFOLD_RENDER_TRACE=1, no frame >20ms
    assert!(
        max_6 < 20.0,
        "6-caster max frame {max_6:.3}ms must be under 20ms"
    );
}
