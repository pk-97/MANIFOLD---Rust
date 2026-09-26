//! Numeric GPU proofs for the material/PBR math in `render_scene.wgsl`.
//!
//! Each assertion renders the production scene graph through Metal and compares
//! it with an independently parameterized control scene.  The controls keep
//! geometry, camera, environment, and raster path identical, so a difference
//! isolates the material math under test.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

fn solid_texture_nodes(id: u32, rgba: [f32; 4]) -> (String, String, u32) {
    let color = format!("[{},{},{},{}]", rgba[0], rgba[1], rgba[2], rgba[3]);
    let nodes = format!(
        r#"{{"id":{id},"typeId":"node.linear_gradient","nodeId":"solid_src_{id}","params":{{
            "cx":{{"type":"Float","value":-5.0}},"softness":{{"type":"Float","value":0.0}}}}}},
        {{"id":{ramp},"typeId":"node.gradient_map","nodeId":"solid_{id}","params":{{
            "color_a":{{"type":"Color","value":{color}}},"color_b":{{"type":"Color","value":{color}}}}}}},"#,
        ramp = id + 1,
    );
    let wires = format!(
        r#"{{"fromNode":{id},"fromPort":"out","toNode":{ramp},"toPort":"source"}},"#,
        ramp = id + 1,
    );
    (nodes, wires, id + 1)
}

fn material_params(color: [f32; 3], metallic: f32, roughness: f32, extras: &str) -> String {
    format!(
        r#"{{"id":4,"typeId":"node.pbr_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":{}}},"color_g":{{"type":"Float","value":{}}},
            "color_b":{{"type":"Float","value":{}}},"metallic":{{"type":"Float","value":{metallic}}},
            "roughness":{{"type":"Float","value":{roughness}}},"ambient":{{"type":"Float","value":0.0}}{extras}}}}},"#,
        color[0], color[1], color[2],
    )
}

fn scene(material: String, orbit: f32, light: bool, mr_map: Option<[f32; 4]>) -> String {
    scene_with_options(material, orbit, light, 1.0, mr_map, None, None, None)
}

fn scene_with_options(
    material: String,
    orbit: f32,
    light: bool,
    light_intensity: f32,
    mr_map: Option<[f32; 4]>,
    normal_map: Option<[f32; 4]>,
    clearcoat_normal_map: Option<[f32; 4]>,
    occlusion_map: Option<[f32; 4]>,
) -> String {
    let (env_nodes, env_wires, env_out) = solid_texture_nodes(700, [1.0, 1.0, 1.0, 1.0]);
    let mut map_nodes = String::new();
    let mut map_wires = env_wires;
    map_wires.push_str(&format!(
        "{{\"fromNode\":{env_out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"envmap\"}},"
    ));
    if let Some(rgba) = mr_map {
        let (nodes, wires, out) = solid_texture_nodes(900, rgba);
        map_nodes.push_str(&nodes);
        map_wires.push_str(&wires);
        map_wires.push_str(&format!(
            "{{\"fromNode\":{out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"mr_map_0\"}},"
        ));
    }
    if let Some(rgba) = normal_map {
        let (nodes, wires, out) = solid_texture_nodes(1000, rgba);
        map_nodes.push_str(&nodes);
        map_wires.push_str(&wires);
        map_wires.push_str(&format!(
            "{{\"fromNode\":{out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"normal_map_0\"}},"
        ));
    }
    if let Some(rgba) = clearcoat_normal_map {
        let (nodes, wires, out) = solid_texture_nodes(1100, rgba);
        map_nodes.push_str(&nodes);
        map_wires.push_str(&wires);
        map_wires.push_str(&format!(
            "{{\"fromNode\":{out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"clearcoat_normal_map_0\"}},"
        ));
    }
    if let Some(rgba) = occlusion_map {
        let (nodes, wires, out) = solid_texture_nodes(1200, rgba);
        map_nodes.push_str(&nodes);
        map_wires.push_str(&wires);
        map_wires.push_str(&format!(
            "{{\"fromNode\":{out},\"fromPort\":\"out\",\"toNode\":20,\"toPort\":\"occlusion_map_0\"}},"
        ));
    }
    let (light_node, light_wire, light_count) = if light {
        (
            format!(
                r#"{{"id":30,"typeId":"node.light","nodeId":"light","params":{{
                "mode":{{"type":"Enum","value":0}},"pos_x":{{"type":"Float","value":0.0}},
                "pos_y":{{"type":"Float","value":30.0}},"pos_z":{{"type":"Float","value":0.0}},
                "aim_x":{{"type":"Float","value":0.0}},"aim_y":{{"type":"Float","value":0.0}},"aim_z":{{"type":"Float","value":0.0}},
                "intensity":{{"type":"Float","value":{light_intensity}}},"cast_shadows":{{"type":"Float","value":0.0}}}}}},"#,
                light_intensity = light_intensity,
            ),
            r#"{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"},"#,
            1,
        )
    } else {
        (String::new(), "", 0)
    };
    format!(
        r#"{{"version":2,"name":"RenderScenePbrFidelity","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{{"max_capacity":{{"type":"Int","value":1024}},"resolution_x":{{"type":"Int","value":16}},"resolution_y":{{"type":"Int","value":16}},"size_x":{{"type":"Float","value":6.0}},"size_y":{{"type":"Float","value":6.0}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{{"src_cols":{{"type":"Int","value":16}},"src_rows":{{"type":"Int","value":16}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{"orbit":{{"type":"Float","value":{orbit}}},"tilt":{{"type":"Float","value":0.65}},"distance":{{"type":"Float","value":4.0}},"fov_y":{{"type":"Float","value":1.1}}}}}},
        {material}
        {env_nodes}
        {map_nodes}
        {light_node}
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{"objects":{{"type":"Int","value":1}},"lights":{{"type":"Int","value":{light_count}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {map_wires}{light_wire}
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}]}}"#,
        material = material,
        map_nodes = map_nodes,
    )
}

