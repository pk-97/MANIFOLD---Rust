//! Freeze boundaries and value parity for the liquid surface mesh stages.
//!
//! The smoothing loop and the normal gather are deliberately materialized
//! stages.  The small pointwise tail in the fixture makes the freeze proof
//! exercise a real region rewrite instead of comparing two unfrozen graphs.

use serde_json::{Value, json};

fn smoothed_surface_with_pointwise_tail() -> manifold_core::effect_graph_def::EffectGraphDef {
    let source = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("WaterDamBreakGpuFlip"),
    )
    .expect("WaterDamBreakGpuFlip bundled");
    let mut preset: Value = serde_json::from_str(&source).expect("WaterDamBreakGpuFlip parses");
    let surface = manifold_node_engine::water::liquid::conformance::json_node_mut(&mut preset, "surface")
        .expect("Liquid Surface group");
    let group = &mut surface["group"];
    let (normals, output) = {
        let normals = manifold_node_engine::water::liquid::conformance::json_node_mut(group, "liquid_normals")
            .expect("surface normals")
            .get("id")
            .cloned()
            .expect("surface normals id");
        let output = group["nodes"].as_array().expect("surface nodes")
            .iter()
            .find(|node| node["typeId"] == "system.group_output")
            .expect("surface group output")
            .get("id")
            .cloned()
            .expect("surface output id");
        (normals, output)
    };
    group["nodes"]
        .as_array_mut()
        .expect("surface nodes")
        .extend([
            json!({
                "id": 100,
                "nodeId": "surface_turn_a",
                "typeId": "node.rotate_3d",
                "params": {"angle_y": {"type": "Float", "value": 0.17}}
            }),
            json!({
                "id": 101,
                "nodeId": "surface_turn_b",
                "typeId": "node.rotate_3d",
                "params": {"angle_y": {"type": "Float", "value": -0.23}}
            }),
        ]);
    let wires = group["wires"].as_array_mut().expect("surface wires");
    let output_wire = wires
        .iter_mut()
        .find(|wire| {
            wire["fromNode"] == normals && wire["toNode"] == output && wire["toPort"] == "vertices"
        })
        .expect("surface normals feed the group output");
    output_wire["fromNode"] = json!(101);
    wires.extend([
        json!({"fromNode": normals.clone(), "fromPort": "out", "toNode": 100, "toPort": "in"}),
        json!({"fromNode": 100, "fromPort": "out", "toNode": 101, "toPort": "in"}),
    ]);
    serde_json::from_value(preset).expect("surface graph loads")
}

#[test]
fn freeze_keeps_surface_stages_and_fuses_the_real_pointwise_tail() {
    let def = smoothed_surface_with_pointwise_tail();
    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    let view = manifold_node_engine::freeze::install::fuse_generator_view(&def, &registry)
        .expect("the two rotate nodes must produce a frozen region");

    for type_id in ["node.volume_surface_mesh", "node.smooth_surface_mesh", "node.surface_mesh_normals"] {
        assert_eq!(
            view.def
                .nodes
                .iter()
                .filter(|node| node.type_id == type_id)
                .count(),
            1,
            "the materialized surface stage must survive freezing: {type_id}"
        );
    }
    let mesh = view.def.nodes.iter().find(|n| n.type_id == "node.volume_surface_mesh").unwrap();
    for port in ["solid", "solid_nodes_x", "solid_nodes_y", "solid_nodes_z"] {
        assert!(view.def.wires.iter().any(|w| w.to_node == mesh.id && w.to_port == port),
            "freeze must preserve mesh contact input {port}");
    }
    assert!(view.node_retarget.contains_key("surface_turn_a"));
    assert_eq!(
        view.node_retarget.get("surface_turn_a"),
        view.node_retarget.get("surface_turn_b"),
        "the real pointwise tail must be retargeted to one frozen region"
    );
    assert!(
        view.def
            .nodes
            .iter()
            .any(|node| node.type_id == "node.wgsl_compute"),
        "freezing the fixture must install a generated region"
    );
}

