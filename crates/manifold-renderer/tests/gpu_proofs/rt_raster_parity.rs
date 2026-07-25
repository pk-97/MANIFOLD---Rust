//! RAYTRACING_DESIGN.md §9.6 Raster-parity reflections gate — computed-pixel
//! parity test for environment + textured material shading at reflection hit point.
//!
//! This test is the BISECTOR for the black-car defect (AMG GT3 with reflections on
//! renders at fraction 0.0244 vs baseline 0.0964). Its trustworthiness is the whole
//! product — if the test fails, the failure is the ANSWER, not something to fix.
//!
//! Two legs:
//!
//! LEG A (env-at-hit): metallic roughness-0 ground plane + emissive quad + KNOWN
//! CONSTANT envmap (uniform 0.5 gray). CPU computes expected reflected radiance
//! INCLUDING the env term (with constant env, `refl_env_sample` returns the constant
//! at every direction/mip). Expectation uses the SAME constants the kernel uses:
//! - RT_REFL_HIT_ENV_DIFFUSE_ROUGHNESS = 1.0 (diffuse irradiance approximation)
//! - RT_REFL_HIT_DIELECTRIC_F0 = 0.04 (Schlick dielectric base)
//! - SUN_BOUNCE_INTENSITY_SCALE = 0.08
//!
//! LEG B (textured albedo): plane with solid-color base-color TEXTURE (red) distinct
//! from its flat factor albedo (gray) + one emissive quad. The mirror image must
//! carry the TEXTURE's color, not the factor's.

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

// Kernel constants from raytrace.rs:1114-1117 — these MUST match exactly
const RT_REFL_HIT_ENV_DIFFUSE_ROUGHNESS: f32 = 1.0;
const RT_REFL_HIT_DIELECTRIC_F0: f32 = 0.04;
const SUN_BOUNCE_INTENSITY_SCALE: f32 = 0.08;

// Leg A: constant env value (uniform 0.5 gray = 0.5 radiance)
const CONSTANT_ENV_VALUE: f32 = 0.5;

// Emissive quad world position (same as rt_r1_reflection probe)
const EMISSIVE_X: f32 = 0.0;
const EMISSIVE_Y: f32 = 0.8;
const EMISSIVE_Z: f32 = 2.0;

// Emissive color from the test fixture (emission_r=1.0, g=0.2, b=0.1, intensity=10.0)
// This yields (1.0, 0.2, 0.1) * 10.0 = (10.0, 2.0, 1.0) linear HDR
const EMISSIVE_COLOR: [f32; 3] = [10.0, 2.0, 1.0];

// Ground material factors (metallic=1.0, roughness=0.01, albedo=0.5 gray)
const GROUND_ALBEDO: f32 = 0.5;
const GROUND_METALLIC: f32 = 1.0;
const GROUND_ROUGHNESS: f32 = 0.01;

