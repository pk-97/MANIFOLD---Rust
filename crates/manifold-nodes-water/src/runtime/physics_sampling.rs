//! CPU ancestry sampling for native rigid-body and fluid simulation inputs.

use manifold_node_engine::exec::effect_node::FrameTime;
use manifold_node_engine::exec::execution_plan::ExecutionPlan;
use manifold_node_engine::graph::Graph;
use manifold_node_engine::param_binding::ResolvedBinding;
use manifold_node_engine::param_binding::ResolvedTarget;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::validation::GraphError;
use manifold_core::Beats;
use manifold_core::Seconds;
use manifold_node_engine::scene::boundary_nodes::GENERATOR_INPUT_TYPE_ID;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_water_rigid::physics::SimStep;
use manifold_water_rigid::node;
use manifold_node_engine::runtime::preset_context::ProjectTempo;
use manifold_core::audio_mod::HopValue;
use manifold_core::effects::PresetInstance;
use manifold_foundation::ParamId;
use manifold_core::liquid_domain::{GPU_FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID};
use manifold_core::tempo::TempoMapConverter;

#[cfg(test)]
mod input_tests;



manifold_core::testkit_visible! {
/// Observe retained inputs under the native authored-sample policy. `step`
/// is the step the sample belongs to; the executor's own frame step is
/// restored afterwards, so a sample never changes the next frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_physics_sample_frame(
    executor: &mut manifold_node_engine::exec::execution::Executor,
    graph: &mut Graph,
    plan: &ExecutionPlan,
    time: FrameTime,
    steps: &[bool],
    params: &[Option<ParamValues>],
    step: SimStep,
) {
    let frame_step = executor.sim_step();
    executor.set_sim_step(step.authored_sample());
    executor.execute_cpu_sample_frame(graph, plan, time, steps, params);
    executor.set_sim_step(frame_step);
}
}

/// GPU liquids whose force field is evaluated at each tick's start.
fn gpu_liquid(kind: &str) -> bool {
    matches!(kind, GPU_FLIP_DOMAIN_TYPE_ID | MATTER_DOMAIN_TYPE_ID)
}

