//! Water-owned history and impulse state for one installed runtime graph.

use std::sync::Arc;

use manifold_node_engine::exec::{effect_node::FrameTime, execution_plan::ExecutionPlan};
use manifold_node_engine::graph::Graph;
use manifold_node_engine::runtime::extensions::{RuntimeContext, RuntimeExtension, RuntimeRegistration};
use manifold_node_engine::runtime::preset_context::ProjectTempo;
use manifold_core::{Beats, PlayedTime};

use super::physics_sampling::{PhysicsInputSnapshot, physics_sample_steps};
use super::scene_impulses::SceneImpulses;

/// A frame the replay holds, on played time: a tempo edit never moves it.
/// Its transport seconds are derived from the current step when needed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HeldFrame {
    pub(crate) played: PlayedTime,
    pub(crate) beats: Beats,
    pub(crate) frame_count: i64,
}

pub(crate) struct WaterRuntimeState {
    pub(crate) impulse_identity: Arc<()>,
    pub(crate) scene_impulses: SceneImpulses,
    pub(crate) sample_steps: Option<Vec<bool>>,
    pub(crate) input_snapshot: Option<PhysicsInputSnapshot>,
    pub(crate) last_frame_time: Option<HeldFrame>,
    /// Where the next replay opens instead of at the last frame: the target of
    /// a jump the last frame landed on, sampled there first.
    pub(crate) replay_from: Option<HeldFrame>,
    pub(crate) project_tempo: Option<ProjectTempo>,
}

impl RuntimeExtension for WaterRuntimeState {
    fn write_fluid_domains(
        &self,
        graph: &Graph,
        slot: manifold_node_engine::runtime::extensions::RuntimeSlot<'_>,
        output: &mut Vec<(manifold_core::NodeId, manifold_core::fluid_domain::FluidDomainSnapshot)>,
    ) {
        let first = output.len();
        for (node_id, instance) in slot.node_map {
            if let Some(snapshot) = graph
                .get_node(*instance)
                .and_then(|node| manifold_water_rigid::node::get(node.node.as_ref()))
                .and_then(|node| node.fluid_domain_snapshot())
            {
                output.push((node_id.clone(), snapshot));
            }
        }
        // A full pool is seen by the liquid's state, downstream of its
        // domain; it joins the snapshot of the domain whose clock feeds it.
        for (_, instance) in slot.node_map {
            let held_back = graph.get_node(*instance).is_some_and(|node| manifold_water_gpu_flip::primitives::inflow_held_back(node.node.as_ref()));
            if !held_back {
                continue;
            }
            let Some(domain) = graph.wires_into(*instance).find(|wire| wire.to.1 == "ticks").map(|wire| wire.from.0) else { continue };
            let Some((domain_id, _)) = slot.node_map.iter().find(|(_, id)| *id == domain) else { continue };
            if let Some((_, snapshot)) = output[first..].iter_mut().find(|(id, _)| id == domain_id) {
                snapshot.fit.pool_full = true;
            }
        }
    }

    fn write_physics_gizmos(
        &self,
        graph: &Graph,
        slot: manifold_node_engine::runtime::extensions::RuntimeSlot<'_>,
        output: &mut Vec<manifold_core::physics_gizmo::PhysicsGizmo>,
    ) {
        for (_, instance) in slot.node_map {
            if let Some(node) = graph.get_node(*instance).and_then(|node| manifold_water_rigid::node::get(node.node.as_ref())) {
                node.write_physics_gizmos(output);
            }
        }
    }

    fn is_scene_impulse_param(&self, param: &str) -> bool {
        Self::is_scene_impulse_param(self, param)
    }

    fn fire_scene_impulse(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        param: &str,
        source: PlayedTime,
        beats: Beats,
        next_sequence: &mut u64,
    ) -> Result<manifold_core::scene_impulse::ImpulseFire, String> {
        super::WaterRuntime::borrow(self, runtime).fire_scene_impulse(param, source, beats, next_sequence)
    }

    fn drain_scene_impulse_diagnostics(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        diagnostics: &mut manifold_core::scene_impulse::SceneImpulseDiagnostics,
    ) {
        let mut water = super::WaterRuntime::borrow(self, runtime);
        water.drain_scene_impulses(|_, event| {
            diagnostics.started = diagnostics.started.saturating_add(1);
            if event.lateness.0 > 0.0 {
                diagnostics.late = diagnostics.late.saturating_add(1);
            }
        });
        let beyond = water.drain_discarded_scene_impulses(|_, _| {
            diagnostics.discarded = diagnostics.discarded.saturating_add(1);
        });
        diagnostics.discarded = diagnostics.discarded.saturating_add(beyond);
    }

    fn before_frame(&mut self, runtime: &mut RuntimeContext<'_>, time: FrameTime) {
        let mut water = super::WaterRuntime::borrow(self, runtime);
        let step = water.executor.sim_step();
        water.sample_physics_history(time, step);
    }

