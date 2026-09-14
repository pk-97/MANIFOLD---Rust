//! Structural contracts for the retained-image Motion Mosh and Data Mosh graphs.
//! These checks keep the editable graph shape, card controls, and trigger wiring
//! stable while GPU and multi-frame behaviour are covered by the renderer proofs.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue,
};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_runtime::PresetRuntime;

const PRESETS: &[(&str, &str)] = &[
    (
        "MotionMosh",
        include_str!("../assets/effect-presets/MotionMosh.json"),
    ),
    (
        "DataMosh",
        include_str!("../assets/effect-presets/DataMosh.json"),
    ),
];

fn parse(name: &str, json: &str) -> EffectGraphDef {
    serde_json::from_str(json).unwrap_or_else(|error| panic!("{name} must parse: {error}"))
}

fn nodes(def: &EffectGraphDef) -> BTreeMap<String, &EffectGraphNode> {
    def.nodes
        .iter()
        .map(|node| (node.node_id.to_string(), node))
        .collect()
}

fn has_wire(
    def: &EffectGraphDef,
    ids: &BTreeMap<String, &EffectGraphNode>,
    from_node: &str,
    from_port: &str,
    to_node: &str,
    to_port: &str,
) -> bool {
    let from = ids[from_node].id;
    let to = ids[to_node].id;
    def.wires.iter().any(|wire| {
        wire.from_node == from
            && wire.from_port == from_port
            && wire.to_node == to
            && wire.to_port == to_port
    })
}

fn param_float(node: &EffectGraphNode, name: &str) -> f32 {
    match node.params.get(name) {
        Some(SerializedParamValue::Float { value }) => *value,
        Some(value) => panic!("{}.{} must be Float, got {value:?}", node.node_id, name),
        None => panic!("{}.{} is required", node.node_id, name),
    }
}

fn param_int(node: &EffectGraphNode, name: &str) -> i32 {
    match node.params.get(name) {
        Some(SerializedParamValue::Int { value }) => *value,
        Some(value) => panic!("{}.{} must be Int, got {value:?}", node.node_id, name),
        None => panic!("{}.{} is required", node.node_id, name),
    }
}

fn param_bool(node: &EffectGraphNode, name: &str) -> bool {
    match node.params.get(name) {
        Some(SerializedParamValue::Bool { value }) => *value,
        Some(value) => panic!("{}.{} must be Bool, got {value:?}", node.node_id, name),
        None => panic!("{}.{} is required", node.node_id, name),
    }
}

fn card_default(def: &EffectGraphDef, id: &str) -> f32 {
    def.preset_metadata
        .as_ref()
        .expect("preset metadata")
        .params
        .iter()
        .find(|param| param.id == id)
        .unwrap_or_else(|| panic!("missing card param {id}"))
        .default_value
}

fn assert_bindings_resolve(def: &EffectGraphDef) {
    let metadata = def.preset_metadata.as_ref().expect("preset metadata");
    let ids = nodes(def);
    let param_ids: BTreeSet<_> = metadata
        .params
        .iter()
        .map(|param| param.id.as_str())
        .collect();
    for binding in &metadata.bindings {
        assert!(
            param_ids.contains(binding.id.as_str()),
            "binding {} has no card param",
            binding.id
        );
        let BindingTarget::Node { node_id, param } = &binding.target else {
            panic!("binding {} must target a node", binding.id);
        };
        let node = ids
            .get(&node_id.to_string())
            .unwrap_or_else(|| panic!("binding {} target node {node_id} is absent", binding.id));
        assert!(
            node.params.contains_key(param),
            "binding {} target {node_id}.{param} is absent",
            binding.id
        );
        let mut pending = vec![node.id];
        let mut visited = BTreeSet::new();
        let final_id = ids["final_output"].id;
        while let Some(id) = pending.pop() {
            if visited.insert(id) {
                pending.extend(
                    def.wires
                        .iter()
                        .filter(|w| w.from_node == id)
                        .map(|w| w.to_node),
                );
            }
        }
        assert!(
            visited.contains(&final_id),
            "{} is disconnected from the image",
            binding.id
        );
    }
}

fn assert_no_clock_wires(def: &EffectGraphDef) {
    let ids = nodes(def);
    for node in ids.values() {
        assert!(
            !node.type_id.contains("clock"),
            "{} embeds a clock node",
            node.node_id
        );
        assert!(
            !node.type_id.contains("frame_time"),
            "{} embeds a frame clock",
            node.node_id
        );
    }
    for wire in &def.wires {
        let source = def
            .nodes
            .iter()
            .find(|node| node.id == wire.from_node)
            .expect("wire source");
        if source.type_id == "system.generator_input" {
            assert!(
                !matches!(
                    wire.from_port.as_str(),
                    "time" | "beat" | "frame_delta" | "frame_count" | "anim_progress"
                ),
                "{} reads playback timing from generator input",
                wire.from_port
            );
        }
    }
}

#[test]
fn both_mosh_presets_parse_compile_roundtrip_and_keep_bindings() {
    let registry = PrimitiveRegistry::with_builtin();
    for (name, json) in PRESETS {
        let def = parse(name, json);
        assert_eq!(def.version, 2);
        assert_bindings_resolve(&def);
        assert_no_clock_wires(&def);
        PresetRuntime::from_json_str(json, &registry)
            .unwrap_or_else(|error| panic!("{name} graph must compile: {error}"));

        let serialized = serde_json::to_string(&def).expect("preset serializes");
        let roundtrip: EffectGraphDef = serde_json::from_str(&serialized)
            .unwrap_or_else(|error| panic!("{name} roundtrip parses: {error}"));
        assert_eq!(roundtrip, def, "{name} roundtrip is stable");
        PresetRuntime::from_def(roundtrip, &registry, None)
            .unwrap_or_else(|error| panic!("{name} roundtrip bindings resolve: {error}"));
    }
}