fn scene_with_light_aim(
    material: String,
    orbit: f32,
    light_intensity: f32,
    aim: [f32; 3],
) -> String {
    let mut json = scene_with_options(
        material,
        orbit,
        true,
        light_intensity,
        None,
        None,
        None,
        None,
    );
    for (name, value) in [("aim_x", aim[0]), ("aim_y", aim[1]), ("aim_z", aim[2])] {
        let needle = format!("\"{name}\":{{\"type\":\"Float\",\"value\":0.0}}");
        let replacement = format!("\"{name}\":{{\"type\":\"Float\",\"value\":{value}}}");
        assert!(json.contains(&needle), "light aim field {name} missing");
        json = json.replace(&needle, &replacement);
    }
    json
}

fn render_center(json: &str) -> [f32; 3] {
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
    .unwrap_or_else(|e| panic!("PBR proof graph must build: {e}\n{json}"));
    let target = h.make_target("render-scene-pbr-fidelity");
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
        let mut enc = h.device.create_encoder("render-scene-pbr-fidelity");
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
    let bytes = h.readback(&target.texture);
    let idx = ((h.height / 2) * h.width + h.width / 2) as usize * 8;
    [
        f16::from_le_bytes([bytes[idx], bytes[idx + 1]]).to_f32(),
        f16::from_le_bytes([bytes[idx + 2], bytes[idx + 3]]).to_f32(),
        f16::from_le_bytes([bytes[idx + 4], bytes[idx + 5]]).to_f32(),
    ]
}

fn assert_rgb_close(a: [f32; 3], b: [f32; 3], tolerance: f32, label: &str) {
    for c in 0..3 {
        assert!(
            (a[c] - b[c]).abs() <= tolerance,
            "{label} channel {c}: {a:?} vs {b:?}"
        );
    }
}

fn anisotropic_d(alpha_t: f32, alpha_b: f32, h: [f32; 3], n_dot_h: f32, tangent_angle: f32) -> f32 {
    let tangent = [tangent_angle.cos(), 0.0, tangent_angle.sin()];
    let bitangent = [tangent_angle.sin(), 0.0, -tangent_angle.cos()];
    let t_dot_h = tangent[0] * h[0] + tangent[1] * h[1] + tangent[2] * h[2];
    let b_dot_h = bitangent[0] * h[0] + bitangent[1] * h[1] + bitangent[2] * h[2];
    let alpha2 = alpha_t * alpha_b;
    let v2 = (alpha_b * t_dot_h).powi(2) + (alpha_t * b_dot_h).powi(2) + (alpha2 * n_dot_h).powi(2);
    let w2 = alpha2 / v2.max(1e-7);
    alpha2 * w2 * w2 / std::f32::consts::PI
}

fn schlick(f0: f32, f90: f32, cos_theta: f32) -> f32 {
    let weight = (1.0 - cos_theta).clamp(0.0, 1.0).powi(5);
    f0 + (f90 - f0) * weight
}

#[test]
fn metallic_roughness_map_multiplies_authored_factors() {
    let control = material_params([0.8, 0.6, 0.2], 0.3, 0.35, "");
    let mapped = material_params([0.8, 0.6, 0.2], 0.6, 0.7, "");
    let control_rgb = render_center(&scene(control, 0.0, false, None));
    let mapped_rgb = render_center(&scene(mapped, 0.0, false, Some([0.0, 0.5, 0.5, 1.0])));
    assert_rgb_close(mapped_rgb, control_rgb, 0.035, "MR modulation");
}

