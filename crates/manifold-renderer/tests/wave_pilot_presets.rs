//! Structural contract for the three mathematical-wave pilot presets.
//!
//! The presets deliberately share one reusable group body. This gate keeps
//! their user-facing bindings and source capacities stable while allowing the
//! wrappers to choose different layouts, framing, phase, and colour.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GROUP_TYPE_ID,
};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_runtime::PresetRuntime;

const PRESETS: &[(&str, &str, u32)] = &[
    (
        "WaveGrid",
        include_str!("../assets/generator-presets/WaveGrid.json"),
        1_000,
    ),
    (
        "WaveRing",
        include_str!("../assets/generator-presets/WaveRing.json"),
        256,
    ),
    (
        "WaveSpiral",
        include_str!("../assets/generator-presets/WaveSpiral.json"),
        512,
    ),
];

const WAVE_ATOMS: &[&str] = &[
    "node.copy_positions",
    "node.wave_field_3d",
    "node.displace_copies",
];

fn parse(name: &str, json: &str) -> EffectGraphDef {
    serde_json::from_str(json).unwrap_or_else(|error| panic!("{name} must parse: {error}"))
}

fn value_int(node: &EffectGraphNode, param: &str) -> i32 {
    match node.params.get(param) {
        Some(manifold_core::effect_graph_def::SerializedParamValue::Int { value }) => *value,
        Some(value) => panic!("{}.{} must be an Int, got {value:?}", node.node_id, param),
        None => panic!("{}.{} is required", node.node_id, param),
    }
}

fn value_float(node: &EffectGraphNode, param: &str) -> f32 {
    match node.params.get(param) {
        Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) => *value,
        Some(value) => panic!("{}.{} must be a Float, got {value:?}", node.node_id, param),
        None => panic!("{}.{} is required", node.node_id, param),
    }
}

#[test]
fn wave_pilot_presets_have_one_shared_group_and_stable_bindings() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut defs = Vec::new();
    let mut shared_group = None;
    let mut shared_binding_targets = None;

    for (name, json, expected_count) in PRESETS {
        let def = parse(name, json);
        assert_eq!(def.version, 2, "{name} uses the ordinary v2 preset schema");
        let metadata = def.preset_metadata.as_ref().expect("wave preset metadata");
        assert!(
            metadata.params.len() <= 8,
            "{name} exposes at most eight controls"
        );

        let flat = manifold_core::flatten::flatten_groups(&def).expect("wave groups flatten");
        let nodes: BTreeMap<_, _> = flat
            .nodes
            .iter()
            .map(|node| (node.node_id.to_string(), node))
            .collect();

        // The loader is the authoritative graph compiler. Keep this call in
        // the structural gate so a preset cannot pass by merely being JSON.
        PresetRuntime::from_json_str(json, &registry)
            .unwrap_or_else(|error| panic!("{name} graph must compile: {error}"));

        let group_node = def
            .nodes
            .iter()
            .find(|node| node.type_id == GROUP_TYPE_ID)
            .expect("Wave Motion group");
        assert_eq!(group_node.node_id.to_string(), "wave_motion");
        assert_eq!(group_node.handle.as_deref(), Some("Wave Motion"));
        let group = group_node.group.as_ref().expect("Wave Motion body");
        if let Some(expected) = &shared_group {
            assert_eq!(
                group, expected,
                "all wave presets share an identical Wave Motion body"
            );
        } else {
            shared_group = Some(group.clone());
        }

        let interface_inputs: Vec<_> = group
            .interface
            .inputs
            .iter()
            .map(|port| (port.name.as_str(), port.port_type.as_str()))
            .collect();
        assert_eq!(
            interface_inputs,
            vec![
                ("instances", "Array(InstanceTransform)"),
                ("reference_instances", "Array(InstanceTransform)"),
                ("phase", "Scalar(F32)"),
            ],
            "Wave Motion interface inputs are stable",
        );
        assert_eq!(group.interface.outputs.len(), 1);
        assert_eq!(group.interface.outputs[0].name, "instances");
        assert_eq!(
            group.interface.outputs[0].port_type,
            "Array(InstanceTransform)"
        );
        let body_types: Vec<_> = group
            .nodes
            .iter()
            .map(|node| node.type_id.as_str())
            .collect();
        assert_eq!(
            body_types,
            vec![
                GROUP_INPUT_TYPE_ID,
                "node.copy_positions",
                "node.wave_field_3d",
                "node.displace_copies",
                GROUP_OUTPUT_TYPE_ID,
            ],
            "Wave Motion has only its reusable motion nodes",
        );

        let arrange = nodes.get("arrange").expect("arrange source");
        assert_eq!(value_int(arrange, "active_count"), *expected_count as i32);
        assert_eq!(value_int(arrange, "max_capacity"), *expected_count as i32);
        assert!(value_float(arrange, "extent_y") > 0.0 || *name != "WaveGrid");

        // No user-authored shader escape hatch is allowed in this pilot.
        for node in nodes.values() {
            assert!(
                node.wgsl_source.is_none(),
                "{name} has raw WGSL on {}",
                node.node_id
            );
            assert!(
                registry.contains(&node.type_id)
                    || node.type_id == GROUP_TYPE_ID
                    || node.type_id == GROUP_INPUT_TYPE_ID
                    || node.type_id == GROUP_OUTPUT_TYPE_ID
                    || WAVE_ATOMS.contains(&node.type_id.as_str()),
                "{name} contains an unrecognised node type {}",
                node.type_id,
            );
        }

        let param_ids: BTreeSet<_> = metadata
            .params
            .iter()
            .map(|param| param.id.as_str())
            .collect();
        let binding_targets: Vec<_> = metadata
            .bindings
            .iter()
            .map(|binding| {
                assert!(
                    param_ids.contains(binding.id.as_str()),
                    "binding {} has no card param",
                    binding.id
                );
                let BindingTarget::Node { node_id, param } = &binding.target else {
                    panic!("binding {} must target a node", binding.id);
                };
                let target_path = node_id.to_string();
                let target = nodes.get(&target_path).unwrap_or_else(|| {
                    panic!("binding {} target node {node_id} must resolve", binding.id)
                });
                assert!(
                    target.params.contains_key(param),
                    "binding {} target {}.{} must resolve",
                    binding.id,
                    node_id,
                    param
                );
                (binding.id.clone(), target_path, param.clone())
            })
            .collect();
        if let Some(expected) = &shared_binding_targets {
            assert_eq!(
                binding_targets, *expected,
                "wave bindings keep stable semantic targets"
            );
        } else {
            shared_binding_targets = Some(binding_targets);
        }

        defs.push(def);
    }

    for ((name, _json, _), def) in PRESETS.iter().zip(defs.iter()) {
        let roundtrip_json = serde_json::to_string(def).expect("preset serializes");
        let roundtrip: EffectGraphDef = serde_json::from_str(&roundtrip_json)
            .unwrap_or_else(|error| panic!("{name} roundtrip parses: {error}"));
        PresetRuntime::from_def(roundtrip, &registry, None)
            .unwrap_or_else(|error| panic!("{name} roundtrip bindings resolve: {error}"));
    }
}
