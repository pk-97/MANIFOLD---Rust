//! CPU ancestry sampling for Physics World inputs.

use super::*;

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
        .filter(|node| node.node.type_id().as_str() == "node.physics_world")
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
        pending.extend(graph.wires_into(node_id).map(|wire| wire.from.0));
    }
    for node_id in &ancestry {
        let node = graph.get_node(*node_id).expect("ancestry node exists");
        let kind = node.node.type_id();
        let type_id = kind.as_str();
        let stateless_cpu = matches!(
            type_id,
            "node.physics_world"
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
                "Physics World cannot sample historical Animated motion through `{type_id}`; use stateless CPU controls before the rigid body"
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
    /// Re-evaluate only stateless CPU producers feeding Physics World at a
    /// stable 240 Hz wall-clock grid. Four authored samples per solver tick
    /// preserve supported nonlinear LFO/beat motion independently of render
    /// frame partitioning, including when preview still owes native ticks.
    pub(super) fn sample_physics_history(
        &mut self,
        current: FrameTime,
        frame_context: Option<FrameContextInputs>,
    ) {
        let (Some(previous), Some(_)) = (
            self.last_physics_frame_time,
            self.physics_sample_steps.as_ref(),
        ) else {
            return;
        };
        let gap = current.seconds.0 - previous.seconds.0;
        if gap <= 0.0 {
            return;
        }
        const SAMPLE_RATE: f64 = 240.0;
        let mut grid = (previous.seconds.0 * SAMPLE_RATE).floor() + 1.0;
        let mut last_time = previous.seconds.0;
        let _scope = crate::node_graph::physics::PhysicsAuthoredSampleScope::new();
        while grid / SAMPLE_RATE < current.seconds.0 - 1.0e-9 {
            let time = grid / SAMPLE_RATE;
            let alpha = (time - previous.seconds.0) / gap;
            let beat = previous.beats.0 + (current.beats.0 - previous.beats.0) * alpha;
            let sample = FrameTime {
                beats: Beats(beat),
                seconds: Seconds(time),
                delta: Seconds(time - last_time),
                frame_count: current.frame_count,
            };
            if let Some(context) = frame_context {
                self.set_frame_context(FrameContextInputs {
                    time: time as f32,
                    beat: beat as f32,
                    ..context
                });
            }
            self.executor.execute_physics_sample_frame(
                &mut self.graph,
                &self.plan,
                sample,
                self.physics_sample_steps.as_ref().expect("checked above"),
            );
            last_time = time;
            grid += 1.0;
        }
        if let Some(context) = frame_context {
            self.set_frame_context(context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;

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
}
