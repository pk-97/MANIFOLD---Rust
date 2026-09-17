use super::*;
use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::scene_modifier_preset::{
    SceneModifierRecipe, SceneModifierStageDef, SceneStageInput, SceneStageOutput,
    SceneStageSource, SceneTargetSelection,
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
            inputs: vec![
                SceneStageInput {
                    port: "current".into(),
                    source: SceneStageSource::Previous {
                        endpoint: SceneEndpoint::Vertices,
                    },
                },
                SceneStageInput {
                    port: "reference".into(),
                    source: SceneStageSource::Reference {
                        endpoint: SceneEndpoint::Vertices,
                    },
                },
            ],
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

pub(super) fn fusion_fixture() -> EffectGraphDef {
    let mut owner = fixture();
    let modifier = owner.scene_modifiers.first_mut().expect("fixture modifier");

    {
        let metadata = modifier
            .graph
            .preset_metadata
            .as_mut()
            .expect("recipe metadata");
        metadata
            .params
            .retain(|param| !param.id.starts_with("mask_"));
        metadata
            .bindings
            .retain(|binding| !binding.id.starts_with("mask_"));
        let recipe = metadata.scene_modifier.as_mut().expect("fixture recipe");
        for stage in &mut recipe.stages {
            stage.inputs.retain(|input| input.port != "reference");
        }
    }

    let group = modifier
        .graph
        .nodes
        .iter_mut()
        .find(|node| node.node_id == NodeId::new("elastic_stage"))
        .and_then(|node| node.group.as_mut())
        .expect("fixture stage group");
    group
        .interface
        .inputs
        .retain(|port| port.name != "reference");
    let removed_ids: Vec<u32> = group
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.node_id.as_str(),
                "group_reference" | "mask" | "mask_morph"
            )
        })
        .map(|node| node.id)
        .collect();
    assert_eq!(removed_ids.len(), 3, "fusion fixture mask topology changed");
    group.nodes.retain(|node| !removed_ids.contains(&node.id));
    assert!(group.nodes.iter().all(|node| {
        !matches!(
            node.node_id.as_str(),
            "group_reference" | "mask" | "mask_morph"
        )
    }));
    group.wires.retain(|wire| {
        !removed_ids.contains(&wire.from_node) && !removed_ids.contains(&wire.to_node)
    });
    let shear_z_id = group
        .nodes
        .iter()
        .find(|node| node.node_id == NodeId::new("shear_z"))
        .expect("fixture cross bend")
        .id;
    let output_id = group
        .nodes
        .iter()
        .find(|node| node.node_id == NodeId::new("group_output"))
        .expect("fixture group output")
        .id;
    // The production gather mask recipe may remain unfused; these tests use
    // the same-topology pointwise mesh path to exercise fusion routing.
    group
        .wires
        .retain(|wire| wire.to_node != output_id || wire.to_port != "vertices");
    group.wires.push(EffectGraphWire {
        from_node: shear_z_id,
        from_port: "out".into(),
        to_node: output_id,
        to_port: "vertices".into(),
    });
    owner
}

#[test]
fn scene_modifier_expand_runtime_loads_canonical_in_watched_and_fused_modes() {
    use crate::node_graph::parameters::ParamValue;
    let registry = PrimitiveRegistry::with_builtin();
    for fused_mode in [false, true] {
        let mut owner = fusion_fixture();
        let original = owner.clone();
        let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
        let mut runtime = crate::preset_runtime::PresetRuntime::from_def_for_render(
            owner.clone(),
            &registry,
            None,
            fused_mode,
        )
        .unwrap();
        assert!(runtime.graph.modifier_buffer_budget().is_some());
        let local = SceneNodeRef {
            scope: vec![NodeId::new("elastic_stage")],
            node: NodeId::new("shear_x"),
        };
        let copies = runtime
            .modifier_node_copies(&owner.scene_modifiers[0].id, &local)
            .unwrap()
            .to_vec();
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().all(|copy| copy.object.is_some()));
        owner.scene_modifiers[0].graph.nodes[0]
            .group
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|node| node.node_id == local.node)
            .unwrap()
            .params
            .insert(
                "amplitude".into(),
                SerializedParamValue::Float { value: 0.27 },
            );
        runtime.apply_inner_param_overrides(&owner);
        let fused = fused_mode.then(|| {
            crate::node_graph::freeze::install::fused_generator_view_for(&prepared.def).unwrap()
        });
        for copy in copies {
            let (target, param) = match &fused {
                Some(view) => view
                    .retarget
                    .get(&(copy.node_id.to_string(), "amplitude".into()))
                    .unwrap()
                    .clone(),
                None => (copy.node_id, "amplitude".into()),
            };
            let id = runtime.graph.instance_by_node_id(&target).unwrap();
            assert_eq!(
                runtime
                    .graph
                    .get_node(id)
                    .unwrap()
                    .params
                    .get(param.as_str()),
                Some(&ParamValue::Float(0.27))
            );
        }
        assert_eq!(
            original,
            fusion_fixture(),
            "loading never mutates the canonical snapshot"
        );
    }
    let graph = fusion_fixture().into_graph(&registry).unwrap();
    assert!(
        graph.modifier_buffer_budget().is_some(),
        "direct host graph loads retain admission metadata too"
    );
}

