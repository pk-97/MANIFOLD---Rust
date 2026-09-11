//! Structural round trips for nested mesh-stage scene modifiers.

use std::collections::BTreeMap;

use manifold_core::AudioSendId;
use manifold_core::audio_mod::{AudioFeature, ParameterAudioMod};
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_TYPE_ID, GroupDef, GroupInterface,
    InterfacePortDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::effects::{ParamConvert, ParameterDriver};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::scene_exposure::SceneParamMetadata;
use manifold_core::scene_modifier::{EnablePlan, MeshStageSplice, SceneModifierPlan, ToggleDecl};
use manifold_core::types::LayerType;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_editing::command::Command;
use manifold_editing::commands::graph::{ApplySceneModifierCommand, RemoveSceneModifierCommand};

fn node(id: u32, stable: &str, type_id: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(stable),
        type_id: type_id.to_string(),
        handle: Some(stable.to_string()),
        params: BTreeMap::<String, SerializedParamValue>::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

fn scene_def() -> EffectGraphDef {
    fn object_group(
        group_id: u32,
        group_stable: &str,
        source_id: u32,
        target_stable: &str,
    ) -> EffectGraphNode {
        let mut object = node(group_id, group_stable, GROUP_TYPE_ID);
        object.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![InterfacePortDef {
                    name: "object".to_string(),
                    port_type: "Object".to_string(),
                }],
                params: vec![],
            },
            nodes: vec![
                node(
                    source_id,
                    &format!("{target_stable}_source"),
                    "node.mesh_source",
                ),
                node(source_id + 1, target_stable, "node.scene_object"),
                node(
                    source_id + 2,
                    &format!("{target_stable}_out"),
                    "system.group_output",
                ),
            ],
            wires: vec![
                wire(source_id, "out", source_id + 1, "vertices"),
                wire(source_id + 1, "object", source_id + 2, "object"),
            ],
            tint: None,
        }));
        object
    }
    let object = object_group(10, "object_group", 1, "scene_object");
    let object_b = object_group(20, "object_group_b", 101, "scene_object_b");
    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("MeshStageTest".to_string()),
            display_name: "Mesh Stage Test".to_string(),
            category: "Geometry".to_string(),
            osc_prefix: "mesh_stage_test".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: vec![],
            bindings: vec![],
            param_aliases: vec![],
            value_aliases: vec![],
            string_params: vec![],
            string_bindings: vec![],
            scene_bounds: None,
        }),
        nodes: vec![
            node(0, "render_scene", "node.render_scene"),
            object,
            object_b,
        ],
        wires: vec![
            wire(10, "object", 0, "object_0"),
            wire(20, "object", 0, "object_1"),
        ],
    }
}

fn stage(doc_id: u32, stable: &str) -> EffectGraphNode {
    let mut group = node(doc_id, stable, GROUP_TYPE_ID);
    group.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: vec![
                InterfacePortDef {
                    name: "current".to_string(),
                    port_type: "Array(MeshVertex)".to_string(),
                },
                InterfacePortDef {
                    name: "reference".to_string(),
                    port_type: "Array(MeshVertex)".to_string(),
                },
            ],
            outputs: vec![InterfacePortDef {
                name: "vertices".to_string(),
                port_type: "Array(MeshVertex)".to_string(),
            }],
            params: vec![],
        },
        nodes: vec![
            node(doc_id + 1, &format!("{stable}_in"), "system.group_input"),
            node(doc_id + 2, &format!("{stable}_op"), "node.mesh_stage"),
            node(doc_id + 3, &format!("{stable}_out"), "system.group_output"),
        ],
        wires: vec![
            wire(doc_id + 1, "current", doc_id + 2, "current"),
            wire(doc_id + 1, "reference", doc_id + 2, "reference"),
            wire(doc_id + 2, "vertices", doc_id + 3, "vertices"),
        ],
        tint: None,
    }));
    group
}

fn plan(stages: Vec<MeshStageSplice>) -> SceneModifierPlan {
    SceneModifierPlan {
        kind_id: "mesh_stage_test".to_string(),
        display_name: "Mesh Stage Test".to_string(),
        trace: vec![],
        new_nodes: vec![],
        new_wires: vec![],
        group_splices: vec![],
        mesh_stages: stages,
        repoints: vec![],
        exposures: vec![],
        shared_params: vec![],
        enable: EnablePlan {
            toggle: ToggleDecl::ValueAtom {
                node_id: manifold_core::NodeId::new("mesh_stage_enable"),
            },
            extra_nodes: vec![],
            extra_wires: vec![],
        },
    }
}

fn stage_splice(doc_id: u32, stable: &str) -> MeshStageSplice {
    stage_splice_at(
        "object_group",
        "scene_object",
        "scene_object_source",
        doc_id,
        stable,
    )
}

