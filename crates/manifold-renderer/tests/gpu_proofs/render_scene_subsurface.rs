//! Numeric end-to-end proofs for `node.render_scene` subsurface transport.
//!
//! These fixtures use the production graph executor and a white environment.
//! The cube is closed with outward normals, while the quad intentionally has
//! an open boundary so the production invalid-result marker is observable.

use half::f16;
use manifold_gpu::{GpuTexture, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

const WARMUP_FRAMES: i64 = 8;
const MAX_SAMPLES: u32 = 64;
const PROOF_WIDTH: u32 = 64;
const PROOF_HEIGHT: u32 = 64;

fn context(frame: i64) -> PresetContext {
    PresetContext {
        time: frame as f64 / 60.0,
        beat: frame as f64 / 30.0,
        dt: 1.0 / 60.0,
        width: PROOF_WIDTH,
        height: PROOF_HEIGHT,
        output_width: PROOF_WIDTH,
        output_height: PROOF_HEIGHT,
        aspect: PROOF_WIDTH as f32 / PROOF_HEIGHT as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

/// Constant white environment, independent of the black raster albedo.
fn environment_nodes() -> (&'static str, &'static str, &'static str) {
    (
        r#"{"id":700,"typeId":"node.linear_gradient","nodeId":"env_src","params":{"cx":{"type":"Float","value":-5.0},"softness":{"type":"Float","value":0.0}}},
        {"id":701,"typeId":"node.gradient_map","nodeId":"env","params":{"color_a":{"type":"Color","value":[1.0,1.0,1.0,1.0]},"color_b":{"type":"Color","value":[1.0,1.0,1.0,1.0]}}},"#,
        r#"{"fromNode":700,"fromPort":"out","toNode":701,"toPort":"source"},"#,
        r#"{"fromNode":701,"fromPort":"out","toNode":20,"toPort":"envmap"}"#,
    )
}

fn material_node(
    weight: f32,
    radius: [f32; 3],
    color: [f32; 3],
    mode: u32,
    samples: u32,
) -> String {
    format!(
        r#"{{"id":4,"typeId":"node.pbr_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":0.0}},"color_g":{{"type":"Float","value":0.0}},"color_b":{{"type":"Float","value":0.0}},
            "ambient":{{"type":"Float","value":0.0}},"metallic":{{"type":"Float","value":0.0}},"roughness":{{"type":"Float","value":0.5}},
            "specular":{{"type":"Float","value":0.0}},"ior":{{"type":"Float","value":1.0}},"emission_intensity":{{"type":"Float","value":0.0}},
            "subsurface_weight":{{"type":"Float","value":{weight}}},
            "subsurface_radius_r":{{"type":"Float","value":{r0}}},"subsurface_radius_g":{{"type":"Float","value":{r1}}},"subsurface_radius_b":{{"type":"Float","value":{r2}}},
            "subsurface_color_r":{{"type":"Float","value":{c0}}},"subsurface_color_g":{{"type":"Float","value":{c1}}},"subsurface_color_b":{{"type":"Float","value":{c2}}},
            "subsurface_mode":{{"type":"Enum","value":{mode}}},"subsurface_samples":{{"type":"Int","value":{samples}}}}}}},"#,
        r0 = radius[0],
        r1 = radius[1],
        r2 = radius[2],
        c0 = color[0],
        c1 = color[1],
        c2 = color[2],
    )
}