/// Build scene JSON for LEG A (env-at-hit test) with constant envmap.
/// The envmap is created as a minimal 1x1 PNG fixture with constant 0.5 gray.
fn scene_json_env_at_hit(env_intensity: f32) -> String {
    let env_v = if env_intensity > 0.0 { env_intensity.to_string() } else { "0.0".to_string() };
    format!(
        r#"{{"version":2,"name":"RasterParityEnvAtHit","nodes":[
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
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"quad_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":4}},
            "resolution_y":{{"type":"Int","value":4}},
            "size_x":{{"type":"Float","value":1.0}},
            "size_y":{{"type":"Float","value":1.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"quad_tris","params":{{
            "src_cols":{{"type":"Int","value":4}},
            "src_rows":{{"type":"Int","value":4}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"quad_xform","params":{{
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":0.8}},
            "pos_z":{{"type":"Float","value":2.0}}}}}},
        {{"id":8,"typeId":"node.pbr_material","nodeId":"quad_mat","params":{{
            "color_r":{{"type":"Float","value":0.5}},
            "color_g":{{"type":"Float","value":0.5}},
            "color_b":{{"type":"Float","value":0.5}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":0.5}},
            "emission_r":{{"type":"Float","value":1.0}},
            "emission_g":{{"type":"Float","value":0.2}},
            "emission_b":{{"type":"Float","value":0.1}},
            "emission_intensity":{{"type":"Float","value":10.0}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.7}},
            "tilt":{{"type":"Float","value":0.95}},
            "distance":{{"type":"Float","value":10.0}},
            "fov_y":{{"type":"Float","value":0.8}}}}}},
        {{"id":4,"typeId":"node.pbr_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":0.5}},
            "color_g":{{"type":"Float","value":0.5}},
            "color_b":{{"type":"Float","value":0.5}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":1.0}},
            "roughness":{{"type":"Float","value":0.01}}}}}},
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
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":true}}}}}}}},
        {{"id":9,"typeId":"node.bake_equirect_envmap","nodeId":"envmap","params":{{
            "intensity":{{"type":"Float","value":{env_v}}}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":9,"fromPort":"envmap","toNode":20,"toPort":"envmap"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Build scene JSON for LEG B (textured albedo test) with solid-color texture.
fn scene_json_textured_albedo() -> String {
    format!(
        r#"{{"version":2,"name":"RasterParityTexturedAlbedo","nodes":[
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
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"quad_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":4}},
            "resolution_y":{{"type":"Int","value":4}},
            "size_x":{{"type":"Float","value":1.0}},
            "size_y":{{"type":"Float","value":1.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"quad_tris","params":{{
            "src_cols":{{"type":"Int","value":4}},
            "src_rows":{{"type":"Int","value":4}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"quad_xform","params":{{
            "pos_x":{{"type":"Float","value":0.0}},
            "pos_y":{{"type":"Float","value":0.8}},
            "pos_z":{{"type":"Float","value":2.0}}}}}},
        {{"id":8,"typeId":"node.pbr_material","nodeId":"quad_mat","params":{{
            "color_r":{{"type":"Float","value":0.5}},
            "color_g":{{"type":"Float","value":0.5}},
            "color_b":{{"type":"Float","value":0.5}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":0.0}},
            "roughness":{{"type":"Float","value":0.5}},
            "emission_r":{{"type":"Float","value":1.0}},
            "emission_g":{{"type":"Float","value":0.2}},
            "emission_b":{{"type":"Float","value":0.1}},
            "emission_intensity":{{"type":"Float","value":10.0}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.7}},
            "tilt":{{"type":"Float","value":0.95}},
            "distance":{{"type":"Float","value":10.0}},
            "fov_y":{{"type":"Float","value":0.8}}}}}},
        {{"id":4,"typeId":"node.pbr_material","nodeId":"ground_mat","params":{{
            "color_r":{{"type":"Float","value":0.5}},
            "color_g":{{"type":"Float","value":0.5}},
            "color_b":{{"type":"Float","value":0.5}},
            "ambient":{{"type":"Float","value":0.0}},
            "metallic":{{"type":"Float","value":1.0}},
            "roughness":{{"type":"Float","value":0.01}}}}}},
        {{"id":10,"typeId":"node.solid_color_texture","nodeId":"red_tex","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":0.0}},
            "color_b":{{"type":"Float","value":0.0}}}}}},
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
            "rt_enabled":{{"type":"Bool","value":true}},
            "rt_reflections":{{"type":"Bool","value":true}}}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":10,"fromPort":"texture","toNode":4,"toPort":"base_color_map"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":8,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

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
    ).expect("JSON parse");

    let target = h.make_target("rt-raster-parity");
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
        let mut enc = h.device.create_encoder("rt-raster-parity-enc");
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

/// Read a 15x15 region around pixel (x, y) and return the mean RGB.
/// Rgba16Float format: 4 channels × 2 bytes per f16 = 8 bytes per pixel.
fn region_mean(bytes: &[u8], width: u32, height: u32, cx: f32, cy: f32, region_size: u32) -> [f32; 3] {
    let cxi = cx.round() as i32;
    let cyi = cy.round() as i32;
    let half_size = (region_size / 2) as i32;
    let mut sum_r = 0.0_f32;
    let mut sum_g = 0.0_f32;
    let mut sum_b = 0.0_f32;
    let mut count = 0_u32;

    for dy in -half_size..=half_size {
        for dx in -half_size..=half_size {
            let x = cxi + dx;
            let y = cyi + dy;
            if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                continue;
            }
            let idx = ((y as u32 * width + x as u32) * 8) as usize;
            let r = f16::from_le_bytes([bytes[idx], bytes[idx + 1]]).to_f32();
            let g = f16::from_le_bytes([bytes[idx + 2], bytes[idx + 3]]).to_f32();
            let b = f16::from_le_bytes([bytes[idx + 4], bytes[idx + 5]]).to_f32();
            sum_r += r;
            sum_g += g;
            sum_b += b;
            count += 1;
        }
    }

    let n = count as f32;
    [sum_r / n, sum_g / n, sum_b / n]
}

