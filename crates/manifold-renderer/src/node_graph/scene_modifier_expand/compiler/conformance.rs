use std::collections::{BTreeSet, HashMap};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GroupDef, GroupInterface, InterfacePortDef,
    SerializedParamValue,
};
use manifold_core::scene_modifier_preset::{
    SceneModifierStageDef, SceneStageInput, SceneStageOutput, SceneStageScope, SceneStageSource,
    SceneTargetSelection,
};

use super::tests::fixture;
use super::{SceneModifierExpandError, expand_scene_modifiers};
use crate::node_graph::persistence::PrimitiveRegistry;

fn registry() -> PrimitiveRegistry {
    PrimitiveRegistry::with_builtin()
}

fn source_types(graph: &EffectGraphDef, port: &str) -> Vec<String> {
    graph
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.wave_shear_mesh")
        .filter_map(|node| {
            graph
                .wires
                .iter()
                .find(|wire| wire.to_node == node.id && wire.to_port == port)
                .and_then(|wire| {
                    graph
                        .nodes
                        .iter()
                        .find(|source| source.id == wire.from_node)
                })
                .map(|source| source.type_id.clone())
        })
        .collect()
}

#[test]
fn scene_modifier_expand_conformance_reference_and_previous_are_distinct() {
    let mut owner = fixture();
    let mut reference = owner.scene_modifiers[0].clone();
    reference.id = NodeId::new("reference_modifier");
    reference
        .graph
        .preset_metadata
        .as_mut()
        .expect("recipe metadata")
        .scene_modifier
        .as_mut()
        .expect("recipe")
        .stages[0]
        .inputs[0]
        .source = SceneStageSource::Reference {
        endpoint: manifold_core::scene_modifier_preset::SceneEndpoint::Vertices,
    };
    owner.scene_modifiers.push(reference);
    let expanded = expand_scene_modifiers(&owner, &registry()).expect("reference expands");
    let sources = source_types(&expanded, "in");
    assert_eq!(
        sources
            .iter()
            .filter(|source| *source == "node.cube_mesh")
            .count(),
        4,
        "both modifiers read the two original mesh producers"
    );
    owner.scene_modifiers[1]
        .graph
        .preset_metadata
        .as_mut()
        .unwrap()
        .scene_modifier
        .as_mut()
        .unwrap()
        .stages[0]
        .inputs[0]
        .source = SceneStageSource::Previous {
        endpoint: manifold_core::scene_modifier_preset::SceneEndpoint::Vertices,
    };
    let previous = expand_scene_modifiers(&owner, &registry()).expect("previous expands");
    assert_eq!(
        source_types(&previous, "in")
            .iter()
            .filter(|source| *source == "node.cube_mesh")
            .count(),
        2,
        "only the first modifier reads original geometry when the second uses Previous"
    );
}

#[test]
fn scene_modifier_expand_conformance_reorder_keeps_generated_identity_set() {
    let mut owner = fixture();
    let mut second = owner.scene_modifiers[0].clone();
    second.id = NodeId::new("second_modifier");
    owner.scene_modifiers.push(second);
    let first = expand_scene_modifiers(&owner, &registry()).expect("stack expands");
    owner.scene_modifiers.swap(0, 1);
    let reordered = expand_scene_modifiers(&owner, &registry()).expect("reordered stack expands");
    let host: BTreeSet<String> = [
        "scan_generator_input",
        "scan_render",
        "scan_camera",
        "scan_left_group",
        "scan_right_group",
        "scan_output",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let ids = |graph: &EffectGraphDef| {
        graph
            .nodes
            .iter()
            .map(|node| node.node_id.as_str().to_string())
            .filter(|id| !host.contains(id))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&first), ids(&reordered));
}

#[test]
fn scene_modifier_expand_conformance_rejects_actual_target_and_stage_paths() {
    let mut missing = fixture();
    missing.scene_modifiers[0].targets = SceneTargetSelection::Explicit {
        objects: vec![manifold_core::SceneNodeRef {
            scope: vec![NodeId::new("scan_left_group")],
            node: NodeId::new("does_not_exist"),
        }],
    };
    assert!(matches!(
        expand_scene_modifiers(&missing, &registry()),
        Err(SceneModifierExpandError::MissingTarget { .. })
    ));

    let mut forward = fixture();
    let recipe = forward.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_mut()
        .expect("recipe metadata")
        .scene_modifier
        .as_mut()
        .expect("recipe");
    let stage = recipe.stages[0].group.clone();
    recipe.stages[0].inputs[0].source = SceneStageSource::StageOutput {
        stage,
        port: "vertices".into(),
    };
    assert!(matches!(
        expand_scene_modifiers(&forward, &registry()),
        Err(SceneModifierExpandError::InvalidRecipe { .. })
    ));
}