#[cfg(feature = "gpu-proofs")]
mod gpu_tests {
    use manifold_node_engine::testkit::liquid_surface::{Harness, params, read};
    use super::super::rotate_3d::Rotate3D;
    use super::super::smooth_surface_mesh::SmoothSurfaceMesh;
    use super::super::surface_mesh_normals::SurfaceMeshNormals;
    use manifold_node_engine::water::primitives::surface_mesh_parity::{fixture, flip_normals, flip_smooth};
    use manifold_node_engine::mesh::MeshVertex;
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use manifold_node_engine::primitive::PrimitiveSpec;
    use glam::DVec3;
    use manifold_gpu::GpuBinding;

    fn rotate(point: DVec3, angles: [f64; 3]) -> DVec3 {
        let (ax, ay, az) = (angles[0], angles[1], angles[2]);
        let (cx, sx) = (ax.cos(), ax.sin());
        let (cy, sy) = (ay.cos(), ay.sin());
        let (cz, sz) = (az.cos(), az.sin());
        let y1 = point.y * cx - point.z * sx;
        let z1 = point.y * sx + point.z * cx;
        let x2 = point.x * cy + z1 * sy;
        let z2 = -point.x * sy + z1 * cy;
        DVec3::new(x2 * cz - y1 * sz, x2 * sz + y1 * cz, z2)
    }

