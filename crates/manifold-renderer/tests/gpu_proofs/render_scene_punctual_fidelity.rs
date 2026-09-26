//! Production render-scene proof for glTF-style punctual light math.
//!
//! It renders a
//! real `node.render_scene` graph and samples only a small centre patch of a
//! flat plane, so no assertion depends on an anti-aliased silhouette edge.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

fn scene(
    mode: u32,
    falloff: u32,
    pos_y: f32,
    aim_x: f32,
    range: f32,
    inner: f32,
    outer: f32,
) -> String {
    format!(
        r#"{{"version":2,"name":"RenderScenePunctualFidelity","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{{
            "max_capacity":{{"type":"Int","value":4096}},"resolution_x":{{"type":"Int","value":24}},
            "resolution_y":{{"type":"Int","value":24}},"size_x":{{"type":"Float","value":6.0}},
            "size_y":{{"type":"Float","value":6.0}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{{
            "src_cols":{{"type":"Int","value":24}},"src_rows":{{"type":"Int","value":24}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.0}},"tilt":{{"type":"Float","value":0.55}},
            "distance":{{"type":"Float","value":8.0}},"fov_y":{{"type":"Float","value":0.9}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},"color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},"ambient":{{"type":"Float","value":0.0}},
            "specular_color_r":{{"type":"Float","value":0.0}},
            "specular_color_g":{{"type":"Float","value":0.0}},
            "specular_color_b":{{"type":"Float","value":0.0}}}}}},
        {{"id":30,"typeId":"node.light","nodeId":"light","params":{{
            "mode":{{"type":"Enum","value":{mode}}},"falloff":{{"type":"Enum","value":{falloff}}},
            "pos_x":{{"type":"Float","value":0.0}},"pos_y":{{"type":"Float","value":{pos_y}}},
            "pos_z":{{"type":"Float","value":0.0}},"aim_x":{{"type":"Float","value":{aim_x}}},
            "aim_y":{{"type":"Float","value":0.0}},"aim_z":{{"type":"Float","value":0.0}},
            "intensity":{{"type":"Float","value":1.0}},"range":{{"type":"Float","value":{range}}},
            "inner_cone_angle":{{"type":"Float","value":{inner}}},
            "outer_cone_angle":{{"type":"Float","value":{outer}}},
            "cast_shadows":{{"type":"Float","value":0.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},"lights":{{"type":"Int","value":1}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}]}}"#,
        mode = mode,
        falloff = falloff,
        pos_y = pos_y,
        aim_x = aim_x,
        range = range,
        inner = inner,
        outer = outer,
    )
}

fn center_luma(bytes: &[u8], width: u32, height: u32) -> f64 {
    let cx = width / 2;
    let cy = height / 2;
    let mut sum = 0.0;
    let mut count = 0u32;
    for y in cy.saturating_sub(2)..=(cy + 2).min(height - 1) {
        for x in cx.saturating_sub(2)..=(cx + 2).min(width - 1) {
            let offset = ((y * width + x) * 8) as usize;
            let r = f16::from_le_bytes([bytes[offset], bytes[offset + 1]]).to_f32();
            let g = f16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]).to_f32();
            let b = f16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]).to_f32();
            assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite punctual-light pixel");
            sum += f64::from((r + g + b) / 3.0);
            count += 1;
        }
    }
    sum / f64::from(count)
}

fn render_center(json: &str) -> f64 {
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
    .unwrap_or_else(|e| panic!("punctual fidelity graph must build: {e}\n{json}"));
    let target = h.make_target("render-scene-punctual-fidelity");
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
        let mut enc = h.device.create_encoder("render-scene-punctual-fidelity-enc");
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
    center_luma(&h.readback(&target.texture), h.width, h.height)
}

#[test]
fn punctual_spot_cone_and_inverse_square_range_are_visible_in_production_graph() {
    // The centre patch sits on the cone axis, in its penumbra, and outside it.
    let center = render_center(&scene(2, 1, 4.0, 0.0, 0.0, 0.15, 0.5));
    // Halfway between the authored cone cosines gives attenuation 0.5².
    // Use this analytic location instead of a point barely outside the core.
    let half_cosine = (0.15f32.cos() + 0.5f32.cos()) * 0.5;
    let penumbra_aim = 4.0 * half_cosine.acos().tan();
    let penumbra = render_center(&scene(2, 1, 4.0, penumbra_aim, 0.0, 0.15, 0.5));
    let outside = render_center(&scene(2, 1, 4.0, 4.0, 0.0, 0.15, 0.5));
    assert!(center.is_finite() && penumbra.is_finite() && outside.is_finite());
    assert!(center > 0.01, "spot centre must be illuminated");
    assert!((penumbra / center - 0.25).abs() < 0.06, "spot cosine ramp: centre={center:.5} penumbra={penumbra:.5}");
    assert!(outside < center * 0.01, "spot outside cone must be dark: outside={outside:.5}");

    // At the centre of the plane, moving an infinite physical point light
    // from d=2 to d=4 should follow inverse square (roughly 4×).
    let near = render_center(&scene(1, 1, 2.0, 0.0, 0.0, 0.0, 0.0));
    let far = render_center(&scene(1, 1, 4.0, 0.0, 0.0, 0.0, 0.0));
    assert!((near / far - 4.0).abs() < 0.2, "inverse-square point ratio: near={near:.5} far={far:.5}");

    // A finite range cuts off at d >= range while an unbounded physical light
    // remains visible at the same source/receiver distance.
    let cutoff = render_center(&scene(1, 1, 4.0, 0.0, 2.0, 0.0, 0.0));
    assert!(cutoff < far * 0.1, "physical range cutoff leaked direct light: cutoff={cutoff:.5} far={far:.5}");
}
