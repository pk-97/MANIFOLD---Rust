use super::*;
use crate::node_graph::{ParamValue, PrimitiveRegistry};
use manifold_core::{Beats, Seconds};

fn trigger_graph(wire_initial_count: bool) -> String {
    let initial_wire = if wire_initial_count {
        r#", { "fromNode": 0, "fromPort": "trigger_baseline", "toNode": 1, "toPort": "initial_count" }"#
    } else {
        ""
    };
    format!(
        r#"{{
            "version": 1,
            "name": "TriggerInitialization",
            "nodes": [
                {{ "id": 0, "nodeId": "input", "typeId": "system.generator_input", "handle": "input" }},
                {{ "id": 1, "nodeId": "gate", "typeId": "node.trigger_gate", "handle": "gate" }},
                {{ "id": 2, "nodeId": "uv", "typeId": "node.uv_field", "handle": "uv" }},
                {{ "id": 3, "nodeId": "scaler", "typeId": "node.scale_offset_image", "handle": "scaler" }},
                {{ "id": 4, "nodeId": "final_output", "typeId": "system.final_output", "handle": "final_output" }}
            ],
            "wires": [
                {{ "fromNode": 0, "fromPort": "trigger_count", "toNode": 1, "toPort": "trigger_count" }}{initial_wire},
                {{ "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "scale" }},
                {{ "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }},
                {{ "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }}
            ]
        }}"#
    )
}

fn runtime(wire_initial_count: bool) -> PresetRuntime {
    PresetRuntime::from_json_str(
        &trigger_graph(wire_initial_count),
        &PrimitiveRegistry::with_builtin(),
    )
    .expect("trigger initialization graph must load")
}

fn frame_time() -> FrameTime {
    FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    }
}

fn execute(runtime: &mut PresetRuntime, trigger_count: f32) {
    runtime.set_frame_context(FrameContextInputs {
        time: 0.0,
        beat: 0.0,
        aspect: 1.0,
        trigger_count,
        anim_progress: 0.0,
        output_width: 1920.0,
        output_height: 1080.0,
    });
    runtime.execute_frame(frame_time());
}

fn scaler_scale(runtime: &PresetRuntime) -> f32 {
    let scaler_id = runtime
        .graph
        .instance_by_node_id(&manifold_core::NodeId::new("scaler"))
        .expect("trigger graph declares scaler");
    let scaler_node_id = runtime.graph.get_node(scaler_id).unwrap().node_id.clone();
    let values = runtime
        .live_node_params_watched()
        .into_iter()
        .find(|(id, _)| *id == scaler_node_id)
        .map(|(_, values)| values)
        .expect("scaler reports live params");
    *values
        .iter()
        .find(|(name, _)| *name == "scale")
        .map(|(_, value)| value)
        .expect("scaler scale is a declared param")
}

#[test]
fn cold_first_real_trigger_uses_zero_baseline() {
    let mut runtime = runtime(true);
    runtime.note_trigger_event(0);
    execute(&mut runtime, 1.0);
    assert_eq!(scaler_scale(&runtime), 1.0);
}

#[test]
fn prepared_parameter_restoration_resumes_execution_and_clears_error() {
    let mut runtime = runtime(true);
    let gate = runtime
        .graph
        .instance_by_node_id(&manifold_core::NodeId::new("gate"))
        .unwrap();
    runtime
        .graph
        .protect_prepared_param(gate, "enable")
        .unwrap();
    execute(&mut runtime, 0.0);
    assert!(
        runtime
            .graph
            .set_param(gate, "enable", ParamValue::Bool(false))
            .is_err()
    );
    execute(&mut runtime, 0.0);
    assert!(
        runtime
            .errors()
            .iter()
            .any(|error| matches!(error, ChainError::PreparedParameterChanged { .. }))
    );
    runtime
        .graph
        .set_param(gate, "enable", ParamValue::Bool(true))
        .unwrap();
    runtime.note_trigger_event(0);
    execute(&mut runtime, 1.0);
    assert_eq!(scaler_scale(&runtime), 1.0);
    assert!(
        !runtime
            .errors()
            .iter()
            .any(|error| matches!(error, ChainError::PreparedParameterChanged { .. }))
    );
}

#[test]
fn loaded_nonzero_count_without_note_has_no_initial_edge() {
    let mut runtime = runtime(true);
    execute(&mut runtime, 7.0);
    assert_eq!(scaler_scale(&runtime), 0.0);
}

#[test]
fn warm_context_clear_then_first_real_trigger_uses_zero_baseline() {
    let mut runtime = runtime(true);
    execute(&mut runtime, 0.0);
    runtime.clear_trigger_state();
    runtime.note_trigger_event(0);
    execute(&mut runtime, 1.0);
    assert_eq!(scaler_scale(&runtime), 1.0);
}

#[test]
fn legacy_unwired_initial_count_preserves_first_count_baseline() {
    let mut runtime = runtime(false);
    runtime.note_trigger_event(0);
    execute(&mut runtime, 1.0);
    assert_eq!(scaler_scale(&runtime), 0.0);
}

#[test]
fn disabled_gate_absorbs_event_and_reenable_without_edge_stays_zero() {
    let mut runtime = runtime(true);
    let gate_id = runtime
        .graph
        .instance_by_node_id(&manifold_core::NodeId::new("gate"))
        .expect("trigger graph declares gate");
    runtime
        .graph
        .set_param(gate_id, "enable", ParamValue::Bool(false))
        .expect("gate enable param");
    runtime.note_trigger_event(0);
    execute(&mut runtime, 1.0);
    assert_eq!(scaler_scale(&runtime), 0.0);

    runtime
        .graph
        .set_param(gate_id, "enable", ParamValue::Bool(true))
        .expect("gate enable param");
    execute(&mut runtime, 1.0);
    assert_eq!(scaler_scale(&runtime), 0.0);

    runtime.note_trigger_event(1);
    execute(&mut runtime, 2.0);
    assert_eq!(scaler_scale(&runtime), 1.0);
}

#[test]
fn rapid_pending_notes_keep_earliest_baseline() {
    let mut runtime = runtime(true);
    runtime.note_trigger_event(0);
    runtime.note_trigger_event(1);
    execute(&mut runtime, 2.0);
    assert_eq!(scaler_scale(&runtime), 2.0);
}

#[test]
fn consumed_marker_does_not_refire_after_gate_clear_at_same_count() {
    let mut runtime = runtime(true);
    runtime.note_trigger_event(0);
    execute(&mut runtime, 1.0);
    assert_eq!(scaler_scale(&runtime), 1.0);

    runtime.clear_trigger_state();
    execute(&mut runtime, 1.0);
    assert_eq!(scaler_scale(&runtime), 0.0);
}

#[test]
fn independent_runtimes_do_not_share_pending_trigger_markers() {
    let mut first = runtime(true);
    let mut second = runtime(true);
    first.note_trigger_event(0);
    execute(&mut first, 1.0);
    execute(&mut second, 1.0);
    assert_eq!(scaler_scale(&first), 1.0);
    assert_eq!(scaler_scale(&second), 0.0);
}

#[test]
fn pending_trigger_baseline_carries_across_same_generator_rebuild() {
    let mut prior = runtime(true);
    prior.note_trigger_event(0);
    let mut rebuilt = runtime(true);
    rebuilt.carry_pending_trigger_from(&prior);
    execute(&mut rebuilt, 1.0);
    assert_eq!(scaler_scale(&rebuilt), 1.0);
}

#[test]
fn modifier_control_state_runtime_preserves_gate_when_another_modifier_is_removed() {
    use crate::node_graph::scene_modifier_expand::{
        PreparedModifierControlState, SceneModifierNodeCopy, SceneModifierNodeRoute,
    };
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::scene_modifier_preset::{
        SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };
    let local: EffectGraphDef = serde_json::from_str(&trigger_graph(true)).unwrap();
    let instance = SceneModifierInstanceDef {
        id: NodeId::new("kept"),
        scene: SceneNodeRef {
            scope: vec![],
            node: NodeId::new("scene"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: vec![],
        graph: Box::new(local.clone()),
    };
    let route = SceneModifierNodeRoute {
        modifier_id: instance.id.clone(),
        local: SceneNodeRef {
            scope: vec![],
            node: NodeId::new("gate"),
        },
        copies: vec![SceneModifierNodeCopy {
            object: None,
            node_id: NodeId::new("gate"),
        }],
    };
    let mut owner = local;
    owner.scene_modifiers.push(instance.clone());
    let mut removed = instance;
    removed.id = NodeId::new("removed");
    owner.scene_modifiers.push(removed);
    let mut prior = runtime(true);
    prior.modifier_control_state = Some(
        PreparedModifierControlState::prepare(&owner, std::slice::from_ref(&route), &prior.graph)
            .unwrap(),
    );
    prior.note_trigger_event(0);
    execute(&mut prior, 1.0);
    assert_eq!(scaler_scale(&prior), 1.0);
    owner.scene_modifiers.pop();
    let mut rebuilt = runtime(true);
    rebuilt.modifier_control_state =
        Some(PreparedModifierControlState::prepare(&owner, &[route], &rebuilt.graph).unwrap());
    rebuilt.carry_modifier_control_state_from(&mut prior);
    execute(&mut rebuilt, 1.0);
    assert_eq!(
        scaler_scale(&rebuilt),
        1.0,
        "the surviving gate retains its accumulated count"
    );
    rebuilt.note_trigger_event(1);
    execute(&mut rebuilt, 2.0);
    assert_eq!(scaler_scale(&rebuilt), 2.0);
}