    fn rotate_node(node_id: u32, input: InputSource) -> RegionNode<'static> {
        RegionNode {
            node_id: NodeInstanceId(node_id),
            fusion_kind: Rotate3D::FUSION_KIND,
            body: Rotate3D::WGSL_BODY.expect("rotate body"),
            params: Rotate3D::PARAMS,
            inputs: vec![input],
            input_access: Rotate3D::INPUT_ACCESS.to_vec(),
            node_inputs: Rotate3D::INPUTS,
            node_outputs: Rotate3D::OUTPUTS,
            node_includes: Rotate3D::WGSL_INCLUDES,
            derived_uniforms: Rotate3D::DERIVED_UNIFORMS,
            type_id: Rotate3D::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        }
    }

    #[test]
    fn smoothed_normals_fused_tail_matches_f64_reference_and_unfused() {
        let f = fixture(0);
        let capacity = f.triangles.len() * 3 + 17;
        let vertices: Vec<MeshVertex> = f
            .points
            .iter()
            .zip(&f.gradients)
            .map(|(point, normal)| MeshVertex {
                position: point.as_vec3().to_array(),
                normal: normal.as_vec3().to_array(),
                color: [0.2, 0.4, 0.6, 1.0],
                ..bytemuck::Zeroable::zeroed()
            })
            .collect();
        let triangle_scan = f.triangle_scan();
        let edge_scan = f.edge_scan();
        let mut harness = Harness::new();
        let (input, _) = harness.array(&vertices, capacity);
        let (field, _) = harness.array(&f.field, f.field.len());
        let (scan, _) = harness.array(&triangle_scan, triangle_scan.len());
        let (edges, _) = harness.array(&edge_scan, edge_scan.len());
        let (smoothed, _) = harness.array::<MeshVertex>(&[], capacity);
        let (normals, _) = harness.array::<MeshVertex>(&[], capacity);
        let settings = params(&[
            ("nodes_x", f.nodes as f32),
            ("nodes_y", f.nodes as f32),
            ("nodes_z", f.nodes as f32),
            ("strength", 0.5),
            ("iterations", 2.0),
        ]);
        let (_, errors) = harness.run(
            &mut SmoothSurfaceMesh::new(),
            &[
                ("vertices", input),
                ("levelset", field),
                ("scan", scan),
                ("edge_scan", edges),
            ],
            &[("relaxed", smoothed)],
            &settings,
        );
        assert!(errors.is_empty(), "smoothing errors: {errors:?}");
        let (_, errors) = harness.run(
            &mut SurfaceMeshNormals::new(),
            &[
                ("vertices", smoothed),
                ("levelset", field),
                ("scan", scan),
                ("edge_scan", edges),
            ],
            &[("out", normals)],
            &settings,
        );
        assert!(errors.is_empty(), "normal errors: {errors:?}");

        let angles = [0.17_f32, -0.23, 0.31];
        let mut turn_a = Rotate3D::new();
        let mut turn_b = Rotate3D::new();
        let (turned_a, _) = harness.array::<MeshVertex>(&[], capacity);
        let (turned_b, turned_b_buffer) = harness.array::<MeshVertex>(&[], capacity);
        let turn_params = params(&[
            ("angle_x", angles[0]),
            ("angle_y", angles[1]),
            ("angle_z", angles[2]),
        ]);
        let (_, errors) = harness.run(
            &mut turn_a,
            &[("in", normals)],
            &[("out", turned_a)],
            &turn_params,
        );
        assert!(errors.is_empty(), "first tail errors: {errors:?}");
        let (_, errors) = harness.run(
            &mut turn_b,
            &[("in", turned_a)],
            &[("out", turned_b)],
            &turn_params,
        );
        assert!(errors.is_empty(), "second tail errors: {errors:?}");
        let unfused = read::<MeshVertex>(&turned_b_buffer, capacity);

        let region = FusionRegion {
            nodes: vec![
                rotate_node(0, InputSource::External(0)),
                rotate_node(1, InputSource::Node(NodeInstanceId(0))),
            ],
            num_external_inputs: 1,
            outputs: vec![(NodeInstanceId(1), "out".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        let fused = generate_fused(&region).expect("rotate tail fuses");
        let mut words: Vec<u32> = fused
            .param_order
            .iter()
            .map(|(_, name)| match *name {
                "angle_x" => angles[0].to_bits(),
                "angle_y" => angles[1].to_bits(),
                "angle_z" => angles[2].to_bits(),
                other => panic!("unexpected rotate parameter {other}"),
            })
            .collect();
        words.push(capacity as u32);
        while !words.len().is_multiple_of(4) {
            words.push(0);
        }
        let fused_buffer = harness
            .device
            .create_buffer_shared((capacity * std::mem::size_of::<MeshVertex>()) as u64);
        let pipeline =
            harness
                .device
                .create_compute_pipeline(&fused.wgsl, ENTRY, "surface-mesh-fused-tail");
        let normal_buffer = harness.buffer(normals);
        let mut encoder = harness.device.create_encoder("surface-mesh-fused-tail");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&words),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &normal_buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &fused_buffer,
                    offset: 0,
                },
            ],
            [(capacity as u32).div_ceil(256), 1, 1],
            "surface-mesh-fused-tail",
        );
        encoder.commit_and_wait_completed();
        let fused_out: Vec<MeshVertex> = read(&fused_buffer, capacity);

        let smoothed = flip_smooth(&f.points, &f.triangles, 0.5, 2);
        let expected_normals = flip_normals(&smoothed, &f.triangles);
        let angles64 = angles.map(f64::from);
        for i in 0..f.points.len() {
            let expected_position = rotate(rotate(smoothed[i], angles64), angles64);
            let expected_normal = rotate(rotate(expected_normals[i], angles64), angles64);
            for (label, value) in [("fused", fused_out[i]), ("unfused", unfused[i])] {
                let position = DVec3::from_array(value.position.map(f64::from));
                let normal = DVec3::from_array(value.normal.map(f64::from));
                assert!(
                    position.distance(expected_position) < 5e-5,
                    "{label} position {i}"
                );
                assert!(
                    normal.distance(expected_normal) < 5e-5,
                    "{label} normal {i}"
                );
                assert_eq!(value.color, vertices[i].color, "{label} color {i}");
            }
            assert!(
                DVec3::from_array(fused_out[i].position.map(f64::from))
                    .distance(DVec3::from_array(unfused[i].position.map(f64::from)))
                    < 3e-6,
                "fused/unfused position {i}"
            );
            assert!(
                DVec3::from_array(fused_out[i].normal.map(f64::from))
                    .distance(DVec3::from_array(unfused[i].normal.map(f64::from)))
                    < 3e-6,
                "fused/unfused normal {i}"
            );
        }
        assert!(
            fused_out[f.points.len()..]
                .iter()
                .all(|v| v.position == [0.0; 3] && v.normal == [0.0; 3])
        );
    }
}
