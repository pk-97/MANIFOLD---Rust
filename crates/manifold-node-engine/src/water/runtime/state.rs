//! Water-owned history and impulse state for one installed runtime graph.

use std::sync::Arc;

use crate::exec::{effect_node::FrameTime, execution_plan::ExecutionPlan};
use crate::graph::Graph;
use crate::runtime::extensions::{RuntimeContext, RuntimeExtension, RuntimeRegistration};
use crate::runtime::preset_context::ProjectTempo;

use super::physics_sampling::{PhysicsInputSnapshot, physics_sample_steps};
use super::scene_impulses::SceneImpulses;

pub(crate) struct WaterRuntimeState {
    /// One source state per immutable effect slot, in construction order.
    #[cfg(feature = "gpu-proofs")]
    pub(crate) sources: Vec<super::physics_source_state::PhysicsSourceState>,
    pub(crate) impulse_identity: Arc<()>,
    pub(crate) scene_impulses: SceneImpulses,
    pub(crate) sample_steps: Option<Vec<bool>>,
    pub(crate) input_snapshot: Option<PhysicsInputSnapshot>,
    pub(crate) last_frame_time: Option<FrameTime>,
    pub(crate) project_tempo: Option<ProjectTempo>,
}

impl RuntimeExtension for WaterRuntimeState {
    fn write_fluid_domains(
        &self,
        graph: &Graph,
        slot: crate::runtime::extensions::RuntimeSlot<'_>,
        output: &mut Vec<(manifold_core::NodeId, crate::scene::fluid_domain::FluidDomainSnapshot)>,
    ) {
        for (node_id, instance) in slot.node_map {
            if let Some(snapshot) = graph
                .get_node(*instance)
                .and_then(|node| crate::water::node::get(node.node.as_ref()))
                .and_then(|node| node.fluid_domain_snapshot())
            {
                output.push((node_id.clone(), snapshot));
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
    ) -> Result<bool, String> {
        super::WaterRuntime::borrow(self, runtime).fire_scene_impulse(param, source, next_sequence)
    }

    fn drain_scene_impulse_diagnostics(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        diagnostics: &mut crate::scene::impulse::SceneImpulseDiagnostics,
    ) {
        let mut water = super::WaterRuntime::borrow(self, runtime);
        water.drain_scene_impulses(|_, event| {
            diagnostics.started = diagnostics.started.saturating_add(1);
            if event.lateness.0 > 0.0 {
                diagnostics.late = diagnostics.late.saturating_add(1);
            }
        });
        water.drain_discarded_scene_impulses(|_, _| {
            diagnostics.discarded = diagnostics.discarded.saturating_add(1);
        });
    }

    fn before_frame(&mut self, runtime: &mut RuntimeContext<'_>, time: FrameTime) {
        super::WaterRuntime::borrow(self, runtime).sample_physics_history(time);
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
        graph: &mut Graph,
        instance: Option<&manifold_core::effects::PresetInstance>,
    ) {
        Self::set_source_instance(self, graph, instance);
    }

    fn prepare_modifiers(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        owner: &manifold_core::effect_graph_def::EffectGraphDef,
        routes: &[crate::load::expand::SceneModifierImpulseRoute],
        registry: &crate::persistence::PrimitiveRegistry,
    ) -> Result<(), crate::load::expand::SceneModifierExpandError> {
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

    #[cfg(feature = "gpu-proofs")]
    fn initialize_chain(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        instances: &[manifold_core::effects::PresetInstance],
        registry: &crate::persistence::PrimitiveRegistry,
    ) {
        super::WaterRuntime::borrow(self, runtime)
            .initialize_chain_physics_sources(instances, registry);
    }
    #[cfg(feature = "gpu-proofs")]
    fn install_sources(&mut self, runtime: &mut RuntimeContext<'_>) {
        super::WaterRuntime::borrow(self, runtime).install_physics_source_identities();
    }
    #[cfg(feature = "gpu-proofs")]
    fn refresh_slot(
        &mut self,
        graph: &mut Graph,
        index: usize,
        slot: crate::runtime::extensions::RuntimeSlot<'_>,
        instance: &manifold_core::effects::PresetInstance,
    ) {
        self.sources[index].refresh_chain(graph, slot.node_map, slot.card_prefix, instance, None);
    }
    #[cfg(feature = "gpu-proofs")]
    fn after_slot_bindings(
        &mut self,
        graph: &mut Graph,
        index: usize,
        instance: &manifold_core::effects::PresetInstance,
    ) {
        self.sources[index].set_instance(graph, Some(instance));
    }
    #[cfg(feature = "gpu-proofs")]
    fn refresh_generator(
        &mut self,
        runtime: &mut RuntimeContext<'_>,
        owner: &manifold_core::effect_graph_def::EffectGraphDef,
    ) {
        super::WaterRuntime::borrow(self, runtime).refresh_physics_source_graphs(owner);
    }
    #[cfg(feature = "gpu-proofs")]
    fn observe_strings(&mut self, graph: &mut Graph) {
        if let Some(source) = self.sources.first_mut() {
            source.observe_strings(graph);
        }
    }
}

inventory::submit! {
    RuntimeRegistration {
        name: "water",
        before_compile: super::physics_sampling::retain_physics_setup_outputs,
        create: |graph, plan, _slots| Ok(Box::new(WaterRuntimeState::new(
            graph, plan,
            #[cfg(feature = "gpu-proofs")]
            _slots,
        )?)),
        #[cfg(feature = "gpu-proofs")]
        prepare: |render, owner, routes, registry| {
            let sources = super::physics_sources::prepare(render, owner, routes, registry);
            Box::new(move |runtime| {
                use super::WaterRuntimeExt;
                runtime.water().apply_physics_source_graphs(sources);
            })
        },
    }
}

impl WaterRuntimeState {
    pub(crate) fn new(
        graph: &Graph,
        plan: &ExecutionPlan,
        #[cfg(feature = "gpu-proofs")] slot_count: usize,
    ) -> Result<Self, String> {
        let sample_steps = physics_sample_steps(graph, plan)?;
        let input_snapshot = sample_steps
            .as_ref()
            .map(|steps| PhysicsInputSnapshot::prepare(graph, plan, steps));
        Ok(Self {
            #[cfg(feature = "gpu-proofs")]
            sources: (0..slot_count).map(|_| Default::default()).collect(),
            impulse_identity: Arc::new(()),
            scene_impulses: SceneImpulses::default(),
            sample_steps,
            input_snapshot,
            last_frame_time: None,
            project_tempo: None,
        })
    }

    pub(crate) fn after_frame(&mut self, graph: &Graph, time: FrameTime) {
        self.last_frame_time = Some(time);
        self.observe_impulse_setup(graph);
    }

    pub(crate) fn reset(&mut self) {
        self.impulse_identity = Arc::new(());
        self.reset_modifier_impulses();
        self.last_frame_time = None;
    }
}