fn scene_json(
    closed: bool,
    weight: f32,
    radius: [f32; 3],
    color: [f32; 3],
    mode: u32,
    samples: u32,
) -> String {
    let (env_nodes, env_wire, envmap_wire) = environment_nodes();
    let material = material_node(weight, radius, color, mode, samples);
    let (geometry, geometry_wire) = if closed {
        (
            r#"{"id":1,"typeId":"node.cube_mesh","nodeId":"cube","params":{"max_capacity":{"type":"Int","value":36},"size":{"type":"Float","value":1.0}}},
        {"id":2,"typeId":"node.transform_3d","nodeId":"slab_scale","params":{"scale_x":{"type":"Float","value":20.0},"scale_y":{"type":"Float","value":20.0},"scale_z":{"type":"Float","value":1.0}}},"#,
            r#"{"fromNode":1,"fromPort":"vertices","toNode":20,"toPort":"mesh_0"},{"fromNode":2,"fromPort":"transform","toNode":20,"toPort":"transform_0"},"#,
        )
    } else {
        (
            r#"{"id":1,"typeId":"node.grid_mesh","nodeId":"quad_grid","params":{"max_capacity":{"type":"Int","value":16},"resolution_x":{"type":"Int","value":2},"resolution_y":{"type":"Int","value":2},"size_x":{"type":"Float","value":2.0},"size_y":{"type":"Float","value":2.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"quad","params":{"src_cols":{"type":"Int","value":2},"src_rows":{"type":"Int","value":2}}},
        {"id":5,"typeId":"node.transform_3d","nodeId":"quad_rotation","params":{"rot_x":{"type":"Float","value":1.57079632679}}},"#,
            r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},{"fromNode":5,"fromPort":"transform","toNode":20,"toPort":"transform_0"},"#,
        )
    };
    format!(
        r#"{{"version":2,"name":"RenderSceneSubsurfaceProof","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {geometry}
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{"orbit":{{"type":"Float","value":1.57079632679}},"tilt":{{"type":"Float","value":0.0}},"distance":{{"type":"Float","value":3.0}},"fov_y":{{"type":"Float","value":0.9}}}}}},
        {material}
        {env_nodes}
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{"objects":{{"type":"Int","value":1}},"lights":{{"type":"Int","value":0}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}],"wires":[
        {geometry_wire}
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {env_wire}
        {envmap_wire},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}]}}"#,
    )
}

struct RenderStats {
    mean: [f32; 4],
    /// Every sampled pixel from every rendered frame. Keeping this bounded
    /// grid makes the zero-weight equality proof cover more than one pixel.
    samples: Vec<[f32; 4]>,
    dispatched: bool,
    last_rgba: Vec<u8>,
    warm_frame_ms: f64,
}

fn render(json: &str) -> RenderStats {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        std::sync::Arc::clone(&h.device),
        PROOF_WIDTH,
        PROOF_HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("subsurface proof graph must build: {e}\n{json}"));
    let target = RenderTarget::new(
        &h.device,
        PROOF_WIDTH,
        PROOF_HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "render-scene-subsurface",
    );
    let mut dispatched = false;
    let mut sums = [0.0f64; 4];
    let mut sample_count = 0usize;
    let mut samples = Vec::new();
    let mut last_rgba = Vec::new();
    let mut warm_elapsed = std::time::Duration::ZERO;
    for frame in 0..WARMUP_FRAMES {
        let started = std::time::Instant::now();
        let mut enc = h.device.create_encoder("render-scene-subsurface");
        let dispatches;
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &context(frame),
                &manifold_core::params::ParamManifest::default(),
            );
            dispatches = gpu.rt_dispatches;
        }
        enc.commit_and_wait_completed();
        if frame > 0 {
            warm_elapsed += started.elapsed();
        }
        dispatched |= dispatches > 0;
        let bytes = readback(&h.device, &target.texture);
        for y in PROOF_HEIGHT / 4..(PROOF_HEIGHT * 3 / 4) {
            for x in PROOF_WIDTH / 4..(PROOF_WIDTH * 3 / 4) {
                let idx = ((y * PROOF_WIDTH + x) * 8) as usize;
                let pixel = [
                    f16::from_le_bytes([bytes[idx], bytes[idx + 1]]).to_f32(),
                    f16::from_le_bytes([bytes[idx + 2], bytes[idx + 3]]).to_f32(),
                    f16::from_le_bytes([bytes[idx + 4], bytes[idx + 5]]).to_f32(),
                    f16::from_le_bytes([bytes[idx + 6], bytes[idx + 7]]).to_f32(),
                ];
                sums.iter_mut()
                    .zip(pixel)
                    .for_each(|(sum, value)| *sum += f64::from(value));
                sample_count += 1;
                samples.push(pixel);
            }
        }
        last_rgba = bytes;
    }
    RenderStats {
        mean: sums.map(|sum| (sum / sample_count as f64) as f32),
        samples,
        dispatched,
        last_rgba,
        warm_frame_ms: warm_elapsed.as_secs_f64() * 1000.0 / (WARMUP_FRAMES - 1) as f64,
    }
}

