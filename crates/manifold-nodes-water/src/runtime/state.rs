//! Water-owned history and impulse state for one installed runtime graph.

use std::sync::Arc;

use manifold_node_engine::exec::{effect_node::FrameTime, execution_plan::ExecutionPlan};
use manifold_node_engine::graph::Graph;
use manifold_node_engine::runtime::extensions::{RuntimeContext, RuntimeExtension, RuntimeRegistration};
use manifold_node_engine::runtime::preset_context::ProjectTempo;

use super::physics_sampling::{PhysicsInputSnapshot, physics_sample_steps};
use super::scene_impulses::SceneImpulses;

pub(crate) struct WaterRuntimeState {
    pub(crate) impulse_identity: Arc<()>,
    pub(crate) scene_impulses: SceneImpulses,
    pub(crate) sample_steps: Option<Vec<bool>>,
    pub(crate) input_snapshot: Option<PhysicsInputSnapshot>,
    pub(crate) last_frame_time: Option<FrameTime>,
    /// Where the next replay opens instead of at the last frame: the target of
    /// a jump the last frame landed on, sampled there first.
    pub(crate) replay_from: Option<FrameTime>,
    pub(crate) project_tempo: Option<ProjectTempo>,
    /// The engine's tempo-edit total these transport times already follow.
    pub(crate) followed_shift: f64,
}

impl RuntimeExtension for WaterRuntimeState {
    fn write_fluid_domains(
        &self,
        graph: &Graph,
        slot: manifold_node_engine::runtime::extensions::RuntimeSlot<'_>,
        output: &mut Vec<(manifold_core::NodeId, manifold_core::fluid_domain::FluidDomainSnapshot)>,
    ) {
        for (node_id, instance) in slot.node_map {
            if let Some(snapshot) = graph
                .get_node(*instance)
                .and_then(|node| manifold_water_rigid::node::get(node.node.as_ref()))
                .and_then(|node| node.fluid_domain_snapshot())
            {
                output.push((node_id.clone(), snapshot));
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
        source: FrameTime,
        next_sequence: &mut u64,
    ) -> Result<manifold_core::scene_impulse::ImpulseFire, String> {
        super::WaterRuntime::borrow(self, runtime).fire_scene_impulse(param, source, next_sequence)
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

    fn after_frame(&mut self, graph: &Graph, time: FrameTime) {
        Self::after_frame(self, graph, time);
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
        self.follow_tempo_shift(runtime.executor.sim_step().tempo_shift.0);
        Self::set_source_instance(self, runtime.graph, instance);
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
            followed_shift: 0.0,
        })
    }

    /// A tempo edit kept the beat and moved its seconds: every transport
    /// time held here moves with it, once, so the replay continues from the
    /// time already simulated. `total` is the sum of every edit's move.
    pub(crate) fn follow_tempo_shift(&mut self, total: f64) {
        let by = total - self.followed_shift;
        if by == 0.0 || !by.is_finite() {
            return;
        }
        self.followed_shift = total;
        for time in [&mut self.last_frame_time, &mut self.replay_from].into_iter().flatten() {
            time.seconds.0 += by;
        }
        if let Some(inputs) = &mut self.input_snapshot {
            inputs.shift_hops(by);
        }
    }

    pub(crate) fn after_frame(&mut self, graph: &Graph, time: FrameTime) {
        self.last_frame_time = Some(time);
        self.observe_impulse_setup(graph);
    }

    pub(crate) fn reset(&mut self) {
        self.impulse_identity = Arc::new(());
        self.reset_modifier_impulses();
        self.last_frame_time = None;
        self.replay_from = None;
    }
}
