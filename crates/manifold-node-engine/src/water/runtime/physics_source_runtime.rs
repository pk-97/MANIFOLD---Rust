//! Generator and runtime wrappers for per-effect-slot physics source state.

use crate::runtime::PresetRuntime;
#[cfg(feature = "gpu-proofs")]
use {crate::runtime::EffectGraphDef, crate::persistence::PrimitiveRegistry, super::physics_sources};
use manifold_core::effects::PresetInstance;

impl PresetRuntime {
    /// Apply prepared source graphs to the standalone generator slot.
    #[cfg(feature = "gpu-proofs")]
    pub(super) fn apply_physics_source_graphs(
        &mut self,
        sources: Result<Vec<physics_sources::PhysicsSourceGraph>, String>,
    ) {
        if let Some(slot) = self.effect_nodes.first_mut() {
            slot.physics_sources.apply_prepared(
                &mut self.graph,
                &slot.node_map,
                &slot.card_prefix,
                sources,
            );
        }
    }

    /// Install every slot's current identity on only its scoped fluid nodes.
    #[cfg(feature = "gpu-proofs")]
    pub(super) fn install_physics_source_identities(&mut self) {
        for slot in &mut self.effect_nodes {
            slot.physics_sources
                .install(&mut self.graph, &slot.node_map, &slot.card_prefix);
        }
    }

    #[cfg(feature = "gpu-proofs")]
    pub(super) fn observe_physics_source_strings(&mut self) {
        if let Some(slot) = self.effect_nodes.first_mut() {
            slot.physics_sources.observe_strings(&mut self.graph);
        }
    }

    #[cfg(feature = "gpu-proofs")]
    pub(super) fn observe_physics_source_assets(&mut self) {
        for slot in &mut self.effect_nodes {
            slot.physics_sources.observe_assets(&mut self.graph);
        }
    }

    /// Called by the existing generator/impulse host before observing a frame.
    /// Only authored configuration is hashed; serializers stream into SHA256
    /// without allocating a per-frame JSON buffer or cloning runtime state.
    pub(crate) fn set_physics_source_instance(&mut self, instance: Option<&PresetInstance>) {
        #[cfg(feature = "gpu-proofs")]
        if let Some(slot) = self.effect_nodes.first_mut() {
            slot.physics_sources.set_instance(&mut self.graph, instance);
        }
        if let Some(inputs) = self.physics_input_snapshot.as_mut() {
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
        if let (Some(slot), Some(old_slot)) =
            (self.effect_nodes.first_mut(), prior.effect_nodes.first())
        {
            slot.physics_sources
                .carry_controls_from(&old_slot.physics_sources);
        }
    }

    /// Runs on authored edits, before card bindings replace graph values with
    /// effective modulation. Numeric modulation never enters this graph digest;
    /// applied string inputs are observed separately through their bindings.
    #[cfg(feature = "gpu-proofs")]
    pub(super) fn refresh_physics_source_graphs(&mut self, owner: &EffectGraphDef) {
        if !self
            .graph
            .nodes()
            .any(|node| node.node.type_id().as_str() == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID)
        {
            return;
        }
        let registry = PrimitiveRegistry::with_cpu_flip_reference();
        if let Some(slot) = self.effect_nodes.first_mut() {
            slot.physics_sources.refresh(
                &mut self.graph,
                &slot.node_map,
                &slot.card_prefix,
                owner,
                &registry,
            );
        }
    }
}
