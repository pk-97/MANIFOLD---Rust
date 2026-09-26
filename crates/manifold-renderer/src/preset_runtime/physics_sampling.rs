//! CPU ancestry sampling for native rigid-body and fluid simulation inputs.

use super::*;
use crate::node_graph::ParamValues;
use crate::node_graph::physics::{PhysicsHistoryDrainScope, offline_simulation};

#[cfg(test)]
#[path = "physics_sampling_inputs_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "physics_history_drain_tests.rs"]
mod drain_tests;

/// The last observed external inputs to the stateless physics ancestry. Keys
/// and storage are prepared with the graph; capturing another frame only
/// replaces values (String/Table values retain their existing Arc storage).
pub(super) struct PhysicsInputSnapshot {
    values: Vec<Option<ParamValues>>,
    clock_steps: Vec<usize>,
}

impl PhysicsInputSnapshot {
    /// Rebuild-only remap: execution order may differ between fused and
    /// editable graphs, while held inputs must still belong to the old frame.
    pub(super) fn carry_from(&mut self, prior: &Self, steps: &[(usize, usize)]) {
        for &(new, old) in steps {
            self.values[new].clone_from(&prior.values[old]);
        }
    }

    pub(super) fn prepare(graph: &Graph, plan: &ExecutionPlan, steps: &[bool]) -> Self {
        assert_eq!(steps.len(), plan.steps().len());
        let mut clock_steps = Vec::new();
        let values = plan
            .steps()
            .iter()
            .zip(steps)
            .enumerate()
            .map(|(index, (step, &sampled))| {
                if !sampled {
                    return None;
                }
                let node = graph.get_node(step.node).expect("compiled physics node exists");
                if node.node.type_id().as_str() == GENERATOR_INPUT_TYPE_ID {
                    clock_steps.push(index);
                }
                Some(node.params.clone())
            })
            .collect();
        Self { values, clock_steps }
    }

    fn capture(&mut self, graph: &Graph, plan: &ExecutionPlan) {
        for (step, values) in plan.steps().iter().zip(&mut self.values) {
            let Some(values) = values else { continue };
            let node = graph.get_node(step.node).expect("compiled physics node exists");
            assert_eq!(
                values.len(), node.params.len(),
                "physics parameter shape requires rebuild"
            );
            for (name, value) in values {
                value.clone_from(
                    node.params.get(name.as_ref()).expect("prepared physics parameter exists"),
                );
            }
        }
    }

    fn set_sample_time(&mut self, time: FrameTime) {
        for &index in &self.clock_steps {
            let params = self.values[index].as_mut().expect("sampled clock has inputs");
            *params.get_mut("time").expect("generator input has time") =
                ParamValue::Float(time.seconds.0 as f32);
            *params.get_mut("beat").expect("generator input has beat") =
                ParamValue::Float(time.beats.0 as f32);
        }
    }
}

