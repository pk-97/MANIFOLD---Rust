//! Bind each effect card's physics provenance within its existing node scope.
use crate::runtime::{core::EffectSlot, PresetRuntime};
use crate::{graph::Graph, persistence::PrimitiveRegistry, load::loaded_preset_view::loaded_preset_view_by_id};
use manifold_core::effects::PresetInstance;

impl EffectSlot {
    pub(crate) fn refresh_chain_physics_source(
        &mut self,
        graph: &mut Graph,
        instance: &PresetInstance,
        registry: Option<&PrimitiveRegistry>,
    ) {
        // Normal cards never construct a registry or prepare a physics graph.
        if !self.node_map.iter().any(|(id, node)| {
            id.as_str().starts_with(&self.card_prefix)
                && graph
                    .get_node(*node)
                    .is_some_and(|node| {
                        node.node.type_id().as_str() == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID
                    })
        }) {
            return;
        }
        let catalog_view = if instance.graph.is_none() {
            loaded_preset_view_by_id(instance.effect_type())
        } else {
            None
        };
        let owner = instance.graph.as_ref().or_else(|| {
            catalog_view.as_ref().map(|view| view.canonical_def.as_ref())
        });
        let Some(owner) = owner else {
            self.physics_sources.apply_prepared(
                graph,
                &self.node_map,
                &self.card_prefix,
                Err("Physics take: effect's authored graph is unavailable".into()),
            );
            return;
        };
        let fallback_registry;
        let registry = match registry {
            Some(registry) => registry,
            None => {
                #[cfg(feature = "gpu-proofs")]
                let registry = PrimitiveRegistry::with_cpu_flip_reference();
                #[cfg(not(feature = "gpu-proofs"))]
                let registry = PrimitiveRegistry::with_builtin();
                fallback_registry = registry;
                &fallback_registry
            }
        };
        self.physics_sources
            .refresh(graph, &self.node_map, &self.card_prefix, owner, registry);
    }
}

impl PresetRuntime {
    pub(crate) fn initialize_chain_physics_sources(
        &mut self,
        instances: &[PresetInstance],
        registry: &PrimitiveRegistry,
    ) {
        for slot in &mut self.effect_nodes {
            if let Some(instance) = instances.get(slot.legacy_index) {
                slot.refresh_chain_physics_source(&mut self.graph, instance, Some(registry));
                slot.physics_sources
                    .set_instance(&mut self.graph, Some(instance));
            }
        }
    }
}