#[test]
fn zero_dielectric_specular_factor_removes_grazing_reflection() {
    let black = [0.0, 0.0, 0.0];
    let zero = material_params(
        black,
        0.0,
        0.12,
        ",\"specular\":{\"type\":\"Float\",\"value\":0.0}",
    );
    let full = material_params(
        black,
        0.0,
        0.12,
        ",\"specular\":{\"type\":\"Float\",\"value\":1.0}",
    );
    let zero_rgb = render_center(&scene(zero, 0.75, true, None));
    let full_rgb = render_center(&scene(full, 0.75, true, None));
    assert!(
        zero_rgb.iter().all(|c| c.abs() < 0.01),
        "zero F90 must remove dielectric reflection: {zero_rgb:?}"
    );
    assert!(
        full_rgb.iter().any(|c| *c > 0.01),
        "unit F90 should retain direct grazing reflection: {full_rgb:?}"
    );
}

#[test]
fn nonzero_anisotropy_matches_the_gltf_width_formula() {
    let roughness = 0.5;
    let strength = 0.5;
    let alpha = roughness * roughness;
    let alpha_t = alpha + (1.0 - alpha) * strength * strength;
    let alpha_b = alpha;
    let tilt: f32 = 0.65;
    let view = [tilt.cos(), tilt.sin(), 0.0];
    let light = [0.0, 1.0, 0.0];
    let h_raw = [view[0] + light[0], view[1] + light[1], view[2] + light[2]];
    let h_len = (h_raw[0] * h_raw[0] + h_raw[1] * h_raw[1] + h_raw[2] * h_raw[2]).sqrt();
    let h = [h_raw[0] / h_len, h_raw[1] / h_len, h_raw[2] / h_len];
    let expected_ratio = anisotropic_d(alpha_t, alpha_b, h, h[1], 0.0)
        / anisotropic_d(alpha_t, alpha_b, h, h[1], std::f32::consts::FRAC_PI_2);
    assert!(expected_ratio > 4.0 && expected_ratio < 5.5);

    let material = |rotation: f32| {
        material_params(
            [0.0, 0.0, 0.0],
            0.0,
            roughness,
            &format!(
                ",\"anisotropy_strength\":{{\"type\":\"Float\",\"value\":{strength}}},\"anisotropy_rotation\":{{\"type\":\"Float\",\"value\":{rotation}}}"
            ),
        )
    };
    let tangent_aligned = render_center(&scene_with_options(
        material(0.0),
        0.0,
        true,
        20.0,
        None,
        None,
        None,
        None,
    ));
    let bitangent_aligned = render_center(&scene_with_options(
        material(std::f32::consts::FRAC_PI_2),
        0.0,
        true,
        20.0,
        None,
        None,
        None,
        None,
    ));
    let measured_ratio = tangent_aligned[0] / bitangent_aligned[0].max(1e-4);
    assert!(
        measured_ratio > 2.0 && measured_ratio < 5.5,
        "anisotropic alphaT/alphaB response {measured_ratio:.3} should follow independent glTF ratio {expected_ratio:.3}; outputs {tangent_aligned:?} vs {bitangent_aligned:?}"
    );
}

