use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue,
};
use manifold_renderer::node_graph::scene_modifier_legacy_migration::migrate_legacy_scene_modifiers;
use manifold_renderer::node_graph::PrimitiveRegistry;

const FIXTURES: &[(&str, &str)] = &[
    (
        "elastic_sculpture",
        include_str!("fixtures/scene-modifiers/elastic_sculpture_applied_v2.json"),
    ),
    (
        "surface_peel",
        include_str!("fixtures/scene-modifiers/surface_peel_applied_v2.json"),
    ),
    (
        "vortex_fragments",
        include_str!("fixtures/scene-modifiers/vortex_fragments_applied_v2.json"),
    ),
    (
        "stack",
        include_str!("fixtures/scene-modifiers/photoscan_stack_applied_v2.json"),
    ),
];

#[test]
fn known_photoscan_v2_shapes_migrate_and_are_idempotent() {
    let registry = PrimitiveRegistry::with_builtin();
    for (name, json) in FIXTURES {
        let mut def: EffectGraphDef = serde_json::from_str(json).expect("fixture parses");
        let original = def.clone();
        let report = migrate_legacy_scene_modifiers(&mut def, &registry);
        assert!(report.changed, "{name} should be adopted: {:?}", report.diagnostics);
        assert!(
            report.diagnostics.is_empty(),
            "{name}: {:?}",
            report.diagnostics
        );
        assert!(
            !def.scene_modifiers.is_empty(),
            "{name} should gain a v3 stack"
        );
        if *name == "stack" {
            assert_eq!(def.scene_modifiers.len(), 3, "{name} keeps the full stack");
        }
        assert!(
            def.nodes
                .iter()
                .all(|node| !node.node_id.as_str().starts_with("photoscan/")),
            "{name} leaves no legacy controls or stage nodes"
        );
        let roundtrip: EffectGraphDef =
            serde_json::from_str(&serde_json::to_string(&def).expect("migrated graph serializes"))
                .expect("migrated graph roundtrips");
        assert_eq!(roundtrip, def, "{name} migration is serde stable");
        let second = migrate_legacy_scene_modifiers(&mut def, &registry);
        assert!(!second.changed, "{name} migration is idempotent");
        assert_eq!(def, roundtrip, "{name} second load is unchanged");
        assert_eq!(original.version, 2, "fixtures remain pre-v3 inputs");
    }
}

#[test]
fn malformed_photoscan_footprint_is_left_byte_for_byte_unchanged() {
    let mut def: EffectGraphDef = serde_json::from_str(FIXTURES[0].1).expect("fixture parses");
    let stage = def
        .nodes
        .iter_mut()
        .find_map(|node| {
            node.group.as_mut().and_then(|group| {
                group.nodes.iter_mut().find(|child| {
                    child
                        .node_id
                        .as_str()
                        .starts_with("photoscan/elastic_sculpture/")
                        && child.type_id == "group"
                })
            })
        })
        .expect("fixture has a photoscan stage");
    let duplicate = stage.group.as_ref().expect("stage has body").nodes[0].clone();
    stage
        .group
        .as_mut()
        .expect("stage has body")
        .nodes
        .push(duplicate);
    let original = def.clone();
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(!report.changed);
    assert!(!report.diagnostics.is_empty());
    assert_eq!(def, original);
}

#[test]
fn host_photo_scan_bindings_retarget_to_modifier_parameters() {
    let mut def: EffectGraphDef = serde_json::from_str(FIXTURES[0].1).expect("fixture parses");
    let original_bindings = def
        .preset_metadata
        .as_ref()
        .expect("fixture metadata")
        .bindings
        .clone();
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(
        report.changed,
        "migration diagnostics: {:?}",
        report.diagnostics
    );
    let id = def.scene_modifiers[0].id.clone();
    let bindings = &def
        .preset_metadata
        .as_ref()
        .expect("migrated metadata")
        .bindings;
    assert!(bindings
        .iter()
        .filter(|binding| matches!(binding.target, BindingTarget::SceneModifier { .. }))
        .all(|binding| matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id } if *modifier_id == id && param_id == &binding.id)));
    for binding in bindings {
        let Some(original) = original_bindings
            .iter()
            .find(|candidate| candidate.id == binding.id)
        else {
            continue;
        };
        assert_eq!(binding.label, original.label);
        assert_eq!(binding.default_value, original.default_value);
        assert_eq!(binding.user_added, original.user_added);
        assert_eq!(
            binding.default_mirrors_node_param,
            original.default_mirrors_node_param
        );
    }
}

