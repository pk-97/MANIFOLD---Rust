use super::*;

use std::collections::BTreeSet;

use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphWire, ParamSpecDef,
};
use manifold_core::scene_modifier_preset::{
    SceneContextValue, SceneModifierInstanceDef, SceneModifierRecipe, SceneModifierStageDef,
    SceneNodeRef, SceneStageInput, SceneStageScope, SceneStageSource, SceneTargetSelection,
};
use manifold_core::{Beats, NodeId, Seconds};

use crate::node_graph::scene_modifier_expand::SceneModifierEventRoute;

fn control_modifier(id: &str) -> EffectGraphDef {
    let mut graph: EffectGraphDef = serde_json::from_str(
        r#"{
          "version": 3,
          "name": "Modifier Event Control",
          "presetMetadata": {
            "id": "ModifierEventControl",
            "displayName": "Modifier Event Control",
            "category": "Control",
            "oscPrefix": "modifier_event_control",
            "available": false,
            "params": [],
            "bindings": []
          },
          "nodes": [{
            "id": 1, "nodeId": "event_stage", "typeId": "group", "handle": "EventStage",
            "group": {
              "interface": {
                "inputs": [
                  {"name":"trigger","portType":"Scalar(F32)"},
                  {"name":"initial_count","portType":"Scalar(F32)"}
                ],
                "outputs": [{"name":"pulse","portType":"Scalar(F32)"}]
              },
              "nodes": [
                {"id":1,"nodeId":"group_trigger","typeId":"system.group_input","handle":"trigger"},
                {"id":2,"nodeId":"group_initial","typeId":"system.group_input","handle":"initial_count"},
                {"id":3,"nodeId":"gate","typeId":"node.trigger_gate","handle":"gate"},
                {"id":4,"nodeId":"envelope","typeId":"node.envelope_beats","handle":"envelope","params":{"window_beats":{"type":"Float","value":0.25}}},
                {"id":5,"nodeId":"zero","typeId":"node.value","handle":"zero","params":{"value":{"type":"Float","value":0.0}}},
                {"id":6,"nodeId":"group_output","typeId":"system.group_output","handle":"output"}
              ],
              "wires": [
                {"fromNode":1,"fromPort":"trigger","toNode":3,"toPort":"trigger_count"},
                {"fromNode":2,"fromPort":"initial_count","toNode":3,"toPort":"initial_count"},
                {"fromNode":3,"fromPort":"out","toNode":4,"toPort":"trigger"},
                {"fromNode":5,"fromPort":"out","toNode":4,"toPort":"initial_count"},
                {"fromNode":4,"fromPort":"out","toNode":6,"toPort":"pulse"}
              ]
            }
          }],
          "wires": []
        }"#,
    )
    .expect("control modifier JSON");
    let metadata = graph.preset_metadata.as_mut().expect("control metadata");
    metadata.params = vec![
        ParamSpecDef {
            id: "enabled".into(),
            name: "Enabled".into(),
            min: 0.0,
            max: 1.0,
            default_value: 1.0,
            is_toggle: true,
            ..Default::default()
        },
        ParamSpecDef {
            id: id.into(),
            name: format!("{id} Gate"),
            min: 0.0,
            max: 1.0,
            default_value: 1.0,
            is_toggle: true,
            is_trigger_gate: true,
            ..Default::default()
        },
    ];
    metadata.bindings.push(BindingDef {
        id: id.into(),
        label: format!("{id} Gate"),
        default_value: 1.0,
        target: BindingTarget::Node {
            node_id: NodeId::new("gate"),
            param: "enable".into(),
        },
        convert: manifold_core::effects::ParamConvert::BoolThreshold,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
        default_mirrors_node_param: false,
    });
    metadata.scene_modifier = Some(SceneModifierRecipe {
        schema_version: 1,
        singleton: false,
        enabled_param: "enabled".into(),
        preparation_params: vec![],
        initializers: vec![],
        calibrations: vec![],
        stages: vec![SceneModifierStageDef {
            group: NodeId::new("event_stage"),
            scope: SceneStageScope::Scene,
            inputs: vec![
                SceneStageInput {
                    port: "trigger".into(),
                    source: SceneStageSource::Context {
                        value: SceneContextValue::TriggerCount,
                    },
                },
                SceneStageInput {
                    port: "initial_count".into(),
                    source: SceneStageSource::Context {
                        value: SceneContextValue::TriggerBaseline,
                    },
                },
            ],
            outputs: vec![],
        }],
    });
    graph
}

fn canonical_owner() -> EffectGraphDef {
    let mut owner: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
    )))
    .expect("canonical scene host");
    owner.version = 3;
    for (id, gate) in [("event_a", "gate_a"), ("event_b", "gate_b")] {
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new(id),
            scene: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("scan_render"),
            },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: vec![],
            legacy_math_view_carrier: None,
            graph: Box::new(control_modifier(gate)),
        });
        let metadata = owner.preset_metadata.as_mut().expect("host metadata");
        metadata.params.push(ParamSpecDef {
            id: gate.into(),
            name: gate.into(),
            min: 0.0,
            max: 1.0,
            default_value: 1.0,
            is_toggle: true,
            is_trigger_gate: true,
            ..Default::default()
        });
        metadata.bindings.push(BindingDef {
            id: gate.into(),
            label: gate.into(),
            default_value: 1.0,
            target: BindingTarget::SceneModifier {
                modifier_id: NodeId::new(id),
                param_id: gate.into(),
            },
            convert: manifold_core::effects::ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        });
    }
    owner
}

