/// Where the face grid nodes fuse in their host graphs. GPU FLIP's component
/// sizes its output from its faces input, so it is an eligible atom, but the
/// u and v components feeding one consumer form no region: the region gates
/// leave each one isolated. The matter component sizes its output from its grid input, so it folds
/// into one region with its consumer and stands alone without one; the
/// fused-vs-unfused GPU proof covers the folded case.
#[test]
fn face_grid_fusion_in_host_graphs() {
    use manifold_node_engine::water::primitives::face_grid_scenes::matter_dam_break_faces;
    use manifold_node_engine::water::primitives::gpu_flip_preset::{FACE_NODES, WaterScene, water_def};
    use manifold_node_engine::freeze::FusionReport;
    let mut registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes(&mut registry);
    let report = |def| {
        let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
        assert!(report.preparation_error.is_none(), "{:?}", report.preparation_error);
        report
    };
    let of_type = |report: &FusionReport, type_id: &str| -> Vec<_> { report.nodes.iter().filter(|n| n.type_id == type_id).cloned().collect() };

    let mut gpu_flip = serde_json::to_value(water_def(WaterScene::dam_break(64).with_faces())).expect("def");
    let nodes = gpu_flip["nodes"].as_array().expect("nodes");
    let face_u = nodes.iter().find(|n| n["nodeId"] == FACE_NODES[0]).expect("face_u")["id"].clone();
    let face_v = nodes.iter().find(|n| n["nodeId"] == FACE_NODES[1]).expect("face_v")["id"].clone();
    let next = nodes.iter().filter_map(|n| n["id"].as_u64()).max().expect("ids") + 1;
    gpu_flip["nodes"].as_array_mut().expect("nodes").push(serde_json::json!({
        "id": next, "nodeId": "face_u_consumer", "typeId": "node.divide_by_value", "params": {},
    }));
    gpu_flip["wires"].as_array_mut().expect("wires").extend([
        serde_json::json!({"fromNode": face_u, "fromPort": "out", "toNode": next, "toPort": "values"}),
        serde_json::json!({"fromNode": face_v, "fromPort": "out", "toNode": next, "toPort": "divisor"}),
    ]);
    let gpu_flip = report(serde_json::from_value(gpu_flip).expect("def"));
    let sampled = of_type(&gpu_flip, "node.face_sample_component");
    assert_eq!(sampled.len(), 3);
    assert!(sampled.iter().all(|n| !n.fused), "GPU FLIP face components stay unfused: {sampled:?}");
    assert!(sampled.iter().all(|n| n.kind == "pointwise"), "GPU FLIP face components are eligible atoms: {sampled:?}");

    let alone = report(matter_dam_break_faces(None, false));
    let components = of_type(&alone, "node.matter_face_component");
    assert_eq!(components.len(), 3);
    assert!(components.iter().all(|n| !n.fused), "a lone matter component is its own dispatch: {components:?}");

    let consumed = report(matter_dam_break_faces(Some(1), false));
    let components = of_type(&consumed, "node.matter_face_component");
    let fused: Vec<_> = components.iter().filter(|n| n.fused).collect();
    assert_eq!(fused.len(), 1, "only the consumed axis fuses: {components:?}");
    let region = &consumed.regions[fused[0].region_index.expect("region")];
    let members: Vec<_> = consumed.nodes.iter().filter(|n| region.member_node_ids.contains(&n.node_id)).map(|n| n.type_id.as_str()).collect();
    assert_eq!(members.len(), 2, "component and consumer only: {members:?}");
    assert!(members.contains(&"node.divide_by_value"), "the consumer shares the region: {members:?}");
}

/// The MPM face scene, at every Resolution its domain admits, with and
/// without the moving box, either refuses by name or covers every dispatch
/// under the liquid extent rules; the fused consumer's lattice is the one at
/// 64.
#[test]
fn matter_face_scene_covers_every_dispatch() {
    use manifold_node_engine::water::primitives::face_grid_scenes::matter_dam_break_faces;
    use manifold_node_engine::water::liquid::extent::{ExtentError, LiquidPreset};
    for collider in [false, true] {
        let mut preset = LiquidPreset::build(&matter_dam_break_faces(None, collider)).expect("the face scene builds");
        let mut ran = 0;
        for res in preset.resolutions() {
            match preset.check(res) {
                Ok(_) => ran += 1,
                Err(ExtentError::Refused { reason, .. }) if reason.contains("Grid Budget") || reason.contains("Resolution") => {}
                Err(error) => panic!("collider {collider}, Resolution {res}: {error}"),
            }
        }
        assert!(ran > 100, "collider {collider}: only {ran} resolutions run");
    }
    let mut consumed = LiquidPreset::build(&matter_dam_break_faces(Some(1), false)).expect("the consumer scene builds");
    consumed.check(64).unwrap_or_else(|error| panic!("consumer at 64: {error}"));
}