#[test]
fn scene_modifier_expand_runtime_rejects_ray_tracing_enabled_by_live_manifest() {
    use manifold_core::params::{Param, ParamManifest};
    let mut owner = fixture();
    let local = owner.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_ref()
        .unwrap();
    let mut spec = local.params[0].clone();
    spec.id = "rt_test".into();
    spec.default_value = 0.0;
    spec.min = 0.0;
    spec.max = 1.0;
    spec.is_toggle = true;
    let mut binding = local.bindings[0].clone();
    binding.id = spec.id.clone();
    binding.default_value = 0.0;
    binding.convert = manifold_core::effects::ParamConvert::BoolThreshold;
    binding.target = BindingTarget::Node {
        node_id: owner.scene_modifiers[0].scene.node.clone(),
        param: "rt_enabled".into(),
    };
    owner
        .preset_metadata
        .as_mut()
        .unwrap()
        .params
        .push(spec.clone());
    owner
        .preset_metadata
        .as_mut()
        .unwrap()
        .bindings
        .push(binding);
    let mut parameter = Param::bundled(spec);
    parameter.value = 1.0;
    let manifest = ParamManifest::from_params(vec![parameter]);
    let registry = PrimitiveRegistry::with_builtin();
    assert!(
        prepare_scene_modifiers(&owner, &registry).is_ok(),
        "authored RT default is off"
    );
    let result = crate::preset_runtime::PresetRuntime::from_def(owner, &registry, Some(&manifest));
    assert!(matches!(
        result,
        Err(
            crate::preset_runtime::JsonGeneratorLoadError::SceneModifier(
                SceneModifierExpandError::UnsupportedRenderMode { .. }
            )
        )
    ));
}

#[test]
fn scene_modifier_expand_cached_values_reach_copies_and_restore_first_edit() {
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::scene_modifier_expand::PreparedGraphValueWrites;
    let mut owner = fixture();
    let registry = PrimitiveRegistry::with_builtin();
    let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
    let route = prepared
        .routes
        .iter()
        .find(|route| route.local.node.as_str() == "shear_x")
        .unwrap();
    assert_eq!(route.copies.len(), 2);
    let mut graph = prepared.def.clone().into_graph(&registry).unwrap();
    let writes = PreparedGraphValueWrites::prepare(
        &owner,
        &prepared.routes,
        &graph,
        &ahash::AHashMap::default(),
    )
    .unwrap();
    let baseline: Vec<_> = route
        .copies
        .iter()
        .map(|copy| {
            let id = graph.instance_by_node_id(&copy.node_id).unwrap();
            (
                id,
                graph
                    .get_node(id)
                    .unwrap()
                    .params
                    .get("amplitude")
                    .unwrap()
                    .clone(),
            )
        })
        .collect();
    let leaf = owner.scene_modifiers[0].graph.nodes[0]
        .group
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "shear_x")
        .unwrap();
    assert!(!leaf.params.contains_key("amplitude"));
    leaf.params.insert(
        "amplitude".into(),
        SerializedParamValue::Float { value: 0.37 },
    );
    writes.apply(&owner, &mut graph).unwrap();
    for (id, _) in &baseline {
        assert_eq!(
            graph.get_node(*id).unwrap().params.get("amplitude"),
            Some(&ParamValue::Float(0.37))
        );
    }
    owner.scene_modifiers[0].graph.nodes[0]
        .group
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "shear_x")
        .unwrap()
        .params
        .remove("amplitude");
    writes.apply(&owner, &mut graph).unwrap();
    for (id, value) in &baseline {
        assert_eq!(
            graph.get_node(*id).unwrap().params.get("amplitude"),
            Some(value)
        );
    }
    // A stale structural path is refused before any leaf can be changed.
    owner.scene_modifiers[0].graph.nodes[0]
        .group
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "shear_x")
        .unwrap()
        .node_id = NodeId::new("replacement");
    assert!(writes.apply(&owner, &mut graph).is_err());
    for (id, value) in &baseline {
        assert_eq!(
            graph.get_node(*id).unwrap().params.get("amplitude"),
            Some(value)
        );
    }
}