/// The retained CPU ancestry of every physics world. Historical sampling
/// evaluates this closure only; GPU nodes and stateful upstream nodes cannot
/// be replayed safely at a past transport time.
pub(super) fn physics_sample_steps(
    graph: &Graph,
    plan: &ExecutionPlan,
) -> Result<Option<Vec<bool>>, String> {
    use std::collections::HashSet;

    let mut pending: Vec<_> = graph
        .nodes()
        .filter(|node| {
            matches!(
                node.node.type_id().as_str(),
                "node.physics_world" | "node.fluid_surface"
            )
        })
        .map(|node| node.id)
        .collect();
    if pending.is_empty() {
        return Ok(None);
    }
    let mut ancestry = HashSet::new();
    while let Some(node_id) = pending.pop() {
        if !ancestry.insert(node_id) {
            continue;
        }
        let is_body = graph.get_node(node_id).is_some_and(|node| node.node.type_id().as_str() == "node.rigid_body");
        // Release events belong to their delivered frame. Historical pose
        // sampling holds the last output instead of replaying trigger state.
        let is_fluid_role = graph.get_node(node_id).is_some_and(|node| {
            node.node.type_id().as_str() == "node.fluid_role_source"
        });
        let is_fluid = graph.get_node(node_id).is_some_and(|node| {
            node.node.type_id().as_str() == "node.fluid_surface"
        });
        // A mesh description is setup state: its prepared geometry stays
        // fixed during historical live-control sampling. Do not replay GPU
        // sources or local topology transforms at historical timestamps.
        pending.extend(graph.wires_into(node_id)
            .filter(|wire| !(is_body && wire.to.1 == "release_count"))
            .filter(|wire| !(is_fluid && wire.to.1 == "domain"))
            .filter(|wire| !is_fluid_role || matches!(wire.to.1,
                "transform" | "role" | "enabled" | "velocity_x" | "velocity_y"
                | "velocity_z" | "inherit_motion" | "friction"))
            .map(|wire| wire.from.0));
    }
    for node_id in &ancestry {
        let node = graph.get_node(*node_id).expect("ancestry node exists");
        let kind = node.node.type_id();
        let type_id = kind.as_str();
        let stateless_cpu = matches!(
            type_id,
            "node.physics_world"
                | "node.fluid_surface"
                | "node.fluid_role_source"
                | "node.rigid_body"
                | "node.transform_3d"
                | "node.lfo"
                | "node.beat_ramp"
                | "system.generator_input"
                | "node.value"
                | "node.math"
                | "node.affine_scalar"
        ) || node.node.is_pure();
        let requires = node.node.requires();
        if !stateless_cpu || requires.gpu_encoder || requires.state_store {
            return Err(format!(
                "Physics cannot sample historical motion through `{type_id}`; use stateless CPU controls before the simulation"
            ));
        }
    }
    Ok(Some(
        plan.steps()
            .iter()
            .map(|step| ancestry.contains(&step.node))
            .collect(),
    ))
}

impl PresetRuntime {
    /// Observe controls at an event producer boundary without running native
    /// ticks or rendering. Close the held-input interval first, then record
    /// the current inputs at the same timestamp. Moving the observation anchor
    /// prevents the next rendered frame from replaying the interval twice.
    pub(super) fn observe_physics_at_source(&mut self, source: FrameTime) -> Result<(), String> {
        if !source.seconds.0.is_finite() || !source.beats.0.is_finite() {
            return Err("Impulse: source clock must be finite".into());
        }
        let previous = self.last_physics_frame_time.ok_or(
            "Impulse: render the scene before capturing an event",
        )?;
        if source.seconds.0 < previous.seconds.0 {
            return Err("Impulse: source precedes the latest physics observation".into());
        }
        if self.graph.prepared_param_violation().is_some() {
            return Err("Impulse: graph preparation is pending".into());
        }
        // A producer callback must never inherit offline history draining.
        let _scope = crate::node_graph::physics::PhysicsStepScope::for_render(false);
        self.sample_physics_history(source);
        let (Some(inputs), Some(steps)) = (
            self.physics_input_snapshot.as_mut(),
            self.physics_sample_steps.as_ref(),
        ) else {
            return Err("Impulse: graph has no prepared physics ancestry".into());
        };
        inputs.set_sample_time(source);
        self.executor.execute_physics_sample_frame(
            &mut self.graph,
            &self.plan,
            source,
            steps,
            &inputs.values,
        );
        self.last_physics_frame_time = Some(source);
        Ok(())
    }

