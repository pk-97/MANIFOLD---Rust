use super::*;
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind};
use crate::node_graph::{EffectNode, EffectNodeContext, EffectNodeType, ParamDef};
use manifold_core::Beats;
use manifold_physics::VectorField;
use std::{borrow::Cow, cell::Cell};

#[path = "source_tests.rs"]
mod source_tests;
#[path = "scene_routes_tests.rs"]
mod scene_routes_tests;
#[cfg(feature = "gpu-proofs")]
#[path = "coupled_playback_tests.rs"]
mod coupled_playback_tests;

const DT: f64 = 1.0 / 60.0;
thread_local! { static POSITIONS: Cell<[f32; 2]> = const { Cell::new([0.0; 2]) }; }
struct ObservePositions(EffectNodeType);
// The native worlds and field graph are real. Only raster presentation is a
// no-op, keeping these scheduling proofs CPU-only.
struct CpuScene(crate::node_graph::primitives::render_scene::RenderScene);
impl EffectNode for CpuScene {
    fn type_id(&self) -> &EffectNodeType {
        self.0.type_id()
    }
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        self.0.depth_rule()
    }
    fn inputs(&self) -> &[NodeInput] {
        self.0.inputs()
    }
    fn outputs(&self) -> &[NodeOutput] {
        self.0.outputs()
    }
    fn parameters(&self) -> &[ParamDef] {
        self.0.parameters()
    }
    fn reconfigure(&mut self, params: &ParamValues) {
        self.0.reconfigure(params);
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}
impl EffectNode for ObservePositions {
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn inputs(&self) -> &[NodeInput] {
        static PORTS: [NodeInput; 2] = [
            NodePort {
                name: Cow::Borrowed("a"),
                ty: PortType::Transform,
                kind: PortKind::Input,
                required: true,
            },
            NodePort {
                name: Cow::Borrowed("b"),
                ty: PortType::Transform,
                kind: PortKind::Input,
                required: true,
            },
        ];
        &PORTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        POSITIONS.set(["a", "b"].map(|port| ctx.inputs.transform(port).unwrap().pos[0]));
    }
}