#[test]
fn scene_modifier_expand_conformance_rejects_each_object_to_scene_stage() {
    let mut owner = fixture();
    let local = owner.scene_modifiers[0].graph.as_mut();
    let mut scene_stage = local.nodes[0].clone();
    scene_stage.id = 2;
    scene_stage.node_id = NodeId::new("scene_stage");
    if let Some(group) = scene_stage.group.as_mut() {
        for node in &mut group.nodes {
            node.node_id = NodeId::new(format!("{}_scene", node.node_id));
        }
    }
    local.nodes.push(scene_stage);
    let recipe = local
        .preset_metadata
        .as_mut()
        .expect("recipe metadata")
        .scene_modifier
        .as_mut()
        .expect("recipe");
    recipe.stages.push(SceneModifierStageDef {
        group: NodeId::new("scene_stage"),
        scope: SceneStageScope::Scene,
        inputs: vec![SceneStageInput {
            port: "current".into(),
            source: SceneStageSource::StageOutput {
                stage: NodeId::new("elastic_stage"),
                port: "vertices".into(),
            },
        }],
        outputs: Vec::<SceneStageOutput>::new(),
    });
    assert!(matches!(
        expand_scene_modifiers(&owner, &registry()),
        Err(SceneModifierExpandError::InvalidRecipe { .. })
    ));
}