fn readback(device: &manifold_gpu::GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = texture.width * 8;
    let total_bytes = u64::from(texture.height * bytes_per_row);
    let buffer = device.create_buffer_shared(total_bytes);
    let mut encoder = device.create_encoder("render-scene-subsurface-readback");
    encoder.copy_texture_to_buffer(
        texture,
        &buffer,
        texture.width,
        texture.height,
        bytes_per_row,
    );
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().expect("subsurface readback must map");
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), total_bytes as usize).to_vec() }
}

fn slab_transmittance(thickness: f32, radius: f32) -> f32 {
    // Independent midpoint integration of 2 * integral(mu * exp(-t/(r*mu))).
    let n = 200_000usize;
    let step = 1.0 / n as f32;
    (0..n)
        .map(|i| {
            let mu = (i as f32 + 0.5) * step;
            2.0 * mu * (-thickness / (radius * mu)).exp() * step
        })
        .sum()
}

#[test]
fn subsurface_weight_zero_is_raster_identical_and_has_no_extra_dispatch() {
    let control = render(&scene_json(false, 0.0, [0.25; 3], [0.0; 3], 1, MAX_SAMPLES));
    let explicit = render(&scene_json(false, 0.0, [0.25; 3], [1.0; 3], 0, 1));
    assert!(
        !control.dispatched && !explicit.dispatched,
        "weight zero must not dispatch SSS"
    );
    assert_eq!(
        control.samples, explicit.samples,
        "weight zero changed raster output"
    );
}

#[test]
fn random_walk_pure_absorption_matches_infinite_slab_transmittance() {
    let radius = 2.0;
    let stats = render(&scene_json(
        true,
        1.0,
        [radius; 3],
        [0.0; 3],
        1,
        MAX_SAMPLES,
    ));
    assert!(
        stats.dispatched,
        "positive SSS must dispatch the production pass"
    );
    assert!(
        stats
            .samples
            .iter()
            .all(|pixel| { pixel[..3].iter().all(|value| value.is_finite()) && pixel[3] >= 0.0 })
    );
    let expected = slab_transmittance(1.0, radius);
    for (channel, value) in stats.mean[..3].iter().enumerate() {
        assert!(
            (*value - expected).abs() < 0.05,
            "channel {channel}: {value} vs {expected}"
        );
    }
}

#[test]
fn closed_white_random_walk_conserves_environment_energy() {
    let stats = render(&scene_json(true, 1.0, [2.0; 3], [1.0; 3], 1, MAX_SAMPLES));
    assert!(stats.dispatched);
    assert!(
        stats.mean[..3]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.25)
    );
    let mean = stats.mean[..3].iter().sum::<f32>() / 3.0;
    assert!(
        (mean - 1.0).abs() <= 0.10,
        "white environment energy drifted: {:?}",
        stats.mean
    );
}

#[test]
fn closed_diffusion_is_finite_and_mode_switch_changes_estimate() {
    let diffusion = render(&scene_json(
        true,
        1.0,
        [0.25; 3],
        [0.8, 0.7, 0.6],
        0,
        MAX_SAMPLES,
    ));
    let random_walk = render(&scene_json(
        true,
        1.0,
        [0.25; 3],
        [0.8, 0.7, 0.6],
        1,
        MAX_SAMPLES,
    ));
    assert!(diffusion.dispatched && random_walk.dispatched);
    assert!(
        diffusion.mean[..3]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
    );
    assert!(
        diffusion.mean[..3].iter().sum::<f32>() > 1e-4,
        "closed diffusion returned no scattering response: {:?}",
        diffusion.mean
    );
    assert!(
        random_walk.mean[..3]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
    );
    let delta = diffusion.mean[..3]
        .iter()
        .zip(random_walk.mean[..3].iter())
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>();
    assert!(
        delta > 1e-4,
        "mode switch did not change the estimate: {:?} vs {:?}",
        diffusion.mean,
        random_walk.mean
    );
}

