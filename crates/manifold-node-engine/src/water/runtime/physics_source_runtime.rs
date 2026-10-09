//! Generator and runtime wrappers for per-effect-slot physics source state.

use crate::runtime::PresetRuntime;
#[cfg(feature = "gpu-proofs")]
use {manifold_core::effect_graph_def::EffectGraphDef, crate::persistence::PrimitiveRegistry, super::physics_sources};
use manifold_core::effects::PresetInstance;

impl PresetRuntime {
    /// Apply prepared source graphs to the standalone generator slot.
    #[cfg(feature = "gpu-proofs")]
    pub(crate) fn apply_physics_source_graphs(
        &mut self,
        sources: Result<Vec<physics_sources::PhysicsSourceGraph>, String>,
    ) {
        if let (Some(slot), Some(source)) =
            (self.effect_nodes.first(), self.water.sources.first_mut())
        {
            source.apply_prepared(
                &mut self.graph,
                &slot.node_map,
                &slot.card_prefix,
                sources,
            );
        }
    }

    /// Install every slot's current identity on only its scoped fluid nodes.
    #[cfg(feature = "gpu-proofs")]
    pub(crate) fn install_physics_source_identities(&mut self) {
        for (slot, source) in self.effect_nodes.iter().zip(&mut self.water.sources) {
            source.install(&mut self.graph, &slot.node_map, &slot.card_prefix);
        }
    }

    #[cfg(feature = "gpu-proofs")]
    pub(crate) fn observe_physics_source_strings(&mut self) {
        if let Some(source) = self.water.sources.first_mut() {
            source.observe_strings(&mut self.graph);
        }
    }

    #[cfg(feature = "gpu-proofs")]
    pub(super) fn observe_physics_source_assets(&mut self) {
        for source in &mut self.water.sources {
            source.observe_assets(&mut self.graph);
        }
    }

    /// Called by the existing generator/impulse host before observing a frame.
    /// Only authored configuration is hashed; serializers stream into SHA256
    /// without allocating a per-frame JSON buffer or cloning runtime state.
    pub fn set_physics_source_instance(&mut self, instance: Option<&PresetInstance>) {
        #[cfg(feature = "gpu-proofs")]
        if let Some(source) = self.water.sources.first_mut() {
            source.set_instance(&mut self.graph, instance);
        }
        if let Some(inputs) = self.water.input_snapshot.as_mut() {
            inputs.set_hops(instance);
        }
        for view in &mut self.math_views {
            for variant in &mut view.variants {
                variant.set_physics_source_instance(instance);
            }
        }
    }

    #[cfg(feature = "gpu-proofs")]
    pub(super) fn carry_physics_source_controls_from(&mut self, prior: &Self) {
        if let (Some(source), Some(old_source)) =
            (self.water.sources.first_mut(), prior.water.sources.first())
        {
            source.carry_controls_from(old_source);
        }
    }

    /// Runs on authored edits, before card bindings replace graph values with
    /// effective modulation. Numeric modulation never enters this graph digest;
    /// applied string inputs are observed separately through their bindings.
    #[cfg(feature = "gpu-proofs")]
    pub(crate) fn refresh_physics_source_graphs(&mut self, owner: &EffectGraphDef) {
        if !self
            .graph
            .nodes()
            .any(|node| node.node.type_id().as_str() == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID)
        {
            return;
        }
        let registry = PrimitiveRegistry::with_cpu_flip_reference();
        if let (Some(slot), Some(source)) =
            (self.effect_nodes.first(), self.water.sources.first_mut())
        {
            source.refresh(
                &mut self.graph,
                &slot.node_map,
                &slot.card_prefix,
                owner,
                &registry,
            );
        }
    }
}