fn control_runtime() -> (PresetRuntime, EffectGraphDef, Vec<SceneModifierEventRoute>) {
    control_runtime_for(canonical_owner())
}

fn control_runtime_for(
    owner: EffectGraphDef,
) -> (PresetRuntime, EffectGraphDef, Vec<SceneModifierEventRoute>) {
    let registry = PrimitiveRegistry::with_builtin();
    let prepared =
        crate::node_graph::scene_modifier_expand::prepare_scene_modifiers(&owner, &registry)
            .expect("canonical event host prepares");
    let routes = prepared.event_routes.clone();
    let control = extract_control_graph(&prepared.def, &routes);
    let mut runtime = PresetRuntime::from_def_for_render(control, &registry, None, false)
        .expect("CPU control graph loads");
    runtime.modifier_events = Some(
        crate::node_graph::scene_modifier_expand::PreparedModifierEvents::prepare(
            &owner,
            &routes,
            &runtime.graph,
        )
        .expect("event routes resolve in extracted graph"),
    );
    (runtime, owner, routes)
}

fn extract_control_graph(
    prepared: &EffectGraphDef,
    routes: &[SceneModifierEventRoute],
) -> EffectGraphDef {
    let keep: BTreeSet<_> = prepared
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.type_id.as_str(),
                "node.value" | "node.trigger_gate" | "node.envelope_beats"
            )
        })
        .map(|node| node.id)
        .collect();
    let mut nodes: Vec<_> = prepared
        .nodes
        .iter()
        .filter(|node| keep.contains(&node.id))
        .cloned()
        .collect();
    let mut wires: Vec<_> = prepared
        .wires
        .iter()
        .filter(|wire| keep.contains(&wire.from_node) && keep.contains(&wire.to_node))
        .cloned()
        .collect();

    // Add a real CPU-observable texture path; the envelope outputs feed the
    // scaler input, while the selected final output keeps the graph runnable.
    let next = nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    let json = format!(
        r#"{{"version":1,"nodes":[
          {{"id":{next},"nodeId":"input","typeId":"system.generator_input","handle":"input"}},
          {{"id":{},"nodeId":"uv","typeId":"node.uv_field","handle":"uv"}},
          {{"id":{},"nodeId":"scaler_a","typeId":"node.scale_offset_image","handle":"scaler_a"}},
          {{"id":{},"nodeId":"scaler_b","typeId":"node.scale_offset_image","handle":"scaler_b"}},
          {{"id":{},"nodeId":"final_output","typeId":"system.final_output","handle":"final_output"}}
        ],"wires":[
          {{"fromNode":{},"fromPort":"out","toNode":{},"toPort":"in"}},
          {{"fromNode":{},"fromPort":"out","toNode":{},"toPort":"in"}},
          {{"fromNode":{},"fromPort":"out","toNode":{},"toPort":"in"}}
        ]}}"#,
        next + 1,
        next + 2,
        next + 3,
        next + 4,
        next + 1,
        next + 2,
        next + 1,
        next + 3,
        next + 2,
        next + 4,
    );
    let mut ordinary: EffectGraphDef = serde_json::from_str(&json).expect("CPU graph JSON");
    let scaler_a = next + 2;
    let scaler_b = next + 3;
    let uv = next + 1;
    let final_output = next + 4;
    assert_eq!(
        nodes
            .iter()
            .filter(|node| node.type_id == "node.envelope_beats")
            .count(),
        routes.len()
    );
    for route in routes {
        let count = prepared
            .nodes
            .iter()
            .find(|node| node.node_id == route.count_node)
            .expect("count producer")
            .id;
        let gate = prepared
            .wires
            .iter()
            .find(|wire| wire.from_node == count && wire.to_port == "trigger_count")
            .expect("gate trigger wire")
            .to_node;
        let envelope = prepared
            .wires
            .iter()
            .find(|wire| wire.from_node == gate && wire.to_port == "trigger")
            .expect("envelope trigger wire")
            .to_node;
        wires.push(EffectGraphWire {
            from_node: envelope,
            from_port: "out".into(),
            to_node: if route.modifier_id.as_str() == "event_a" {
                scaler_a
            } else {
                scaler_b
            },
            to_port: "scale".into(),
        });
    }
    wires.push(EffectGraphWire {
        from_node: uv,
        from_port: "out".into(),
        to_node: scaler_a,
        to_port: "in".into(),
    });
    wires.push(EffectGraphWire {
        from_node: scaler_a,
        from_port: "out".into(),
        to_node: scaler_b,
        to_port: "in".into(),
    });
    wires.push(EffectGraphWire {
        from_node: scaler_b,
        from_port: "out".into(),
        to_node: final_output,
        to_port: "in".into(),
    });
    nodes.append(&mut ordinary.nodes);
    EffectGraphDef {
        version: 1,
        name: Some("Modifier Event CPU Response".into()),
        description: None,
        preset_metadata: None,
        scene_modifiers: vec![],
        nodes,
        wires,
    }
}

