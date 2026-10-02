//! CPU ancestry sampling for native rigid-body and fluid simulation inputs.

use super::*;
use crate::node_graph::ParamValues;
use crate::node_graph::physics::{PhysicsHistoryDrainScope, offline_simulation};
use crate::preset_context::ProjectTempo;
use manifold_core::liquid_domain::{FLIP_DOMAIN_TYPE_ID, GPU_FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID};
use manifold_core::tempo::TempoMapConverter;

#[cfg(test)]
#[path = "physics_sampling_inputs_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "physics_history_drain_tests.rs"]
mod drain_tests;

/// GPU liquids whose force field is evaluated at each tick's start.
fn gpu_liquid(kind: &str) -> bool {
    matches!(kind, GPU_FLIP_DOMAIN_TYPE_ID | MATTER_DOMAIN_TYPE_ID)
}

fn setup_input(kind: &str, port: &str) -> bool {
    if gpu_liquid(kind) {
        return port != "acceleration_field" && !port.starts_with("role_");
    }
    match kind {
        "node.rigid_body" => matches!(port, "release_count" | "source"),
        FLIP_DOMAIN_TYPE_ID => port == "domain",
        "node.fluid_role_source" => !matches!(
            port,
            "transform"
                | "role"
                | "enabled"
                | "velocity_x"
                | "velocity_y"
                | "velocity_z"
                | "inherit_motion"
                | "friction"
        ),
        _ => false,
    }
}

/// Historical CPU passes hold these full-frame outputs instead of evaluating
/// their setup/GPU ancestry. Pin them before compilation so the last ordinary
/// reader cannot return their slots to the pool between observations.
pub(super) fn retain_physics_setup_outputs(graph: &mut Graph) -> Result<(), GraphError> {
    let outputs: Vec<_> = graph
        .nodes()
        .flat_map(|node| {
            let kind = node.node.type_id().as_str();
            graph
                .wires_into(node.id)
                // A GPU liquid's sample run reads only its force field and roles.
                .filter(move |wire| !gpu_liquid(kind) && setup_input(kind, wire.to.1))
                .map(|wire| wire.from)
        })
        .collect();
    for (node, port) in outputs {
        graph.add_external_output(node, port)?;
    }
    Ok(())
}

/// The last observed external inputs to the stateless physics ancestry. Keys
/// and storage are prepared with the graph; capturing another frame only
/// replaces values (String/Table values retain their existing Arc storage).
pub(super) struct PhysicsInputSnapshot {
    values: Vec<Option<ParamValues>>,
    clock_steps: Vec<usize>,
    project_tempo: Option<ProjectTempo>,
    /// Transport times GPU liquids asked to sample this interval: their
    /// ticks' starts, so forces are evaluated per tick.
    tick_times: Vec<f64>,
}

