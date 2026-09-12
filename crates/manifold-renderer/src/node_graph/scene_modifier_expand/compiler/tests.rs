use super::*;
use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::scene_modifier_preset::{
    SceneModifierRecipe, SceneModifierStageDef, SceneStageInput, SceneStageOutput,
    SceneTargetSelection,
};

pub(super) fn fixture() -> EffectGraphDef {
    let mut owner: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
    )))
    .unwrap();
    let mut recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/ElasticSculpture.json"
    )))
    .unwrap();
    recipe.version = 3;
    recipe.preset_metadata.as_mut().unwrap().scene_modifier = Some(SceneModifierRecipe {
        schema_version: 1,
        singleton: false,
        enabled_param: "enabled".into(),
        preparation_params: vec![],
        initializers: vec![],
        calibrations: vec![],
        stages: vec![SceneModifierStageDef {
            group: NodeId::new("elastic_stage"),
            scope: SceneStageScope::EachObject,
            inputs: vec![SceneStageInput {
                port: "current".into(),
                source: SceneStageSource::Previous {
                    endpoint: SceneEndpoint::Vertices,
                },
            }],
            outputs: vec![SceneStageOutput {
                port: "vertices".into(),
                endpoint: SceneEndpoint::Vertices,
            }],
        }],
    });
    owner.version = 3;
    owner.scene_modifiers.push(SceneModifierInstanceDef {
        id: NodeId::new("test_modifier"),
        scene: SceneNodeRef {
            scope: vec![],
            node: NodeId::new("scan_render"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: vec![],
        graph: Box::new(recipe),
    });
    owner
}

#[test]
fn scene_modifier_expand_compiler_attaches_preserves_host_and_is_idempotent() {
    let owner = fixture();
    let canonical = owner.clone();
    let registry = PrimitiveRegistry::with_builtin();
    let expanded = expand_scene_modifiers(&owner, &registry).unwrap();
    assert!(expanded.scene_modifiers.is_empty());
    assert_eq!(
        expanded
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.wave_shear_mesh")
            .count(),
        4
    );
    assert_eq!(
        expanded
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.scene_object")
            .count(),
        2
    );
    for id in ["left_object", "right_object"] {
        let object = expanded
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == id)
            .unwrap();
        for port in ["material", "base_color_map", "transform"] {
            assert_eq!(
                expanded
                    .wires
                    .iter()
                    .filter(|wire| wire.to_node == object.id && wire.to_port == port)
                    .count(),
                1
            );
        }
    }
    assert_eq!(
        expand_scene_modifiers(&expanded, &registry).unwrap(),
        expanded
    );
    assert_eq!(owner, canonical);
}

#[test]
fn scene_modifier_expand_compiler_stack_order_and_remove_restore_routes() {
    let mut owner = fixture();
    let mut second = owner.scene_modifiers[0].clone();
    second.id = NodeId::new("second_modifier");
    owner.scene_modifiers.push(second);
    let registry = PrimitiveRegistry::with_builtin();
    let expanded = expand_scene_modifiers(&owner, &registry).unwrap();
    assert_eq!(
        expanded
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.wave_shear_mesh")
            .count(),
        8
    );
    let left = expanded
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == "left_object")
        .unwrap();
    let last = expanded
        .wires
        .iter()
        .find(|wire| wire.to_node == left.id && wire.to_port == "vertices")
        .unwrap();
    let node = expanded
        .nodes
        .iter()
        .find(|node| node.id == last.from_node)
        .unwrap();
    let last_id = node.node_id.clone();
    owner.scene_modifiers.swap(0, 1);
    let reordered = expand_scene_modifiers(&owner, &registry).unwrap();
    let left = reordered
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == "left_object")
        .unwrap();
    let last = reordered
        .wires
        .iter()
        .find(|wire| wire.to_node == left.id && wire.to_port == "vertices")
        .unwrap();
    assert_ne!(
        reordered
            .nodes
            .iter()
            .find(|node| node.id == last.from_node)
            .unwrap()
            .node_id,
        last_id
    );
    let before: BTreeSet<_> = expanded
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect();
    let after: BTreeSet<_> = reordered
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect();
    assert_eq!(before, after, "reordering retains generated identities");
    owner.scene_modifiers.clear();
    assert_eq!(expand_scene_modifiers(&owner, &registry).unwrap(), owner);
}

#[test]
fn scene_modifier_expand_compiler_macro_fanout_keeps_real_leaf_conversion() {
    let mut owner = fixture();
    let local = owner.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_ref()
        .unwrap();
    let spec = local
        .params
        .iter()
        .find(|param| param.id == "bend")
        .unwrap()
        .clone();
    let mut binding = local
        .bindings
        .iter()
        .find(|binding| binding.id == "bend")
        .unwrap()
        .clone();
    binding.target = BindingTarget::SceneModifier {
        modifier_id: owner.scene_modifiers[0].id.clone(),
        param_id: "bend".into(),
    };
    binding.default_value = 0.3;
    owner.preset_metadata.as_mut().unwrap().params.push(spec);
    owner
        .preset_metadata
        .as_mut()
        .unwrap()
        .bindings
        .push(binding);
    let expanded = expand_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin()).unwrap();
    let bindings = &expanded.preset_metadata.as_ref().unwrap().bindings;
    assert_eq!(bindings.len(), 2);
    for binding in bindings {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            panic!("binding must resolve")
        };
        let node = expanded
            .nodes
            .iter()
            .find(|node| node.node_id == *node_id)
            .unwrap();
        assert_eq!(
            node.params[param],
            SerializedParamValue::Float { value: 0.3 }
        );
    }
}

#[test]
fn scene_modifier_expand_compiler_rejects_invalid_endpoint_and_dynamic_rt_even_disabled() {
    let mut owner = fixture();
    owner
        .nodes
        .iter_mut()
        .find(|node| node.type_id == "node.render_scene")
        .unwrap()
        .params
        .insert(
            "rt_enabled".into(),
            SerializedParamValue::Bool { value: true },
        );
    for binding in &mut owner.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_mut()
        .unwrap()
        .bindings
    {
        if binding.id == "enabled" {
            binding.default_value = 0.0;
        }
    }
    let canonical = owner.clone();
    assert!(matches!(
        expand_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin()),
        Err(SceneModifierExpandError::UnsupportedRenderMode { .. })
    ));
    assert_eq!(owner, canonical);
    owner
        .nodes
        .iter_mut()
        .find(|node| node.type_id == "node.render_scene")
        .unwrap()
        .params
        .clear();
    owner.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_mut()
        .unwrap()
        .scene_modifier
        .as_mut()
        .unwrap()
        .stages[0]
        .outputs[0]
        .endpoint = SceneEndpoint::Camera;
    assert!(matches!(
        expand_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin()),
        Err(SceneModifierExpandError::UnsupportedEndpoint { .. })
    ));
}
