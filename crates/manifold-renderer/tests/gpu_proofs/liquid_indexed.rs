//! End-to-end proof that the compact indexed liquid surface has the same
//! raster result as the legacy triangle-list surface.
//!
//! The fixture is deliberately small and CPU-authored.  It still drives the
//! production count/scan/mesh/relax chain and the production scene renderer;
//! no CPU mesh is substituted for the actual marching-cubes output.

use manifold_gpu::{GpuBuffer, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::depth_rule::DepthRule;
use manifold_renderer::node_graph::{
    ArrayType, EffectNode, EffectNodeContext, EffectNodeType, NodeInput, NodeOutput, NodePort,
    ParamDef, ParamValues, PortKind, PortType, PrimitiveRegistry,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use serde_json::{json, Value};

use crate::harness;

#[derive(Clone, Copy)]
struct Fixture {
    type_id: &'static str,
    nodes: u32,
    asymmetric: bool,
}

const SPHERE: Fixture = Fixture {
    type_id: "test.liquid_sphere_8",
    nodes: 8,
    asymmetric: false,
};

const ASYMMETRIC: Fixture = Fixture {
    type_id: "test.liquid_asymmetric_16",
    nodes: 16,
    asymmetric: true,
};

/// A CPU-created level set source.  Its only device operation is the normal
/// graph copy into the preallocated Array(f32) output; the proof never opens
/// a second device or bypasses the graph executor.
struct LevelsetSource {
    type_id: EffectNodeType,
    outputs: Vec<NodeOutput>,
    values: Vec<f32>,
    staging: Option<GpuBuffer>,
}

impl LevelsetSource {
    fn new(fixture: Fixture) -> Self {
        let nodes = fixture.nodes as usize;
        let mut values = Vec::with_capacity(nodes * nodes * nodes);
        for z in 0..nodes {
            for y in 0..nodes {
                for x in 0..nodes {
                    let position = |index: usize| {
                        -2.0 + 4.0 * index as f32 / (nodes.saturating_sub(1)) as f32
                    };
                    let [px, py, pz] = [position(x), position(y), position(z)];
                    let value = if fixture.asymmetric {
                        let [dx, dy, dz] = [px - 0.22, py + 0.14, pz - 0.08];
                        let ellipsoid = (dx * dx / 1.15_f32.powi(2)
                            + dy * dy / 0.78_f32.powi(2)
                            + dz * dz / 0.92_f32.powi(2))
                            .sqrt()
                            - 1.0;
                        ellipsoid + 0.04 * dx * dz
                    } else {
                        (px * px + py * py + pz * pz).sqrt() - 1.05
                    };
                    values.push(value);
                }
            }
        }
        Self {
            type_id: EffectNodeType::new(fixture.type_id),
            outputs: vec![NodePort {
                name: "levelset".into(),
                ty: PortType::Array(ArrayType::of_known::<f32>()),
                kind: PortKind::Output,
                required: false,
            }],
            values,
            staging: None,
        }
    }
}

impl EffectNode for LevelsetSource {
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
        let Some(dst) = ctx.outputs.array("levelset") else {
            return;
        };
        let bytes = bytemuck::cast_slice(self.values.as_slice());
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
        port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "levelset").then_some(self.values.len() as u32)
    }
}

fn scalar(value: f32) -> Value {
    json!({"type": "Float", "value": value})
}

fn int(value: u32) -> Value {
    json!({"type": "Int", "value": value})
}

fn bool_param(value: bool) -> Value {
    json!({"type": "Bool", "value": value})
}