fn fixture() -> EffectGraphDef {
    serde_json::from_value(serde_json::json!({
        "version": 3, "name": "Captured scene impulses",
        "nodes": [
            {"id":0,"nodeId":"clock","typeId":"system.generator_input"},
            {"id":1,"nodeId":"field","typeId":"node.uniform_vector_field","params":{
                "x":{"type":"Float","value":2.0}, "y":{"type":"Float","value":0.0}}},
            {"id":2,"nodeId":"pose_a","typeId":"node.transform_3d","params":{"pos_y":{"type":"Float","value":10.0}}},
            {"id":3,"nodeId":"pose_b","typeId":"node.transform_3d","params":{
                "pos_x":{"type":"Float","value":5.0},"pos_y":{"type":"Float","value":10.0}}},
            {"id":4,"nodeId":"body_a","typeId":"node.rigid_body"},
            {"id":5,"nodeId":"body_b","typeId":"node.rigid_body"},
            {"id":6,"nodeId":"world","typeId":"node.physics_world","params":{"gravity_y":{"type":"Float","value":0.0}}},
            {"id":7,"nodeId":"part_a","typeId":"node.scene_object"},
            {"id":8,"nodeId":"part_a_2","typeId":"node.scene_object"},
            {"id":9,"nodeId":"part_b","typeId":"node.scene_object"},
            {"id":10,"nodeId":"scene","typeId":"node.render_scene","params":{"objects":{"type":"Int","value":3}}},
            {"id":11,"nodeId":"output","typeId":"system.final_output"},
            {"id":20,"nodeId":"camera","typeId":"node.look_at_camera"},
            {"id":12,"nodeId":"observer","typeId":"test.positions"}
        ],
        "wires": [
            {"fromNode":20,"fromPort":"out","toNode":10,"toPort":"camera"},
            {"fromNode":1,"fromPort":"out","toNode":6,"toPort":"acceleration_field"},
            {"fromNode":2,"fromPort":"transform","toNode":4,"toPort":"transform"},
            {"fromNode":3,"fromPort":"transform","toNode":5,"toPort":"transform"},
            {"fromNode":4,"fromPort":"body","toNode":6,"toPort":"body_0"},
            {"fromNode":5,"fromPort":"body","toNode":6,"toPort":"body_1"},
            {"fromNode":6,"fromPort":"pose_0","toNode":7,"toPort":"transform"},
            {"fromNode":6,"fromPort":"pose_0","toNode":8,"toPort":"transform"},
            {"fromNode":6,"fromPort":"pose_1","toNode":9,"toPort":"transform"},
            {"fromNode":7,"fromPort":"object","toNode":10,"toPort":"object_0"},
            {"fromNode":8,"fromPort":"object","toNode":10,"toPort":"object_1"},
            {"fromNode":9,"fromPort":"object","toNode":10,"toPort":"object_2"},
            {"fromNode":10,"fromPort":"color","toNode":11,"toPort":"in"},
            {"fromNode":6,"fromPort":"pose_0","toNode":12,"toPort":"a"},
            {"fromNode":6,"fromPort":"pose_1","toNode":12,"toPort":"b"}
        ]
    })).unwrap()
}
fn registry() -> PrimitiveRegistry {
    #[cfg(feature = "gpu-proofs")]
    let mut registry = PrimitiveRegistry::with_cpu_flip_reference();
    #[cfg(not(feature = "gpu-proofs"))]
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("node.render_scene", || {
        Box::new(CpuScene(
            crate::node_graph::primitives::render_scene::RenderScene::new(),
        ))
    });
    registry.register("test.positions", || {
        Box::new(ObservePositions(EffectNodeType::new("test.positions")))
    });
    registry
}
fn runtime(def: &EffectGraphDef) -> PresetRuntime {
    PresetRuntime::from_json_str(&serde_json::to_string(def).unwrap(), &registry()).unwrap()
}
fn time(seconds: f64) -> FrameTime {
    FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds::ZERO,
        frame_count: 0,
    }
}
fn reference(id: &str) -> SceneNodeRef {
    SceneNodeRef {
        scope: vec![],
        node: NodeId::new(id),
    }
}
fn prepare(runtime: &PresetRuntime, def: &EffectGraphDef, ids: &[&str]) -> PreparedSceneImpulse {
    runtime
        .prepare_scene_impulse(
            def,
            &reference("scene"),
            &SceneTargetSelection::Explicit {
                objects: ids.iter().map(|id| reference(id)).collect(),
            },
            &NodeId::new("field"),
            "out",
        )
        .unwrap()
}
fn edit(runtime: &mut PresetRuntime, node: &str, param: &str, value: f32) {
    let id = runtime
        .graph
        .instance_by_node_id(&NodeId::new(node))
        .unwrap();
    runtime
        .graph
        .set_param(id, param, ParamValue::Float(value))
        .unwrap();
}
fn capture(
    runtime: &mut PresetRuntime,
    binding: &mut PreparedSceneImpulse,
    target: &mut CapturedSceneImpulse,
    sequence: u64,
) {
    runtime
        .capture_scene_impulse(binding, target, time(0.0), sequence, |_, source| Ok(source))
        .unwrap();
}

