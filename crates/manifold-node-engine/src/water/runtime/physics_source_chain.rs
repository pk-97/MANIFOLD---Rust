//! Bind each effect card's physics provenance within its existing node scope.
use crate::runtime::PresetRuntime;
use crate::exec::effect_node::NodeInstanceId;
use super::physics_source_state::PhysicsSourceState;
use crate::{graph::Graph, persistence::PrimitiveRegistry, load::loaded_preset_view::loaded_preset_view_by_id};
use manifold_core::effects::PresetInstance;
use manifold_core::NodeId;

impl PhysicsSourceState {
    pub(crate) fn refresh_chain(
        &mut self,
        graph: &mut Graph,
        node_map: &[(NodeId, NodeInstanceId)],
        card_prefix: &str,
        instance: &PresetInstance,
        registry: Option<&PrimitiveRegistry>,
    ) {
        // Normal cards never construct a registry or prepare a physics graph.
        if !node_map.iter().any(|(id, node)| {
            id.as_str().starts_with(card_prefix)
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
            self.apply_prepared(
                graph,
                node_map,
                card_prefix,
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
        self.refresh(graph, node_map, card_prefix, owner, registry);
    }
}

impl PresetRuntime {
    pub(crate) fn initialize_chain_physics_sources(
        &mut self,
        instances: &[PresetInstance],
        registry: &PrimitiveRegistry,
    ) {
        for (slot, source) in self.effect_nodes.iter().zip(&mut self.water.sources) {
            if let Some(instance) = instances.get(slot.legacy_index) {
                source.refresh_chain(
                    &mut self.graph, &slot.node_map, &slot.card_prefix, instance, Some(registry),
                );
                source.set_instance(&mut self.graph, Some(instance));
            }
        }
    }
}
