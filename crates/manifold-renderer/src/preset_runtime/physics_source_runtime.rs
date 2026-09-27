//! Install authored graph and host-control identities without reading changing
//! execution values. The existing PresetInstance remains their only model.
use super::{EffectGraphDef, PresetRuntime, PrimitiveRegistry, physics_sources};
use crate::node_graph::NodeInstanceId;
use manifold_core::effects::PresetInstance;
use sha2::{Digest, Sha256};

pub(super) struct InstalledSource {
    node: NodeInstanceId,
    source: physics_sources::PhysicsSourceGraph,
    /// None means the standalone graph has no host. A host-aware graph must
    /// retain an explicit error until its current controls have been observed.
    controls: Option<Result<[u8; 32], String>>,
}

impl InstalledSource {
    fn identity(&self) -> Result<[u8; 32], String> {
        let Some(controls) = &self.controls else {
            return Ok(self.source.digest);
        };
        let controls = controls.as_ref().map_err(Clone::clone)?;
        let mut hash = Sha256::new();
        hash.update(b"manifold.physics.graph-and-controls.v1");
        hash.update(self.source.digest);
        hash.update(controls);
        Ok(hash.finalize().into())
    }
}

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
    ) -> Result<Vec<InstalledSource>, String> {
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
            // Layout/value edits need not discard the last host observation.
            // A changed control selection does require a fresh host observation.
            let controls = self
                .physics_source_graphs
                .as_ref()
                .ok()
                .and_then(|prior| {
                    prior
                        .iter()
                        .find(|prior| {
                            prior.source.fluid == source.fluid
                                && prior.source.control_ids == source.control_ids
                        })
                        .and_then(|prior| prior.controls.clone())
                })
                .or_else(|| {
                    self.physics_source_has_instance.then(|| {
                        Err("Physics take: current host controls have not been observed".into())
                    })
                });
            resolved.push(InstalledSource {
                node,
                source,
                controls,
            });
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
        for source in sources {
            self.graph
                .get_node_mut(source.node)
                .expect("prepared fluid exists")
                .node
                .set_physics_source_identity(source.identity());
        }
    }

    /// Called by the existing generator/impulse host before observing a frame.
    /// Only authored configuration is hashed; serializers stream into SHA256
    /// without allocating a per-frame JSON buffer or cloning runtime state.
    pub(crate) fn set_physics_source_instance(&mut self, instance: Option<&PresetInstance>) {
        self.physics_source_has_instance = instance.is_some();
        for view in &mut self.math_views {
            for variant in &mut view.variants {
                variant.set_physics_source_instance(instance);
            }
        }
        let Ok(sources) = &mut self.physics_source_graphs else {
            return;
        };
        for source in sources {
            let controls = instance.map(|instance| {
                super::physics_source_controls::digest(&source.source.control_ids, instance)
            });
            if controls != source.controls {
                source.controls = controls;
                self.graph
                    .get_node_mut(source.node)
                    .expect("prepared fluid exists")
                    .node
                    .set_physics_source_identity(source.identity());
            }
        }
    }

    pub(super) fn carry_physics_source_controls_from(&mut self, prior: &Self) {
        if self.physics_source_has_instance {
            return;
        }
        self.physics_source_has_instance = prior.physics_source_has_instance;
        let (Ok(sources), Ok(previous)) = (
            &mut self.physics_source_graphs,
            &prior.physics_source_graphs,
        ) else {
            return;
        };
        for source in sources {
            source.controls = previous
                .iter()
                .find(|old| {
                    old.source.fluid == source.source.fluid
                        && old.source.control_ids == source.source.control_ids
                })
                .and_then(|old| old.controls.clone())
                .or_else(|| {
                    self.physics_source_has_instance.then(|| {
                        Err("Physics take: current host controls have not been observed".into())
                    })
                });
        }
    }

    /// Runs on authored edits, before card bindings replace graph values with
    /// effective modulation. Frame sampling never hashes mutable node params.
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
