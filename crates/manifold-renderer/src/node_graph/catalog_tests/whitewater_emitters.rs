use manifold_node_engine::water::primitives::testkit as water_nodes;
use manifold_node_engine::testkit::liquid_surface::{Harness, params};
use manifold_node_engine::exec::effect_node::NodeInstanceId;
use manifold_node_engine::freeze::{classify::CapacityExpr, codegen::FusionRegion, codegen::InputSource, codegen::generate_fused};
use manifold_node_engine::testkit::water_codegen::{member, fused, run};
use manifold_nodes_image::node_graph::primitives::divide_by_value::DivideByValue;

use manifold_node_engine::water::primitives::testkit::GridBox as Box3;
fn values(extra: &[(&'static str, f32)]) -> Vec<(&'static str, f32)> {
    let mut v = vec![
        ("center_x", 4.0),
        ("center_y", 4.0),
        ("center_z", 4.0),
        ("size_x", 8.0),
        ("size_y", 8.0),
        ("size_z", 8.0),
        ("nodes_x", 9.0),
        ("nodes_y", 9.0),
        ("nodes_z", 9.0),
        ("cell_size", 1.0),
        ("face_cells_x", 8.0),
        ("face_cells_y", 8.0),
        ("face_cells_z", 8.0),
    ];
    v.extend_from_slice(extra);
    v
}

fn close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (a - b).abs() <= 3e-5 * b.abs().max(1.0),
            "element {i}: GPU {a}, CPU {b}"
        );
    }
}


#[test]
fn whitewater_turbulence_values_and_fusion() {
    let grid = Box3::new([8; 3], [4.0; 3], [8.0; 3]);
    let faces: [Vec<f32>; 3] = std::array::from_fn(|a| {
        (0..576)
            .map(|i| ((i * 7 + a * 13) % 31) as f32 - 15.0)
            .collect()
    });
    let phi: Vec<f32> = (0..512)
        .map(|i| if i % 7 == 0 { 1.0 } else { -1.0 })
        .collect();
    let want = water_nodes::turbulence(faces.each_ref().map(Vec::as_slice), [8; 3], &phi, grid);
    let mut h = Harness::new();
    let d = h.array(&phi, 512);
    let f = faces.each_ref().map(|v| h.array(v, 576));
    let v = values(&[]);
    let p = params(&v);
    let got: Vec<f32> = run(
        &mut h,
        &mut water_nodes::turbulence_field(),
        &[
            ("distance", d.0),
            ("face_u", f[0].0),
            ("face_v", f[1].0),
            ("face_w", f[2].0),
        ],
        512,
        &p,
    );
    close(&got, &want);
    let scalar = h.array(&[2.0f32], 1);
    let t = h.array(&got, 512);
    let unfused: Vec<f32> = run(
        &mut h,
        &mut DivideByValue::new(),
        &[("values", t.0), ("divisor", scalar.0)],
        512,
        &p,
    );
    let fused = fused::<f32>(
        &mut h,
        vec![
            water_nodes::member("turbulence_field", 0, (0..4).map(InputSource::External).collect()),
            member::<DivideByValue>(
                1,
                vec![
                    InputSource::Node(NodeInstanceId(0)),
                    InputSource::External(4),
                ],
            ),
        ],
        &[&d.1, &f[0].1, &f[1].1, &f[2].1, &scalar.1],
        512,
        &v,
    );
    close(&fused, &unfused);
}

#[test]
fn whitewater_influence_values_and_fusion() {
    let mut h = Harness::new();
    let old = [0.0, 4.0, 0.0, 4.0, 4.0, 0.0];
    let src = [water_nodes::source(0.25, 1.0, 2, 0); 6];
    let source = h.array(&src, 6);
    let solid = h.array(&[4.0f32, -4.0, 0.0, 2.99, -3.01, 3.0], 6);
    let previous = h.array(&old, 6);
    let divisor = h.array(&[2.0f32], 1);
    let v = values(&[("dt", 0.25), ("decay_rate", 2.0), ("base_level", 1.0)]);
    let p = params(&v);
    let got: Vec<f32> = run(
        &mut h,
        &mut water_nodes::whitewater_influence(),
        &[
            ("values", previous.0),
            ("solid", solid.0),
            ("source", source.0),
        ],
        6,
        &p,
    );
    close(&got, &[0.5, 3.5, 0.25, 0.25, 3.5, 0.25]);
    let input = h.array(&got, 6);
    let unfused: Vec<f32> = run(
        &mut h,
        &mut DivideByValue::new(),
        &[("values", input.0), ("divisor", divisor.0)],
        6,
        &p,
    );
    let folded = fused::<f32>(
        &mut h,
        vec![
            water_nodes::member("whitewater_influence", 0, (0..3).map(InputSource::External).collect()),
            member::<DivideByValue>(
                1,
                vec![
                    InputSource::Node(NodeInstanceId(0)),
                    InputSource::External(3),
                ],
            ),
        ],
        &[&previous.1, &solid.1, &source.1, &divisor.1],
        6,
        &v,
    );
    close(&folded, &unfused);
}

#[test]
fn whitewater_emitter_fusion_compiles_without_device() {
    let node = |n| InputSource::Node(NodeInstanceId(n));
    let ext = InputSource::External;
    let cases = [
        (
            vec![
                water_nodes::member("turbulence_field", 0, (0..4).map(ext).collect()),
                member::<DivideByValue>(1, vec![node(0), ext(4)]),
            ],
            5,
        ),
        (
            vec![
                water_nodes::member("inside_turbulence_potential", 0, (0..4).map(ext).collect()),
                water_nodes::member("turbulence_emission_count", 1, vec![ext(0), ext(4), ext(5), node(0), ext(6)]),
            ],
            7,
        ),
        (
            vec![
                water_nodes::member("dust_potential", 0, (0..4).map(ext).collect()),
                water_nodes::member("turbulence_emission_count", 1, vec![ext(0), ext(4), ext(5), node(0), ext(6)]),
            ],
            7,
        ),
        (
            vec![
                water_nodes::member("whitewater_influence", 0, (0..3).map(ext).collect()),
                member::<DivideByValue>(1, vec![node(0), ext(3)]),
            ],
            4,
        ),
        (
            vec![
                water_nodes::member("whitewater_emitter_velocity", 0, (0..3).map(ext).collect()),
                water_nodes::member("energy_potential", 1, vec![node(0)]),
            ],
            3,
        ),
        (
            vec![
                water_nodes::member("whitewater_obstacle_source", 0, (1..4).map(ext).collect()),
                water_nodes::member("whitewater_influence", 1, vec![ext(0), ext(4), node(0)]),
            ],
            5,
        ),
    ];
    for (nodes, num_external_inputs) in cases {
        let name = nodes[0].type_id.clone();
        let region = FusionRegion {
            nodes,
            num_external_inputs,
            outputs: vec![(NodeInstanceId(1), "out".to_owned())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: vec![],
            sampled_externals: vec![],
            camera_externals: 0,
            output_capacity: Some(CapacityExpr::Slot(0)),
        };
        let g = generate_fused(&region).unwrap();
        let m = naga::front::wgsl::parse_str(&g.wgsl)
            .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&g.wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&m)
        .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&g.wgsl)));
    }
}