    fn after_frame(&mut self, graph: &Graph, time: FrameTime, played: PlayedTime) {
        Self::after_frame(self, graph, time, played);
    }
    fn reset(&mut self) {
        Self::reset(self);
    }
    fn set_project_tempo(&mut self, graph: &mut Graph, tempo: Option<&ProjectTempo>) {
        Self::set_project_tempo(self, graph, tempo);
    }
    fn set_source_instance(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        instance: Option<&manifold_core::effects::PresetInstance>,
    ) {
        let tempo_shift = runtime.executor.sim_step().tempo_shift;
        Self::set_source_instance(self, runtime.graph, instance, tempo_shift);
    }

    fn prepare_modifiers(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        owner: &manifold_core::effect_graph_def::EffectGraphDef,
        routes: &[manifold_node_engine::load::expand::SceneModifierImpulseRoute],
        registry: &manifold_node_engine::persistence::PrimitiveRegistry,
    ) -> Result<(), manifold_node_engine::load::expand::SceneModifierExpandError> {
        super::WaterRuntime::borrow(self, runtime)
            .prepare_modifier_impulses(owner, routes, registry)
    }

    fn carry_from(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        prior: &mut dyn RuntimeExtension,
        previous: &mut RuntimeContext<'_>,
    ) {
        let prior = prior
            .as_any_mut()
            .downcast_mut::<Self>()
            .expect("matching registered runtime extension");
        super::WaterRuntime::borrow(self, runtime)
            .carry_physics_state_from(&mut super::WaterRuntime::borrow(prior, previous));
    }
}

inventory::submit! {
    RuntimeRegistration {
        name: "water",
        before_compile: super::physics_sampling::retain_physics_setup_outputs,
        create: |graph, plan, _slots| Ok(Box::new(WaterRuntimeState::new(
            graph, plan,
        )?)),
        #[cfg(feature = "gpu-proofs")]
        prepare: |_, _, _, _| Box::new(|_| {}),
    }
}

impl WaterRuntimeState {
    pub(crate) fn new(
        graph: &Graph,
        plan: &ExecutionPlan,
    ) -> Result<Self, String> {
        let sample_steps = physics_sample_steps(graph, plan)?;
        let input_snapshot = sample_steps
            .as_ref()
            .map(|steps| PhysicsInputSnapshot::prepare(graph, plan, steps));
        Ok(Self {
            impulse_identity: Arc::new(()),
            scene_impulses: SceneImpulses::default(),
            sample_steps,
            input_snapshot,
            last_frame_time: None,
            replay_from: None,
            project_tempo: None,
        })
    }

    pub(crate) fn after_frame(&mut self, graph: &Graph, time: FrameTime, played: PlayedTime) {
        self.last_frame_time = Some(HeldFrame { played, beats: time.beats, frame_count: time.frame_count });
        self.observe_impulse_setup(graph);
    }

    pub(crate) fn reset(&mut self) {
        self.impulse_identity = Arc::new(());
        self.reset_modifier_impulses();
        self.last_frame_time = None;
        self.replay_from = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::NodeId;
    use manifold_core::id::EffectId;
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_node_engine::runtime::extensions::RuntimeSlot;
    use manifold_water_liquid::primitives::liquid_stats::LiquidTickStats;

    /// BUG-3zq4r (inflow runs out): a tick that refused inflow births marks
    /// the snapshot of the domain whose clock feeds that liquid, so the
    /// setup check shows Pool Full; a tick that refused none leaves it clear.
    #[test]
    fn a_refused_birth_marks_the_domain_pool_full() {
        let registry = PrimitiveRegistry::with_builtin();
        let state_with = |refused: u32| {
            let mut graph = Graph::new();
            let domain = graph.add_node(registry.construct("node.gpu_flip_domain").expect("domain"));
            let liquid = graph.add_node(registry.construct("node.liquid_state").expect("liquid state"));
            graph.connect((domain, "ticks"), (liquid, "ticks")).expect("the domain clocks the liquid");
            let stats = LiquidTickStats { inflow_births: 3, inflow_refused: refused, ..LiquidTickStats::default() };
            manifold_water_gpu_flip::primitives::retire_stats_for_test(graph.get_node_mut(liquid).expect("liquid").node.as_mut(), stats);
            let node_map = [(NodeId::new("water"), domain), (NodeId::new("liquid"), liquid)];
            let runtime = WaterRuntimeState {
                impulse_identity: Arc::new(()),
                scene_impulses: SceneImpulses::default(),
                sample_steps: None,
                input_snapshot: None,
                last_frame_time: None,
                replay_from: None,
                project_tempo: None,
            };
            let effect = EffectId::new("water_effect");
            let slot = RuntimeSlot { effect_id: &effect, legacy_index: 0, node_map: &node_map, card_prefix: "", def_content_key: 0, bindings: &[] };
            let mut output = Vec::new();
            runtime.write_fluid_domains(&graph, slot, &mut output);
            assert_eq!(output.len(), 1, "one domain snapshot");
            assert_eq!(output[0].0, NodeId::new("water"));
            output[0].1.fit.pool_full
        };
        assert!(state_with(5), "refused births mark the pool full");
        assert!(!state_with(0), "a tick with room leaves it clear");
    }
}