#[test]
fn scene_impulse_captures_fields_and_targets_before_edits_and_applies_once() {
    let def = fixture();
    // The same field also supplies continuous acceleration. Compare native
    // runs to isolate the impulse without assuming a CCD substep count.
    let mut control = runtime(&def);
    control.execute_frame(time(0.0));
    edit(&mut control, "field", "x", -10.0);
    control.execute_frame(time(DT));
    let baseline = POSITIONS.get();
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let mut first = prepare(&runtime, &def, &["part_a", "part_a_2"]);
    let mut hit_a = first.new_capture();
    capture(&mut runtime, &mut first, &mut hit_a, 0);
    edit(&mut runtime, "field", "x", 7.0);
    let mut second = prepare(&runtime, &def, &["part_b"]);
    let mut hit_b = second.new_capture();
    capture(&mut runtime, &mut second, &mut hit_b, 1);
    edit(&mut runtime, "field", "x", -10.0);
    assert_eq!(
        POSITIONS.get(),
        [0.0, 5.0],
        "capture cannot step native worlds"
    );
    runtime.deliver_scene_impulse(&mut hit_a).unwrap();
    runtime.deliver_scene_impulse(&mut hit_a).unwrap();
    runtime.deliver_scene_impulse(&mut hit_b).unwrap();
    assert!(hit_a.is_scheduled() && hit_b.is_scheduled());
    runtime.execute_frame(time(DT));
    let positions = POSITIONS.get();
    assert!(
        (positions[0] - baseline[0] - (2.0 * DT) as f32).abs() < 1e-5,
        "{positions:?}"
    );
    assert!(
        (positions[1] - baseline[1] - (7.0 * DT) as f32).abs() < 1e-5,
        "{positions:?}"
    );
    let mut receipts = Vec::new();
    runtime.drain_scene_impulses(|id, event| {
        assert_eq!(id.as_str(), "world");
        receipts.push(event);
    });
    assert_eq!(
        receipts.len(),
        2,
        "parts and retries must not multiply a hit"
    );
    for (index, event) in receipts.iter().enumerate() {
        assert_eq!(event.source.sequence, index as u64);
        assert_eq!(event.applied.tick, 0);
        assert_eq!(
            event.value.field.sample([0.0; 3]),
            [if index == 0 { 2.0 } else { 7.0 }, 0.0, 0.0]
        );
    }
    runtime.drain_scene_impulses(|_, _| panic!("receipts must drain once"));
}

#[test]
fn scene_impulse_requires_initialized_recipients_and_explicit_clock_mapping() {
    let def = fixture();
    let mut runtime = runtime(&def);
    let mut binding = prepare(&runtime, &def, &["part_a"]);
    let mut hit = binding.new_capture();
    assert!(
        runtime
            .capture_scene_impulse(&mut binding, &mut hit, time(0.0), 0, |_, t| Ok(t))
            .unwrap_err()
            .contains("not initialized")
    );
    runtime.execute_frame(time(0.0));
    assert!(
        runtime
            .capture_scene_impulse(&mut binding, &mut hit, time(12.0), 0, |_, _| Err(
                "clock unavailable".into()
            ))
            .unwrap_err()
            .contains("clock unavailable")
    );
    assert!(hit.field.is_none());
    runtime
        .capture_scene_impulse(&mut binding, &mut hit, time(12.0), 0, |_, t| {
            Ok(Seconds(t.0 - 12.0))
        })
        .unwrap();
    assert_eq!(hit.source_time().unwrap().seconds, Seconds(12.0));
    runtime.deliver_scene_impulse(&mut hit).unwrap();
    assert_eq!(hit.scheduled_ticks().next().unwrap().1.unwrap().tick, 0);
}

#[test]
fn scene_impulse_does_not_overwrite_pending_capture_or_reallocate_recipient_storage() {
    let def = fixture();
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let mut binding = prepare(&runtime, &def, &["part_a", "part_b"]);
    let mut hit = binding.new_capture();
    let pointer = hit.stamps.as_ptr();
    capture(&mut runtime, &mut binding, &mut hit, 0);
    edit(&mut runtime, "field", "x", 9.0);
    assert!(
        runtime
            .capture_scene_impulse(&mut binding, &mut hit, time(1.0), 1, |_, t| Ok(t))
            .is_err()
    );
    assert_eq!(
        hit.field.as_ref().unwrap().sample([0.0; 3]),
        [2.0, 0.0, 0.0]
    );
    hit.clear();
    capture(&mut runtime, &mut binding, &mut hit, 1);
    assert_eq!(pointer, hit.stamps.as_ptr());
    assert_eq!(
        hit.field.as_ref().unwrap().sample([0.0; 3]),
        [9.0, 0.0, 0.0]
    );
    assert_eq!(
        hit.recipients.len(),
        1,
        "both body slots share one queue entry"
    );
}

#[test]
fn scene_impulse_rejects_reset_and_replacement_worlds() {
    let def = fixture();
    let mut first_runtime = runtime(&def);
    first_runtime.execute_frame(time(0.0));
    let mut binding = prepare(&first_runtime, &def, &["part_a"]);
    let mut hit = binding.new_capture();
    capture(&mut first_runtime, &mut binding, &mut hit, 0);
    let mut replacement = runtime(&def);
    replacement.execute_frame(time(0.0));
    assert!(
        replacement
            .deliver_scene_impulse(&mut hit)
            .unwrap_err()
            .contains("rebuilt")
    );
    edit(&mut first_runtime, "world", "reset", 1.0);
    first_runtime.execute_frame(time(0.0));
    assert!(
        first_runtime
            .deliver_scene_impulse(&mut hit)
            .unwrap_err()
            .contains("reset")
    );
    assert!(!hit.is_scheduled());
}