#[test]
fn nonzero_clearcoat_applies_view_fresnel_once_and_uses_geometric_fallback() {
    let base = material_params([0.0, 0.0, 0.0], 0.0, 0.32, "");
    let coat_only = material_params(
        [0.0, 0.0, 0.0],
        0.0,
        0.32,
        ",\"specular\":{\"type\":\"Float\",\"value\":0.0},\"clearcoat\":{\"type\":\"Float\",\"value\":1.0},\"clearcoat_roughness\":{\"type\":\"Float\",\"value\":0.25}",
    );
    let layered = material_params(
        [0.0, 0.0, 0.0],
        0.0,
        0.32,
        ",\"clearcoat\":{\"type\":\"Float\",\"value\":0.5},\"clearcoat_roughness\":{\"type\":\"Float\",\"value\":0.25}",
    );
    let base_rgb = render_center(&scene_with_options(
        base, 0.0, true, 20.0, None, None, None, None,
    ));
    let coat_rgb = render_center(&scene_with_options(
        coat_only, 0.0, true, 20.0, None, None, None, None,
    ));
    let layered_rgb = render_center(&scene_with_options(
        layered, 0.0, true, 20.0, None, None, None, None,
    ));
    // An isolated coat uses the same dielectric BRDF as an uncoated black
    // dielectric with matching roughness. This absolute control catches a
    // duplicated Fresnel factor even when both layered controls share it.
    let matching_dielectric = render_center(&scene_with_options(
        material_params([0.0, 0.0, 0.0], 0.0, 0.25, ""),
        0.0, true, 20.0, None, None, None, None,
    ));
    assert_rgb_close(coat_rgb, matching_dielectric, 0.055, "coat dielectric energy");
    let view_cos = 0.65f32.sin();
    let view_fresnel = schlick(0.04, 1.0, view_cos);
    let expected = [
        base_rgb[0] * (1.0 - 0.5 * view_fresnel) + coat_rgb[0] * 0.5,
        base_rgb[1] * (1.0 - 0.5 * view_fresnel) + coat_rgb[1] * 0.5,
        base_rgb[2] * (1.0 - 0.5 * view_fresnel) + coat_rgb[2] * 0.5,
    ];
    assert_rgb_close(layered_rgb, expected, 0.055, "single clearcoat Fresnel");

    let normal_map = [0.5, 0.8, 0.9, 1.0];
    let geometric_default = render_center(&scene_with_options(
        material_params(
            [0.0, 0.0, 0.0],
            0.0,
            0.32,
            ",\"clearcoat\":{\"type\":\"Float\",\"value\":0.7},\"clearcoat_roughness\":{\"type\":\"Float\",\"value\":0.25}",
        ),
        0.0,
        true,
        20.0,
        None,
        Some(normal_map),
        None,
        None,
    ));
    let explicit_geometric = render_center(&scene_with_options(
        material_params(
            [0.0, 0.0, 0.0],
            0.0,
            0.32,
            ",\"clearcoat\":{\"type\":\"Float\",\"value\":0.7},\"clearcoat_roughness\":{\"type\":\"Float\",\"value\":0.25}",
        ),
        0.0,
        true,
        20.0,
        None,
        Some(normal_map),
        Some([0.5, 0.5, 1.0, 1.0]),
        None,
    ));
    assert_rgb_close(
        geometric_default,
        explicit_geometric,
        0.055,
        "geometric clearcoat fallback",
    );
}

#[test]
fn iridescence_direct_response_changes_with_light_at_fixed_view() {
    let material = material_params(
        [0.7, 0.5, 0.2],
        0.0,
        0.28,
        ",\"iridescence\":{\"type\":\"Float\",\"value\":1.0},\"iridescence_ior\":{\"type\":\"Float\",\"value\":1.3},\"iridescence_thickness_min\":{\"type\":\"Float\",\"value\":100.0},\"iridescence_thickness_max\":{\"type\":\"Float\",\"value\":400.0}",
    );
    let left = render_center(&scene_with_light_aim(
        material.clone(),
        0.0,
        20.0,
        [-20.0, 0.0, 0.0],
    ));
    let right = render_center(&scene_with_light_aim(material, 0.0, 20.0, [20.0, 0.0, 0.0]));
    let tilt = 0.65f32;
    let view = [tilt.cos(), tilt.sin(), 0.0];
    let normalize = |v: [f32; 3]| {
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / len, v[1] / len, v[2] / len]
    };
    let half_angle = |light: [f32; 3]| {
        let h = normalize([view[0] + light[0], view[1] + light[1], view[2] + light[2]]);
        view[0] * h[0] + view[1] * h[1] + view[2] * h[2]
    };
    let left_v_dot_h = half_angle(normalize([20.0, 30.0, 0.0]));
    let right_v_dot_h = half_angle(normalize([-20.0, 30.0, 0.0]));
    assert!(
        (left_v_dot_h - right_v_dot_h).abs() > 0.01,
        "light pair must exercise distinct VdotH values: {left_v_dot_h} vs {right_v_dot_h}"
    );
    assert!(left.iter().all(|c| c.is_finite()) && right.iter().all(|c| c.is_finite()));
    let delta = left
        .iter()
        .zip(right)
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>();
    assert!(
        delta > 0.005,
        "thin-film direct response must vary with light VdotH at fixed view: {left:?} vs {right:?}"
    );
}