fn stage_splice_at(
    group: &str,
    target: &str,
    source: &str,
    doc_id: u32,
    stable: &str,
) -> MeshStageSplice {
    MeshStageSplice {
        scope_path: vec![manifold_core::NodeId::new(group)],
        target_node_id: manifold_core::NodeId::new(target),
        stage: stage(doc_id, stable),
        reference_source: (manifold_core::NodeId::new(source), "out".to_string()),
    }
}

fn project_with_scene() -> (Project, usize) {
    let mut project = Project::default();
    let index = project.timeline.add_layer(
        "Mesh Stage",
        LayerType::Generator,
        PresetTypeId::from_string("MeshStageTest".to_string()),
    );
    project.timeline.layers[index].gen_params_or_init().graph = Some(scene_def());
    (project, index)
}

fn graph(project: &Project, index: usize) -> EffectGraphDef {
    project.timeline.layers[index]
        .gen_params()
        .and_then(|params| params.graph.clone())
        .expect("scene graph")
}

fn target(project: &Project, index: usize) -> manifold_core::GraphTarget {
    manifold_core::GraphTarget::Generator(project.timeline.layers[index].layer_id.clone())
}

#[test]
fn nested_mesh_stage_apply_serde_remove_restores_graph() {
    let (mut project, index) = project_with_scene();
    let target = target(&project, index);
    let before = graph(&project, index);
    let splice = stage_splice(50, "stage_one");
    let mut plan = plan(vec![splice.clone()]);
    plan.exposures
        .push(manifold_core::scene_modifier::NodeExposure {
            node_doc_id: 52,
            node_id: manifold_core::NodeId::new("stage_one_op"),
            type_id: "node.mesh_stage".to_string(),
            params: BTreeMap::new(),
            metadata: vec![SceneParamMetadata {
                name: "amount".to_string(),
                label: "Amount".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: SerializedParamValue::Float { value: 0.5 },
                is_angle: false,
                wraps: false,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: vec![],
                convert: ParamConvert::Float,
            }],
        });

    let mut apply = ApplySceneModifierCommand::new(
        target.clone(),
        vec![],
        plan.clone(),
        EffectGraphDef { ..before.clone() },
    );
    apply.execute(&mut project);
    let applied = graph(&project, index);
    assert_eq!(
        applied
            .preset_metadata
            .as_ref()
            .expect("metadata")
            .bindings
            .len(),
        1
    );
    let serialized = serde_json::to_string(&applied).expect("serialize graph");
    let reloaded: EffectGraphDef = serde_json::from_str(&serialized).expect("reload graph");
    project.timeline.layers[index].gen_params_or_init().graph = Some(reloaded);
    let object = graph(&project, index)
        .nodes
        .into_iter()
        .find(|node| node.node_id.as_str() == "object_group")
        .expect("object group");
    let body = object.group.expect("object body");
    assert!(
        body.nodes
            .iter()
            .any(|node| node.node_id.as_str() == "stage_one")
    );
    assert!(body.wires.iter().any(|wire| {
        wire.from_node == 1
            && wire.from_port == "out"
            && wire.to_node == 50
            && wire.to_port == "reference"
    }));

    let instance = project.timeline.layers[index].gen_params_or_init();
    instance.drivers = Some(vec![ParameterDriver {
        param_id: std::borrow::Cow::Owned("52_amount".to_string()),
        beat_division: BeatDivision::Quarter,
        waveform: DriverWaveform::Sine,
        enabled: true,
        phase: 0.0,
        base_value: 0.5,
        trim_min: 0.0,
        trim_max: 1.0,
        reversed: false,
        free_period_beats: None,
        frame_aligned: false,
        legacy_param_index: None,
        is_paused_by_user: false,
    }]);
    instance.audio_mods = Some(vec![ParameterAudioMod::new(
        "52_amount".to_string().into(),
        AudioSendId::new("send-1"),
        AudioFeature::default(),
    )]);

    let mut remove = RemoveSceneModifierCommand::new(target, vec![], plan);
    remove.execute(&mut project);
    assert_eq!(graph(&project, index), before);
    let instance = project.timeline.layers[index]
        .gen_params()
        .expect("instance");
    assert!(instance.drivers.as_ref().is_none_or(Vec::is_empty));
    assert!(instance.audio_mods.as_ref().is_none_or(Vec::is_empty));
}

