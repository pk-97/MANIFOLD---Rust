//! `node.render_scene` unbounded-lights proof
//! (RENDER_SCENE_UNBOUNDED_LIGHTS_DESIGN section 4 gate items 3 + the acceptance
//! PNG). The lights design moves light data out of the fixed uniform array
//! into an `@binding(8) var<storage, read>` buffer, uncapping light count.
//! `fragment_storage.rs` already proves the fragment-stage storage-read
//! MECHANIC in isolation (via `draw_fullscreen`); what THIS proves is the
//! integration the isolated probe can't reach: that binding 8 actually
//! arrives at render_scene's `draw_instanced_depth_msaa_batch` pipeline —
//! a different encoder path — and that MORE THAN THE OLD CAP OF 4 lights
//! visibly contribute.
//!
//! Decisive design: a flat lit plane under EIGHT sun lights — lights 0–3
//! RED (dim), lights 4–7 GREEN (bright). If only the first four were
//! honoured (the old `MAX_LIGHTS = 4` array), the plane renders RED. If
//! all eight contribute (the storage buffer), it renders GREEN-dominant.
//! The green-over-red assertion is a direct readout of "lights past index
//! 3 reached the shader". A second graph wires ZERO lights and asserts the
//! frame renders finite without a Metal validation error — the D4 proof
//! that binding 8's one zeroed entry keeps the buffer validly bound.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// Build a render_scene generator graph: a flat grid plane lit by
/// `light_specs.len()` sun lights, each `(r, g, b, intensity)`. Lights all
/// sit overhead (pos_y = 30) aiming at the origin, so every one fully
/// illuminates the +Y plane normal (N·L = 1) and its colour lands directly
/// in the summed diffuse term.
fn scene_json(light_specs: &[(f32, f32, f32, f32)]) -> String {
    let n = light_specs.len();
    let mut nodes = String::new();

    // Mesh: 16×16 grid plane → triangle list.
    nodes.push_str(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{
            "max_capacity":{"type":"Int","value":8192},
            "resolution_x":{"type":"Int","value":16},
            "resolution_y":{"type":"Int","value":16},
            "size_x":{"type":"Float","value":4.0},
            "size_y":{"type":"Float","value":4.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{
            "src_cols":{"type":"Int","value":16},
            "src_rows":{"type":"Int","value":16}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.6},
            "tilt":{"type":"Float","value":0.6},
            "distance":{"type":"Float","value":6.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.02}}},"#,
    );

    // The render_scene node: 1 object, `n` lights.
    nodes.push_str(&format!(
        r#"{{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":{n}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}"#,
    ));

    // One light node per spec.
    for (i, (r, g, b, intensity)) in light_specs.iter().enumerate() {
        let id = 30 + i;
        nodes.push_str(&format!(
            r#",{{"id":{id},"typeId":"node.light","nodeId":"light_{i}","params":{{
                "mode":{{"type":"Enum","value":0}},
                "pos_x":{{"type":"Float","value":0.0}},
                "pos_y":{{"type":"Float","value":30.0}},
                "pos_z":{{"type":"Float","value":0.0}},
                "aim_x":{{"type":"Float","value":0.0}},
                "aim_y":{{"type":"Float","value":0.0}},
                "aim_z":{{"type":"Float","value":0.0}},
                "color_r":{{"type":"Float","value":{r}}},
                "color_g":{{"type":"Float","value":{g}}},
                "color_b":{{"type":"Float","value":{b}}},
                "intensity":{{"type":"Float","value":{intensity}}},
                "cast_shadows":{{"type":"Float","value":0.0}}}}}}"#,
        ));
    }

    // Wires: mesh chain, camera, material, each light into light_i, terminal.
    let mut wires = String::from(
        r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#,
    );
    for i in 0..n {
        let id = 30 + i;
        wires.push_str(&format!(
            r#",{{"fromNode":{id},"fromPort":"out","toNode":20,"toPort":"light_{i}"}}"#,
        ));
    }

    format!(r#"{{"version":2,"name":"RenderSceneLightsProof","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

/// Render a scene-graph JSON to `WxH` `Rgba16Float`, returning the readback
/// bytes. Two committed frames so any first-frame pipeline/target warm-up is
/// past before we read. `commit_and_wait_completed` inside the executor hard-
/// checks for Metal GPU errors, so a broken binding-8 bind surfaces as a
/// panic here, not a silently wrong frame.
fn render_scene_readback(json: &str) -> (Vec<u8>, u32, u32) {
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
    .expect("render_scene lights graph must build");

    let target = h.make_target("render-scene-lights");
    for frame in 0..2 {
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
        let mut enc = h.device.create_encoder("render-scene-lights-enc");
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

/// Sum per-channel intensity and peak luma over an `Rgba16Float` readback.
fn channel_sums(bytes: &[u8]) -> (f64, f64, f64, f32) {
    let mut sr = 0.0f64;
    let mut sg = 0.0f64;
    let mut sb = 0.0f64;
    let mut peak = 0.0f32;
    for px in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
        let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
        let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
        assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
        sr += r as f64;
        sg += g as f64;
        sb += b as f64;
        peak = peak.max(r.max(g).max(b));
    }
    (sr, sg, sb, peak)
}

/// Build the small, direct-light-only fixture used by the light contract gate.
/// The PBR environment is explicitly black so every material reads the same
/// point-light discriminators; RT terms are disabled individually so the
/// comparison stays about surface lighting rather than AO/GI/reflections or
/// shadows.
fn light_contract_scene_json(
    material: &str,
    mode: u32,
    pos_y: f32,
    aim_x: f32,
    range: f32,
    rt_enabled: bool,
) -> String {
    let rt = if rt_enabled { "true" } else { "false" };
    let material_node = match material {
        "phong" => r#"{"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.0},
            "specular_color_r":{"type":"Float","value":0.0},
            "specular_color_g":{"type":"Float","value":0.0},
            "specular_color_b":{"type":"Float","value":0.0}}}"#,
        "pbr" => r#"{"id":4,"typeId":"node.pbr_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.0},
            "metallic":{"type":"Float","value":0.0},
            "roughness":{"type":"Float","value":1.0}}}"#,
        "cel" => r#"{"id":4,"typeId":"node.cel_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "cel_bands":{"type":"Int","value":4},
            "band_low":{"type":"Float","value":0.1},
            "band_high":{"type":"Float","value":1.0}}}"#,
        other => panic!("unknown light-contract material {other}"),
    };
    let env = if material == "pbr" {
        r#",{"id":5,"typeId":"node.bake_environment","nodeId":"env","params":{
            "width":{"type":"Int","value":16},
            "height":{"type":"Int","value":8},
            "intensity":{"type":"Float","value":0.0}}}"#
    } else {
        ""
    };
    let env_wire = if material == "pbr" {
        r#",{"fromNode":5,"fromPort":"envmap","toNode":20,"toPort":"envmap"}"#
    } else {
        ""
    };
    format!(
        r#"{{"version":2,"name":"LightContract","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":24}},
            "resolution_y":{{"type":"Int","value":24}},
            "size_x":{{"type":"Float","value":6.0}},
            "size_y":{{"type":"Float","value":6.0}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{{
            "src_cols":{{"type":"Int","value":24}},
            "src_rows":{{"type":"Int","value":24}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.0}},
            "tilt":{{"type":"Float","value":0.55}},
            "distance":{{"type":"Float","value":8.0}},
            "fov_y":{{"type":"Float","value":0.9}}}}}},
        {material_node}{env},
        {{"id":30,"typeId":"node.light","nodeId":"light","params":{{
            "mode":{{"type":"Enum","value":{mode}}},
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":{pos_y}}},
            "pos_z":{{"type":"Float","value":0.0}},
            "aim_x":{{"type":"Float","value":{aim_x}}},
            "aim_y":{{"type":"Float","value":0.0}},
            "aim_z":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "intensity":{{"type":"Float","value":1.0}},
            "range":{{"type":"Float","value":{range}}},
            "cast_shadows":{{"type":"Float","value":0.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":1}},
            "rt_enabled":{{"type":"Bool","value":{rt}}},
            "rt_reflections":{{"type":"Bool","value":false}},
            "rt_shadows":{{"type":"Bool","value":false}},
            "rt_ao":{{"type":"Bool","value":false}},
            "rt_gi":{{"type":"Bool","value":false}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}}{env_wire},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#,
        material_node = material_node,
        env = env,
        mode = mode,
        pos_y = pos_y,
        aim_x = aim_x,
        range = range,
        rt = rt,
        env_wire = env_wire,
    )
}

fn render_light_contract_scene(json: &str, rt_enabled: bool, assert_dispatch: bool) -> f64 {
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
    .expect("light contract graph must build");
    let target = h.make_target("light-contract");
    let frames: i64 = if rt_enabled { 16 } else { 2 };
    let render_frame = |runtime: &mut PresetRuntime, frame: i64| {
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
        let mut enc = h.device.create_encoder("light-contract-enc");
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
    };
    for frame in 0..frames {
        render_frame(&mut runtime, frame);
    }
    if assert_dispatch {
        harness::assert_rt_dispatched(
            || render_frame(&mut runtime, frames),
            "light_contract RT point-light scene",
        );
    }
    let (r, g, b, _) = channel_sums(&h.readback(&target.texture));
    r + g + b
}

/// Tonemap an `Rgba16Float` readback to sRGB-ish `rgba8` and write a PNG.
fn write_png(bytes: &[u8], w: u32, h: u32, path: &str) {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in bytes.chunks_exact(8) {
        for c in 0..4 {
            let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
            let mapped = (v / (1.0 + v)).clamp(0.0, 1.0); // Reinhard
            out.push((mapped.powf(1.0 / 2.2) * 255.0).round() as u8);
        }
    }
    image::save_buffer(path, &out, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
}

#[test]
fn eight_lights_render_past_the_old_cap_of_four() {
    // Lights 0–3 red (dim), lights 4–7 green (bright). All eight overhead.
    let specs: Vec<(f32, f32, f32, f32)> = (0..8)
        .map(|i| if i < 4 { (1.0, 0.0, 0.0, 0.3) } else { (0.0, 1.0, 0.0, 0.8) })
        .collect();
    let (bytes, w, h) = render_scene_readback(&scene_json(&specs));

    write_png(&bytes, w, h, "/tmp/render_scene_8_lights.png");
    let (sr, sg, sb, peak) = channel_sums(&bytes);

    // Something is lit — not a black frame.
    assert!(peak > 0.2, "8-light plane is unlit (peak {peak}) — lights not contributing");
    // Green (lights 4–7) dominates red (lights 0–3): the 5th–8th lights,
    // impossible under the old fixed-array cap of 4, are contributing.
    assert!(
        sg > sr * 1.5,
        "green (lights 4–7) should dominate red (lights 0–3): \
         sum_r={sr:.1} sum_g={sg:.1} sum_b={sb:.1} — lights past index 3 did NOT reach binding 8"
    );
}

#[test]
fn zero_lights_render_without_validation_error() {
    // D4: no light ports wired. render_scene must still bind the one zeroed
    // storage entry so Metal never sees a null slot. Bump ambient so the
    // plane is visible-from-ambient (proving the draw ran, not just that it
    // didn't crash). A GPU validation error would panic in the executor's
    // commit_and_wait; reaching the asserts means binding 8 stayed valid.
    let json = scene_json(&[]).replace(
        r#""ambient":{"type":"Float","value":0.02}"#,
        r#""ambient":{"type":"Float","value":0.4}"#,
    );
    let (bytes, w, h) = render_scene_readback(&json);
    write_png(&bytes, w, h, "/tmp/render_scene_0_lights.png");
    let (_sr, _sg, _sb, peak) = channel_sums(&bytes);
    assert!(peak > 0.1, "zero-light ambient plane should still render (peak {peak})");
}

#[test]
fn light_contract_point_position_aim_range_and_sun_controls() {
    // The same direct-light probe runs through all three surface pipelines.
    // Aim must not affect a Point light: only its position determines L.
    for material in ["phong", "pbr", "cel"] {
        let point = |pos_y: f32, aim_x: f32, range: f32| {
            render_light_contract_scene(
                &light_contract_scene_json(material, 1, pos_y, aim_x, range, false),
                false,
                false,
            )
        };
        let near = point(4.0, 0.0, 4.0);
        let retargeted = point(4.0, 10.0, 4.0);
        let far = point(8.0, 0.0, 4.0);
        assert!(near.is_finite() && retargeted.is_finite() && far.is_finite());
        assert!(near > 0.01, "{material} point probe is unlit: {near:.5}");
        assert!(
            (retargeted - near).abs() / near < 0.08,
            "{material} Point aim must not change direct illumination: aim0={near:.5} aim10={retargeted:.5}"
        );
        assert!(
            near > far * 1.35,
            "{material} Point position must attenuate with distance: near={near:.5} far={far:.5}"
        );

        // Sun range is a frustum control, never a direct-light attenuation.
        let sun_short = render_light_contract_scene(
            &light_contract_scene_json(material, 0, 4.0, 0.0, 0.5, false),
            false,
            false,
        );
        let sun_long = render_light_contract_scene(
            &light_contract_scene_json(material, 0, 4.0, 0.0, 100.0, false),
            false,
            false,
        );
        assert!(
            (sun_short - sun_long).abs() / sun_long.max(0.01) < 0.08,
            "{material} Sun range must not attenuate direct lighting: short={sun_short:.5} long={sun_long:.5}"
        );
    }

    // With negligible distance falloff, moving the Point sideways must
    // still change N dot L. This distinguishes position-derived direction
    // from a fix that only applies distance attenuation to the old Sun vector.
    let overhead_json = light_contract_scene_json("phong", 1, 4.0, 0.0, 1_000_000.0, false);
    let side_json = overhead_json.replace(
        r#""pos_x":{"type":"Float","value":0.0}"#,
        r#""pos_x":{"type":"Float","value":6.0}"#,
    );
    assert_ne!(overhead_json, side_json);
    let overhead = render_light_contract_scene(&overhead_json, false, false);
    let side = render_light_contract_scene(&side_json, false, false);
    assert!(overhead > side * 1.2, "Point direction must follow position: overhead={overhead} side={side}");

    // A zero-range Point is finite and contributes no direct light. Keep this
    // on Phong to isolate the exact zero-range contract from PBR IBL policy.
    let zero = render_light_contract_scene(
        &light_contract_scene_json("phong", 1, 4.0, 0.0, 0.0, false),
        false,
        false,
    );
    assert!(zero.is_finite() && zero < 0.01, "zero-range Point must be dark and finite: {zero:.5}");
    let short_range = render_light_contract_scene(
        &light_contract_scene_json("phong", 1, 4.0, 0.0, 2.0, false),
        false,
        false,
    );
    let long_range = render_light_contract_scene(
        &light_contract_scene_json("phong", 1, 4.0, 0.0, 8.0, false),
        false,
        false,
    );
    assert!(
        long_range > short_range * 1.8,
        "Point range must attenuate direct lighting: short={short_range:.5} long={long_range:.5}"
    );

    // RT remains a real dispatch even with AO/GI/reflections/shadows off. Its
    // direct surface result must retain the same Point position attenuation.
    let rt_near = render_light_contract_scene(
        &light_contract_scene_json("pbr", 1, 4.0, 0.0, 4.0, true),
        true,
        true,
    );
    let rt_far = render_light_contract_scene(
        &light_contract_scene_json("pbr", 1, 8.0, 0.0, 4.0, true),
        true,
        false,
    );
    assert!(
        rt_near > rt_far * 1.35,
        "RT Point position must attenuate with distance: near={rt_near:.5} far={rt_far:.5}"
    );
}
