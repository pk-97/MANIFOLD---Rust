//! `node.render_scene` unbounded-lights proof
//! (RENDER_SCENE_UNBOUNDED_LIGHTS_DESIGN §4 gate items 3 + the acceptance
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
