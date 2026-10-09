//! Family-owned runtime state, installed once and borrowed at existing lifecycle boundaries.

use std::sync::LazyLock;

use manifold_core::{
    NodeId, PresetTypeId, effect_graph_def::EffectGraphDef, effects::PresetInstance, id::EffectId,
};

use super::{PresetRuntime, core::EffectSlot, preset_context::ProjectTempo};
use crate::{
    exec::{
        effect_node::{AsAny, FrameTime, NodeInstanceId},
        execution::Executor,
        execution_plan::ExecutionPlan,
    },
    graph::Graph,
    load::expand::{SceneModifierExpandError, SceneModifierImpulseRoute},
    param_binding::ResolvedBinding,
    persistence::PrimitiveRegistry,
    validation::GraphError,
};

/// Read-only authored scope. The runtime retains ownership of maps and bindings.
#[derive(Clone, Copy)]
pub struct RuntimeSlot<'a> {
    pub effect_id: &'a EffectId,
    pub legacy_index: usize,
    pub node_map: &'a [(NodeId, NodeInstanceId)],
    pub card_prefix: &'a str,
    pub def_content_key: u64,
    pub bindings: &'a [ResolvedBinding],
}

#[derive(Clone, Copy)]
pub struct RuntimeSlots<'a>(&'a [EffectSlot]);

impl<'a> RuntimeSlots<'a> {
    pub fn first(self) -> Option<RuntimeSlot<'a>> {
        self.iter().next()
    }
    pub fn iter(self) -> impl ExactSizeIterator<Item = RuntimeSlot<'a>> {
        self.0.iter().map(EffectSlot::extension_scope)
    }
}

impl EffectSlot {
    pub(super) fn extension_scope(&self) -> RuntimeSlot<'_> {
        RuntimeSlot {
            effect_id: &self.effect_id,
            legacy_index: self.legacy_index,
            node_map: &self.node_map,
            card_prefix: &self.card_prefix,
            def_content_key: self.def_content_key,
            bindings: &self.bound.bindings,
        }
    }
}

/// The execution state a family may borrow. UI, caches and modifier ownership stay private.
pub struct RuntimeContext<'a> {
    pub graph: &'a mut Graph,
    pub plan: &'a ExecutionPlan,
    pub executor: &'a mut Executor,
    pub effect_nodes: RuntimeSlots<'a>,
    pub type_id: Option<&'a PresetTypeId>,
    pub width: u32,
    pub height: u32,
    pub last_forced_outputs_epoch: u64,
    pub forced_outputs_stale: bool,
}

pub trait RuntimeExtension: AsAny + Send + 'static {
    fn before_frame(&mut self, _runtime: &mut RuntimeContext<'_>, _time: FrameTime) {}
    fn after_frame(&mut self, _graph: &Graph, _time: FrameTime) {}
    fn reset(&mut self) {}
    fn set_project_tempo(&mut self, _graph: &mut Graph, _tempo: Option<&ProjectTempo>) {}
    fn set_source_instance(&mut self, _graph: &mut Graph, _instance: Option<&PresetInstance>) {}
    fn prepare_modifiers(
        &mut self,
        _runtime: &mut RuntimeContext<'_>,
        _owner: &EffectGraphDef,
        _routes: &[SceneModifierImpulseRoute],
        _registry: &PrimitiveRegistry,
    ) -> Result<(), SceneModifierExpandError> {
        Ok(())
    }
    fn carry_from(
        &mut self,
        _runtime: &mut RuntimeContext<'_>,
        _prior: &mut dyn RuntimeExtension,
        _previous: &mut RuntimeContext<'_>,
    ) {
    }

    #[cfg(feature = "gpu-proofs")]
    fn initialize_chain(
        &mut self,
        _runtime: &mut RuntimeContext<'_>,
        _instances: &[PresetInstance],
        _registry: &PrimitiveRegistry,
    ) {
    }
    #[cfg(feature = "gpu-proofs")]
    fn install_sources(&mut self, _runtime: &mut RuntimeContext<'_>) {}
    #[cfg(feature = "gpu-proofs")]
    fn refresh_slot(
        &mut self,
        _graph: &mut Graph,
        _index: usize,
        _slot: RuntimeSlot<'_>,
        _instance: &PresetInstance,
    ) {
    }
    #[cfg(feature = "gpu-proofs")]
    fn after_slot_bindings(
        &mut self,
        _graph: &mut Graph,
        _index: usize,
        _instance: &PresetInstance,
    ) {
    }
    #[cfg(feature = "gpu-proofs")]
    fn refresh_generator(&mut self, _runtime: &mut RuntimeContext<'_>, _owner: &EffectGraphDef) {}
    #[cfg(feature = "gpu-proofs")]
    fn observe_strings(&mut self, _graph: &mut Graph) {}
}

pub type RuntimeFactory =
    fn(&Graph, &ExecutionPlan, usize) -> Result<Box<dyn RuntimeExtension>, String>;
/// Preparation captures unfused provenance and installs it after graph construction.
pub type PreparedRuntime = Box<dyn FnOnce(&mut PresetRuntime)>;
pub type RuntimePreparation = fn(
    &EffectGraphDef,
    &EffectGraphDef,
    &[SceneModifierImpulseRoute],
    &PrimitiveRegistry,
) -> PreparedRuntime;

pub struct RuntimeRegistration {
    pub name: &'static str,
    pub before_compile: fn(&mut Graph) -> Result<(), GraphError>,
    pub create: RuntimeFactory,
    #[cfg(feature = "gpu-proofs")]
    pub prepare: RuntimePreparation,
}

inventory::collect!(RuntimeRegistration);
static REGISTRATIONS: LazyLock<Vec<&'static RuntimeRegistration>> = LazyLock::new(|| {
    let mut registrations: Vec<_> = inventory::iter::<RuntimeRegistration>.into_iter().collect();
    registrations.sort_unstable_by_key(|entry| entry.name);
    for pair in registrations.windows(2) {
        assert_ne!(
            pair[0].name, pair[1].name,
            "duplicate runtime extension name"
        );
    }
    registrations
});