#[test]
fn scene_modifier_expand_conformance_control_stage_broadcasts_once() {
    let mut owner = fixture();
    let local = owner.scene_modifiers[0].graph.as_mut();
    let control = EffectGraphNode {
        id: 2,
        node_id: NodeId::new("control_stage"),
        type_id: "group".into(),
        handle: Some("ControlStage".into()),
        params: HashMap::new().into_iter().collect(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: HashMap::new().into_iter().collect(),
        output_canvas_scales: HashMap::new().into_iter().collect(),
        group: Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![InterfacePortDef {
                    name: "trigger_count".into(),
                    port_type: "Scalar(F32)".into(),
                }],
                outputs: vec![InterfacePortDef {
                    name: "out".into(),
                    port_type: "Scalar(F32)".into(),
                }],
                params: Vec::new(),
            },
            nodes: vec![
                EffectGraphNode {
                    id: 1,
                    node_id: NodeId::new("control_input"),
                    type_id: "system.group_input".into(),
                    handle: None,
                    params: HashMap::new().into_iter().collect(),
                    exposed_params: BTreeSet::new(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: HashMap::new().into_iter().collect(),
                    output_canvas_scales: HashMap::new().into_iter().collect(),
                    group: None,
                },
                EffectGraphNode {
                    id: 2,
                    node_id: NodeId::new("control_gate"),
                    type_id: "node.trigger_gate".into(),
                    handle: None,
                    params: [("enable".into(), SerializedParamValue::Bool { value: true })]
                        .into_iter()
                        .collect(),
                    exposed_params: BTreeSet::new(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: HashMap::new().into_iter().collect(),
                    output_canvas_scales: HashMap::new().into_iter().collect(),
                    group: None,
                },
                EffectGraphNode {
                    id: 3,
                    node_id: NodeId::new("control_output"),
                    type_id: "system.group_output".into(),
                    handle: None,
                    params: HashMap::new().into_iter().collect(),
                    exposed_params: BTreeSet::new(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: HashMap::new().into_iter().collect(),
                    output_canvas_scales: HashMap::new().into_iter().collect(),
                    group: None,
                },
            ],
            wires: vec![
                EffectGraphWire {
                    from_node: 1,
                    from_port: "trigger_count".into(),
                    to_node: 2,
                    to_port: "trigger_count".into(),
                },
                EffectGraphWire {
                    from_node: 2,
                    from_port: "out".into(),
                    to_node: 3,
                    to_port: "out".into(),
                },
            ],
            tint: None,
        })),
    };
    local.nodes.insert(0, control);
    let shear_group = local.nodes[1].group.as_mut().expect("elastic group");
    shear_group.interface.inputs.push(InterfacePortDef {
        name: "trigger".into(),
        port_type: "Scalar(F32)".into(),
    });
    shear_group.nodes.push(EffectGraphNode {
        id: 6,
        node_id: NodeId::new("group_trigger"),
        type_id: "system.group_input".into(),
        handle: None,
        params: HashMap::new().into_iter().collect(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: HashMap::new().into_iter().collect(),
        output_canvas_scales: HashMap::new().into_iter().collect(),
        group: None,
    });
    shear_group.wires.push(EffectGraphWire {
        from_node: 6,
        from_port: "trigger".into(),
        to_node: 3,
        to_port: "amplitude".into(),
    });
    let recipe = local
        .preset_metadata
        .as_mut()
        .expect("recipe metadata")
        .scene_modifier
        .as_mut()
        .expect("recipe");
    recipe.stages.insert(
        0,
        SceneModifierStageDef {
            group: NodeId::new("control_stage"),
            scope: SceneStageScope::Scene,
            inputs: vec![SceneStageInput {
                port: "trigger_count".into(),
                source: SceneStageSource::Context {
                    value: manifold_core::scene_modifier_preset::SceneContextValue::TriggerCount,
                },
            }],
            outputs: Vec::new(),
        },
    );
    recipe.stages[1].inputs.push(SceneStageInput {
        port: "trigger".into(),
        source: SceneStageSource::StageOutput {
            stage: NodeId::new("control_stage"),
            port: "out".into(),
        },
    });
    let expanded = expand_scene_modifiers(&owner, &registry()).expect("control stage expands");
    assert_eq!(
        expanded
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.trigger_gate")
            .count(),
        1
    );
    let gate = expanded
        .nodes
        .iter()
        .find(|node| node.type_id == "node.trigger_gate")
        .expect("shared gate");
    let amplitude_sources: Vec<u32> = expanded
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.wave_shear_mesh")
        .filter_map(|node| {
            expanded
                .wires
                .iter()
                .find(|wire| wire.to_node == node.id && wire.to_port == "amplitude")
                .map(|wire| wire.from_node)
        })
        .collect();
    assert_eq!(amplitude_sources.len(), 2);
    assert!(amplitude_sources.iter().all(|source| *source == gate.id));
}

#[test]
fn scene_modifier_expand_conformance_rejects_modifier_stack_over_capacity() {
    let mut owner = fixture();
    let template = owner.scene_modifiers[0].clone();
    for index in 0..16 {
        let mut instance = template.clone();
        instance.id = NodeId::new(format!("modifier_{index}"));
        owner.scene_modifiers.push(instance);
    }
    assert!(matches!(
        expand_scene_modifiers(&owner, &registry()),
        Err(SceneModifierExpandError::CapacityExceeded { .. })
    ));
}

#[test]
fn scene_modifier_expand_conformance_preflights_replication_and_actual_ports() {
    let mut invalid = fixture();
    invalid.scene_modifiers[0].graph.nodes[0]
        .group
        .as_mut()
        .unwrap()
        .wires[0]
        .to_port = "missing_live_input".into();
    assert!(expand_scene_modifiers(&invalid, &registry()).is_err());

    let mut owner = fixture();
    let object = owner
        .nodes
        .iter()
        .find_map(|node| node.group.as_ref())
        .unwrap()
        .nodes
        .iter()
        .find(|node| node.type_id == "node.scene_object")
        .unwrap()
        .clone();
    for index in 0..100 {
        let mut copy = object.clone();
        copy.id = 5000 + index;
        copy.node_id = NodeId::new(format!("capacity_object_{index}"));
        owner.nodes.push(copy);
        owner.wires.push(EffectGraphWire {
            from_node: 5000 + index,
            from_port: "object".into(),
            to_node: 1,
            to_port: format!("object_{}", index + 2),
        });
    }
    let group = owner.scene_modifiers[0].graph.nodes[0]
        .group
        .as_mut()
        .unwrap();
    let mut value = group.nodes[2].clone();
    value.type_id = "node.value".into();
    value.params.clear();
    for index in 0..1000 {
        let mut copy = value.clone();
        copy.id = 6000 + index;
        copy.node_id = NodeId::new(format!("capacity_value_{index}"));
        group.nodes.push(copy);
    }
    let canonical = owner.clone();
    assert!(matches!(
        expand_scene_modifiers(&owner, &registry()),
        Err(SceneModifierExpandError::CapacityExceeded { .. })
    ));
    assert_eq!(owner, canonical);
}
