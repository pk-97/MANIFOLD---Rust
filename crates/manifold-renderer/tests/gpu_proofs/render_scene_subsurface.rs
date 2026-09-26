//! Numeric end-to-end proofs for `node.render_scene` subsurface transport.
//!
//! These fixtures use the production graph executor and a white environment.
//! The cube is closed with outward normals, while the quad intentionally has
//! an open boundary so the production invalid-result marker is observable.

use half::f16;
use manifold_core::NodeId;
use manifold_gpu::{GpuBuffer, GpuTexture, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::generators::mesh_common::MeshVertex;
use manifold_renderer::node_graph::depth_rule::DepthRule;
use manifold_renderer::node_graph::{
    ArrayType, EffectNode, EffectNodeContext, EffectNodeType, NodeInput, NodeOutput, NodePort,
    ParamDef, ParamValue, ParamValues, PortKind, PortType, PrimitiveRegistry,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

const WARMUP_FRAMES: i64 = 8;
const MAX_SAMPLES: u32 = 64;
const PROOF_WIDTH: u32 = 64;
const PROOF_HEIGHT: u32 = 64;
const PROOF_SAMPLE_PIXELS: usize = (PROOF_WIDTH as usize / 2) * (PROOF_HEIGHT as usize / 2);
const CURVED_PROOF_WIDTH: u32 = 128;
const CURVED_PROOF_HEIGHT: u32 = 128;

fn context_at(frame: i64, width: u32, height: u32) -> PresetContext {
    PresetContext {
        time: frame as f64 / 60.0,
        beat: frame as f64 / 30.0,
        dt: 1.0 / 60.0,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

/// Test-only closed curved mesh source. The seam and both poles use exact
/// positions so the ray traversal sees one watertight sphere despite the
/// non-indexed triangle stream.
struct CurvedSphereSource {
    type_id: EffectNodeType,
    outputs: Vec<NodeOutput>,
    vertices: Vec<MeshVertex>,
    staging: Option<GpuBuffer>,
}

impl CurvedSphereSource {
    fn new() -> Self {
        Self {
            type_id: EffectNodeType::new("test.sss_uv_sphere"),
            outputs: vec![NodePort {
                name: std::borrow::Cow::Borrowed("vertices"),
                ty: PortType::Array(ArrayType::of_known::<MeshVertex>()),
                kind: PortKind::Output,
                required: false,
            }],
            vertices: curved_uv_sphere(),
            staging: None,
        }
    }
}

impl EffectNode for CurvedSphereSource {
    fn depth_rule(&self) -> DepthRule {
        DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn inputs(&self) -> &[NodeInput] {
        &[]
    }

    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }

    fn parameters(&self) -> &[ParamDef] {
        &[]
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let bytes = bytemuck::cast_slice(self.vertices.as_slice());
        let staging = self.staging.get_or_insert_with(|| {
            ctx.gpu_encoder()
                .device
                .create_buffer_shared(bytes.len() as u64)
        });
        unsafe { staging.write(0, bytes) };
        ctx.gpu_encoder()
            .native_enc
            .copy_buffer_to_buffer(staging, dst, bytes.len() as u64);
    }

    fn array_output_capacity(
        &self,
        _port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        Some(self.vertices.len() as u32)
    }
}

fn curved_uv_sphere() -> Vec<MeshVertex> {
    const LATITUDE_SEGMENTS: usize = 32;
    const LONGITUDE_SEGMENTS: usize = 64;
    const RADIUS: f32 = 0.5;
    let mut vertices = Vec::with_capacity(
        LONGITUDE_SEGMENTS * 6 + (LATITUDE_SEGMENTS - 2) * LONGITUDE_SEGMENTS * 6,
    );
    let vertex = |latitude: usize, longitude: usize| {
        let pole = latitude == 0 || latitude == LATITUDE_SEGMENTS;
        let (position, normal) = if pole {
            let y = if latitude == 0 { RADIUS } else { -RADIUS };
            ([0.0, y, 0.0], [0.0, y / RADIUS, 0.0])
        } else {
            let theta = std::f32::consts::PI * latitude as f32 / LATITUDE_SEGMENTS as f32;
            let longitude_angle = if longitude == LONGITUDE_SEGMENTS {
                0.0
            } else {
                std::f32::consts::TAU * longitude as f32 / LONGITUDE_SEGMENTS as f32
            };
            let sin_theta = theta.sin();
            let normal = [
                sin_theta * longitude_angle.cos(),
                theta.cos(),
                sin_theta * longitude_angle.sin(),
            ];
            (
                [
                    RADIUS * normal[0],
                    RADIUS * normal[1],
                    RADIUS * normal[2],
                ],
                normal,
            )
        };
        MeshVertex {
            position,
            _pad0: 0.0,
            normal,
            _pad1: 0.0,
            uv: [
                longitude as f32 / LONGITUDE_SEGMENTS as f32,
                latitude as f32 / LATITUDE_SEGMENTS as f32,
            ],
            _pad2: [0.0; 2],
            tangent: [0.0; 4],
            color: [1.0; 4],
        }
    };

    for longitude in 0..LONGITUDE_SEGMENTS {
        vertices.extend([
            vertex(0, longitude),
            vertex(1, longitude + 1),
            vertex(1, longitude),
        ]);
    }
    for latitude in 1..(LATITUDE_SEGMENTS - 1) {
        for longitude in 0..LONGITUDE_SEGMENTS {
            vertices.extend([
                vertex(latitude, longitude),
                vertex(latitude, longitude + 1),
                vertex(latitude + 1, longitude + 1),
                vertex(latitude, longitude),
                vertex(latitude + 1, longitude + 1),
                vertex(latitude + 1, longitude),
            ]);
        }
    }
    for longitude in 0..LONGITUDE_SEGMENTS {
        vertices.extend([
            vertex(LATITUDE_SEGMENTS - 1, longitude),
            vertex(LATITUDE_SEGMENTS - 1, longitude + 1),
            vertex(LATITUDE_SEGMENTS, longitude),
        ]);
    }
    vertices
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

fn curved_scene_json(mode: u32) -> String {
    let mut scene: serde_json::Value = serde_json::from_str(&scene_json(
        true,
        1.0,
        [0.4; 3],
        [0.8; 3],
        mode,
        MAX_SAMPLES,
    ))
    .expect("curved SSS fixture JSON");
    let nodes = scene["nodes"].as_array_mut().expect("scene nodes");
    nodes.retain(|node| node["nodeId"] != "cube" && node["nodeId"] != "slab_scale");
    nodes.push(serde_json::json!({
        "id": 1,
        "typeId": "test.sss_uv_sphere",
        "nodeId": "curved_sphere",
        "params": {}
    }));
    let camera = nodes
        .iter_mut()
        .find(|node| node["nodeId"] == "cam")
        .expect("curved sphere camera");
    camera["params"]["distance"]["value"] = 3.0.into();
    scene["wires"]
        .as_array_mut()
        .expect("scene wires")
        .retain(|wire| wire["fromNode"] != 2 && wire["toNode"] != 2);
    scene.to_string()
}

struct RenderStats {
    mean: [f32; 4],
    /// Every sampled pixel from every rendered frame. Keeping this bounded
    /// grid makes the zero-weight equality proof cover more than one pixel.
    samples: Vec<[f32; 4]>,
    dispatched: bool,
    last_rgba: Vec<u8>,
    warm_frame_ms: f64,
    invalid_pixels: usize,
    first_invalid: Option<(i64, usize, [f32; 4])>,
}

fn render(json: &str) -> RenderStats {
    render_with_frame_mutation(json, |_, _| {})
}

fn render_with_frame_mutation<F>(json: &str, mutate: F) -> RenderStats
where
    F: FnMut(&mut PresetRuntime, i64),
{
    let registry = PrimitiveRegistry::with_builtin();
    render_with_registry_dimensions(
        json,
        &registry,
        PROOF_WIDTH,
        PROOF_HEIGHT,
        mutate,
    )
}

fn render_with_registry_dimensions<F>(
    json: &str,
    registry: &PrimitiveRegistry,
    width: u32,
    height: u32,
    mut mutate: F,
) -> RenderStats
where
    F: FnMut(&mut PresetRuntime, i64),
{
    let h = harness::shared();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        registry,
        std::sync::Arc::clone(&h.device),
        width,
        height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("subsurface proof graph must build: {e}\n{json}"));
    let target = RenderTarget::new(
        &h.device,
        width,
        height,
        GpuTextureFormat::Rgba16Float,
        "render-scene-subsurface",
    );
    let mut dispatched = false;
    let mut sums = [0.0f64; 4];
    let mut sample_count = 0usize;
    let mut samples = Vec::new();
    let mut last_rgba = Vec::new();
    let mut invalid_pixels = 0usize;
    let mut first_invalid = None;
    let mut warm_elapsed = std::time::Duration::ZERO;
    for frame in 0..WARMUP_FRAMES {
        mutate(&mut runtime, frame);
        let started = std::time::Instant::now();
        let mut enc = h.device.create_encoder("render-scene-subsurface");
        let dispatches;
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &context_at(frame, width, height),
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
        for (index, chunk) in bytes.chunks_exact(8).enumerate() {
            let pixel: [f32; 4] = std::array::from_fn(|c| {
                f16::from_le_bytes([chunk[c*2],chunk[c*2+1]]).to_f32()
            });
            if !pixel.iter().all(|v| v.is_finite()) || pixel[3] < -0.5
                || (pixel[0] > 0.75 && pixel[1] < 0.25 && pixel[2] > 0.75)
            {
                invalid_pixels += 1;
                first_invalid.get_or_insert((frame,index,pixel));
            }
        }
        for y in height / 4..(height * 3 / 4) {
            for x in width / 4..(width * 3 / 4) {
                let idx = ((y * width + x) * 8) as usize;
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
        invalid_pixels,
        first_invalid,
        dispatched,
        last_rgba,
        warm_frame_ms: warm_elapsed.as_secs_f64() * 1000.0 / (WARMUP_FRAMES - 1) as f64,
    }
}

fn render_with_material_reset(json: &str) -> RenderStats {
    render_with_frame_mutation(json, |runtime, frame| {
        if frame != WARMUP_FRAMES / 2 {
            return;
        }
        let material = runtime
            .graph
            .instance_by_node_id(&NodeId::new("mat"))
            .expect("material node must exist");
        for name in [
            "subsurface_color_r",
            "subsurface_color_g",
            "subsurface_color_b",
        ] {
            runtime
                .graph
                .set_param(material, name, ParamValue::Float(0.05))
                .unwrap_or_else(|error| panic!("material reset parameter {name}: {error}"));
        }
    })
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

fn assert_curved_image_is_valid_and_scattering(stats: &RenderStats, mode: u32) {
    let mut positive_coverage = 0usize;
    let mut radiance = 0.0f32;
    for px in stats.last_rgba.chunks_exact(8) {
        let pixel = [
            f16::from_le_bytes([px[0], px[1]]).to_f32(),
            f16::from_le_bytes([px[2], px[3]]).to_f32(),
            f16::from_le_bytes([px[4], px[5]]).to_f32(),
            f16::from_le_bytes([px[6], px[7]]).to_f32(),
        ];
        let luma = rgb_luma(pixel);
        if pixel[3] > 0.5 && luma > 0.005 {
            positive_coverage += 1;
            radiance += luma;
        }
    }
    assert!(
        stats.invalid_pixels == 0,
        "closed curved SSS mode {mode} produced invalid magenta pixels: first={:?}, count={}",
        stats.first_invalid,
        stats.invalid_pixels
    );
    assert!(
        positive_coverage > (CURVED_PROOF_WIDTH * CURVED_PROOF_HEIGHT / 100) as usize,
        "closed curved SSS mode {mode} had insufficient visible coverage: {positive_coverage} pixels"
    );
    assert!(
        radiance > 0.1,
        "closed curved SSS mode {mode} had no nontrivial scattering radiance: coverage={positive_coverage}, radiance={radiance}"
    );
}

fn frame_means(stats: &RenderStats) -> Vec<[f32; 4]> {
    assert_eq!(
        stats.samples.len() % PROOF_SAMPLE_PIXELS,
        0,
        "subsurface proof samples must contain complete frames"
    );
    stats
        .samples
        .chunks_exact(PROOF_SAMPLE_PIXELS)
        .map(|frame| {
            let mut sum = [0.0f64; 4];
            for pixel in frame {
                for (slot, value) in sum.iter_mut().zip(pixel) {
                    *slot += f64::from(*value);
                }
            }
            sum.map(|value| (value / PROOF_SAMPLE_PIXELS as f64) as f32)
        })
        .collect()
}

fn rgb_luma(mean: [f32; 4]) -> f32 {
    mean[0] * 0.2126 + mean[1] * 0.7152 + mean[2] * 0.0722
}

fn temporal_luma_variance(frames: &[[f32; 4]]) -> f32 {
    let mean = frames.iter().map(|frame| rgb_luma(*frame)).sum::<f32>() / frames.len() as f32;
    frames
        .iter()
        .map(|frame| {
            let delta = rgb_luma(*frame) - mean;
            delta * delta
        })
        .sum::<f32>()
        / frames.len() as f32
}

fn moving_camera_scene() -> String {
    let mut scene: serde_json::Value =
        serde_json::from_str(&scene_json(true, 1.0, [0.4; 3], [0.8; 3], 1, 1))
            .expect("moving-camera fixture JSON");
    // Stay outside the 20 x 20 x 1 slab. Orbit zero puts this camera
    // inside the volume and correctly produces invalid transport markers.
    scene["nodes"].as_array_mut().unwrap().push(serde_json::json!({
        "id": 31, "typeId": "node.math", "nodeId": "camera_orbit",
        "params": {"b": {"type":"Float", "value":std::f32::consts::FRAC_PI_2},
                   "op": {"type":"Enum", "value":0}}
    }));
    scene["wires"].as_array_mut().unwrap().extend([
        serde_json::json!({"fromNode":0,"fromPort":"time","toNode":31,"toPort":"a"}),
        serde_json::json!({"fromNode":31,"fromPort":"out","toNode":3,"toPort":"orbit"}),
    ]);
    scene.to_string()
}

fn moving_light_scene() -> String {
    let mut scene: serde_json::Value =
        serde_json::from_str(&scene_json(true, 1.0, [0.4; 3], [0.8; 3], 1, 1))
            .expect("moving-light fixture JSON");
    let nodes = scene["nodes"].as_array_mut().expect("scene nodes");
    nodes
        .iter_mut()
        .find(|node| node["nodeId"] == "env")
        .expect("environment node")["params"]["color_a"]["value"] =
        serde_json::json!([0.0, 0.0, 0.0, 1.0]);
    nodes
        .iter_mut()
        .find(|node| node["nodeId"] == "env")
        .expect("environment node")["params"]["color_b"]["value"] =
        serde_json::json!([0.0, 0.0, 0.0, 1.0]);
    nodes
        .iter_mut()
        .find(|node| node["nodeId"] == "scene")
        .expect("scene node")["params"]["lights"]["value"] = 1.into();
    nodes.push(serde_json::json!({
        "id": 30,
        "typeId": "node.light",
        "nodeId": "moving_light",
        "params": {
            "mode": {"type": "Enum", "value": 1},
            "falloff": {"type": "Enum", "value": 1},
            "pos_x": {"type": "Float", "value": 0.0},
            "pos_y": {"type": "Float", "value": 0.0},
            "pos_z": {"type": "Float", "value": -2.0},
            "range": {"type": "Float", "value": 4.0},
            "intensity": {"type": "Float", "value": 10.0},
            "cast_shadows": {"type": "Float", "value": 0.0}
        }
    }));
    nodes.push(serde_json::json!({
        "id": 31,
        "typeId": "node.math",
        "nodeId": "light_ramp",
        "params": {
            "a": {"type": "Float", "value": 0.0},
            "b": {"type": "Float", "value": 8.0},
            "op": {"type": "Enum", "value": 2}
        }
    }));
    scene["wires"].as_array_mut().expect("scene wires").extend([
        serde_json::json!({"fromNode": 30, "fromPort": "out", "toNode": 20, "toPort": "light_0"}),
        serde_json::json!({"fromNode": 0, "fromPort": "time", "toNode": 31, "toPort": "a"}),
        serde_json::json!({"fromNode": 31, "fromPort": "out", "toNode": 30, "toPort": "intensity"}),
    ]);
    scene.to_string()
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
fn low_spp_static_reconstruction_reduces_temporal_variance_and_preserves_energy() {
    let low = render(&scene_json(true, 1.0, [0.4; 3], [0.8; 3], 1, 1));
    let reference = render(&scene_json(true, 1.0, [0.4; 3], [0.8; 3], 1, MAX_SAMPLES));
    assert!(low.dispatched && reference.dispatched);
    let low_frames = frame_means(&low);
    let reference_frames = frame_means(&reference);
    assert!(low_frames.len() >= 4 && reference_frames.len() >= 1);
    let early = temporal_luma_variance(&low_frames[..2]);
    let late = temporal_luma_variance(&low_frames[low_frames.len() / 2..]);
    assert!(
        late <= early * 0.8 + 5e-4,
        "low-spp static SSS did not settle: early variance={early}, late variance={late}, frames={low_frames:?}"
    );
    let low_last = *low_frames.last().expect("low-spp final frame");
    let reference_mean = reference.mean;
    for channel in 0..3 {
        assert!(
            low_last[channel].is_finite() && reference_mean[channel].is_finite(),
            "non-finite reconstructed energy: low={low_last:?}, reference={reference_mean:?}"
        );
        assert!(
            (low_last[channel] - reference_mean[channel]).abs() < 0.2,
            "low-spp energy drifted from the 64-spp reference in channel {channel}: low={low_last:?}, reference={reference_mean:?}"
        );
    }
}

#[test]
fn moving_camera_reseeds_sss_history_instead_of_holding_static_radiance() {
    let stats = render(&moving_camera_scene());
    assert!(stats.dispatched);
    let frames = frame_means(&stats);
    let first = frames.first().expect("moving-camera first frame");
    let last = frames.last().expect("moving-camera last frame");
    let movement = (0..3)
        .map(|channel| (last[channel] - first[channel]).abs())
        .sum::<f32>();
    let late_variance = temporal_luma_variance(&frames[frames.len() / 2..]);
    assert!(
        movement > 1e-3,
        "camera motion did not change the reconstructed surface: first={first:?}, last={last:?}"
    );
    assert!(
        late_variance > 1e-7,
        "camera motion appears to be showing stale static SSS history: frames={frames:?}"
    );
}

#[test]
fn changing_light_reseeds_sss_history_without_stale_energy() {
    let stats = render(&moving_light_scene());
    assert!(stats.dispatched);
    let frames = frame_means(&stats);
    let first = frames.first().expect("moving-light first frame");
    let last = frames.last().expect("moving-light last frame");
    assert!(
        frames
            .iter()
            .all(|frame| frame[..3].iter().all(|value| value.is_finite())),
        "moving-light reconstruction produced non-finite radiance: {frames:?}"
    );
    assert!(
        rgb_luma(*last) > rgb_luma(*first) + 1e-4,
        "light change did not update SSS radiance: first={first:?}, last={last:?}"
    );
}

#[test]
fn changing_material_reseeds_sss_history_without_stale_energy() {
    let stats = render_with_material_reset(&scene_json(true, 1.0, [0.4; 3], [0.8; 3], 1, 1));
    assert!(stats.dispatched);
    let frames = frame_means(&stats);
    let before = rgb_luma(frames[WARMUP_FRAMES as usize / 2 - 1]);
    let after = rgb_luma(*frames.last().expect("material-reset final frame"));
    assert!(
        after.is_finite() && before.is_finite(),
        "material reset produced non-finite radiance: {frames:?}"
    );
    assert!(
        after < before * 0.5,
        "material reset left stale SSS energy: before={before}, after={after}, frames={frames:?}"
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
fn closed_curved_sphere_has_no_invalid_transport_pixels_in_either_mode() {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.sss_uv_sphere", || Box::new(CurvedSphereSource::new()));
    for mode in [0, 1] {
        let stats = render_with_registry_dimensions(
            &curved_scene_json(mode),
            &registry,
            CURVED_PROOF_WIDTH,
            CURVED_PROOF_HEIGHT,
            |_, _| {},
        );
        assert!(stats.dispatched, "curved SSS mode {mode} must dispatch");
        assert_curved_image_is_valid_and_scattering(&stats, mode);
    }
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
