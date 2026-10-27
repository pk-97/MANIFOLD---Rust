use manifold_node_engine::exec::effect_node::NodeInstanceId;
use manifold_node_engine::freeze::{
    classify::CapacityExpr, codegen::FusionRegion, codegen::InputSource, codegen::generate_fused,
};
use manifold_node_engine::testkit::array_harness::{Harness, params};
use manifold_node_engine::testkit::fused_atoms::{fused, member, run};
use manifold_nodes_image::node_graph::primitives::divide_by_value::DivideByValue;
use manifold_water_whitewater::primitives::testkit::whitewater as whitewater_nodes;

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
fn whitewater_influence_values_and_fusion() {
    let mut h = Harness::new();
    let old = [0.0, 4.0, 0.0, 4.0, 4.0, 0.0];
    let src = [whitewater_nodes::source(0.25, 1.0, 2, 0); 6];
    let source = h.array(&src, 6);
    let solid = h.array(&[4.0f32, -4.0, 0.0, 2.99, -3.01, 3.0], 6);
    let previous = h.array(&old, 6);
    let divisor = h.array(&[2.0f32], 1);
    let v = vec![
        ("dt", 0.25),
        ("decay_rate", 2.0),
        ("base_level", 1.0),
        ("cell_size", 1.0),
    ];
    let p = params(&v);
    let got: Vec<f32> = run(
        &mut h,
        &mut whitewater_nodes::whitewater_influence(),
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
            whitewater_nodes::member(
                "whitewater_influence",
                0,
                (0..3).map(InputSource::External).collect(),
            ),
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
    // Each atom on the step's world tiles, `tiles` its last array input. The
    // obstacle source publishes two arrays, so it runs alone.
    let cases = [
        (
            vec![
                whitewater_nodes::member("turbulence_field", 0, (0..3).map(ext).collect()),
                member::<DivideByValue>(1, vec![node(0), ext(3)]),
            ],
            4,
        ),
        (
            vec![
                whitewater_nodes::member(
                    "inside_turbulence_potential",
                    0,
                    (0..5).map(ext).collect(),
                ),
                whitewater_nodes::member(
                    "turbulence_emission_count",
                    1,
                    vec![ext(0), ext(5), ext(6), node(0), ext(7), ext(4)],
                ),
            ],
            8,
        ),
        (
            vec![
                whitewater_nodes::member("dust_potential", 0, (0..5).map(ext).collect()),
                whitewater_nodes::member(
                    "turbulence_emission_count",
                    1,
                    vec![ext(0), ext(5), ext(6), node(0), ext(7), ext(4)],
                ),
            ],
            8,
        ),
        (
            vec![
                whitewater_nodes::member("whitewater_influence", 0, (0..3).map(ext).collect()),
                member::<DivideByValue>(1, vec![node(0), ext(3)]),
            ],
            4,
        ),
        (
            vec![
                whitewater_nodes::member(
                    "whitewater_emitter_velocity",
                    0,
                    (0..4).map(ext).collect(),
                ),
                whitewater_nodes::member("energy_potential", 1, vec![node(0)]),
            ],
            4,
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
            live_anchor: None,
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