#[test]
fn scene_impulse_invalid_field_never_reuses_previous_value() {
    let def = fixture();
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let mut binding = prepare(&runtime, &def, &["part_a"]);
    let mut hit = binding.new_capture();
    capture(&mut runtime, &mut binding, &mut hit, 0);
    hit.clear();
    let field = runtime
        .graph
        .instance_by_node_id(&NodeId::new("field"))
        .unwrap();
    runtime
        .graph
        .set_param_unchecked(field, "x", ParamValue::Float(f32::NAN));
    assert!(
        runtime
            .capture_scene_impulse(&mut binding, &mut hit, time(0.0), 1, |_, t| Ok(t))
            .is_err()
    );
    assert!(hit.field.is_none());
    assert!(runtime.deliver_scene_impulse(&mut hit).is_err());
    edit(&mut runtime, "field", "x", 3.0);
    capture(&mut runtime, &mut binding, &mut hit, 1);
    assert_eq!(
        hit.field.as_ref().unwrap().sample([0.0; 3]),
        [3.0, 0.0, 0.0]
    );
}

#[test]
fn scene_impulse_samples_clock_at_capture_without_rewriting_graph_controls() {
    let mut def = fixture();
    def.wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: 0,
            from_port: "time".into(),
            to_node: 1,
            to_port: "x".into(),
        });
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let mut binding = prepare(&runtime, &def, &["part_a"]);
    let mut hit = binding.new_capture();
    runtime
        .capture_scene_impulse(&mut binding, &mut hit, time(0.125), 0, |_, _| {
            Ok(Seconds::ZERO)
        })
        .unwrap();
    assert_eq!(
        hit.field.as_ref().unwrap().sample([0.0; 3]),
        [0.125, 0.0, 0.0]
    );
    let clock = runtime
        .graph
        .instance_by_node_id(&NodeId::new("clock"))
        .unwrap();
    assert_eq!(
        runtime.graph.get_node(clock).unwrap().params["time"],
        ParamValue::Float(0.0)
    );
    assert_eq!(POSITIONS.get(), [0.0, 5.0]);
}

#[test]
fn scene_impulse_rejects_stateful_ancestry_and_inactive_selections() {
    let mut def = fixture();
    def.nodes.push(
        serde_json::from_value(
            serde_json::json!({"id":13,"nodeId":"smooth","typeId":"node.smoothing"}),
        )
        .unwrap(),
    );
    def.wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: 0,
            from_port: "time".into(),
            to_node: 13,
            to_port: "in".into(),
        });
    def.wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: 13,
            from_port: "out".into(),
            to_node: 1,
            to_port: "x".into(),
        });
    let error = PresetRuntime::from_json_str(&serde_json::to_string(&def).unwrap(), &registry())
        .err()
        .unwrap();
    assert!(error.to_string().contains("smoothing"), "{error}");
    let def = fixture();
    let runtime = runtime(&def);
    assert!(
        runtime
            .prepare_scene_impulse(
                &def,
                &reference("scene"),
                &SceneTargetSelection::Explicit { objects: vec![] },
                &NodeId::new("field"),
                "out"
            )
            .is_err()
    );
}

