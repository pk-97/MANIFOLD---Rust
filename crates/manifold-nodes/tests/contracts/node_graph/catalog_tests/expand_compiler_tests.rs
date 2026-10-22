use manifold_core::scene_modifier_preset::{SceneStageScope, SceneEndpoint, SceneModifierInstanceDef};
use manifold_node_engine::load::expand::PreparedSceneModifierGraph;
use std::collections::BTreeSet;
use manifold_node_engine::load::expand::*;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphWire, SerializedParamValue};
use manifold_core::NodeId;
use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};
use manifold_node_engine::load::expand::SceneModifierExpandError;
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
    // Low-level preparation tests receive a typed authoring document directly;
    // model the normal project/runtime load contract before route discovery.
    manifold_core::phong_migration::migrate_phong_to_pbr(&mut owner);
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
        impulses: vec![],
        initializers: vec![],
        calibrations: vec![],
        shatter: None,
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
        scene: NodeId::new("scan_render"),
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: vec![],
        legacy_math_view_carrier: None,
        graph: Box::new(recipe),
    });
    owner
}

#[test]
fn scene_modifier_capacity_splits_stage_carriers_and_math_views() {
    let registry = PrimitiveRegistry::with_builtin();
    // 17 stage-carrying modifiers still exceed the stage cap (BUG-ty86 keeps
    // the historic rule for them).
    let mut owner = fixture();
    let template = owner.scene_modifiers[0].clone();
    for i in 0..16 {
        let mut instance = template.clone();
        instance.id = NodeId::new(format!("modifier_{i}"));
        owner.scene_modifiers.push(instance);
    }
    assert_eq!(owner.scene_modifiers.len(), 17);
    assert!(
        matches!(
            prepare_scene_modifiers(&owner, &registry),
            Err(SceneModifierExpandError::CapacityExceeded { .. })
        ),
        "17 stage-carrying modifiers are rejected"
    );
    // One carrier plus 16 Math View instances is 17 total and prepares: the
    // views are bounded by their own cap, not the stage-carrier count.
    let mut owner = math_view_fixture();
    let view_template = owner.scene_modifiers[1].clone();
    for i in 0..15 {
        let mut instance = view_template.clone();
        instance.id = NodeId::new(format!("math_view_{i}"));
        owner.scene_modifiers.push(instance);
    }
    assert_eq!(owner.scene_modifiers.len(), 17);
    prepare_scene_modifiers(&owner, &registry)
        .expect("16 views plus one stage carrier prepare under the split cap");
    // A 17th view exceeds the view cap.
    let mut extra = view_template.clone();
    extra.id = NodeId::new("math_view_overflow");
    owner.scene_modifiers.push(extra);
    assert!(
        matches!(
            prepare_scene_modifiers(&owner, &registry),
            Err(SceneModifierExpandError::CapacityExceeded { .. })
        ),
        "17 Math View instances are rejected"
    );
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
    use manifold_node_engine::parameters::ParamValue;
    let registry = PrimitiveRegistry::with_builtin();
    for fused_mode in [false, true] {
        let mut owner = fusion_fixture();
        let original = owner.clone();
        let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
        let mut runtime = manifold_node_engine::runtime::PresetRuntime::from_def_for_render(
            owner.clone(),
            &registry,
            None,
            fused_mode,
        )
        .unwrap();
        assert!(runtime.graph.modifier_buffer_budget().is_some());
        let local = NodeId::new("shear_x");
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
            .find(|node| node.node_id == *local)
            .unwrap()
            .params
            .insert(
                "amplitude".into(),
                SerializedParamValue::Float { value: 0.27 },
            );
        runtime.apply_inner_param_overrides(&owner);
        let fused = fused_mode.then(|| {
            manifold_node_engine::freeze::install::fused_generator_view_for(&prepared.def).unwrap()
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
    let graph = fusion_fixture().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).unwrap();
    assert!(
        graph.modifier_buffer_budget().is_some(),
        "direct host graph loads retain admission metadata too"
    );
}

#[test]
fn scene_modifier_expand_runtime_accepts_ray_tracing_enabled_by_live_manifest() {
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
        node_id: owner.scene_modifiers[0].scene.clone(),
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
    let runtime = manifold_node_engine::runtime::PresetRuntime::from_def(owner, &registry, Some(&manifest))
        .expect("live RT toggle remains admissible");
    let scene = runtime
        .graph
        .instance_by_node_id(&NodeId::new("scan_render"))
        .expect("render scene");
    assert_eq!(
        runtime.graph.get_node(scene).unwrap().params.get("rt_enabled"),
        Some(&manifold_node_engine::parameters::ParamValue::Bool(true))
    );
}

#[test]
fn scene_modifier_expand_cached_values_reach_copies_and_restore_first_edit() {
    use manifold_node_engine::parameters::ParamValue;
    use manifold_node_engine::load::expand::PreparedGraphValueWrites;
    let mut owner = fixture();
    let registry = PrimitiveRegistry::with_builtin();
    let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
    let route = prepared
        .routes
        .iter()
        .find(|route| route.local.as_str() == "shear_x")
        .unwrap();
    assert_eq!(route.copies.len(), 2);
    let mut graph = prepared.def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).unwrap();
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
    use manifold_node_engine::parameters::ParamValue;
    use manifold_node_engine::load::expand::PreparedGraphValueWrites;
    let mut owner = fusion_fixture();
    let registry = PrimitiveRegistry::with_builtin();
    let prepared = prepare_scene_modifiers(&owner, &registry).unwrap();
    let fused = manifold_node_engine::freeze::install::fused_generator_view_for(&prepared.def)
        .expect("existing elastic mesh atoms fuse");
    let mut graph = (*fused.def).clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).unwrap();
    use manifold_node_engine::exec::resource_allocation::plan_array_allocations;
    use manifold_node_engine::load::expand::PreparedModifierBufferBudget;
    let allocation = plan_array_allocations(
        &graph,
        &manifold_node_engine::exec::execution_plan::compile(&graph).unwrap(),
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
    let unfused_graph = prepared.def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).unwrap();
    let unfused_allocation = plan_array_allocations(
        &unfused_graph,
        &manifold_node_engine::exec::execution_plan::compile(&unfused_graph).unwrap(),
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
        .find(|route| route.local.as_str() == "shear_x")
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
        manifold_node_engine::freeze::fusion_report::fusion_report(&owner, &registry),
        manifold_node_engine::freeze::fusion_report::fusion_report(&expanded, &registry),
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
    use manifold_node_engine::exec::bound_graph::BoundGraph;
    use manifold_node_engine::param_binding::{BindingSource, ResolvedBinding, ResolvedTarget};
    use manifold_node_engine::parameters::ParamValue;
    use manifold_core::params::{Param, ParamManifest};
    let mut graph = expanded.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).unwrap();
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
    let writes = manifold_node_engine::load::expand::PreparedGraphValueWrites::prepare(
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
fn scene_modifier_expand_compiler_accepts_authored_rt_and_rejects_invalid_endpoint() {
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
    assert!(expand_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin()).is_ok());
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
    crate::contracts::node_graph::catalog_tests::math_view_fixtures::test_owner()
}

fn prepare_parent(owner: &EffectGraphDef) -> PreparedSceneModifierGraph {
    prepare_scene_modifiers(owner, &PrimitiveRegistry::with_builtin()).unwrap()
}

fn node_by_id(
    prepared: &PreparedSceneModifierGraph,
    id: u32,
) -> &manifold_core::effect_graph_def::EffectGraphNode {
    prepared.def.nodes.iter().find(|node| node.id == id).unwrap()
}

fn diagram_input_source(
    prepared: &PreparedSceneModifierGraph,
    diagram_id: u32,
    port: &str,
) -> Option<(u32, String)> {
    prepared
        .def
        .wires
        .iter()
        .find(|wire| wire.to_node == diagram_id && wire.to_port == port)
        .map(|wire| (wire.from_node, wire.from_port.clone()))
}

fn math_diagrams(prepared: &PreparedSceneModifierGraph) -> Vec<u32> {
    prepared
        .def
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.render_mesh_diagram")
        .map(|node| node.id)
        .collect()
}

/// The real mesh a diagram's sampled port reads: diagram port <- sampler <- mesh.
fn sampled_mesh(prepared: &PreparedSceneModifierGraph, diagram: u32, port: &str) -> (u32, String) {
    let (sampler, _) = diagram_input_source(prepared, diagram, port).unwrap();
    assert_eq!(node_by_id(prepared, sampler).type_id, "node.sample_mesh_triangles");
    diagram_input_source(prepared, sampler, "in").unwrap()
}

fn object_vertices_source(
    prepared: &PreparedSceneModifierGraph,
    owner: &EffectGraphDef,
    frame: usize,
) -> (u32, String) {
    let target = &owner.scene_modifiers[0].mesh_frames[frame].target;
    let object = prepared
        .def
        .nodes
        .iter()
        // Prepared parents flatten groups; object ids are unique in the fixture.
        .find(|node| node.type_id == "node.scene_object" && node.node_id == *target)
        .expect("scene object for frame")
        .id;
    diagram_input_source(prepared, object, "vertices").expect("object vertices wire")
}

fn stage_node_count(prepared: &PreparedSceneModifierGraph, type_id: &str) -> usize {
    prepared.def.nodes.iter().filter(|node| node.type_id == type_id).count()
}

#[test]
fn scene_modifier_math_view_builds_in_the_parent_graph_with_stable_identities() {
    let owner = math_view_fixture();
    let prepared = prepare_parent(&owner);
    let modifier = &owner.scene_modifiers[1];
    for role in ["diagram", "current_samples", "reference_samples"] {
        let expected: std::collections::HashSet<_> = modifier
            .mesh_frames
            .iter()
            .map(|frame| manifold_node_engine::load::expand::testkit::resource_node_id(&modifier.id, &frame.target, role))
            .collect();
        let actual: std::collections::HashSet<_> = prepared
            .def
            .nodes
            .iter()
            .filter(|node| expected.contains(&node.node_id))
            .map(|node| node.node_id.clone())
            .collect();
        assert_eq!(actual, expected, "Math View {role} identities are stable");
    }
    assert_eq!(math_diagrams(&prepared).len(), 2, "one diagram per sampled object");
    for retired in ["system.mesh_input", "system.mesh_output", "node.set_alpha"] {
        assert_eq!(stage_node_count(&prepared, retired), 0, "no {retired} in the parent graph");
    }
    assert!(
        prepared.def.wires.iter().all(|wire| wire.to_port != "surface_depth"),
        "the surface-depth chain is gone"
    );

    // final_output <- switch(selector = mode x enabled, [scene, diagrams, over]).
    let plain = prepare_parent(&crate::contracts::node_graph::catalog_tests::math_view_fixtures::without_view(&owner));
    let final_of = |prepared: &PreparedSceneModifierGraph| {
        let id = prepared.def.nodes.iter().find(|node| node.type_id == "system.final_output").unwrap().id;
        diagram_input_source(prepared, id, "in").unwrap()
    };
    let (switch, _) = final_of(&prepared);
    assert_eq!(node_by_id(&prepared, switch).type_id, "node.switch_texture");
    let scene_image = final_of(&plain);
    let in_0 = diagram_input_source(&prepared, switch, "in_0").unwrap();
    assert_eq!(
        node_by_id(&prepared, in_0.0).node_id,
        node_by_id(&plain, scene_image.0).node_id,
        "Scene mode selects the image that fed final_output before the view"
    );
    let (math, _) = diagram_input_source(&prepared, switch, "in_1").unwrap();
    assert_eq!(node_by_id(&prepared, math).type_id, "node.over", "two diagrams stack with over");
    let (overlay, _) = diagram_input_source(&prepared, switch, "in_2").unwrap();
    assert_eq!(node_by_id(&prepared, overlay).type_id, "node.over");
    assert_eq!(diagram_input_source(&prepared, overlay, "top").unwrap().0, math);
    assert_eq!(diagram_input_source(&prepared, overlay, "bottom").unwrap(), in_0);
    let (selector, _) = diagram_input_source(&prepared, switch, "selector").unwrap();
    assert_eq!(node_by_id(&prepared, selector).type_id, "node.math");
    let mode = diagram_input_source(&prepared, math_diagrams(&prepared)[0], "mode");
    assert_eq!(diagram_input_source(&prepared, selector, "a"), mode, "selector a is the mode control");
    let (enabled, _) = diagram_input_source(&prepared, selector, "b").unwrap();
    assert_eq!(node_by_id(&prepared, enabled).type_id, "node.value", "selector b is the enabled toggle");
    assert_ne!(Some(enabled), mode.map(|(id, _)| id));

    // The scene's own depth texture feeds every diagram directly.
    let scene = prepared
        .def
        .nodes
        .iter()
        .find(|node| node.node_id == modifier.scene)
        .unwrap()
        .id;
    for diagram in math_diagrams(&prepared) {
        assert_eq!(
            diagram_input_source(&prepared, diagram, "scene_depth"),
            Some((scene, "depth".to_string()))
        );
    }
}

#[test]
fn scene_modifier_math_view_routes_one_shared_grid_control() {
    let prepared = prepare_parent(&math_view_fixture());
    let diagrams = math_diagrams(&prepared);
    assert_eq!(diagrams.len(), 2);
    assert!(diagrams.iter().all(|diagram| matches!(
        node_by_id(&prepared, *diagram).params.get("grid"),
        Some(SerializedParamValue::Bool { value: false })
    )));
    let grid_wires = prepared
        .def
        .wires
        .iter()
        .filter(|wire| diagrams.contains(&wire.to_node) && wire.to_port == "grid")
        .count();
    assert_eq!(grid_wires, 1);
}

#[test]
fn scene_modifier_math_view_diagrams_share_camera_and_controls() {
    let prepared = prepare_parent(&math_view_fixture());
    let diagrams = math_diagrams(&prepared);
    for diagram in &diagrams {
        for port in [
            "current", "reference", "incoming", "current_mesh", "reference_mesh", "camera",
            "transform", "scene_depth", "mode", "occlusion", "axes",
        ] {
            assert!(
                diagram_input_source(&prepared, *diagram, port).is_some(),
                "{} is missing {port}",
                node_by_id(&prepared, *diagram).node_id
            );
        }
        assert_eq!(
            diagram_input_source(&prepared, *diagram, "incoming"),
            diagram_input_source(&prepared, *diagram, "reference"),
            "arrow tails read the reference samples"
        );
    }
    let camera = |diagram| diagram_input_source(&prepared, diagram, "camera");
    assert_eq!(camera(diagrams[0]), camera(diagrams[1]), "one scene camera");
}

#[test]
fn scene_modifier_math_view_reads_the_real_mesh_at_its_position_without_rerunning_the_chain() {
    // vortex_a -> vortex_b -> math_view. The diagram samples the parent's own
    // producers: current is the real full mesh after vortex_b (what the scene
    // object draws), reference is the object's real mesh wire at chain start.
    // The preceding chain is not evaluated a second time: the patch stage
    // count matches the same chain without the view.
    let owner = math_view_fixture();
    let mut vortex_b = owner.scene_modifiers[0].clone();
    vortex_b.id = NodeId::new("vortex_b");
    let mut owner =
        manifold_nodes_scene::node_graph::scene_modifier_authoring::attach_scene_modifier(&owner, vortex_b).unwrap();
    let vortex_b = owner.scene_modifiers.pop().unwrap();
    owner.scene_modifiers.insert(1, vortex_b);
    let prepared = prepare_parent(&owner);
    let plain = prepare_parent(&crate::contracts::node_graph::catalog_tests::math_view_fixtures::without_view(&owner));
    assert_eq!(
        stage_node_count(&prepared, "node.transform_mesh_patches"),
        stage_node_count(&plain, "node.transform_mesh_patches"),
        "Math View adds no second evaluation of the preceding chain"
    );
    let diagrams = math_diagrams(&prepared);
    for (frame, diagram) in diagrams.iter().enumerate() {
        let current = sampled_mesh(&prepared, *diagram, "current");
        assert_eq!(
            current,
            object_vertices_source(&prepared, &owner, frame),
            "current samples the producer the scene object itself draws"
        );
        assert_eq!(
            node_by_id(&prepared, current.0).type_id,
            "node.morph_mesh",
            "current is the last preceding modifier's real output"
        );
        assert_eq!(diagram_input_source(&prepared, *diagram, "current_mesh"), Some(current.clone()));
        let reference = sampled_mesh(&prepared, *diagram, "reference");
        assert_ne!(reference, current, "the reference is the undeformed mesh");
        assert_eq!(diagram_input_source(&prepared, *diagram, "reference_mesh"), Some(reference));
    }
}

#[test]
fn scene_modifier_math_view_first_in_chain_reads_the_host_mesh_and_ignores_later_modifiers() {
    // BUG-jvn5 (post-view leak): with the view first, every later modifier
    // stays out of the diagram — current and reference both sample the
    // object's real host mesh wire, and no instances are wired.
    let mut owner = math_view_fixture();
    let view = owner.scene_modifiers.remove(1);
    owner.scene_modifiers.insert(0, view);
    let prepared = prepare_parent(&owner);
    let plain = prepare_parent(&crate::contracts::node_graph::catalog_tests::math_view_fixtures::without_view(&owner));
    for diagram in math_diagrams(&prepared) {
        let current = sampled_mesh(&prepared, diagram, "current");
        let reference = sampled_mesh(&prepared, diagram, "reference");
        assert_eq!(current, reference, "nothing precedes the view");
        assert_ne!(
            node_by_id(&prepared, current.0).type_id,
            "node.transform_mesh_patches",
            "the post-view vortex never reaches the diagram"
        );
        assert!(
            diagram_input_source(&prepared, diagram, "instances").is_none(),
            "post-view instances never reach the diagram"
        );
    }
    // The post-view modifier still drives the scene exactly as before.
    assert_eq!(
        stage_node_count(&prepared, "node.transform_mesh_patches"),
        stage_node_count(&plain, "node.transform_mesh_patches")
    );
}

#[test]
fn scene_modifier_math_view_captures_preceding_instance_echoes() {
    // BUG-uvts: SpatialEchoes writes only SceneEndpoint::Instances. The view
    // wires that producer; the vertices-only fixture leaves the port unwired.
    let owner = crate::contracts::node_graph::catalog_tests::math_view_fixtures::test_owner_with_instance_echoes();
    let prepared = prepare_parent(&owner);
    for diagram in math_diagrams(&prepared) {
        let instances = diagram_input_source(&prepared, diagram, "instances")
            .expect("echo chain: instances input is wired");
        let current = sampled_mesh(&prepared, diagram, "current");
        assert_ne!(instances.0, current.0, "each endpoint reads its own producer");
        assert_ne!(
            node_by_id(&prepared, instances.0).type_id,
            "node.arrange_copies",
            "instances come from the echo stage, not the identity fallback"
        );
    }
    let plain = prepare_parent(&math_view_fixture());
    for diagram in math_diagrams(&plain) {
        assert!(
            diagram_input_source(&plain, diagram, "instances").is_none(),
            "vertices-only chain keeps the instances port unwired"
        );
    }
}

#[test]
fn scene_modifier_math_view_follows_the_final_scene_camera_past_the_view() {
    // A post-view modifier that writes Camera (SceneLoop's lens stage) still
    // frames the diagram; its instance copies are post-view and stay out.
    let owner = math_view_fixture();
    let loop_recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/SceneLoop.json"
    )))
    .unwrap();
    let scene = owner.scene_modifiers[1].scene.clone();
    let owner = manifold_nodes_scene::node_graph::scene_modifier_authoring::attach_scene_modifier(&owner, SceneModifierInstanceDef {
        id: NodeId::new("scene_loop"),
        scene,
        targets: SceneTargetSelection::AllObjects,
        // SceneLoop consumes no mesh coordinate context, so saved frames are
        // rejected for it; its EachObject stage resolves targets directly.
        mesh_frames: Vec::new(),
        legacy_math_view_carrier: None,
        graph: Box::new(loop_recipe),
    })
    .unwrap();
    let prepared = prepare_parent(&owner);
    let plain = prepare_parent(&math_view_fixture());
    let diagram = math_diagrams(&prepared)[0];
    let plain_diagram = math_diagrams(&plain)[0];
    let (camera, _) = diagram_input_source(&prepared, diagram, "camera").unwrap();
    let (plain_camera, _) = diagram_input_source(&plain, plain_diagram, "camera").unwrap();
    assert_ne!(
        node_by_id(&prepared, camera).node_id,
        node_by_id(&plain, plain_camera).node_id,
        "post-view camera writes keep framing the diagram (final scene camera)"
    );
    assert!(
        diagram_input_source(&prepared, diagram, "instances").is_none(),
        "post-view instance writes stay out of the capture"
    );
}

#[test]
fn scene_modifier_math_view_that_cannot_resolve_never_fails_the_layer() {
    // The view's enabled toggle is missing: the view is skipped with a
    // warning, the layer prepares, and final_output keeps the scene image.
    let mut owner = math_view_fixture();
    let view = &mut owner.scene_modifiers[1].graph;
    let renamed = NodeId::new("mv_toggle");
    for node in &mut view.nodes {
        if node.node_id.as_str() == "mv_enabled" {
            node.node_id = renamed.clone();
        }
    }
    for binding in &mut view.preset_metadata.as_mut().unwrap().bindings {
        if let manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } = &mut binding.target
            && node_id.as_str() == "mv_enabled"
        {
            *node_id = renamed.clone();
        }
    }
    let prepared = prepare_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin())
        .expect("a Math View problem never fails the layer");
    assert!(math_diagrams(&prepared).is_empty());
    assert_eq!(stage_node_count(&prepared, "node.switch_texture"), stage_node_count(
        &prepare_parent(&crate::contracts::node_graph::catalog_tests::math_view_fixtures::without_view(&owner)), "node.switch_texture"));
}