#[test]
fn scene_modifier_expand_cached_values_follow_fused_mesh_uniforms() {
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::scene_modifier_expand::PreparedGraphValueWrites;
    let mut owner = fusion_fixture();
    let registry = PrimitiveRegistry::with_builtin();
    let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
    let fused = crate::node_graph::freeze::install::fused_generator_view_for(&prepared.def)
        .expect("existing elastic mesh atoms fuse");
    let mut graph = (*fused.def).clone().into_graph(&registry).unwrap();
    use crate::node_graph::resource_allocation::plan_array_allocations;
    use crate::node_graph::scene_modifier_expand::PreparedModifierBufferBudget;
    let allocation = plan_array_allocations(
        &graph,
        &crate::node_graph::compile(&graph).unwrap(),
        (1024, 1024),
        &ahash::AHashMap::default(),
    )
    .unwrap();
    let budget = PreparedModifierBufferBudget::prepare(
        &owner,
        &prepared.routes,
        &graph,
        &fused.node_retarget,
    )
    .unwrap();
    let fused_usage = budget.account(&allocation).unwrap();
    let scene = &owner.scene_modifiers[0].scene;
    assert!(fused_usage.modifier_bytes[scene] > 0);
    let unfused_graph = prepared.def.clone().into_graph(&registry).unwrap();
    let unfused_allocation = plan_array_allocations(
        &unfused_graph,
        &crate::node_graph::compile(&unfused_graph).unwrap(),
        (1024, 1024),
        &ahash::AHashMap::default(),
    )
    .unwrap();
    let unfused_budget = PreparedModifierBufferBudget::prepare(
        &owner,
        &prepared.routes,
        &unfused_graph,
        &ahash::AHashMap::default(),
    )
    .unwrap();
    let unfused_usage = unfused_budget.account(&unfused_allocation).unwrap();
    assert!(fused_usage.modifier_bytes[scene] <= unfused_usage.modifier_bytes[scene]);
    assert!(
        unfused_usage.baseline_bytes > 0,
        "host source buffers reported separately"
    );
    let writes =
        PreparedGraphValueWrites::prepare(&owner, &prepared.routes, &graph, &fused.retarget)
            .unwrap();
    let leaf = owner.scene_modifiers[0].graph.nodes[0]
        .group
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "shear_x")
        .unwrap();
    leaf.params.insert(
        "amplitude".into(),
        SerializedParamValue::Float { value: 0.41 },
    );
    leaf.params
        .insert("axis".into(), SerializedParamValue::Enum { value: 1 });
    writes.apply(&owner, &mut graph).unwrap();
    let route = prepared
        .routes
        .iter()
        .find(|route| route.local.node.as_str() == "shear_x")
        .unwrap();
    for copy in &route.copies {
        for (param, value) in [("amplitude", 0.41), ("axis", 1.0)] {
            let (target, field) = fused
                .retarget
                .get(&(copy.node_id.to_string(), param.into()))
                .expect("mesh parameter has a fused uniform route");
            let id = graph.instance_by_node_id(target).unwrap();
            assert_eq!(
                graph.get_node(id).unwrap().params.get(field.as_str()),
                Some(&ParamValue::Float(value))
            );
        }
    }
}