impl PhysicsInputSnapshot {
    /// Rebuild-only remap: execution order may differ between fused and
    /// editable graphs, while held inputs must still belong to the old frame.
    pub(super) fn carry_from(&mut self, prior: &Self, steps: &[(usize, usize)]) {
        for &(new, old) in steps {
            self.values[new].clone_from(&prior.values[old]);
        }
        self.project_tempo.clone_from(&prior.project_tempo);
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
                let node = graph
                    .get_node(step.node)
                    .expect("compiled physics node exists");
                if node.node.type_id().as_str() == GENERATOR_INPUT_TYPE_ID {
                    clock_steps.push(index);
                }
                Some(node.params.clone())
            })
            .collect();
        Self {
            values,
            clock_steps,
            project_tempo: None,
            tick_times: Vec::new(),
        }
    }

    fn capture(&mut self, graph: &Graph, plan: &ExecutionPlan, tempo: &Option<ProjectTempo>) {
        self.project_tempo.clone_from(tempo);
        for (step, values) in plan.steps().iter().zip(&mut self.values) {
            let Some(values) = values else { continue };
            let node = graph
                .get_node(step.node)
                .expect("compiled physics node exists");
            assert_eq!(
                values.len(),
                node.params.len(),
                "physics parameter shape requires rebuild"
            );
            for (name, value) in values {
                value.clone_from(
                    node.params
                        .get(name.as_ref())
                        .expect("prepared physics parameter exists"),
                );
            }
        }
    }

    fn set_sample_time(&mut self, time: FrameTime) {
        for &index in &self.clock_steps {
            let params = self.values[index]
                .as_mut()
                .expect("sampled clock has inputs");
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

    let replays_history = |type_id: &str| {
        matches!(type_id, "node.physics_world" | FLIP_DOMAIN_TYPE_ID) || gpu_liquid(type_id)
    };
    // A GPU liquid replays its force field, roles and paired world at each
    // tick's start; its paired world's scene is captured for it there.
    let mut pending: Vec<_> = graph
        .nodes()
        .filter(|node| replays_history(node.node.type_id().as_str()))
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
        let kind = graph
            .get_node(node_id)
            .expect("physics ancestor exists")
            .node
            .type_id();
        // A mesh description is setup state: its prepared geometry stays
        // fixed during historical live-control sampling. Do not replay GPU
        // sources or local topology transforms at historical timestamps.
        pending.extend(
            graph
                .wires_into(node_id)
                .filter(|wire| !setup_input(kind.as_str(), wire.to.1))
                .map(|wire| wire.from.0),
        );
    }
    for node_id in &ancestry {
        let node = graph.get_node(*node_id).expect("ancestry node exists");
        let kind = node.node.type_id();
        let type_id = kind.as_str();
        let stateless_cpu = matches!(
            type_id,
            "node.physics_world"
                | FLIP_DOMAIN_TYPE_ID
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
        // A GPU liquid's sample run only records its field; it never encodes.
        if gpu_liquid(type_id) {
            continue;
        }
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
    /// Install the host's immutable project tempo before a frame or source
    /// observation. This is the existing project map, not a new clock. Held
    /// input intervals retain their prior snapshot until the next observation.
    /// Synthetic warmup and standalone graphs explicitly supply `None`.
    pub fn set_project_tempo(&mut self, tempo: Option<&ProjectTempo>) {
        let unchanged = match (self.physics_project_tempo.as_ref(), tempo) {
            (Some(current), Some(next)) => current.shares_mapping(next),
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            self.physics_project_tempo = tempo.cloned();
        }
        // Newly rebuilt native nodes need the snapshot even when the host's
        // map did not change. The node dirty-checks its retained source.
        if self.physics_sample_steps.is_some() {
            for instance in self.graph.nodes_mut() {
                instance.node.set_physics_project_tempo(tempo);
            }
        }
        for view in &mut self.math_views {
            for variant in &mut view.variants {
                variant.set_project_tempo(tempo);
            }
        }
    }

    /// Observe controls at an event producer boundary without running native
    /// ticks or rendering. Close the held-input interval first, then record
    /// the current inputs at the same timestamp. Moving the observation anchor
    /// prevents the next rendered frame from replaying the interval twice.
    pub(super) fn observe_physics_at_source(&mut self, source: FrameTime) -> Result<(), String> {
        if !source.seconds.0.is_finite() || !source.beats.0.is_finite() {
            return Err("Impulse: source clock must be finite".into());
        }
        let previous = self
            .last_physics_frame_time
            .ok_or("Impulse: render the scene before capturing an event")?;
        if source.seconds.0 < previous.seconds.0 {
            return Err("Impulse: source precedes the latest physics observation".into());
        }
        if self.graph.prepared_param_violation().is_some() {
            return Err("Impulse: graph preparation is pending".into());
        }
        // A producer callback must never inherit offline history draining.
        let _scope = crate::node_graph::physics::PhysicsStepScope::for_render(false);
        self.sample_physics_history(source);
        // No ancestry means no solver here replays held-input history, so
        // there is no interval to close.
        let (Some(inputs), Some(steps)) = (
            self.physics_input_snapshot.as_mut(),
            self.physics_sample_steps.as_ref(),
        ) else {
            self.last_physics_frame_time = Some(source);
            return Ok(());
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
        self.observe_physics_source_assets();
        let (Some(inputs), Some(steps)) = (
            self.physics_input_snapshot.as_mut(),
            self.physics_sample_steps.as_ref(),
        ) else {
            return;
        };
        let Some(previous) = self.last_physics_frame_time else {
            inputs.capture(&self.graph, &self.plan, &self.physics_project_tempo);
            return;
        };
        let gap = current.seconds.0 - previous.seconds.0;
        if !gap.is_finite() || gap <= 0.0 {
            inputs.capture(&self.graph, &self.plan, &self.physics_project_tempo);
            return;
        }
        let drain_offline = offline_simulation();
        if drain_offline {
            // An offline render may inherit a preview backlog. Drain the
            // already-observed prefix before inserting another sample into a
            // nearly-full history; the current edit must not reach that prefix.
            let _drain = PhysicsHistoryDrainScope::new();
            let sample = FrameTime {
                delta: Seconds::ZERO,
                ..previous
            };
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
        const DRAIN_INTERVAL: usize = crate::node_graph::physics::AUTHORED_HISTORY_CAPACITY / 4;
        let mut samples_since_drain = 0;
        // Clone only the shared map handle. Keep this borrow independent from
        // the scratch inputs mutated for each sample. Tempo edits take effect
        // at the current observation, just like held parameter edits.
        let tempo = inputs.project_tempo.clone();
        let beat_at = |seconds: f64| {
            tempo.as_ref().map_or_else(
                || {
                    let alpha = (seconds - previous.seconds.0) / gap;
                    Beats(previous.beats.0 + (current.beats.0 - previous.beats.0) * alpha)
                },
                |tempo| {
                    TempoMapConverter::seconds_to_beat_immut(
                        tempo.map(),
                        Seconds(seconds),
                        tempo.fallback_bpm(),
                    )
                },
            )
        };
        // Include exact tempo boundaries as well as the fixed observation
        // grid. A recorded clock must not smear a breakpoint between samples.
        let mut boundaries = tempo.as_ref().map(|tempo| {
            let start = beat_at(previous.seconds.0).0.max(0.0);
            let points = tempo.map().points();
            let first = points.partition_point(|point| point.beat.0 <= start);
            points[first..]
                .iter()
                .map(move |point| {
                    TempoMapConverter::beat_to_seconds_immut(
                        tempo.map(),
                        point.beat,
                        tempo.fallback_bpm(),
                    )
                    .0
                })
                .peekable()
        });
        // GPU liquids sample exactly at their ticks' starts: each tick's force
        // comes from its own simulated time, whatever the display rate.
        inputs.tick_times.clear();
        for instance in self.graph.nodes_mut() {
            instance
                .node
                .request_physics_samples(previous.seconds.0, current.seconds.0, &mut inputs.tick_times);
        }
        inputs.tick_times.sort_by(f64::total_cmp);
        let mut tick_index = 0;
        let mut grid = (previous.seconds.0 * SAMPLE_RATE).floor() + 1.0;
        let mut last_time = previous.seconds.0;
        loop {
            let boundary = boundaries
                .as_mut()
                .and_then(|values| values.peek().copied())
                .unwrap_or(f64::INFINITY);
            let tick_time = inputs.tick_times.get(tick_index).copied().unwrap_or(f64::INFINITY);
            let grid_time = grid / SAMPLE_RATE;
            let time = grid_time.min(boundary).min(tick_time);
            if time >= current.seconds.0 {
                break;
            }
            if time == grid_time {
                grid += 1.0;
            }
            if time == boundary {
                boundaries.as_mut().expect("finite boundary").next();
            }
            while inputs.tick_times.get(tick_index) == Some(&time) {
                tick_index += 1;
            }
            if time <= last_time {
                continue;
            }
            let sample = FrameTime {
                beats: beat_at(time),
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
        }
        let closing_beat = if tempo.is_some() {
            let held_beat = beat_at(current.seconds.0);
            let unchanged_at_boundary = self.physics_project_tempo.as_ref().is_some_and(|tempo| {
                TempoMapConverter::seconds_to_beat_immut(
                    tempo.map(),
                    current.seconds,
                    tempo.fallback_bpm(),
                ) == held_beat
            });
            // External beat authority computes seconds from beats. Preserve
            // that exact host stamp when both maps agree here: a floating-point
            // roundtrip must not invent a discontinuity at the same second.
            // Genuine tempo edits retain the old interval's left limit.
            if unchanged_at_boundary {
                current.beats
            } else {
                held_beat
            }
        } else {
            current.beats
        };
        let closing = FrameTime {
            beats: closing_beat,
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
        inputs.capture(&self.graph, &self.plan, &self.physics_project_tempo);
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
        ))
        .unwrap();
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 500, "nodeId": "pouring_mesh", "typeId": "node.fluid_role_source"
            }));
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 501, "nodeId": "visible_source", "typeId": "node.cube_mesh"
            }));
        def["wires"].as_array_mut().unwrap().extend([
            serde_json::json!({"fromNode": 5, "fromPort": "transform", "toNode": 500, "toPort": "transform"}),
            serde_json::json!({"fromNode": 501, "fromPort": "source", "toNode": 500, "toPort": "mesh_0"}),
            serde_json::json!({"fromNode": 500, "fromPort": "role", "toNode": 4, "toPort": "role_0"}),
        ]);
        let runtime =
            PresetRuntime::from_json_str(&def.to_string(), &PrimitiveRegistry::with_builtin())
                .expect("typed fluid role ancestry loads");
        let mut saw_source = false;
        let mut saw_motion = false;
        for (step, sampled) in runtime
            .plan
            .steps()
            .iter()
            .zip(runtime.physics_sample_steps.as_ref().unwrap())
        {
            let kind = runtime.graph.get_node(step.node).unwrap().node.type_id();
            match kind.as_str() {
                "node.fluid_role_source" => {
                    assert!(sampled);
                    saw_source = true;
                }
                "node.lfo" => {
                    assert!(sampled);
                    saw_motion = true;
                }
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
        assert!(sampled.iter().any(|kind| kind == FLIP_DOMAIN_TYPE_ID));
        assert!(sampled.iter().any(|kind| kind == "node.lfo"));
        assert!(!sampled.iter().any(|kind| kind == "node.scene_object"));
        assert!(!sampled.iter().any(|kind| kind == "node.render_scene"));
    }

    #[test]
    fn matter_liquid_samples_its_field_and_its_world_per_tick() {
        let runtime = PresetRuntime::from_json_str(
            include_str!("../../assets/generator-presets/WaterFloatingBoxMatter.json"),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("WaterFloatingBoxMatter loads");
        let pairs = runtime.plan.coupled_scenes();
        assert!(!pairs.is_empty(), "the box and the liquid are one coupled scene");
        let mask = runtime.physics_sample_steps.as_ref().expect("the liquid samples its field per tick");
        for pair in pairs {
            assert!(mask[pair.fluid_step], "the liquid samples");
            assert!(mask[pair.rigid_step], "its owned world's scene samples per tick");
        }
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

    #[test]
    fn rigid_body_source_is_setup_and_excludes_gpu_mesh_ancestors_from_history() {
        let mut def: serde_json::Value = serde_json::from_str(include_str!(
            "../../assets/generator-presets/PhysicsSolids.json"
        ))
        .unwrap();
        // Source-driven bodies take shape from the visible mesh. Remove the
        // legacy body's opposite shape route before wiring that source back.
        def["wires"]
            .as_array_mut()
            .unwrap()
            .retain(|wire| !(wire["fromNode"] == 101 && wire["toNode"] == 102));
        def["wires"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "fromNode": 102,
                "fromPort": "source",
                "toNode": 101,
                "toPort": "source"
            }));
        let runtime = PresetRuntime::from_json_str(
            &serde_json::to_string(&def).unwrap(),
            &PrimitiveRegistry::with_builtin(),
        )
        .expect("rigid body source graph loads");
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
        assert!(sampled.iter().any(|kind| kind == "node.rigid_body"));
        assert!(
            !sampled
                .iter()
                .any(|kind| kind == "node.platonic_solid_mesh")
        );
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
        ))
        .unwrap();
        def["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": 500, "nodeId": "release_event", "typeId": "node.trigger_gate"
            }));
        def["wires"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "fromNode": 500, "fromPort": "out", "toNode": 111, "toPort": "release_count"
            }));
        let registry = PrimitiveRegistry::with_builtin();
        let runtime = PresetRuntime::from_json_str(&def.to_string(), &registry)
            .expect("release events are not replayed during historical pose sampling");
        let event = runtime
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("release_event"))
            .unwrap();
        for (step, sampled) in runtime
            .plan
            .steps()
            .iter()
            .zip(runtime.physics_sample_steps.as_ref().unwrap())
        {
            if step.node == event {
                assert!(!sampled);
            }
        }
        def["wires"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "fromNode": 500, "fromPort": "out", "toNode": 110, "toPort": "pos_x"
            }));
        assert!(
            PresetRuntime::from_json_str(&def.to_string(), &registry).is_err(),
            "stateful pose ancestry must still be rejected"
        );
    }
}
