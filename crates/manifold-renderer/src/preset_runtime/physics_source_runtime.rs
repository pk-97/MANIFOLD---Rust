//! Install authored graph identities without reading changing execution values.
use super::{EffectGraphDef, PresetRuntime, PrimitiveRegistry, physics_sources};

impl PresetRuntime {
    pub(super) fn apply_physics_source_graphs(
        &mut self,
        sources: Result<Vec<physics_sources::PhysicsSourceGraph>, String>,
    ) {
        self.physics_source_graphs =
            sources.and_then(|sources| self.resolve_physics_source_graphs(sources));
        self.install_physics_source_identities();
    }

    fn resolve_physics_source_graphs(
        &self,
        sources: Vec<physics_sources::PhysicsSourceGraph>,
    ) -> Result<Vec<(crate::node_graph::NodeInstanceId, [u8; 32])>, String> {
        let mut resolved = Vec::with_capacity(sources.len());
        for source in sources {
            let node = self
                .graph
                .instance_by_node_id(&source.fluid)
                .ok_or_else(|| {
                    format!(
                        "Physics take: authored fluid {} is absent from the installed graph",
                        source.fluid
                    )
                })?;
            resolved.push((node, source.digest));
        }
        if resolved.len()
            != self
                .graph
                .nodes()
                .filter(|node| node.node.type_id().as_str() == "node.fluid_surface")
                .count()
        {
            return Err(
                "Physics take: authored fluid membership changed; rebuild the scene".into(),
            );
        }
        Ok(resolved)
    }

    pub(super) fn install_physics_source_identities(&mut self) {
        let sources = match &self.physics_source_graphs {
            Ok(sources) => sources,
            Err(error) => {
                for node in self
                    .graph
                    .nodes_mut()
                    .filter(|node| node.node.type_id().as_str() == "node.fluid_surface")
                {
                    node.node.set_physics_source_identity(Err(error.clone()));
                }
                return;
            }
        };
        for &(node, digest) in sources {
            self.graph
                .get_node_mut(node)
                .expect("prepared fluid exists")
                .node
                .set_physics_source_identity(Ok(digest));
        }
    }

    /// This runs on an authored graph edit, before card bindings replace its
    /// values with effective modulation. Ordinary frame sampling never hashes
    /// mutable node parameters.
    pub(super) fn refresh_physics_source_graphs(&mut self, owner: &EffectGraphDef) {
        if !self
            .graph
            .nodes()
            .any(|node| node.node.type_id().as_str() == "node.fluid_surface")
        {
            return;
        }
        let registry = PrimitiveRegistry::with_builtin();
        let result = (|| {
            let prepared = if manifold_core::scene_modifier_preset::has_scene_modifier_data(owner)
                || crate::node_graph::scene_modifier_expand::contains_fragments(owner)
            {
                Some(
                    crate::node_graph::scene_modifier_expand::prepare_scene_modifiers(
                        owner, &registry,
                    )
                    .map_err(|error| error.to_string())?,
                )
            } else {
                None
            };
            let expanded = prepared.as_ref().map_or(owner, |prepared| &prepared.def);
            let routes = prepared
                .as_ref()
                .map_or(&[][..], |prepared| prepared.impulse_routes.as_slice());
            physics_sources::prepare(expanded, owner, routes, &registry)
        })();
        self.apply_physics_source_graphs(result);
    }
}