#[test]
fn scene_modifier_expand_compiler_attaches_preserves_host_and_is_idempotent() {
    let owner = fixture();
    let canonical = owner.clone();
    let registry = PrimitiveRegistry::with_builtin();
    let expanded = expand_scene_modifiers(&owner, &registry).unwrap();
    assert_eq!(
        crate::node_graph::freeze::fusion_report::fusion_report(&owner, &registry),
        crate::node_graph::freeze::fusion_report::fusion_report(&expanded, &registry),
        "diagnostics inspect the same prepared graph as rendering"
    );
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
    let registry = PrimitiveRegistry::with_builtin();
    let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
    let expanded = &prepared.def;
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
    use crate::node_graph::bound_graph::BoundGraph;
    use crate::node_graph::param_binding::{BindingSource, ResolvedBinding, ResolvedTarget};
    use crate::node_graph::parameters::ParamValue;
    use manifold_core::params::{Param, ParamManifest};
    let mut graph = expanded.clone().into_graph(&registry).unwrap();
    let resolved = bindings
        .iter()
        .map(|binding| {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                panic!("leaf binding")
            };
            ResolvedBinding::assemble(
                binding.id.clone().into(),
                binding.label.clone().into(),
                binding.default_value,
                ResolvedTarget::Node {
                    node: graph.instance_by_node_id(node_id).unwrap(),
                    param: param.clone().into(),
                },
                binding.convert,
                BindingSource::Static,
                binding.id.clone().into(),
                None,
                false,
                false,
            )
        })
        .collect();
    let mut bound = BoundGraph::new(resolved, &mut graph, Some(expanded));
    let writes = crate::node_graph::scene_modifier_expand::PreparedGraphValueWrites::prepare(
        &owner,
        &prepared.routes,
        &graph,
        &ahash::AHashMap::default(),
    )
    .unwrap();
    bound
        .install_prepared_routes(writes, prepared.binding_sources)
        .unwrap();
    let mut parameter = Param::bundled(owner.preset_metadata.as_ref().unwrap().params[0].clone());
    parameter.value = 0.25;
    let manifest = ParamManifest::from_params(vec![parameter]);
    owner.preset_metadata.as_mut().unwrap().bindings[0].scale = 0.5;
    owner.preset_metadata.as_mut().unwrap().bindings[0].offset = 0.125;
    let local = owner.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_mut()
        .unwrap()
        .bindings
        .iter_mut()
        .find(|binding| binding.id == "bend")
        .unwrap();
    local.scale = 2.0;
    local.offset = 0.25;
    bound.rebake_reshapes(&manifest, Some(&owner));
    bound.apply(&mut graph, &manifest);
    // The cached inner-edit path resets leaf values, then unchanged live
    // controls must reassert their composed mapping on the next apply.
    bound.apply_inner_overrides(&mut graph, &[], Some(&owner));
    bound.apply(&mut graph, &manifest);
    for binding in &bound.bindings {
        let ResolvedTarget::Node { node, param } = &binding.target else {
            panic!("leaf binding")
        };
        assert_eq!(
            graph.get_node(*node).unwrap().params.get(param.as_ref()),
            Some(&ParamValue::Float(0.75))
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

fn math_view_fixture() -> EffectGraphDef {
    super::super::math_view::test_owner()
}

#[test]
fn scene_modifier_math_view_is_sparse_and_cuts_final_output_at_requested_stage() {
    let owner = math_view_fixture();
    let registry = PrimitiveRegistry::with_builtin();
    let prepared = prepare_scene_modifier_math_view(
        &owner,
        &registry,
        &NodeId::new("vortex_math_view"),
        MathViewScope::ThisModifier,
    )
    .unwrap();

    assert_eq!(
        prepared
            .def
            .nodes
            .iter()
            .filter(|node| node.type_id == "system.mesh_input")
            .count(),
        2
    );
    assert_eq!(
        prepared
            .def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.render_mesh_diagram")
            .count(),
        4,
        "each captured object has a colour diagram and a depth surface"
    );
    let modifier = &owner.scene_modifiers[0];
    for role in ["diagram", "surface"] {
        let expected: std::collections::HashSet<_> = modifier
            .mesh_frames
            .iter()
            .map(|frame| super::math_events::resource_node_id(&modifier.id, &frame.target, role))
            .collect();
        let actual: std::collections::HashSet<_> = prepared
            .def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.render_mesh_diagram")
            .filter(|node| expected.contains(&node.node_id))
            .map(|node| node.node_id.clone())
            .collect();
        assert_eq!(actual, expected, "Math View {role} identities are stable");
    }
    let final_id = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.type_id == "system.final_output")
        .unwrap()
        .id;
    let final_source = prepared
        .def
        .wires
        .iter()
        .find(|wire| wire.to_node == final_id && wire.to_port == "in")
        .unwrap();
    assert_eq!(
        prepared
            .def
            .nodes
            .iter()
            .find(|node| node.id == final_source.from_node)
            .unwrap()
            .type_id,
        "node.set_alpha"
    );

    // The imported scene remains in the authoring snapshot, but is outside
    // the final-output dependency closure, so its full mesh/material inputs
    // cannot receive live execution-plan allocations.
    let mut live = BTreeSet::from([final_id]);
    let mut changed = true;
    while changed {
        changed = false;
        for wire in &prepared.def.wires {
            if live.contains(&wire.to_node) && live.insert(wire.from_node) {
                changed = true;
            }
        }
    }
    assert!(
        prepared
            .def
            .nodes
            .iter()
            .filter(|node| live.contains(&node.id))
            .all(|node| {
                node.type_id != "node.render_scene" && node.type_id != "node.gltf_mesh_source"
            })
    );
}

#[test]
fn scene_modifier_math_view_routes_one_shared_grid_control() {
    for scope in [MathViewScope::ThisModifier, MathViewScope::WithinChain] {
        let prepared = prepare_scene_modifier_math_view(
            &math_view_fixture(),
            &PrimitiveRegistry::with_builtin(),
            &NodeId::new("vortex_math_view"),
            scope,
        )
        .unwrap();
        let owner = math_view_fixture();
        let modifier = &owner.scene_modifiers[0];
        let diagram_ids: std::collections::HashSet<_> = modifier
            .mesh_frames
            .iter()
            .map(|frame| {
                super::math_events::resource_node_id(&modifier.id, &frame.target, "diagram")
            })
            .collect();
        let diagrams: Vec<_> = prepared
            .def
            .nodes
            .iter()
            .filter(|node| diagram_ids.contains(&node.node_id))
            .collect();
        assert_eq!(diagrams.len(), 2);

        assert!(diagrams.iter().all(|diagram| matches!(
            diagram.params.get("grid"),
            Some(SerializedParamValue::Bool { value: false })
        )));
        let grid_wires: Vec<_> = prepared
            .def
            .wires
            .iter()
            .filter(|wire| {
                diagrams.iter().any(|diagram| diagram.id == wire.to_node)
                    && wire.to_port == "grid"
            })
            .collect();
        assert_eq!(grid_wires.len(), 1);
    }
}

#[test]
fn scene_modifier_math_view_shares_depth_and_appearance_across_surfaces() {
    let owner = math_view_fixture();
    let modifier = &owner.scene_modifiers[0];
    let parent = prepare_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin()).unwrap();
    let scene_id = parent
        .def
        .nodes
        .iter()
        .find(|candidate| candidate.node_id == modifier.scene.node)
        .expect("parent render scene")
        .id;
    let parent_depth_sources: BTreeSet<_> = modifier
        .mesh_frames
        .iter()
        .map(|frame| {
            let export_id =
                super::math_events::resource_node_id(&modifier.id, &frame.target, "export");
            let export = parent
                .def
                .nodes
                .iter()
                .find(|candidate| candidate.node_id == export_id)
                .expect("generated parent mesh export")
                .id;
            parent
                .def
                .wires
                .iter()
                .find(|wire| wire.to_node == export && wire.to_port == "depth")
                .map(|wire| (wire.from_node, wire.from_port.clone()))
                .expect("parent scene depth reaches every mesh export")
        })
        .collect();
    assert_eq!(
        parent_depth_sources,
        BTreeSet::from([(scene_id, "depth".to_string())]),
        "all mesh exports borrow one parent scene depth"
    );
    let prepared = prepare_scene_modifier_math_view(
        &owner,
        &PrimitiveRegistry::with_builtin(),
        &NodeId::new("vortex_math_view"),
        MathViewScope::ThisModifier,
    )
    .unwrap();

    let frames: Vec<_> = modifier.mesh_frames.iter().collect();
    let surfaces: Vec<_> = frames
        .iter()
        .map(|frame| {
            let id = super::math_events::resource_node_id(&modifier.id, &frame.target, "surface");
            prepared
                .def
                .nodes
                .iter()
                .find(|candidate| candidate.node_id == id)
                .expect("generated Math View surface")
        })
        .collect();
    let diagrams: Vec<_> = frames
        .iter()
        .map(|frame| {
            let id = super::math_events::resource_node_id(&modifier.id, &frame.target, "diagram");
            prepared
                .def
                .nodes
                .iter()
                .find(|candidate| candidate.node_id == id)
                .expect("generated Math View diagram")
        })
        .collect();

    for candidate in surfaces.iter().chain(diagrams.iter()) {
        for port in [
            "current",
            "reference",
            "incoming",
            "camera",
            "transform",
            "mesh_weights",
            "scan_weights",
        ] {
            assert!(
                prepared
                    .def
                    .wires
                    .iter()
                    .any(|wire| wire.to_node == candidate.id && wire.to_port == port),
                "{} is missing shared {port} input",
                candidate.node_id
            );
        }
        for port in ["mode", "occlusion"] {
            assert!(
                prepared
                    .def
                    .wires
                    .iter()
                    .any(|wire| wire.to_node == candidate.id && wire.to_port == port),
                "{} is missing {port} control",
                candidate.node_id
            );
        }
    }
    for (surface, diagram) in surfaces.iter().zip(&diagrams) {
        for port in ["current", "reference", "incoming", "camera", "transform", "mesh_weights", "scan_weights"] {
            let source = |node| prepared.def.wires.iter()
                .find(|wire| wire.to_node == node && wire.to_port == port)
                .map(|wire| (wire.from_node, wire.from_port.as_str()));
            assert_eq!(source(surface.id), source(diagram.id), "surface and colour disagree on {port}");
        }
    }
    assert!(prepared.def.wires.iter().all(|wire| {
        !surfaces
            .iter()
            .any(|surface| surface.id == wire.to_node && wire.to_port == "grid")
    }));
    for candidate in surfaces.iter().chain(diagrams.iter()) {
        assert!(
            prepared
                .def
                .wires
                .iter()
                .any(|wire| wire.to_node == candidate.id && wire.to_port == "scene_depth"),
            "{} is missing borrowed scene depth",
            candidate.node_id
        );
    }
    for window in surfaces.windows(2) {
        assert!(prepared.def.wires.iter().any(|wire| {
            wire.from_node == window[0].id
                && wire.from_port == "depth"
                && wire.to_node == window[1].id
                && wire.to_port == "surface_depth"
        }));
    }
    let final_surface = surfaces.last().expect("surface depth accumulator");
    for diagram in &diagrams {
        assert!(prepared.def.wires.iter().any(|wire| {
            wire.from_node == final_surface.id
                && wire.from_port == "depth"
                && wire.to_node == diagram.id
                && wire.to_port == "surface_depth"
        }));
    }
    assert!(prepared.def.wires.iter().all(|wire| {
        !surfaces
            .iter()
            .any(|surface| surface.id == wire.from_node && wire.from_port == "color")
    }));
}

