//! Bind each effect card's physics provenance within its existing node scope.
use super::{EffectSlot, PresetRuntime};
use crate::node_graph::{Graph, PrimitiveRegistry, loaded_preset_view_by_id};
use manifold_core::effects::PresetInstance;

impl EffectSlot {
    pub(super) fn refresh_chain_physics_source(
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
                    .is_some_and(|node| node.node.type_id().as_str() == "node.fluid_surface")
        }) {
            return;
        }
        let owner = instance.graph.as_ref().or_else(|| {
            loaded_preset_view_by_id(instance.effect_type()).map(|view| view.canonical_def.as_ref())
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
                fallback_registry = PrimitiveRegistry::with_builtin();
                &fallback_registry
            }
        };
        self.physics_sources
            .refresh(graph, &self.node_map, &self.card_prefix, owner, registry);
    }
}

impl PresetRuntime {
    pub(super) fn initialize_chain_physics_sources(
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