/// CPU expectation for LEG A with constant env.
/// From kernel raytrace.rs:1135: `traced = hit_emissive + hit_albedo * hit_diffuse_env + hit_f0 * hit_specular_env + sun_bounce_term`
///
/// With constant env (0.5) at metallic=1.0, roughness=0.01 (mirror), albedo=0.5:
/// - hit_emissive = (10.0, 2.0, 1.0) (emissive quad)
/// - hit_albedo = 0.5 (ground albedo)
/// - hit_metallic = 1.0 (ground is metallic)
/// - hit_f0 = mix(0.04, 0.5, 1.0) = 0.5 (fully metallic, F0 = albedo)
/// - hit_diffuse_env = 0.5 (constant env at roughness 1.0)
/// - hit_specular_env = 0.5 (constant env at roughness 0.01, still 0.5)
/// - sun_bounce_term ≈ 0 (mirror surface reflects sun away from camera; exact NdotL~0)
///
/// Expected: (10.0, 2.0, 1.0) + 0.5*0.5 + 0.5*0.5 = (10.5, 2.5, 1.5)
fn cpu_expectation_env_at_hit() -> [f32; 3] {
    let hit_emissive = EMISSIVE_COLOR;
    let hit_albedo = GROUND_ALBEDO;
    let hit_metallic = GROUND_METALLIC;
    let hit_f0_dielectric = RT_REFL_HIT_DIELECTRIC_F0;
    let env_value = CONSTANT_ENV_VALUE;

    // Schlick F0 mix: dielectric base + metallic contribution
    let hit_f0 = hit_f0_dielectric * (1.0 - hit_metallic) + hit_albedo * hit_metallic;

    // Diffuse env term (irradiance approximation at roughness 1.0)
    let diffuse_env = hit_albedo * env_value;

    // Specular env term (one-bounce continuation at hit roughness)
    let specular_env = hit_f0 * env_value;

    // Total expectation (sun term ~0 for mirror geometry)
    [
        hit_emissive[0] + diffuse_env + specular_env,
        hit_emissive[1] + diffuse_env + specular_env,
        hit_emissive[2] + diffuse_env + specular_env,
    ]
}

/// CPU expectation for LEG A without env (pre-parity shading).
/// From kernel raytrace.rs:1133 (R1 formula): `traced = hit_emissive + hit_albedo * sun_bounce`
/// This is the control leg: shows the delta raster-parity adds.
fn cpu_expectation_no_env() -> [f32; 3] {
    let hit_emissive = EMISSIVE_COLOR;
    // Pre-parity: only emissive + sun (sun term ~0 for mirror)
    // No env contribution at all
    hit_emissive
}