fn lattice_params(nodes: u32) -> Value {
    json!({
        "nodes_x": scalar(nodes as f32),
        "nodes_y": scalar(nodes as f32),
        "nodes_z": scalar(nodes as f32),
    })
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> Value {
    json!({
        "fromNode": from_node,
        "fromPort": from_port,
        "toNode": to_node,
        "toPort": to_port,
    })
}

fn material_node(volume: bool) -> Value {
    let mut params = json!({
        "color_r": scalar(0.72),
        "color_g": scalar(0.86),
        "color_b": scalar(1.0),
        "ambient": scalar(0.2),
        "metallic": scalar(0.0),
        "roughness": scalar(0.25),
        "specular": scalar(0.2),
        "ior": scalar(1.45),
        "emission_intensity": scalar(0.0),
        "transmission": scalar(if volume { 0.82 } else { 0.0 }),
        "volume_thickness": scalar(if volume { 0.65 } else { 0.0 }),
        "volume_attenuation_distance": scalar(2.5),
        "volume_attenuation_color_r": scalar(0.78),
        "volume_attenuation_color_g": scalar(0.92),
        "volume_attenuation_color_b": scalar(1.0),
        "volume_geometry": scalar(if volume { 1.0 } else { 0.0 }),
        "volume_scattering_density": scalar(if volume { 0.28 } else { 0.0 }),
        "volume_scattering_color_r": scalar(0.35),
        "volume_scattering_color_g": scalar(0.65),
        "volume_scattering_color_b": scalar(1.0),
        "volume_particle_density": scalar(0.0),
        "alpha_mode": json!({"type": "Enum", "value": 0}),
    });
    params["ambient"] = scalar(0.15);
    json!({"id": 9, "typeId": "node.pbr_material", "nodeId": "material", "params": params})
}

fn camera_node() -> Value {
    json!({
        "id": 10,
        "typeId": "node.orbit_camera",
        "nodeId": "camera",
        "params": {
            "orbit": scalar(std::f32::consts::FRAC_PI_2),
            "tilt": scalar(0.0),
            "distance": scalar(4.8),
            "fov_y": scalar(0.85),
        }
    })
}

fn graph_json(fixture: Fixture, indexed: bool, volume: bool, objects: u32) -> String {
    let nodes = fixture.nodes;
    let mut graph_nodes = vec![
        json!({"id": 0, "typeId": "system.generator_input", "nodeId": "input"}),
        json!({"id": 1, "typeId": fixture.type_id, "nodeId": "levelset", "params": {}}),
        json!({"id": 2, "typeId": "node.count_surface_triangles", "nodeId": "triangles", "params": lattice_params(nodes)}),
        json!({"id": 3, "typeId": "node.running_total", "nodeId": "triangle_scan", "params": {"per_item": int(3)}}),
        json!({"id": 6, "typeId": "node.volume_surface_mesh", "nodeId": "mesh", "params": {
            "center_x": scalar(0.0), "center_y": scalar(0.0), "center_z": scalar(0.0),
            "size_x": scalar(4.0), "size_y": scalar(4.0), "size_z": scalar(4.0),
            "resolution_scale": int(2), "max_capacity": int(0),
            "nodes_x": scalar(nodes as f32), "nodes_y": scalar(nodes as f32), "nodes_z": scalar(nodes as f32),
        }}),
        json!({"id": 7, "typeId": "node.relax_surface_mesh", "nodeId": "relax_a", "params": {
            "nodes_x": scalar(nodes as f32), "nodes_y": scalar(nodes as f32), "nodes_z": scalar(nodes as f32), "strength": scalar(0.32),
        }}),
        json!({"id": 8, "typeId": "node.relax_surface_mesh", "nodeId": "relax_b", "params": {
            "nodes_x": scalar(nodes as f32), "nodes_y": scalar(nodes as f32), "nodes_z": scalar(nodes as f32), "strength": scalar(0.32),
        }}),
        material_node(volume),
        camera_node(),
        json!({"id": 11, "typeId": "node.bake_environment", "nodeId": "environment", "params": {
            "width": int(64), "height": int(32), "intensity": scalar(1.0)
        }}),
        json!({"id": 12, "typeId": "node.scene_object", "nodeId": "object", "params": {}}),
        json!({"id": 20, "typeId": "node.render_scene", "nodeId": "scene", "params": {
            "objects": int(objects), "lights": int(0), "rt_enabled": bool_param(false),
            "rt_reflections": bool_param(false), "rt_gi": bool_param(false),
            "rt_ao": bool_param(false), "rt_shadows": bool_param(false)
        }}),
        json!({"id": 99, "typeId": "system.final_output", "nodeId": "output"}),
    ];
    let mut wires = vec![
        wire(1, "levelset", 2, "levelset"),
        wire(2, "counts", 3, "in"),
        wire(1, "levelset", 6, "levelset"),
        wire(3, "out", 6, "scan"),
        wire(3, "total", 6, "total"),
        wire(3, "extent", 6, "extent"),
        wire(6, "vertices", 7, "vertices"),
        wire(1, "levelset", 7, "levelset"),
        wire(3, "out", 7, "scan"),
        wire(3, "extent", 7, "extent"),
        wire(7, "relaxed", 8, "vertices"),
        wire(1, "levelset", 8, "levelset"),
        wire(3, "out", 8, "scan"),
        wire(3, "extent", 8, "extent"),
        wire(8, "relaxed", 12, "vertices"),
        wire(9, "out", 12, "material"),
        wire(10, "out", 20, "camera"),
        wire(11, "envmap", 20, "envmap"),
        wire(12, "object", 20, "object_0"),
        wire(20, "color", 99, "in"),
    ];
    if indexed {
        graph_nodes.insert(
            4,
            json!({"id": 4, "typeId": "node.count_surface_edges", "nodeId": "edges", "params": lattice_params(nodes)}),
        );
        graph_nodes.insert(
            5,
            json!({"id": 5, "typeId": "node.running_total", "nodeId": "edge_scan", "params": {"per_item": int(1)}}),
        );
        wires.extend([
            wire(1, "levelset", 4, "levelset"),
            wire(4, "counts", 5, "in"),
            wire(5, "out", 6, "edge_scan"),
            wire(5, "out", 7, "edge_scan"),
            wire(5, "out", 8, "edge_scan"),
            wire(6, "indices", 12, "indices"),
        ]);
    }
    json!({
        "version": 2,
        "name": if indexed { "LiquidIndexed" } else { "LiquidTriangleList" },
        "nodes": graph_nodes,
        "wires": wires,
    })
    .to_string()
}

/// The final parity stages in the existing bounded render fixture.
fn smoothed_graph_json(fixture: Fixture) -> String {
    let mut graph: Value = serde_json::from_str(&graph_json(fixture, true, false, 1)).unwrap();
    for node in graph["nodes"].as_array_mut().unwrap() {
        if node["id"] == 7 {
            node["typeId"] = json!("node.smooth_surface_mesh");
            node["params"]["strength"] = scalar(0.5);
            node["params"]["iterations"] = int(2);
        }
        if node["id"] == 8 {
            node["typeId"] = json!("node.surface_mesh_normals");
            node["params"].as_object_mut().unwrap().remove("strength");
        }
    }
    for wire in graph["wires"].as_array_mut().unwrap() {
        if wire["fromNode"] == 8 { wire["fromPort"] = json!("out"); }
    }
    // A real pointwise region makes this a compiler-on/off proof even
    // though the shared-edge gather must retain its own dispatch.
    graph["nodes"].as_array_mut().unwrap().extend([
        json!({"id": 30, "typeId": "node.rotate_3d", "nodeId": "turn_a", "params": {"angle_y": scalar(0.0)}}),
        json!({"id": 31, "typeId": "node.rotate_3d", "nodeId": "turn_b", "params": {"angle_y": scalar(0.0)}}),
    ]);
    let wires = graph["wires"].as_array_mut().unwrap();
    wires.retain(|w| w["toNode"] != 12 || w["toPort"] != "vertices");
    wires.extend([wire(8,"out",30,"in"),wire(30,"out",31,"in"),wire(31,"out",12,"vertices")]);
    graph.to_string()
}

fn fused_smoothed_graph(fixture: Fixture) -> String {
    let def = serde_json::from_str(&smoothed_graph_json(fixture)).unwrap();
    let view = manifold_renderer::node_graph::freeze::install::fuse_generator_view(&def, &registry_for(fixture))
        .expect("the downstream pointwise pair must fuse");
    for ty in ["node.smooth_surface_mesh", "node.surface_mesh_normals"] {
        assert_eq!(view.def.nodes.iter().filter(|n| n.type_id == ty).count(),1,"{ty} survives freezing");
    }
    serde_json::to_string(&view.def).unwrap()
}

#[test]
fn smoothed_liquid_fixture_graphs_compile_on_cpu() {
    for fixture in [SPHERE, ASYMMETRIC] {
        for json in [smoothed_graph_json(fixture),fused_smoothed_graph(fixture)] {
            PresetRuntime::from_json_str(&json,&registry_for(fixture)).expect("smoothed fixture compiles");
        }
    }
}

#[test]
fn liquid_surface_smoothing_normals_fused_matches_unfused() {
    for fixture in [SPHERE, ASYMMETRIC] {
        let registry=registry_for(fixture);
        let plain=render(&smoothed_graph_json(fixture),&registry);
        let fused=render(&fused_smoothed_graph(fixture),&registry);
        assert_eq!(plain,fused,"smoothed normals change when freezing {}",fixture.type_id);
        let hidden=render(&hidden_scene_json(fixture,false),&registry);
        assert_ne!(plain,hidden,"surface must actually draw");
    }
}

fn hidden_scene_json(fixture: Fixture, volume: bool) -> String {
    let mut graph: Value = serde_json::from_str(&graph_json(fixture, false, volume, 1)).unwrap();
    let object = graph["nodes"].as_array_mut().unwrap().iter_mut()
        .find(|node| node["id"] == 12).unwrap();
    object["params"]["visible"] = scalar(0.0);
    graph.to_string()
}

#[test]
fn indexed_liquid_fixture_graphs_compile_on_cpu() {
    for fixture in [SPHERE, ASYMMETRIC] {
        let registry = registry_for(fixture);
        for volume in [false, true] {
            for indexed in [false, true] {
                PresetRuntime::from_json_str(&graph_json(fixture, indexed, volume, 1), &registry)
                    .expect("liquid proof graph compiles on the mock backend");
            }
            PresetRuntime::from_json_str(&hidden_scene_json(fixture, volume), &registry)
                .expect("hidden control compiles on the mock backend");
        }
    }
}

fn registry_for(fixture: Fixture) -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register(fixture.type_id, move || Box::new(LevelsetSource::new(fixture)));
    registry
}

