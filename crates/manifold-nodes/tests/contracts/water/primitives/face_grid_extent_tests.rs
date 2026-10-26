/// Where the matter face nodes fuse in their host graph. The matter component
/// sizes its output from its grid input, so it folds into one region with its
/// consumer and stands alone without one; the fused-vs-unfused GPU proof
/// covers the folded case.
#[test]
fn face_grid_fusion_in_host_graphs() {
    use manifold_node_engine::freeze::FusionReport;
    use manifold_nodes_water::testkit::face_grid_scenes::matter_dam_break_faces;
    let mut registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes(&mut registry);
    let report = |def| {
        let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
        assert!(
            report.preparation_error.is_none(),
            "{:?}",
            report.preparation_error
        );
        report
    };
    let of_type = |report: &FusionReport, type_id: &str| -> Vec<_> {
        report
            .nodes
            .iter()
            .filter(|n| n.type_id == type_id)
            .cloned()
            .collect()
    };

    let alone = report(matter_dam_break_faces(None));
    let components = of_type(&alone, "node.matter_face_component");
    assert_eq!(components.len(), 3);
    assert!(
        components.iter().all(|n| !n.fused),
        "a lone matter component is its own dispatch: {components:?}"
    );

    let consumed = report(matter_dam_break_faces(Some(1)));
    let components = of_type(&consumed, "node.matter_face_component");
    let fused: Vec<_> = components.iter().filter(|n| n.fused).collect();
    assert_eq!(
        fused.len(),
        1,
        "only the consumed axis fuses: {components:?}"
    );
    let region = &consumed.regions[fused[0].region_index.expect("region")];
    let members: Vec<_> = consumed
        .nodes
        .iter()
        .filter(|n| region.member_node_ids.contains(&n.node_id))
        .map(|n| n.type_id.as_str())
        .collect();
    assert_eq!(members.len(), 2, "component and consumer only: {members:?}");
    assert!(
        members.contains(&"node.divide_by_value"),
        "the consumer shares the region: {members:?}"
    );
}

/// The MPM face scene, at every Resolution its domain admits, either
/// refuses by name or covers every dispatch
/// under the liquid extent rules; the fused consumer's lattice is the one at
/// 64.
#[test]
fn matter_face_scene_covers_every_dispatch() {
    use manifold_node_engine::exec::extent::ExtentError;
    use manifold_nodes_water::testkit::face_grid_scenes::matter_dam_break_faces;
    use manifold_nodes_water::testkit::preset_extents::LiquidPreset;
    let mut preset =
        LiquidPreset::build(&matter_dam_break_faces(None)).expect("the face scene builds");
    let mut ran = 0;
    for res in preset.resolutions() {
        match preset.check(res) {
            Ok(_) => ran += 1,
            Err(ExtentError::Refused { reason, .. })
                if reason.contains("Grid Budget") || reason.contains("Resolution") => {}
            Err(error) => panic!("Resolution {res}: {error}"),
        }
    }
    assert!(ran > 100, "only {ran} resolutions run");
    let mut consumed =
        LiquidPreset::build(&matter_dam_break_faces(Some(1))).expect("the consumer scene builds");
    consumed
        .check(64)
        .unwrap_or_else(|error| panic!("consumer at 64: {error}"));
}