#[test]
fn scene_modifier_math_view_scope_changes_the_captured_incoming_route() {
    let mut owner = math_view_fixture();
    let mut earlier = owner.scene_modifiers[0].clone();
    earlier.id = NodeId::new("earlier_vortex");
    owner.scene_modifiers.insert(0, earlier);
    let requested = NodeId::new("vortex_math_view");
    let registry = PrimitiveRegistry::with_builtin();

    let isolated = prepare_scene_modifier_math_view(
        &owner,
        &registry,
        &requested,
        MathViewScope::ThisModifier,
    )
    .unwrap();
    let chain =
        prepare_scene_modifier_math_view(&owner, &registry, &requested, MathViewScope::WithinChain)
            .unwrap();

    fn incoming_source(prepared: &PreparedSceneModifierGraph) -> (u32, String) {
        let diagram = prepared
            .def
            .nodes
            .iter()
            .find(|node| node.type_id == "node.render_mesh_diagram")
            .unwrap();
        let wire = prepared
            .def
            .wires
            .iter()
            .find(|wire| wire.to_node == diagram.id && wire.to_port == "incoming")
            .unwrap();
        (
            wire.from_node,
            prepared
                .def
                .nodes
                .iter()
                .find(|node| node.id == wire.from_node)
                .unwrap()
                .type_id
                .clone(),
        )
    }

    let isolated_incoming = incoming_source(&isolated);
    let chain_incoming = incoming_source(&chain);
    assert_eq!(
        isolated_incoming.1, "system.mesh_input",
        "ThisModifier starts its incoming route at the sparse source"
    );
    assert_ne!(
        chain_incoming.0, isolated_incoming.0,
        "WithinChain includes the earlier modifier's vertex output"
    );
}