pub(crate) fn before_compile(graph: &mut Graph) -> Result<(), GraphError> {
    for entry in &*REGISTRATIONS {
        (entry.before_compile)(graph)?;
    }
    Ok(())
}

pub(crate) fn create(
    graph: &Graph,
    plan: &ExecutionPlan,
    slots: usize,
) -> Result<Vec<Box<dyn RuntimeExtension>>, String> {
    REGISTRATIONS
        .iter()
        .map(|entry| (entry.create)(graph, plan, slots))
        .collect()
}

#[cfg(feature = "gpu-proofs")]
pub(crate) fn prepare(
    render: &EffectGraphDef,
    owner: &EffectGraphDef,
    routes: &[SceneModifierImpulseRoute],
    registry: &PrimitiveRegistry,
) -> Vec<PreparedRuntime> {
    REGISTRATIONS
        .iter()
        .map(|entry| (entry.prepare)(render, owner, routes, registry))
        .collect()
}

impl PresetRuntime {
    pub fn extension<T: RuntimeExtension>(&self) -> Option<&T> {
        self.extensions
            .iter()
            .find_map(|value| value.as_ref().as_any().downcast_ref())
    }

    pub fn extension_mut<T: RuntimeExtension>(&mut self) -> Option<(&mut T, RuntimeContext<'_>)> {
        let (extensions, context) = self.extension_context();
        extensions
            .iter_mut()
            .find_map(|value| value.as_mut().as_any_mut().downcast_mut())
            .map(|state| (state, context))
    }

    pub fn extension_slots(&self) -> RuntimeSlots<'_> {
        RuntimeSlots(&self.effect_nodes)
    }
    pub fn compiled_outputs_epoch(&self) -> u64 {
        self.last_forced_outputs_epoch
    }

    pub(super) fn extension_context(
        &mut self,
    ) -> (&mut [Box<dyn RuntimeExtension>], RuntimeContext<'_>) {
        (
            &mut self.extensions,
            RuntimeContext {
                graph: &mut self.graph,
                plan: &self.plan,
                executor: &mut self.executor,
                effect_nodes: RuntimeSlots(&self.effect_nodes),
                type_id: self.type_id.as_ref(),
                width: self.width,
                height: self.height,
                last_forced_outputs_epoch: self.last_forced_outputs_epoch,
                forced_outputs_stale: self.forced_outputs_stale,
            },
        )
    }

    pub(super) fn for_each_extension(
        &mut self,
        mut apply: impl FnMut(&mut dyn RuntimeExtension, &mut RuntimeContext<'_>),
    ) {
        let (extensions, mut context) = self.extension_context();
        for extension in extensions {
            apply(extension.as_mut(), &mut context);
        }
    }

    pub fn set_project_tempo(&mut self, tempo: Option<&ProjectTempo>) {
        self.for_each_extension(|extension, context| {
            extension.set_project_tempo(context.graph, tempo)
        });
        for view in &mut self.math_views {
            for variant in &mut view.variants {
                variant.set_project_tempo(tempo);
            }
        }
    }

    pub fn set_source_instance(&mut self, instance: Option<&PresetInstance>) {
        self.for_each_extension(|extension, context| {
            extension.set_source_instance(context.graph, instance)
        });
        for view in &mut self.math_views {
            for variant in &mut view.variants {
                variant.set_source_instance(instance);
            }
        }
    }

    pub fn carry_generator_state_from(&mut self, prior: &mut Self) {
        let (extensions, mut context) = self.extension_context();
        let (previous, mut old_context) = prior.extension_context();
        for (extension, old) in extensions.iter_mut().zip(previous) {
            extension.carry_from(&mut context, old.as_mut(), &mut old_context);
        }
        self.carry_modifier_control_state_from(prior);
    }
}
