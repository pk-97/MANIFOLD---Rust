//! Scene-modifier invariants through the v3 recipe and compiler seams.
//!
//! Descriptor trace and plan-shape assertions retired with the legacy factory.
//! These tests retain the meaningful gates: canonical insertion, expansion,
//! bypass state, shared binding identities, and complete inverse edits.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::params::{Param, ParamManifest};
use manifold_core::scene_modifier_edit::{delete_scene_modifier, insert_scene_modifier};
use manifold_core::scene_modifier_preset::SceneTargetSelection;
use manifold_core::NodeId;
use manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers;
use manifold_renderer::node_graph::{ParamValue, PrimitiveRegistry};
use manifold_renderer::preset_runtime::PresetRuntime;

#[path = "common/scene_modifier.rs"]
mod common;

const HOST: &str = include_str!("fixtures/scene-modifiers/nested_multimaterial_v2.json");

fn host() -> EffectGraphDef {
    serde_json::from_str(HOST).expect("nested v2 host parses")
}

fn attach(host: &EffectGraphDef, recipe: &str, id: &str) -> EffectGraphDef {
    common::attach(host, recipe, id)
}

fn binding_id(def: &EffectGraphDef, modifier_id: &str, param_id: &str) -> String {
    def.preset_metadata
        .as_ref()
        .and_then(|metadata| {
            metadata.bindings.iter().find_map(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::SceneModifier { modifier_id: id, param_id: param }
                        if id == &NodeId::new(modifier_id) && param == param_id
                )
                .then(|| binding.id.clone())
            })
        })
        .unwrap_or_else(|| panic!("missing binding {modifier_id}/{param_id}"))
}

fn manifest_with(def: &EffectGraphDef, id: &str, value: f32) -> ParamManifest {
    let metadata = def.preset_metadata.as_ref().expect("host metadata");
    let mut manifest = ParamManifest::from_params(
        metadata.params.iter().cloned().map(Param::bundled).collect(),
    );
    let param = manifest
        .get_mut(id)
        .unwrap_or_else(|| panic!("missing host manifest param {id}"));
    param.value = value;
    param.base = value;
    manifest
}

#[test]
fn loop_and_fog_instances_prepare_from_bundled_recipes() {
    let host = host();
    for (recipe, id) in [("SceneLoop", "loop"), ("SceneFog", "fog")] {
        let attached = attach(&host, recipe, id);
        assert_eq!(attached.scene_modifiers.len(), 1, "{recipe} inserted once");
        let prepared = prepare_scene_modifiers(&attached, &PrimitiveRegistry::with_builtin())
            .unwrap_or_else(|error| panic!("{recipe} preparation failed: {error}"));
        assert!(prepared.def.scene_modifiers.is_empty(), "{recipe} expands to ordinary graph");
        assert!(prepared.routes.iter().any(|route| route.modifier_id == NodeId::new(id)));
        assert!(attached.preset_metadata.as_ref().unwrap().bindings.iter().any(|binding| {
            matches!(binding.target, BindingTarget::SceneModifier { .. })
        }));
    }
}

#[test]
fn loop_camera_switch_bypass_keeps_shared_pattern_and_spacing_routes() {
    let attached = attach(&host(), "SceneLoop", "loop-bypass");
    let enabled_id = binding_id(&attached, "loop-bypass", "enabled");
    let manifest = manifest_with(&attached, &enabled_id, 0.0);
    let mut runtime = PresetRuntime::from_def(
        attached.clone(),
        &PrimitiveRegistry::with_builtin(),
        Some(&manifest),
    )
    .expect("bypassed loop runtime builds");
    runtime.apply_param_values(&manifest);
    let prepared = prepare_scene_modifiers(&attached, &PrimitiveRegistry::with_builtin())
        .expect("bypassed loop prepares");
    let switch_route = prepared
        .routes
        .iter()
        .find(|route| {
            route.modifier_id == NodeId::new("loop-bypass")
                && route.local.node.as_str() == "loop_cam_switch"
        })
        .expect("expanded loop switch route");
    let switch_id = runtime
        .graph
        .instance_by_node_id(&switch_route.copies[0].node_id)
        .expect("runtime loop switch");
    let switch = runtime.graph.get_node(switch_id).expect("runtime switch node");
    assert_eq!(switch.params.get("select"), Some(&ParamValue::Enum(0)));
    let metadata = attached.preset_metadata.as_ref().unwrap();
    for param_id in ["pattern_length", "cell_size"] {
        assert!(metadata.bindings.iter().any(|binding| {
            matches!(
                &binding.target,
                BindingTarget::SceneModifier { modifier_id, param_id: id }
                    if modifier_id == &NodeId::new("loop-bypass") && id == param_id
            )
        }), "missing exact loop binding for {param_id}");
    }
}

#[test]
fn fog_gate_bypass_retains_atmosphere_output_and_enabled_route() {
    let attached = attach(&host(), "SceneFog", "fog-bypass");
    let enabled_id = binding_id(&attached, "fog-bypass", "enabled");
    let manifest = manifest_with(&attached, &enabled_id, 0.0);
    let mut runtime = PresetRuntime::from_def(
        attached.clone(),
        &PrimitiveRegistry::with_builtin(),
        Some(&manifest),
    )
    .expect("bypassed fog runtime builds");
    runtime.apply_param_values(&manifest);
    let prepared = prepare_scene_modifiers(&attached, &PrimitiveRegistry::with_builtin())
        .expect("bypassed fog prepares");
    let enabled_route = prepared
        .routes
        .iter()
        .find(|route| {
            route.modifier_id == NodeId::new("fog-bypass")
                && route.local.node.as_str() == "fog_enabled"
        })
        .expect("expanded fog enabled route");
    let runtime_enabled_id = runtime
        .graph
        .instance_by_node_id(&enabled_route.copies[0].node_id)
        .expect("runtime fog enabled");
    assert_eq!(
        runtime.graph.get_node(runtime_enabled_id).unwrap().params.get("value"),
        Some(&ParamValue::Float(0.0))
    );
    assert!(prepared.def.wires.iter().any(|wire| {
        prepared.def.nodes.iter().any(|node| {
            node.id == wire.from_node && node.type_id == "node.atmosphere"
        }) && wire.to_port == "atmosphere"
    }));
    assert_eq!(binding_id(&attached, "fog-bypass", "enabled"), enabled_id);
}

#[test]
fn insert_then_delete_restores_the_complete_owner_graph() {
    let owner = host();
    let instance = prepare_new_scene_modifier(
        &owner,
        &common::stock_recipe("SceneLoop"),
        NodeId::new("inverse-loop"),
        common::render_scene(&owner),
        SceneTargetSelection::AllObjects,
    )
    .expect("loop prepares");
    let inserted = insert_scene_modifier(&owner, 0, instance)
        .expect("loop inserts")
        .graph;
    let deleted = delete_scene_modifier(&inserted, &NodeId::new("inverse-loop"))
        .expect("loop deletes")
        .graph;
    assert_eq!(deleted.version, owner.version.max(3));
    assert_eq!(deleted.nodes, owner.nodes);
    assert_eq!(deleted.wires, owner.wires);
    assert_eq!(deleted.scene_modifiers, owner.scene_modifiers);
    assert_eq!(deleted.preset_metadata, owner.preset_metadata);
}