#[test]
fn subsurface_walk_ignores_enclosed_foreign_instance_boundary() {
    for mode in [0, 1] {
        let baseline = render(&scene_json(true, 1.0, [2.0; 3], [0.8; 3], mode, 16));
        assert!(baseline.dispatched, "baseline SSS must dispatch for mode {mode}");
        assert!(baseline.samples.iter().any(|rgb| rgb[0] > 0.01), "baseline must scatter visible light");

        let mut nested: serde_json::Value =
            serde_json::from_str(&scene_json(true, 1.0, [2.0; 3], [0.8; 3], mode, 16))
                .expect("baseline SSS fixture JSON");
        let nodes = nested["nodes"].as_array_mut().expect("scene nodes");
        nodes.push(serde_json::json!({
            "id": 31,
            "typeId": "node.transform_3d",
            "nodeId": "nested_scale",
            "params": {
                "scale_x": {"type": "Float", "value": 10.0},
                "scale_y": {"type": "Float", "value": 10.0},
                "scale_z": {"type": "Float", "value": 0.1}
            }
        }));
        nodes.push(serde_json::json!({
            "id": 32,
            "typeId": "node.unlit_material",
            "nodeId": "nested_black",
            "params": {
                "color_r": {"type": "Float", "value": 0.0},
                "color_g": {"type": "Float", "value": 0.0},
                "color_b": {"type": "Float", "value": 0.0}
            }
        }));
        nested["nodes"]
            .as_array_mut()
            .expect("scene nodes")
            .iter_mut()
            .find(|node| node["nodeId"] == "scene")
            .expect("render scene node")["params"]["objects"]["value"] = serde_json::json!(2);
        let wires = nested["wires"].as_array_mut().expect("scene wires");
        wires.push(serde_json::json!({
            "fromNode": 1,
            "fromPort": "vertices",
            "toNode": 20,
            "toPort": "mesh_1"
        }));
        wires.push(serde_json::json!({
            "fromNode": 31,
            "fromPort": "transform",
            "toNode": 20,
            "toPort": "transform_1"
        }));
        wires.push(serde_json::json!({
            "fromNode": 32,
            "fromPort": "out",
            "toNode": 20,
            "toPort": "material_1"
        }));

        let enclosed = render(&nested.to_string());
        assert!(enclosed.dispatched, "nested SSS must dispatch for mode {mode}");
        assert_eq!(
            baseline.samples, enclosed.samples,
            "an enclosed foreign instance changed outer SSS transport in mode {mode}"
        );
    }
}

#[test]
fn open_quad_random_walk_reports_invalid_transport_as_magenta() {
    let stats = render(&scene_json(false, 1.0, [0.2; 3], [0.8, 0.7, 0.6], 1, 8));
    assert!(stats.dispatched);
    let rgba = stats.mean;
    assert!(
        (rgba[0] - 1.0).abs() < 0.08 && rgba[1] < 0.08 && (rgba[2] - 1.0).abs() < 0.08,
        "open boundary must expose magenta invalid marker: {rgba:?}"
    );
}