#[test]
fn motion_mosh_has_flow_masked_colour_history_and_trigger_contract() {
    let def = parse("MotionMosh", PRESETS[0].1);
    let ids = nodes(&def);
    let types: BTreeSet<_> = def.nodes.iter().map(|node| node.type_id.as_str()).collect();
    for required in [
        "node.optical_flow",
        "node.block_sample",
        "node.uv_displace_by_flow",
        "node.vector_length",
        "node.smoothstep",
        "node.masked_mix",
        "node.feedback",
    ] {
        assert!(
            types.contains(required),
            "Motion Mosh is missing {required}"
        );
    }
    assert_eq!(
        def.nodes
            .iter()
            .filter(|node| node.type_id == "node.feedback")
            .count(),
        2,
        "Motion Mosh retains colour and movement-mask history"
    );
    assert_eq!(card_default(&def, "drag"), 2.0);
    assert_eq!(card_default(&def, "persistence"), 0.985);
    assert_eq!(card_default(&def, "block_size"), 16.0);
    assert_eq!(card_default(&def, "motion_threshold"), 0.75);
    assert_eq!(card_default(&def, "clip_trigger"), 1.0);
    assert_eq!(card_default(&def, "trigger_visual"), 0.0);
    assert_eq!(param_int(ids["optical_flow"], "analysis_max_dim"), 192);
    assert_eq!(param_int(ids["optical_flow"], "update_interval"), 1);
    assert!(param_bool(ids["optical_flow"], "fixed_lag"));
    assert!(param_bool(ids["feedback_color"], "seed_on_reset"));
    assert!(param_bool(ids["feedback_color"], "copy_capture"));
    assert!(has_wire(
        &def,
        &ids,
        "generator_input",
        "trigger_count",
        "trigger_gate",
        "trigger_count"
    ));
    assert!(!ids["mask_feedback"].params.contains_key("seed_on_reset"));

    assert!(has_wire(
        &def,
        &ids,
        "optical_flow",
        "out",
        "flow_block",
        "in"
    ));
    assert!(has_wire(&def, &ids, "flow_block", "out", "warp", "flow"));
    assert!(has_wire(
        &def,
        &ids,
        "flow_block",
        "out",
        "flow_valid",
        "source"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "flow_block",
        "out",
        "flow_confidence",
        "source"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "confidence_mask",
        "out",
        "mask_max",
        "a"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "mask_max",
        "out",
        "mask_feedback",
        "in"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "mask_max",
        "out",
        "reconstruct",
        "mask"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "recover_mix",
        "out",
        "feedback_color",
        "in"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "trigger_gate",
        "pulse",
        "recover_choice",
        "in_0"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "trigger_gate",
        "pulse",
        "kick_choice",
        "in_2"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "trigger_gate",
        "out",
        "reverse_cycle",
        "trigger_count"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "clip_enabled",
        "out",
        "reverse_enable",
        "selector"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "recover_max",
        "out",
        "recover_mix",
        "amount"
    ));
}

#[test]
fn data_mosh_has_clean_zero_controls_explicit_pattern_time_and_trigger_contract() {
    let def = parse("DataMosh", PRESETS[1].1);
    let ids = nodes(&def);
    let types: BTreeSet<_> = def.nodes.iter().map(|node| node.type_id.as_str()).collect();
    for required in [
        "node.feedback",
        "node.block_sample",
        "node.block_displace_field",
        "node.remap",
        "node.smoothstep",
        "node.masked_mix",
        "node.clip_trigger_cycle",
    ] {
        assert!(types.contains(required), "Data Mosh is missing {required}");
    }
    assert_eq!(card_default(&def, "corruption"), 0.65);
    assert_eq!(card_default(&def, "persistence"), 0.985);
    assert_eq!(card_default(&def, "block_size"), 16.0);
    assert_eq!(card_default(&def, "displacement"), 0.3);
    assert_eq!(card_default(&def, "pattern"), 0.0);
    assert_eq!(card_default(&def, "recover"), 0.0);
    assert_eq!(card_default(&def, "clip_trigger"), 1.0);
    assert!(param_bool(ids["feedback_data"], "seed_on_reset"));
    assert_eq!(param_float(ids["field"], "speed"), 1.0);
    assert_eq!(param_float(ids["field"], "time"), 0.0);
    assert_eq!(param_int(ids["reblock_cycle"], "modulus"), 64);

    assert!(has_wire(
        &def,
        &ids,
        "feedback_data",
        "out",
        "block_sample",
        "in"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "block_sample",
        "out",
        "warped",
        "source"
    ));
    assert!(has_wire(
        &def, &ids, "field", "offset", "warped", "uv_field"
    ));
    assert!(has_wire(&def, &ids, "pattern_sum", "out", "field", "time"));
    assert!(has_wire(
        &def,
        &ids,
        "trigger_gate",
        "out",
        "reblock_cycle",
        "trigger_count"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "reblock_cycle",
        "out",
        "pattern_mode",
        "in_1"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "trigger_gate",
        "pulse",
        "recover_choice",
        "in_0"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "trigger_gate",
        "pulse",
        "kick_choice",
        "in_2"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "recover_mix",
        "out",
        "feedback_data",
        "in"
    ));
    assert!(has_wire(
        &def,
        &ids,
        "recover_max",
        "out",
        "recover_mix",
        "amount"
    ));

    let low = param_float(ids["threshold_low"], "a");
    assert_eq!(low, 1.0);
    assert_eq!(param_float(ids["threshold_low"], "b"), 0.65);
    assert_eq!(param_float(ids["threshold_high"], "b"), 0.001);
}