#[test]
fn env_at_hit_reflection_shows_constant_envmap() {
    // Render with constant env (intensity 1.0 = env value 0.5)
    let json = scene_json_env_at_hit(1.0);
    let (bytes, width, height) = render_readback(&json);

    // Compute reflection region: mirror image of emissive quad at (EMISSIVE_X, -EMISSIVE_Y, EMISSIVE_Z)
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, NEAR, FAR, width as f32, height as f32);
    let virtual_image = [EMISSIVE_X, -EMISSIVE_Y, EMISSIVE_Z];
    let refl_px = cam.project_to_pixel(virtual_image, width, height)
        .expect("reflection probe point must project in front of the camera");

    let region_size = 15_u32;
    let mean = region_mean(&bytes, width, height, refl_px.px, refl_px.py, region_size);
    let expected = cpu_expectation_env_at_hit();

    // Tolerance: allow 5% relative error for numeric precision + sampling variance
    let tolerance = 0.05;
    let rel_error_r = (mean[0] - expected[0]).abs() / expected[0].max(1e-6);
    let rel_error_g = (mean[1] - expected[1]).abs() / expected[1].max(1e-6);
    let rel_error_b = (mean[2] - expected[2]).abs() / expected[2].max(1e-6);

    println!("LEG A (env-at-hit):");
    println!("  Reflection region at pixel ({}, {})", refl_px.px, refl_px.py);
    println!("  Mean (R, G, B): ({:.4}, {:.4}, {:.4})", mean[0], mean[1], mean[2]);
    println!("  Expected (R, G, B): ({:.4}, {:.4}, {:.4})", expected[0], expected[1], expected[2]);
    println!("  Relative errors: R={:.4}, G={:.4}, B={:.4}", rel_error_r, rel_error_g, rel_error_b);

    assert!(rel_error_r < tolerance,
        "LEG A env-at-hit: R channel relative error {} exceeds tolerance {} (mean {:.4} vs expected {:.4})",
        rel_error_r, tolerance, mean[0], expected[0]);
    assert!(rel_error_g < tolerance,
        "LEG A env-at-hit: G channel relative error {} exceeds tolerance {} (mean {:.4} vs expected {:.4})",
        rel_error_g, tolerance, mean[1], expected[1]);
    assert!(rel_error_b < tolerance,
        "LEG A env-at-hit: B channel relative error {} exceeds tolerance {} (mean {:.4} vs expected {:.4})",
        rel_error_b, tolerance, mean[2], expected[2]);

    // Control leg: mean must EXCEED pre-parity expectation by a margin
    let expected_no_env = cpu_expectation_no_env();
    let margin = 0.2; // 20% margin accounts for diffuse+specular env terms
    let delta_r = mean[0] - expected_no_env[0];
    let delta_g = mean[1] - expected_no_env[1];
    let delta_b = mean[2] - expected_no_env[2];

    println!("  Pre-parity expectation (no env): ({:.4}, {:.4}, {:.4})", expected_no_env[0], expected_no_env[1], expected_no_env[2]);
    println!("  Delta (with - without env): ({:.4}, {:.4}, {:.4})", delta_r, delta_g, delta_b);
    println!("  Margin check: delta must exceed {}", margin);

    assert!(delta_r > margin,
        "LEG A env-at-hit control leg: R delta {} does not exceed margin {} (env-at-hit may not be firing)", delta_r, margin);
    assert!(delta_g > margin,
        "LEG A env-at-hit control leg: G delta {} does not exceed margin {} (env-at-hit may not be firing)", delta_g, margin);
    assert!(delta_b > margin,
        "LEG A env-at-hit control leg: B delta {} does not exceed margin {} (env-at-hit may not be firing)", delta_b, margin);
}

#[test]
fn textured_albedo_reflection_carries_texture_color() {
    // Render with solid red texture (RGB=1.0, 0.0, 0.0) vs gray factor (0.5)
    let json = scene_json_textured_albedo();
    let (bytes, width, height) = render_readback(&json);

    // Compute reflection region
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, NEAR, FAR, width as f32, height as f32);
    let virtual_image = [EMISSIVE_X, -EMISSIVE_Y, EMISSIVE_Z];
    let refl_px = cam.project_to_pixel(virtual_image, width, height)
        .expect("reflection probe point must project in front of the camera");

    let region_size = 15_u32;
    let mean = region_mean(&bytes, width, height, refl_px.px, refl_px.py, region_size);

    println!("LEG B (textured albedo):");
    println!("  Reflection region at pixel ({}, {})", refl_px.px, refl_px.py);
    println!("  Mean (R, G, B): ({:.4}, {:.4}, {:.4})", mean[0], mean[1], mean[2]);

    // The mirror image must carry the TEXTURE's red color, not the factor's gray.
    // Threshold: red channel > 0.8 (dominates), green/blue < 0.3 (suppressed)
    let red_threshold = 0.8;
    let gb_threshold = 0.3;

    assert!(mean[0] > red_threshold,
        "LEG B textured albedo: R channel {} does not exceed threshold {} (texture color may not be sampled)",
        mean[0], red_threshold);
    assert!(mean[1] < gb_threshold,
        "LEG B textured albedo: G channel {} exceeds threshold {} (gray factor may be leaking instead of texture)",
        mean[1], gb_threshold);
    assert!(mean[2] < gb_threshold,
        "LEG B textured albedo: B channel {} exceeds threshold {} (gray factor may be leaking instead of texture)",
        mean[2], gb_threshold);

    // CPU expectation: texture color (1.0, 0.0, 0.0) at metallic=1.0, roughness=0.01
    // hit_emissive + hit_albedo_texture * env + hit_f0_texture * env
    // With no env wired, we only get emissive, but the key is that the R channel
    // dominates due to the red texture being sampled during the ray hit.
    // For this test, the channel assertions above are the primary check.
    println!("  Texture color sampled: red dominates, green/blue suppressed");
}