#[test]
fn scene_impulse_partial_admission_retry_does_not_duplicate_successful_world() {
    use crate::node_graph::physics::RigidImpulseTargets;
    let mut def = fixture();
    let mut control = runtime(&def);
    control.execute_frame(time(0.0));
    control.execute_frame(time(DT));
    let baseline = POSITIONS.get()[0];
    let mut second = def
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == "world")
        .unwrap()
        .clone();
    second.id = 14;
    second.node_id = NodeId::new("world_b");
    def.nodes.push(second);
    def.wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: 4,
            from_port: "body".into(),
            to_node: 14,
            to_port: "body_0".into(),
        });
    let wire = def
        .wires
        .iter_mut()
        .find(|wire| wire.to_node == 9 && wire.to_port == "transform")
        .unwrap();
    wire.from_node = 14;
    wire.from_port = "pose_0".into();
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let second = runtime
        .graph
        .instance_by_node_id(&NodeId::new("world_b"))
        .unwrap();
    let node = &mut runtime.graph.get_node_mut(second).unwrap().node;
    let epoch = node.physics_impulse_epoch().unwrap();
    for sequence in 0..256 {
        node.enqueue_physics_impulse(
            EventStamp {
                epoch,
                time: Seconds(1.0),
                sequence,
            },
            ResolvedNodeImpulse {
                field: FieldValue::uniform([1.0, 0.0, 0.0]).unwrap(),
                target: ImpulseTarget::Rigid(RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                }),
            },
        )
        .unwrap();
    }
    let mut binding = prepare(&runtime, &def, &["part_a", "part_b"]);
    let mut hit = binding.new_capture();
    capture(&mut runtime, &mut binding, &mut hit, 1000);
    assert!(runtime.deliver_scene_impulse(&mut hit).is_err());
    assert_eq!(
        hit.scheduled_ticks()
            .filter(|(_, tick)| tick.is_some())
            .count(),
        1
    );
    assert!(!hit.is_scheduled());
    assert!(runtime.deliver_scene_impulse(&mut hit).is_err());
    runtime.execute_frame(time(DT));
    assert!((POSITIONS.get()[0] - baseline - (2.0 * DT) as f32).abs() < 1e-5);
    let mut receipts = 0;
    runtime.drain_scene_impulses(|id, event| {
        assert_eq!(id.as_str(), "world");
        assert_eq!(event.source.sequence, 1000);
        receipts += 1;
    });
    assert_eq!(receipts, 1);
}

#[test]
fn scene_impulse_captures_spatial_shape_before_center_edits() {
    let mut def = fixture();
    let node = def
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "field")
        .unwrap();
    node.type_id = "node.radial_vector_field".into();
    node.params.clear();
    for (name, value) in [
        ("center_x", -1.0),
        ("center_y", 10.0),
        ("radius", 5.0),
        ("falloff", 0.0),
    ] {
        node.params.insert(name.into(), ParamValue::Float(value).into());
    }
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let mut binding = prepare(&runtime, &def, &["part_a"]);
    let mut first = binding.new_capture();
    let mut second = binding.new_capture();
    capture(&mut runtime, &mut binding, &mut first, 0);
    edit(&mut runtime, "field", "center_x", 1.0);
    capture(&mut runtime, &mut binding, &mut second, 1);
    assert_eq!(
        first.field.as_ref().unwrap().sample([0.0, 10.0, 0.0]),
        [1.0, 0.0, 0.0]
    );
    assert_eq!(
        second.field.as_ref().unwrap().sample([0.0, 10.0, 0.0]),
        [-1.0, 0.0, 0.0]
    );
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn scene_impulse_selection_combines_body_slots_copies_and_fluid_domain() {
    use crate::node_graph::physics::RigidImpulseTargets;
    let mut def = fixture();
    def.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id":14, "nodeId":"fluid", "typeId":manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID
        }))
        .unwrap(),
    );
    def.wires
        .retain(|wire| !(matches!(wire.to_node, 8 | 9) && wire.to_port == "transform"));
    for (from_node, from_port, to_node, to_port) in [
        (4, "body", 6, "copies"),
        (6, "instances", 8, "instances"),
        (14, "vertices", 9, "vertices"),
    ] {
        def.wires
            .push(manifold_core::effect_graph_def::EffectGraphWire {
                from_node,
                from_port: from_port.into(),
                to_node,
                to_port: to_port.into(),
            });
    }
    let runtime = runtime(&def);
    let binding = prepare(&runtime, &def, &["part_a", "part_a_2", "part_b"]);
    assert_eq!(binding.recipients.len(), 1, "coupled participants share one native event owner");
    assert_eq!(binding.recipients[0].id.as_str(), "fluid");
    assert_eq!(
        binding.recipients[0].target,
        ImpulseTarget::FluidAndRigid(RigidImpulseTargets {
            bodies: 1,
            copies: true
        })
    );
}