fn context(frame: i64, width: u32, height: u32) -> PresetContext {
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

fn render(json: &str, registry: &PrimitiveRegistry) -> Vec<u8> {
    let h = harness::shared();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|error| panic!("liquid indexed proof graph must build: {error}\n{json}"));
    let target = h.make_target("liquid-indexed-proof");
    // Running totals publish the CPU total one frame late.  Four committed
    // frames also exercise both relaxation passes after the first mesh write.
    for frame in 0..4 {
        let mut encoder = h.device.create_encoder("liquid-indexed-proof");
        {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &context(frame, h.width, h.height),
                &manifold_core::params::ParamManifest::default(),
            );
            assert_eq!(gpu.frame_status(), manifold_renderer::frame_status::FrameRenderStatus::Complete,
                "liquid indexed image proof frame {frame} must complete");
        }
        encoder.commit_and_wait_completed();
    }
    h.readback(&target.texture)
}

fn assert_nonempty_and_exact(fixture: Fixture, volume: bool) {
    let indexed = render(&graph_json(fixture, true, volume, 1), &registry_for(fixture));
    let triangle_list = render(&graph_json(fixture, false, volume, 1), &registry_for(fixture));
    let hidden = render(&hidden_scene_json(fixture, volume), &registry_for(fixture));
    assert_eq!(
        indexed, triangle_list,
        "indexed and triangle-list renders diverged for {} (volume={volume})",
        fixture.type_id
    );
    assert_ne!(
        indexed, hidden,
        "{} (volume={volume}) rendered the same bytes as its hidden-mesh control",
        fixture.type_id
    );
    assert!(
        indexed.chunks_exact(8).any(|pixel| pixel[..6].iter().any(|byte| *byte != 0)),
        "{} (volume={volume}) produced no visible RGB bytes",
        fixture.type_id
    );
}

#[test]
fn indexed_liquid_sphere_matches_triangle_list_for_opaque_and_volume_materials() {
    assert_nonempty_and_exact(SPHERE, false);
    assert_nonempty_and_exact(SPHERE, true);
}

#[test]
fn indexed_asymmetric_liquid_matches_triangle_list_for_opaque_and_volume_materials() {
    assert_nonempty_and_exact(ASYMMETRIC, false);
    assert_nonempty_and_exact(ASYMMETRIC, true);
}
