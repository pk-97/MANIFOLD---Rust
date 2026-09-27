//! Per-effect-slot state for authored physics source identities.
//!
//! Source graphs are authored in a card-local node-id namespace while the
//! chain graph installs those cards into a prefixed shared namespace.  This
//! type keeps the authored source data and host-control observation separate
//! from the runtime graph, and resolves the local ids through the owning
//! slot's node map whenever identities are installed.

use super::{EffectGraphDef, PrimitiveRegistry, physics_source_controls, physics_sources};
use crate::node_graph::{Graph, NodeInstanceId};
use manifold_core::NodeId;
use manifold_core::effects::PresetInstance;
use sha2::{Digest, Sha256};

pub(super) struct PhysicsSourceState {
    sources: Result<Vec<InstalledSource>, String>,
    has_instance: bool,
}

impl Default for PhysicsSourceState {
    fn default() -> Self {
        Self {
            sources: Ok(Vec::new()),
            has_instance: false,
        }
    }
}

struct InstalledSource {
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

impl PhysicsSourceState {
    /// Replace prepared authored source graphs and resolve their local fluid
    /// ids through this effect slot's prefixed runtime node map.
    pub(super) fn apply_prepared(
        &mut self,
        graph: &mut Graph,
        node_map: &[(NodeId, NodeInstanceId)],
        prefix: &str,
        sources: Result<Vec<physics_sources::PhysicsSourceGraph>, String>,
    ) {
        self.sources =
            sources.and_then(|sources| self.resolve_sources(graph, node_map, prefix, sources));
        self.install(graph, node_map, prefix);
    }

    /// Rebuild authored source identities after an edit, preserving any
    /// compatible host-control observation already carried by this state.
    pub(super) fn refresh(
        &mut self,
        graph: &mut Graph,
        node_map: &[(NodeId, NodeInstanceId)],
        prefix: &str,
        owner: &EffectGraphDef,
        registry: &PrimitiveRegistry,
    ) {
        if scoped_fluid_count(graph, node_map, prefix) == 0 {
            return;
        }
        let result = (|| {
            let prepared = if manifold_core::scene_modifier_preset::has_scene_modifier_data(owner)
                || crate::node_graph::scene_modifier_expand::contains_fragments(owner)
            {
                Some(
                    crate::node_graph::scene_modifier_expand::prepare_scene_modifiers(
                        owner, registry,
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
            physics_sources::prepare(expanded, owner, routes, registry)
        })();
        self.apply_prepared(graph, node_map, prefix, result);
    }

    /// Update the host-control digest for the currently installed sources.
    /// The successful path hashes directly through the existing digest helper
    /// and does not allocate a per-frame JSON representation.
    pub(super) fn set_instance(&mut self, graph: &mut Graph, instance: Option<&PresetInstance>) {
        self.has_instance = instance.is_some();
        let Ok(sources) = &mut self.sources else {
            return;
        };
        for source in sources {
            let controls = instance.map(|instance| {
                physics_source_controls::digest(&source.source.control_ids, instance)
            });
            if controls != source.controls {
                source.controls = controls;
                graph
                    .get_node_mut(source.node)
                    .expect("prepared fluid exists")
                    .node
                    .set_physics_source_identity(source.identity());
            }
        }
    }

    /// Install the currently retained identities on the fluid nodes scoped to
    /// this slot. Errors from one slot therefore cannot overwrite another.
    pub(super) fn install(
        &self,
        graph: &mut Graph,
        node_map: &[(NodeId, NodeInstanceId)],
        prefix: &str,
    ) {
        match &self.sources {
            Ok(sources) => {
                for source in sources {
                    graph
                        .get_node_mut(source.node)
                        .expect("prepared fluid exists")
                        .node
                        .set_physics_source_identity(source.identity());
                }
            }
            Err(error) => {
                for (id, node) in node_map {
                    if id.as_str().starts_with(prefix)
                        && let Some(node) = graph.get_node_mut(*node)
                        && node.node.type_id().as_str() == "node.fluid_surface"
                    {
                        node.node.set_physics_source_identity(Err(error.clone()));
                    }
                }
            }
        }
    }

    /// Carry compatible host-control observations across a generator rebuild.
    pub(super) fn carry_controls_from(&mut self, prior: &Self) {
        if self.has_instance {
            return;
        }
        self.has_instance = prior.has_instance;
        let (Ok(sources), Ok(previous)) = (&mut self.sources, &prior.sources) else {
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
                    self.has_instance.then(|| {
                        Err("Physics take: current host controls have not been observed".into())
                    })
                });
        }
    }

    fn resolve_sources(
        &self,
        graph: &Graph,
        node_map: &[(NodeId, NodeInstanceId)],
        prefix: &str,
        sources: Vec<physics_sources::PhysicsSourceGraph>,
    ) -> Result<Vec<InstalledSource>, String> {
        let mut resolved = Vec::with_capacity(sources.len());
        for source in sources {
            let local = prefixed_node_id(prefix, &source.fluid);
            let node = node_map
                .iter()
                .find_map(|(node_id, instance)| (node_id == &local).then_some(*instance))
                .ok_or_else(|| {
                    format!(
                        "Physics take: authored fluid {} is absent from the installed graph",
                        source.fluid
                    )
                })?;
            if graph
                .get_node(node)
                .is_none_or(|node| node.node.type_id().as_str() != "node.fluid_surface")
            {
                return Err(format!(
                    "Physics take: authored fluid {} resolves to a different node type",
                    source.fluid
                ));
            }
            // Layout/value edits need not discard the last host observation.
            // A changed control selection does require a fresh host observation.
            let controls = self
                .sources
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
                    self.has_instance.then(|| {
                        Err("Physics take: current host controls have not been observed".into())
                    })
                });
            resolved.push(InstalledSource {
                node,
                source,
                controls,
            });
        }
        if resolved.len() != scoped_fluid_count(graph, node_map, prefix) {
            return Err(
                "Physics take: authored fluid membership changed; rebuild the scene".into(),
            );
        }
        Ok(resolved)
    }
}

fn prefixed_node_id(prefix: &str, node_id: &NodeId) -> NodeId {
    if prefix.is_empty() {
        node_id.clone()
    } else {
        NodeId::new(format!("{prefix}{node_id}"))
    }
}

fn scoped_fluid_count(graph: &Graph, node_map: &[(NodeId, NodeInstanceId)], prefix: &str) -> usize {
    scoped_fluid_nodes(graph, node_map, prefix).count()
}

fn scoped_fluid_nodes<'a>(
    graph: &'a Graph,
    node_map: &'a [(NodeId, NodeInstanceId)],
    prefix: &'a str,
) -> impl Iterator<Item = (&'a NodeId, NodeInstanceId)> + 'a {
    node_map.iter().filter_map(move |(node_id, instance)| {
        if prefix.is_empty() {
            return graph
                .get_node(*instance)
                .filter(|node| node.node.type_id().as_str() == "node.fluid_surface")
                .map(|_| (node_id, *instance));
        }
        node_id
            .as_str()
            .strip_prefix(prefix)
            .and_then(|local| (!local.is_empty()).then_some(local))
            .and_then(|_| {
                graph
                    .get_node(*instance)
                    .filter(|node| node.node.type_id().as_str() == "node.fluid_surface")
                    .map(|_| (node_id, *instance))
            })
    })
}