fn setup_input(kind: &str, port: &str) -> bool {
    if gpu_liquid(kind) {
        return port != "acceleration_field" && port != "speed" && !port.starts_with("role_");
    }
    match kind {
        "node.rigid_body" => matches!(port, "release_count" | "source"),
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
pub(crate) fn retain_physics_setup_outputs(graph: &mut Graph) -> Result<(), GraphError> {
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
pub(crate) struct PhysicsInputSnapshot {
    values: Vec<Option<ParamValues>>,
    clock_steps: Vec<usize>,
    project_tempo: Option<ProjectTempo>,
    /// Transport times GPU liquids asked to sample this interval: their
    /// ticks' starts, so forces are evaluated per tick.
    tick_times: Vec<f64>,
    /// This frame's per-hop effective values of host audio-modulated params,
    /// `hops[..hop_len]`, so each tick reads the value at its own time
    /// (BUG-2jx6 (host-fed modulation sampled per liquid tick)). Storage is
    /// reused across frames.
    hops: Vec<(ParamId, Vec<HopValue>)>,
    hop_len: usize,
    /// `(binding, step, hop entry)` for bindings into the sampled ancestry.
    hop_routes: Vec<(usize, usize, usize)>,
}

impl PhysicsInputSnapshot {
    /// Rebuild-only remap: execution order may differ between fused and
    /// editable graphs, while held inputs must still belong to the old frame.
    pub(super) fn carry_from(&mut self, prior: &Self, steps: &[(usize, usize)]) {
        for &(new, old) in steps {
            self.values[new].clone_from(&prior.values[old]);
        }
        self.project_tempo.clone_from(&prior.project_tempo);
        self.hops.clone_from(&prior.hops);
        self.hop_len = prior.hop_len;
    }

    /// Copy the host instance's per-hop values for this frame.
    pub(super) fn set_hops(&mut self, instance: Option<&PresetInstance>) {
        self.hop_len = 0;
        let mods = instance.and_then(|i| i.audio_mods.as_deref()).unwrap_or_default();
        for m in mods.iter().filter(|m| !m.hop_timeline.values.is_empty()) {
            if self.hop_len == self.hops.len() {
                self.hops.push(Default::default());
            }
            let (id, values) = &mut self.hops[self.hop_len];
            id.clone_from(&m.param_id);
            values.clone_from(&m.hop_timeline.values);
            self.hop_len += 1;
        }
    }

    /// Route bindings whose source has hop values into sampled node params.
    fn route_hops(&mut self, bindings: &[ResolvedBinding], plan: &ExecutionPlan) {
        self.hop_routes.clear();
        if self.hop_len == 0 {
            return;
        }
        for (b, binding) in bindings.iter().enumerate() {
            let ResolvedTarget::Node { node, param } = &binding.target else { continue };
            let Some(h) = self.hops[..self.hop_len]
                .iter()
                .position(|(id, _)| *id == binding.source_id)
            else {
                continue;
            };
            let Some(s) = plan.steps().iter().position(|step| step.node == *node) else {
                continue;
            };
            if self.values[s].as_ref().is_some_and(|v| v.get(param.as_ref()).is_some()) {
                self.hop_routes.push((b, s, h));
            }
        }
    }

    /// Write each routed param's latest hop value at or before `time`.
    /// Before the frame's first hop the held value stays.
    fn apply_hops(&mut self, bindings: &[ResolvedBinding], time: f64) {
        for &(b, s, h) in &self.hop_routes {
            let values = &self.hops[h].1;
            let n = values.partition_point(|hop| hop.time.0 <= time);
            let Some(hop) = n.checked_sub(1).map(|i| values[i]) else { continue };
            let binding = &bindings[b];
            let ResolvedTarget::Node { param, .. } = &binding.target else { continue };
            if let Some(slot) = self.values[s].as_mut().and_then(|v| v.get_mut(param.as_ref())) {
                *slot = binding.write_value(hop.value);
            }
        }
    }

    pub(crate) fn prepare(graph: &Graph, plan: &ExecutionPlan, steps: &[bool]) -> Self {
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
            hops: Vec::new(),
            hop_len: 0,
            hop_routes: Vec::new(),
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
pub(crate) fn physics_sample_steps(
    graph: &Graph,
    plan: &ExecutionPlan,
) -> Result<Option<Vec<bool>>, String> {
    node::initialize();
    use std::collections::HashSet;

    let replays_history = |type_id: &str| {
        type_id == "node.physics_world" || gpu_liquid(type_id)
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

impl super::WaterRuntimeState {
    /// Install the host's immutable project tempo before a frame or source
    /// observation. This is the existing project map, not a new clock. Held
    /// input intervals retain their prior snapshot until the next observation.
    /// Synthetic warmup and standalone graphs explicitly supply `None`.
    pub fn set_project_tempo(&mut self, graph: &mut Graph, tempo: Option<&ProjectTempo>) {
        let unchanged = match (self.project_tempo.as_ref(), tempo) {
            (Some(current), Some(next)) => current.shares_mapping(next),
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            self.project_tempo = tempo.cloned();
        }
        // Newly rebuilt native nodes need the snapshot even when the host's
        // map did not change. The node dirty-checks its retained source.
        if self.sample_steps.is_some() {
            for instance in graph.nodes_mut() {
                if let Some(native) = node::get_mut(instance.node.as_mut()) {
                    native.set_physics_project_tempo(tempo);
                }
            }
        }
    }
}

impl super::WaterRuntime<'_> {
    /// Observe controls at an event producer boundary without running native
    /// ticks or rendering. Close the held-input interval first, then record
    /// the current inputs at the same timestamp. Moving the observation anchor
    /// prevents the next rendered frame from replaying the interval twice.
    pub(super) fn observe_physics_at_source(&mut self, source: FrameTime) -> Result<(), String> {
        if !source.seconds.0.is_finite() || !source.beats.0.is_finite() {
            return Err("Impulse: source clock must be finite".into());
        }
        let previous = self
            .water
            .last_frame_time
            .ok_or("Impulse: render the scene before capturing an event")?;
        if source.seconds.0 < previous.seconds.0 {
            return Err("Impulse: source precedes the latest physics observation".into());
        }
        if self.graph.prepared_param_violation().is_some() {
            return Err("Impulse: graph preparation is pending".into());
        }
        // A producer callback observes live at the project Sim Rate and never
        // drains offline history.
        let step = SimStep::live(self.executor.sim_step().interval);
        self.sample_physics_history(source, step);
        // No ancestry means no solver here replays held-input history, so
        // there is no interval to close.
        let (Some(inputs), Some(steps)) = (
            self.water.input_snapshot.as_mut(),
            self.water.sample_steps.as_ref(),
        ) else {
            self.water.last_frame_time = Some(source);
            return Ok(());
        };
        inputs.set_sample_time(source);
        execute_physics_sample_frame(
            self.executor,
            self.graph,
            self.plan,
            source,
            steps,
            &inputs.values,
            step,
        );
        self.water.last_frame_time = Some(source);
        Ok(())
    }

    pub(crate) fn sample_physics_history(&mut self, current: FrameTime, step: SimStep) {
        let bindings = self
            .effect_nodes
            .first()
            .map_or(&[][..], |slot| slot.bindings);
        self.water.sample_physics_history(
            self.graph, self.plan, self.executor, bindings, current, step,
        );
    }
}

impl super::WaterRuntimeState {
    /// Sample stateless authored motion on the existing 240 Hz grid, holding
    /// external parameters at their last observed values. Today's parameters
    /// must not be substituted into an earlier tick. The final left-limit
    /// sample closes the old interval before the full frame applies edits at
    /// the same timestamp; InputHistory preserves that discontinuity. Offline
    /// catch-up drains native ticks in bounded input batches without publishing
    /// intermediate graph outputs. Preview continues to retain its time debt.
    pub(super) fn sample_physics_history(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        executor: &mut manifold_node_engine::exec::execution::Executor,
        bindings: &[ResolvedBinding],
        current: FrameTime,
        step: SimStep,
    ) {
        let (Some(inputs), Some(steps)) = (
            self.input_snapshot.as_mut(),
            self.sample_steps.as_ref(),
        ) else {
            return;
        };
        let Some(previous) = self.last_frame_time else {
            inputs.capture(graph, plan, &self.project_tempo);
            return;
        };
        let gap = current.seconds.0 - previous.seconds.0;
        if !gap.is_finite() || gap <= 0.0 {
            inputs.capture(graph, plan, &self.project_tempo);
            return;
        }
        inputs.route_hops(bindings, plan);
        let drain_offline = step.offline();
        if drain_offline {
            // An offline render may inherit a preview backlog. Drain the
            // already-observed prefix before inserting another sample into a
            // nearly-full history; the current edit must not reach that prefix.
            let sample = FrameTime {
                delta: Seconds::ZERO,
                ..previous
            };
            inputs.set_sample_time(sample);
            execute_physics_sample_frame(
                executor,
                graph,
                plan,
                sample,
                steps,
                &inputs.values,
                step.draining_history(),
            );
        }
        const SAMPLE_RATE: f64 = 240.0;
        // Leave room for the retained tick endpoints and edit discontinuities.
        // This bounds input storage, not the amount of requested offline time.
        const DRAIN_INTERVAL: usize = manifold_water_rigid::physics::AUTHORED_HISTORY_CAPACITY / 4;
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
        for instance in graph.nodes_mut() {
            if let Some(native) = node::get_mut(instance.node.as_mut()) {
                native.request_physics_samples(
                    previous.seconds.0,
                    current.seconds.0,
                    &mut inputs.tick_times,
                );
            }
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
            inputs.apply_hops(bindings, time);
            samples_since_drain += 1;
            let drain = drain_offline && samples_since_drain == DRAIN_INTERVAL;
            execute_physics_sample_frame(
                executor,
                graph,
                plan,
                sample,
                steps,
                &inputs.values,
                if drain { step.draining_history() } else { step },
            );
            if drain {
                samples_since_drain = 0;
            }
            last_time = time;
        }
        let closing_beat = if tempo.is_some() {
            let held_beat = beat_at(current.seconds.0);
            let unchanged_at_boundary = self.project_tempo.as_ref().is_some_and(|tempo| {
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
        inputs.apply_hops(bindings, current.seconds.0);
        execute_physics_sample_frame(
            executor,
            graph,
            plan,
            closing,
            steps,
            &inputs.values,
            if drain_offline { step.draining_history() } else { step },
        );
        inputs.capture(graph, plan, &self.project_tempo);
    }
}