fn run_at(runtime: &mut PresetRuntime, beat: f32) {
    runtime.set_frame_context(FrameContextInputs {
        time: beat,
        beat,
        aspect: 1.0,
        trigger_count: 0.0,
        anim_progress: 0.0,
        output_width: 1920.0,
        output_height: 1080.0,
    });
    runtime.execute_frame(FrameTime {
        beats: Beats(f64::from(beat)),
        seconds: Seconds(f64::from(beat)),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    });
}

fn param(runtime: &PresetRuntime, node_id: &NodeId, name: &str) -> f32 {
    let id = runtime
        .graph
        .instance_by_node_id(node_id)
        .expect("observed node");
    let stable_id = runtime
        .graph
        .get_node(id)
        .expect("observed node")
        .node_id
        .clone();
    runtime
        .live_node_params_watched()
        .into_iter()
        .find(|(node, _)| *node == stable_id)
        .and_then(|(_, values)| {
            values
                .into_iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value)
        })
        .expect("scalar observation")
}

#[test]
fn scene_modifier_event_runtime_emits_beat_window_pulse() {
    let (mut runtime, _, routes) = control_runtime();
    runtime.note_modifier_clip_event(None);
    run_at(&mut runtime, 0.0);
    let envelope = runtime
        .graph
        .nodes()
        .find(|node| node.node.type_id().as_str() == "node.envelope_beats")
        .expect("envelope node")
        .node_id
        .clone();
    assert_eq!(routes.len(), 2);
    assert_eq!(param(&runtime, &envelope, "window_beats"), 0.25);

    let scaler = NodeId::new("scaler_a");
    assert!((param(&runtime, &scaler, "scale") - 1.0).abs() < 1e-6);
    run_at(&mut runtime, 0.125);
    assert!((param(&runtime, &scaler, "scale") - 0.5).abs() < 1e-6);
    run_at(&mut runtime, 0.25);
    assert!(param(&runtime, &scaler, "scale").abs() < 1e-6);
}

#[test]
fn scene_modifier_event_runtime_loads_canonical_in_both_modes() {
    let owner = canonical_owner();
    let registry = PrimitiveRegistry::with_builtin();
    let prepared =
        crate::node_graph::scene_modifier_expand::prepare_scene_modifiers(&owner, &registry)
            .unwrap();
    for fused in [false, true] {
        let mut runtime =
            PresetRuntime::from_def_for_render(owner.clone(), &registry, None, fused).unwrap();
        assert!(runtime.note_modifier_audio_event("gate_a"));
        runtime.set_frame_context(FrameContextInputs {
            time: 0.0,
            beat: 0.0,
            aspect: 1.0,
            trigger_count: 99.0,
            anim_progress: 0.0,
            output_width: 1920.0,
            output_height: 1080.0,
        });
        for route in &prepared.event_routes {
            let id = runtime
                .graph
                .instance_by_node_id(&route.count_node)
                .unwrap();
            let expected = if route.modifier_id.as_str() == "event_a" {
                1.0
            } else {
                0.0
            };
            assert_eq!(
                runtime.graph.get_node(id).unwrap().params.get("value"),
                Some(&crate::node_graph::ParamValue::Float(expected))
            );
        }
    }
}

#[test]
fn scene_modifier_event_runtime_audio_targets_one_modifier_and_clip_targets_both() {
    let (mut runtime, _, _) = control_runtime();
    assert!(runtime.note_modifier_audio_event("gate_a"));
    run_at(&mut runtime, 0.0);
    assert_eq!(param(&runtime, &NodeId::new("scaler_a"), "scale"), 1.0);
    assert_eq!(param(&runtime, &NodeId::new("scaler_b"), "scale"), 0.0);
    run_at(&mut runtime, 0.25);
    runtime.note_modifier_clip_event(None);
    run_at(&mut runtime, 0.5);
    assert_eq!(param(&runtime, &NodeId::new("scaler_a"), "scale"), 1.0);
    assert_eq!(param(&runtime, &NodeId::new("scaler_b"), "scale"), 1.0);
}

#[test]
fn scene_modifier_event_runtime_carries_pending_event_by_modifier_identity() {
    let (mut prior, mut owner, routes) = control_runtime();
    run_at(&mut prior, 0.0);
    assert!(prior.note_modifier_audio_event("gate_a"));

    owner.scene_modifiers.reverse();
    let (mut rebuilt, _, rebuilt_routes) = control_runtime_for(owner);
    assert_ne!(routes, rebuilt_routes);
    rebuilt.carry_modifier_control_state_from(&mut prior);
    run_at(&mut rebuilt, 0.0);
    assert_eq!(param(&rebuilt, &NodeId::new("scaler_a"), "scale"), 1.0);
    assert_eq!(param(&rebuilt, &NodeId::new("scaler_b"), "scale"), 0.0);
}