#[test]
fn subsurface_preserves_surface_emission_and_metal_reflection() {
    for metal in [false, true] {
        let mut scene: serde_json::Value =
            serde_json::from_str(&scene_json(true, 0.0, [0.25; 3], [0.0; 3], 0, 1))
                .expect("fixture JSON");
        let params = scene["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["nodeId"] == "mat")
            .unwrap()["params"]
            .as_object_mut()
            .unwrap();
        for (key, value) in if metal {
            [
                ("metallic", 1.0),
                ("color_r", 1.0),
                ("color_g", 0.4),
                ("color_b", 0.1),
            ]
        } else {
            [
                ("emission_intensity", 1.0),
                ("emission_r", 0.2),
                ("emission_g", 0.4),
                ("emission_b", 0.8),
            ]
        } {
            params.insert(
                key.into(),
                serde_json::json!({"type":"Float","value":value}),
            );
        }
        let off = render(&scene.to_string());
        let params = scene["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["nodeId"] == "mat")
            .unwrap()["params"]
            .as_object_mut()
            .unwrap();
        params.insert(
            "subsurface_weight".into(),
            serde_json::json!({"type":"Float","value":1.0}),
        );
        let on = render(&scene.to_string());
        assert!(
            off.mean[..3].iter().sum::<f32>() > 0.1,
            "unlit fixture cannot prove lobe preservation"
        );
        assert_eq!(
            off.samples, on.samples,
            "SSS altered emission/reflection (metal={metal})"
        );
    }
}

/// A rear point light illuminates a closed slab. Only transport through the
/// geometry can illuminate its front. This is also the bounded cost/visual
/// fixture; timings include CPU encoding and completion, excluding compilation
/// in the first frame and readback. They are not a whole-app frame-rate claim.
#[test]
fn subsurface_backlit_radius_and_quality_cost() {
    let mut captures = Vec::new();
    for (mode, samples, name) in [(0, 8, "diffusion"), (1, 64, "random-walk")] {
        let mut means = Vec::new();
        for (radius, label) in [(0.03, "narrow"), (0.3, "wide")] {
            let mut graph: serde_json::Value = serde_json::from_str(&scene_json(
                true,
                1.0,
                [radius; 3],
                [0.95, 0.7, 0.35],
                mode,
                samples,
            ))
            .unwrap();
            let nodes = graph["nodes"].as_array_mut().unwrap();
            for node in nodes.iter_mut() {
                if node["nodeId"] == "slab_scale" {
                    node["params"]["scale_z"]["value"] = 0.2.into();
                } else if node["nodeId"] == "env" {
                    for key in ["color_a", "color_b"] {
                        node["params"][key]["value"] = serde_json::json!([0.0, 0.0, 0.0, 1.0]);
                    }
                } else if node["nodeId"] == "scene" {
                    node["params"]["lights"]["value"] = 1.into();
                }
            }
            nodes.push(serde_json::json!({"id":30,"typeId":"node.light","nodeId":"backlight","params":{
                "mode":{"type":"Enum","value":1},"falloff":{"type":"Enum","value":1},
                "pos_x":{"type":"Float","value":0.0},"pos_y":{"type":"Float","value":0.0},
                "pos_z":{"type":"Float","value":-0.35},"range":{"type":"Float","value":0.0},
                "intensity":{"type":"Float","value":0.5},"cast_shadows":{"type":"Float","value":0.0}
            }}));
            graph["wires"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"
                }));
            let stats = render(&graph.to_string());
            assert!(stats.dispatched);
            assert!(
                stats
                    .samples
                    .iter()
                    .all(|p| p[..3].iter().all(|v| v.is_finite()))
            );
            eprintln!(
                "SSS backlit {name}/{label} {PROOF_WIDTH}x{PROOF_HEIGHT} spp={samples}: mean={:?}, warm encode+GPU={:.3}ms",
                stats.mean, stats.warm_frame_ms
            );
            means.push(stats.mean[0]);
            captures.push((format!("{name}-{label}"), stats.last_rgba));
        }
        assert!(
            means[1] > means[0] * 1.5 && means[1] > 0.005,
            "{name}: a wider mean free path must transmit this backlight through the slab: {means:?}"
        );
    }
    if let Some(directory) = std::env::var_os("MANIFOLD_SSS_CAPTURE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).expect("SSS capture directory");
        for (name, rgba) in captures {
            let rgb: Vec<u8> = rgba
                .chunks_exact(8)
                .flat_map(|p| {
                    [0, 2, 4].map(|i| {
                        let value = f16::from_le_bytes([p[i], p[i + 1]]).to_f32().max(0.0);
                        ((value / (1.0 + value)).powf(1.0 / 2.2) * 255.0).round() as u8
                    })
                })
                .collect();
            image::save_buffer(
                directory.join(format!("{name}.png")),
                &rgb,
                PROOF_WIDTH,
                PROOF_HEIGHT,
                image::ExtendedColorType::Rgb8,
            )
            .expect("SSS capture PNG");
        }
    }
}