    /// Sample stateless authored motion on the existing 240 Hz grid, holding
    /// external parameters at their last observed values. Today's parameters
    /// must not be substituted into an earlier tick. The final left-limit
    /// sample closes the old interval before the full frame applies edits at
    /// the same timestamp; InputHistory preserves that discontinuity. Offline
    /// catch-up drains native ticks in bounded input batches without publishing
    /// intermediate graph outputs. Preview continues to retain its time debt.
    pub(super) fn sample_physics_history(&mut self, current: FrameTime) {
        let (Some(inputs), Some(steps)) = (
            self.physics_input_snapshot.as_mut(),
            self.physics_sample_steps.as_ref(),
        ) else {
            return;
        };
        let Some(previous) = self.last_physics_frame_time else {
            inputs.capture(&self.graph, &self.plan);
            return;
        };
        let gap = current.seconds.0 - previous.seconds.0;
        if !gap.is_finite() || gap <= 0.0 {
            inputs.capture(&self.graph, &self.plan);
            return;
        }
        let drain_offline = offline_simulation();
        if drain_offline {
            // An offline render may inherit a preview backlog. Drain the
            // already-observed prefix before inserting another sample into a
            // nearly-full history; the current edit must not reach that prefix.
            let _drain = PhysicsHistoryDrainScope::new();
            let sample = FrameTime { delta: Seconds::ZERO, ..previous };
            inputs.set_sample_time(sample);
            self.executor.execute_physics_sample_frame(
                &mut self.graph,
                &self.plan,
                sample,
                steps,
                &inputs.values,
            );
        }
        const SAMPLE_RATE: f64 = 240.0;
        // Leave room for the retained tick endpoints and edit discontinuities.
        // This bounds input storage, not the amount of requested offline time.
        const DRAIN_INTERVAL: usize =
            crate::node_graph::physics::AUTHORED_HISTORY_CAPACITY / 4;
        let mut samples_since_drain = 0;
        let mut grid = (previous.seconds.0 * SAMPLE_RATE).floor() + 1.0;
        let mut last_time = previous.seconds.0;
        while grid / SAMPLE_RATE < current.seconds.0 {
            let time = grid / SAMPLE_RATE;
            let alpha = (time - previous.seconds.0) / gap;
            let beat = previous.beats.0 + (current.beats.0 - previous.beats.0) * alpha;
            let sample = FrameTime {
                beats: Beats(beat),
                seconds: Seconds(time),
                delta: Seconds(time - last_time),
                frame_count: current.frame_count,
            };
            inputs.set_sample_time(sample);
            samples_since_drain += 1;
            let drain = drain_offline && samples_since_drain == DRAIN_INTERVAL;
            let _drain = drain.then(PhysicsHistoryDrainScope::new);
            self.executor.execute_physics_sample_frame(
                &mut self.graph,
                &self.plan,
                sample,
                steps,
                &inputs.values,
            );
            if drain {
                samples_since_drain = 0;
            }
            last_time = time;
            grid += 1.0;
        }
        let closing = FrameTime {
            delta: Seconds(current.seconds.0 - last_time),
            ..current
        };
        inputs.set_sample_time(closing);
        let _drain = drain_offline.then(PhysicsHistoryDrainScope::new);
        self.executor.execute_physics_sample_frame(
            &mut self.graph,
            &self.plan,
            closing,
            steps,
            &inputs.values,
        );
        inputs.capture(&self.graph, &self.plan);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;

    #[test]
    fn scene_physics_role_history_samples_live_controls_without_rendering() {
        let mut def: serde_json::Value = serde_json::from_str(include_str!(
            "../../assets/generator-presets/WaterBasin.json"
        )).unwrap();
        def["nodes"].as_array_mut().unwrap().push(serde_json::json!({
            "id": 500, "nodeId": "pouring_mesh", "typeId": "node.fluid_role_source"
        }));
        def["nodes"].as_array_mut().unwrap().push(serde_json::json!({
            "id": 501, "nodeId": "visible_source", "typeId": "node.cube_mesh"
        }));
        def["wires"].as_array_mut().unwrap().extend([
            serde_json::json!({"fromNode": 5, "fromPort": "transform", "toNode": 500, "toPort": "transform"}),
            serde_json::json!({"fromNode": 501, "fromPort": "source", "toNode": 500, "toPort": "mesh_0"}),
            serde_json::json!({"fromNode": 500, "fromPort": "role", "toNode": 4, "toPort": "role_0"}),
        ]);
        let runtime = PresetRuntime::from_json_str(&def.to_string(), &PrimitiveRegistry::with_builtin())
            .expect("typed fluid role ancestry loads");
        let mut saw_source = false;
        let mut saw_motion = false;
        for (step, sampled) in runtime.plan.steps().iter().zip(runtime.physics_sample_steps.as_ref().unwrap()) {
            let kind = runtime.graph.get_node(step.node).unwrap().node.type_id();
            match kind.as_str() {
                "node.fluid_role_source" => { assert!(sampled); saw_source = true; }
                "node.lfo" => { assert!(sampled); saw_motion = true; }
                "node.scene_object" | "node.render_scene" | "node.cube_mesh" => assert!(!sampled),
                _ => {}
            }
        }
        assert!(saw_source && saw_motion);
    }

    #[test]
    fn water_history_samples_fluid_controls_without_rendering() {
        let runtime = PresetRuntime::from_json_str(
            include_str!("../../assets/generator-presets/WaterBasin.json"),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("WaterBasin loads");
        let mask = runtime
            .physics_sample_steps
            .as_ref()
            .expect("fluid ancestry");
        let sampled: Vec<_> = runtime
            .plan
            .steps()
            .iter()
            .zip(mask)
            .filter(|(_, enabled)| **enabled)
            .map(|(step, _)| {
                runtime
                    .graph
                    .get_node(step.node)
                    .unwrap()
                    .node
                    .type_id()
                    .as_str()
                    .to_owned()
            })
            .collect();
        assert!(sampled.iter().any(|kind| kind == "node.fluid_surface"));
        assert!(sampled.iter().any(|kind| kind == "node.lfo"));
        assert!(!sampled.iter().any(|kind| kind == "node.scene_object"));
        assert!(!sampled.iter().any(|kind| kind == "node.render_scene"));
    }

    #[test]
    fn physics_history_mask_includes_nonlinear_lfo_and_excludes_rendering() {
        let mut def: serde_json::Value = serde_json::from_str(include_str!(
            "../../assets/generator-presets/PhysicsSolids.json"
        ))
        .unwrap();
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 500,
                "nodeId": "animated_x",
                "typeId": "node.lfo",
                "params": {
                    "rate_mode": { "type": "Enum", "value": 1 },
                    "angular_rate": { "type": "Float", "value": 12.0 },
                    "min": { "type": "Float", "value": -2.0 },
                    "max": { "type": "Float", "value": 2.0 }
                }
            }));
        def["wires"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "fromNode": 500, "fromPort": "out", "toNode": 100, "toPort": "pos_x"
            }));
        let runtime = PresetRuntime::from_json_str(
            &serde_json::to_string(&def).unwrap(),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("PhysicsSolids with an LFO-authored body loads");
        let mask = runtime
            .physics_sample_steps
            .as_ref()
            .expect("physics ancestry");
        let sampled: Vec<_> = runtime
            .plan
            .steps()
            .iter()
            .zip(mask)
            .filter(|(_, enabled)| **enabled)
            .map(|(step, _)| {
                runtime
                    .graph
                    .get_node(step.node)
                    .unwrap()
                    .node
                    .type_id()
                    .as_str()
                    .to_owned()
            })
            .collect();
        assert!(sampled.iter().any(|kind| kind == "node.lfo"));
        assert!(sampled.iter().any(|kind| kind == "node.physics_world"));
        assert!(!sampled.iter().any(|kind| kind == "node.render_scene"));
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn explicit_reset_clears_physics_history_clock() {
        let mut runtime = PresetRuntime::from_json_str(
            include_str!("../../assets/generator-presets/PhysicsSolids.json"),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("PhysicsSolids loads");
        runtime.last_physics_frame_time = Some(FrameTime {
            beats: Beats(1.0),
            seconds: Seconds(1.0),
            delta: Seconds(1.0),
            frame_count: 1,
        });
        runtime.reset_state(&crate::test_device());
        assert!(runtime.last_physics_frame_time.is_none());
    }

    #[test]
    fn physics_history_holds_release_events_without_replaying_trigger_state() {
        let mut def: serde_json::Value = serde_json::from_str(include_str!(
            "../../assets/generator-presets/PhysicsSolids.json"
        )).unwrap();
        def["nodes"].as_array_mut().unwrap().push(serde_json::json!({
            "id": 500, "nodeId": "release_event", "typeId": "node.trigger_gate"
        }));
        def["wires"].as_array_mut().unwrap().push(serde_json::json!({
            "fromNode": 500, "fromPort": "out", "toNode": 111, "toPort": "release_count"
        }));
        let registry = PrimitiveRegistry::with_builtin();
        let runtime = PresetRuntime::from_json_str(&def.to_string(), &registry)
            .expect("release events are not replayed during historical pose sampling");
        let event = runtime.graph.instance_by_node_id(&manifold_core::NodeId::new("release_event")).unwrap();
        for (step, sampled) in runtime.plan.steps().iter().zip(runtime.physics_sample_steps.as_ref().unwrap()) {
            if step.node == event { assert!(!sampled); }
        }
        def["wires"].as_array_mut().unwrap().push(serde_json::json!({
            "fromNode": 500, "fromPort": "out", "toNode": 110, "toPort": "pos_x"
        }));
        assert!(PresetRuntime::from_json_str(&def.to_string(), &registry).is_err(),
            "stateful pose ancestry must still be rejected");
    }
}
