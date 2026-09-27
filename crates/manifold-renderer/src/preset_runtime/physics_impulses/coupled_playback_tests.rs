//! Exercise real paired native playback through the ordinary graph executor.
use super::*;
use crate::node_graph::fluid::CoupledRigidFrame;
use crate::node_graph::physics::{PhysicsStepScope, RigidImpulseTargets};

fn coupled_fixture() -> EffectGraphDef {
    let mut def = fixture();
    for node in &mut def.nodes {
        if node.node_id.as_str() == "pose_a" {
            node.params.insert(
                "pos_y".into(),
                manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1.0 },
            );
        }
        if node.node_id.as_str() == "body_a" {
            node.params.insert(
                "mass".into(),
                manifold_core::effect_graph_def::SerializedParamValue::Float { value: 90.0 },
            );
        }
    }
    // Preserve every existing rigid scene member; add the liquid as another
    // object using the same authoring path as an ordinary scene.
    def.nodes.extend([
        serde_json::from_value(serde_json::json!({
            "id":14,"nodeId":"fluid","typeId":"node.fluid_surface",
            "params":{
                "resolution":{"type":"Float","value":12.0},
                "gravity":{"type":"Float","value":0.0},
                "fill_height":{"type":"Float","value":0.5},
                "emission":{"type":"Float","value":0.0}
            }
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "id":15,"nodeId":"liquid","typeId":"node.scene_object"
        }))
        .unwrap(),
    ]);
    def.wires.extend([
        serde_json::from_value(serde_json::json!({
            "fromNode":14,"fromPort":"vertices","toNode":15,"toPort":"vertices"
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "fromNode":15,"fromPort":"object","toNode":10,"toPort":"object_3"
        }))
        .unwrap(),
    ]);
    def.nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "scene")
        .unwrap()
        .params
        .insert(
            "objects".into(),
            manifold_core::effect_graph_def::SerializedParamValue::Int { value: 4 },
        );
    def
}

fn paired_frame(runtime: &PresetRuntime) -> CoupledRigidFrame {
    let fluid = runtime
        .graph
        .instance_by_node_id(&NodeId::new("fluid"))
        .unwrap();
    runtime
        .graph
        .get_node(fluid)
        .unwrap()
        .node
        .coupled_rigid_frame()
        .expect("completed paired native frame")
        .clone()
}

fn assert_visible_pair(runtime: &PresetRuntime, frame: &CoupledRigidFrame) {
    assert_eq!(
        POSITIONS.get(),
        [frame.poses[0].pos[0], frame.poses[1].pos[0]],
        "downstream consumers must see the same rigid frame accepted by liquid"
    );
    let world = runtime
        .graph
        .instance_by_node_id(&NodeId::new("world"))
        .unwrap();
    assert!(
        runtime
            .graph
            .get_node(world)
            .unwrap()
            .node
            .physics_impulse_epoch()
            .is_none(),
        "the rigid publisher must not own another native simulation"
    );
}

#[test]
fn coupled_graph_preview_holds_pair_then_offline_drains_without_double_advancement() {
    let mut runtime = runtime(&coupled_fixture());
    runtime.execute_frame(time(0.0));
    let initial = paired_frame(&runtime);
    assert_eq!(initial.stamp.tick, 0);
    assert_visible_pair(&runtime, &initial);
    {
        let _preview = PhysicsStepScope::for_render(false);
        runtime.execute_frame(time(3.0 * DT));
        let held = paired_frame(&runtime);
        // Nonblocking submit cannot accept the newly submitted reply in this
        // call, even if the worker finishes immediately on another core.
        assert_eq!(held.stamp, initial.stamp);
        assert_eq!(held.poses, initial.poses);
        assert_visible_pair(&runtime, &held);
    }
    runtime.execute_frame(time(3.0 * DT));
    let caught_up = paired_frame(&runtime);
    assert_eq!(caught_up.stamp.tick, 3);
    assert_eq!(caught_up.stamp.epoch, initial.stamp.epoch);
    assert!(caught_up.poses[0].pos[0] > initial.poses[0].pos[0]);
    assert_visible_pair(&runtime, &caught_up);
    runtime.execute_frame(time(3.0 * DT));
    assert_eq!(
        paired_frame(&runtime).poses,
        caught_up.poses,
        "paused frame adds no tick"
    );
    assert_visible_pair(&runtime, &caught_up);
}

#[test]
fn coupled_graph_either_reset_restarts_both_visible_participants() {
    let mut runtime = runtime(&coupled_fixture());
    runtime.execute_frame(time(0.0));
    runtime.execute_frame(time(DT));
    let before = paired_frame(&runtime);
    for (node, value) in [("world", 1.0), ("fluid", 1.0), ("world", 2.0)] {
        let previous = paired_frame(&runtime).stamp;
        edit(&mut runtime, node, "reset", value);
        runtime.execute_frame(time(DT));
        let reset = paired_frame(&runtime);
        assert_eq!(reset.stamp.epoch, previous.epoch + 1);
        assert_eq!(reset.stamp.tick, 0);
        assert_eq!(reset.poses[0].pos[0], 0.0);
        assert_visible_pair(&runtime, &reset);
    }
    assert!(before.poses[0].pos[0] > 0.0);
    let previous = paired_frame(&runtime).stamp;
    edit(&mut runtime, "world", "reset", 3.0);
    edit(&mut runtime, "fluid", "reset", 2.0);
    runtime.execute_frame(time(DT));
    let reset = paired_frame(&runtime);
    assert_eq!(reset.stamp.epoch, previous.epoch + 1);
    assert_eq!(reset.stamp.tick, 0);
    assert_visible_pair(&runtime, &reset);
}

#[test]
fn coupled_graph_merges_shared_impulses_and_preserves_single_material_selections() {
    let def = coupled_fixture();
    let mut runtime = runtime(&def);
    runtime.execute_frame(time(0.0));
    let body = RigidImpulseTargets {
        bodies: 1,
        copies: false,
    };
    for (sequence, selection, target) in [
        (1, vec!["part_a"], ImpulseTarget::Rigid(body)),
        (2, vec!["liquid"], ImpulseTarget::Fluid),
        (
            3,
            vec!["part_a", "part_a_2", "liquid"],
            ImpulseTarget::FluidAndRigid(body),
        ),
    ] {
        let mut binding = prepare(&runtime, &def, &selection);
        assert_eq!(binding.recipients.len(), 1);
        assert_eq!(binding.recipients[0].id.as_str(), "fluid");
        assert_eq!(binding.recipients[0].target, target);
        let mut captured = binding.new_capture();
        runtime
            .capture_scene_impulse_at_source(&mut binding, &mut captured, time(0.0), sequence)
            .unwrap();
        runtime.deliver_scene_impulse(&mut captured).unwrap();
        runtime.deliver_scene_impulse(&mut captured).unwrap();
        assert_eq!(captured.scheduled_ticks().count(), 1);
    }
    runtime.execute_frame(time(DT));
    let mut receipts = Vec::new();
    runtime.drain_scene_impulses(|id, event| {
        assert_eq!(id.as_str(), "fluid");
        assert_eq!(event.applied.tick, 0);
        receipts.push((event.source.sequence, event.value.target));
    });
    assert_eq!(
        receipts,
        vec![
            (1, ImpulseTarget::Rigid(body)),
            (2, ImpulseTarget::Fluid),
            (3, ImpulseTarget::FluidAndRigid(body)),
        ]
    );
    assert_visible_pair(&runtime, &paired_frame(&runtime));
}