#[test]
fn one_plan_splices_two_nested_objects_and_remove_restores_both() {
    let (mut project, index) = project_with_scene();
    let target = target(&project, index);
    let before = graph(&project, index);
    let stages = vec![
        stage_splice(50, "stage_one"),
        stage_splice_at(
            "object_group_b",
            "scene_object_b",
            "scene_object_b_source",
            70,
            "stage_two",
        ),
    ];
    let plan = plan(stages);
    let mut apply =
        ApplySceneModifierCommand::new(target.clone(), vec![], plan.clone(), before.clone());
    apply.execute(&mut project);
    let applied = graph(&project, index);
    for (group, stage_id) in [
        ("object_group", "stage_one"),
        ("object_group_b", "stage_two"),
    ] {
        let body = applied
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == group)
            .and_then(|node| node.group.as_deref())
            .expect("object body");
        assert!(
            body.nodes
                .iter()
                .any(|node| node.node_id.as_str() == stage_id)
        );
    }
    let mut remove = RemoveSceneModifierCommand::new(target, vec![], plan);
    remove.execute(&mut project);
    assert_eq!(graph(&project, index), before);
}

#[test]
fn removing_middle_stage_preserves_later_stage() {
    let (mut project, index) = project_with_scene();
    let target = target(&project, index);
    let before = graph(&project, index);
    let first = plan(vec![stage_splice(50, "stage_one")]);
    let second = plan(vec![stage_splice(60, "stage_two")]);
    let third = plan(vec![stage_splice(70, "stage_three")]);
    let default = before.clone();

    let mut apply_first =
        ApplySceneModifierCommand::new(target.clone(), vec![], first.clone(), default.clone());
    apply_first.execute(&mut project);
    let mut apply_second =
        ApplySceneModifierCommand::new(target.clone(), vec![], second.clone(), default);
    apply_second.execute(&mut project);
    let mut apply_third =
        ApplySceneModifierCommand::new(target.clone(), vec![], third.clone(), before.clone());
    apply_third.execute(&mut project);

    let mut remove_middle = RemoveSceneModifierCommand::new(target.clone(), vec![], second);
    remove_middle.execute(&mut project);
    let after_middle_remove = graph(&project, index);
    let object = after_middle_remove
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == "object_group")
        .and_then(|node| node.group.as_deref())
        .expect("object body");
    assert!(
        object
            .nodes
            .iter()
            .any(|node| node.node_id.as_str() == "stage_one")
    );
    assert!(
        object
            .nodes
            .iter()
            .any(|node| node.node_id.as_str() == "stage_three")
    );
    assert!(object.wires.iter().any(|wire| {
        wire.from_node == 50
            && wire.from_port == "vertices"
            && wire.to_node == 70
            && wire.to_port == "current"
    }));

    let mut remove_first = RemoveSceneModifierCommand::new(target.clone(), vec![], first);
    remove_first.execute(&mut project);
    let after_first_remove = graph(&project, index);
    let object = after_first_remove
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == "object_group")
        .and_then(|node| node.group.as_deref())
        .expect("object body");
    assert!(
        !object
            .nodes
            .iter()
            .any(|node| node.node_id.as_str() == "stage_one")
    );
    assert!(
        object
            .nodes
            .iter()
            .any(|node| node.node_id.as_str() == "stage_three")
    );
    assert!(object.wires.iter().any(|wire| {
        wire.from_node == 1
            && wire.from_port == "out"
            && wire.to_node == 70
            && wire.to_port == "current"
    }));

    let mut remove_third = RemoveSceneModifierCommand::new(target, vec![], third);
    remove_third.execute(&mut project);
    assert_eq!(graph(&project, index), before);
}

#[test]
fn invalid_mesh_stage_batch_is_atomic() {
    let (mut project, index) = project_with_scene();
    let target = target(&project, index);
    let before = graph(&project, index);
    let mut invalid = stage_splice(50, "stage_one");
    invalid
        .stage
        .group
        .as_mut()
        .expect("stage body")
        .interface
        .inputs
        .push(InterfacePortDef {
            name: "current".to_string(),
            port_type: "Array(MeshVertex)".to_string(),
        });
    let mut invalid_plan = plan(vec![stage_splice(50, "stage_one"), invalid]);
    invalid_plan
        .exposures
        .push(manifold_core::scene_modifier::NodeExposure {
            node_doc_id: 52,
            node_id: manifold_core::NodeId::new("stage_one_op"),
            type_id: "node.mesh_stage".to_string(),
            params: BTreeMap::new(),
            metadata: vec![SceneParamMetadata {
                name: "amount".to_string(),
                label: "Amount".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: SerializedParamValue::Float { value: 0.5 },
                is_angle: false,
                wraps: false,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: vec![],
                convert: ParamConvert::Float,
            }],
        });
    let mut command = ApplySceneModifierCommand::new(target, vec![], invalid_plan, before.clone());
    command.execute(&mut project);
    assert_eq!(graph(&project, index), before);
}