#[test]
fn normal_map_scale_changes_direct_response() {
    let normal_map = Some([0.5, 0.8, 0.9, 1.0]);
    let unscaled = material_params(
        [0.0, 0.0, 0.0],
        0.0,
        0.2,
        ",\"normal_scale\":{\"type\":\"Float\",\"value\":0.0}",
    );
    let scaled = material_params(
        [0.0, 0.0, 0.0],
        0.0,
        0.2,
        ",\"normal_scale\":{\"type\":\"Float\",\"value\":1.0}",
    );
    let unscaled_rgb = render_center(&scene_with_options(
        unscaled, 0.0, true, 20.0, None, normal_map, None, None,
    ));
    let scaled_rgb = render_center(&scene_with_options(
        scaled, 0.0, true, 20.0, None, normal_map, None, None,
    ));
    let delta = unscaled_rgb
        .iter()
        .zip(scaled_rgb)
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>();
    assert!(
        delta > 0.01,
        "normal_scale must modulate tangent-space XY response: {unscaled_rgb:?} vs {scaled_rgb:?}"
    );
}

#[test]
fn occlusion_strength_only_darkens_diffuse_ibl() {
    let unoccluded = material_params(
        [0.7, 0.5, 0.2],
        0.0,
        0.85,
        ",\"occlusion_strength\":{\"type\":\"Float\",\"value\":0.0}",
    );
    let fully_occluded = material_params(
        [0.7, 0.5, 0.2],
        0.0,
        0.85,
        ",\"occlusion_strength\":{\"type\":\"Float\",\"value\":1.0}",
    );
    let unoccluded_rgb = render_center(&scene_with_options(
        unoccluded,
        0.0,
        false,
        1.0,
        None,
        None,
        None,
        Some([0.0, 0.0, 0.0, 1.0]),
    ));
    let fully_occluded_rgb = render_center(&scene_with_options(
        fully_occluded,
        0.0,
        false,
        1.0,
        None,
        None,
        None,
        Some([0.0, 0.0, 0.0, 1.0]),
    ));
    assert!(
        unoccluded_rgb
            .iter()
            .zip(fully_occluded_rgb)
            .all(|(open, closed)| *open > closed + 0.01),
        "occlusion strength must darken diffuse IBL: {unoccluded_rgb:?} vs {fully_occluded_rgb:?}"
    );
}

#[test]
fn diffuse_transmission_splits_lambert_and_preserves_rgb_tint() {
    let opaque = material_params([0.7, 0.5, 0.2], 0.0, 0.85, "");
    let red_transmission = material_params(
        [0.7, 0.5, 0.2],
        0.0,
        0.85,
        ",\"translucency\":{\"type\":\"Float\",\"value\":1.0},\"translucency_color_r\":{\"type\":\"Float\",\"value\":1.0},\"translucency_color_g\":{\"type\":\"Float\",\"value\":0.0},\"translucency_color_b\":{\"type\":\"Float\",\"value\":0.0}",
    );
    let green_transmission = material_params(
        [0.7, 0.5, 0.2],
        0.0,
        0.85,
        ",\"translucency\":{\"type\":\"Float\",\"value\":1.0},\"translucency_color_r\":{\"type\":\"Float\",\"value\":0.0},\"translucency_color_g\":{\"type\":\"Float\",\"value\":1.0},\"translucency_color_b\":{\"type\":\"Float\",\"value\":0.0}",
    );
    let opaque_rgb = render_center(&scene(opaque, 0.0, false, None));
    let red_rgb = render_center(&scene(red_transmission, 0.0, false, None));
    let green_rgb = render_center(&scene(green_transmission, 0.0, false, None));
    assert!(
        red_rgb[0] > red_rgb[1] + 0.02 && green_rgb[1] > green_rgb[0] + 0.02,
        "diffuse transmission must retain independent RGB tint: red {red_rgb:?}, green {green_rgb:?}"
    );
    assert!(
        red_rgb[0] > opaque_rgb[0] + 0.02,
        "front Lambert must be replaced by back Lambert at full transmission: opaque {opaque_rgb:?}, transmitted {red_rgb:?}"
    );
}

#[test]
fn sheen_ibl_energy_stays_bounded_across_roughness() {
    for roughness in [0.1, 0.5, 0.9] {
        let material = material_params(
            [0.0, 0.0, 0.0],
            0.0,
            0.5,
            &format!(
                ",\"sheen_color_r\":{{\"type\":\"Float\",\"value\":1.0}},\"sheen_color_g\":{{\"type\":\"Float\",\"value\":0.5}},\"sheen_color_b\":{{\"type\":\"Float\",\"value\":0.2}},\"sheen_roughness\":{{\"type\":\"Float\",\"value\":{roughness}}}"
            ),
        );
        let rgb = render_center(&scene(material, 0.0, false, None));
        assert!(
            rgb.iter().all(|c| c.is_finite() && *c >= 0.0 && *c <= 1.2),
            "sheen IBL energy must remain bounded at roughness {roughness}: {rgb:?}"
        );
    }
}