#[test]
fn migration_preserves_nondefault_local_cell_size() {
    let mut def: EffectGraphDef = serde_json::from_str(FIXTURES[1].1).expect("fixture parses");
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(
        report.changed,
        "migration diagnostics: {:?}",
        report.diagnostics
    );
    let group = def.scene_modifiers[0].graph.nodes[0]
        .group
        .as_ref()
        .expect("migrated stage body");
    let patch = group
        .nodes
        .iter()
        .find(|node| node.type_id == "node.transform_mesh_patches")
        .expect("migrated patch atom");
    assert_eq!(
        patch.params.get("cell_size"),
        Some(&SerializedParamValue::Float { value: 0.15 })
    );
}

#[test]
fn common_renamed_stage_handles_are_preserved() {
    let mut def: EffectGraphDef = serde_json::from_str(FIXTURES[0].1).expect("fixture parses");
    for_each_node_mut(&mut def.nodes, &mut |node| {
        if node
            .node_id
            .as_str()
            .starts_with("photoscan/elastic_sculpture/")
            && node.node_id.as_str().ends_with("/stage")
        {
            node.handle = Some("My Shared Stage".into());
        }
    });
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(
        report.changed,
        "migration diagnostics: {:?}",
        report.diagnostics
    );
    assert_eq!(
        def.scene_modifiers[0].graph.nodes[0].handle.as_deref(),
        Some("My Shared Stage")
    );
}

#[test]
fn edited_per_target_stage_is_rejected_atomically() {
    let mut def: EffectGraphDef = serde_json::from_str(FIXTURES[0].1).expect("fixture parses");
    for_each_node_mut(&mut def.nodes, &mut |node| {
        if node
            .node_id
            .as_str()
            .contains("photoscan/elastic_sculpture/right_object/shear_x")
        {
            node.params.insert(
                "phase_offset".into(),
                SerializedParamValue::Float { value: 0.75 },
            );
        }
    });
    let original = def.clone();
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(!report.changed);
    assert!(!report.diagnostics.is_empty());
    assert_eq!(def, original);
}

#[test]
fn leaf_affine_divergence_is_rejected_atomically() {
    let mut def: EffectGraphDef = serde_json::from_str(FIXTURES[0].1).expect("fixture parses");
    let binding = def
        .preset_metadata
        .as_mut()
        .expect("fixture metadata")
        .bindings
        .iter_mut()
        .find(|binding| {
            matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id.as_str().contains("/right_object/") && param == "amplitude")
        })
        .expect("right target leaf binding");
    binding.scale = 2.0;
    let original = def.clone();
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(!report.changed);
    assert!(!report.diagnostics.is_empty());
    assert_eq!(def, original);
}

#[test]
fn full_stack_conflicting_order_is_rejected_atomically() {
    let mut def: EffectGraphDef = serde_json::from_str(FIXTURES[3].1).expect("fixture parses");
    let left_group = def
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "scan_left_group")
        .and_then(|node| node.group.as_mut())
        .expect("left group");
    let edge = left_group
        .wires
        .iter_mut()
        .find(|wire| wire.from_node == 32 && wire.to_node == 41 && wire.to_port == "current")
        .expect("elastic to surface edge");
    edge.from_node = 51;
    let original = def.clone();
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(!report.changed);
    assert!(!report.diagnostics.is_empty());
    assert_eq!(def, original);
}

#[test]
fn controls_are_disconnected_and_host_macros_collapse_once() {
    let mut def: EffectGraphDef = serde_json::from_str(FIXTURES[0].1).expect("fixture parses");
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(
        report.changed,
        "migration diagnostics: {:?}",
        report.diagnostics
    );
    let host_bindings = &def
        .preset_metadata
        .as_ref()
        .expect("host metadata")
        .bindings;
    assert_eq!(
        host_bindings
            .iter()
            .filter(|binding| matches!(binding.target, BindingTarget::SceneModifier { .. }))
            .count(),
        7
    );
    let local = &def.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_ref()
        .expect("local metadata")
        .bindings;
    assert_eq!(local.len(), 19);
    let controller_ids: Vec<_> = def.scene_modifiers[0].graph.nodes[0]
        .group
        .as_ref()
        .expect("local stage")
        .nodes
        .iter()
        .filter(|node| node.node_id.as_str().starts_with("controller_"))
        .map(|node| node.id)
        .collect();
    assert!(def.scene_modifiers[0].graph.nodes[0]
        .group
        .as_ref()
        .expect("local stage")
        .wires
        .iter()
        .all(|wire| {
            !controller_ids.contains(&wire.from_node) && !controller_ids.contains(&wire.to_node)
        }));
}

fn for_each_node_mut(nodes: &mut [EffectGraphNode], f: &mut impl FnMut(&mut EffectGraphNode)) {
    for node in nodes {
        f(node);
        if let Some(group) = node.group.as_mut() {
            for_each_node_mut(&mut group.nodes, f);
        }
    }
}
