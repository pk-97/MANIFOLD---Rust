//! CPU trigger-latch state carried across prepared scene-modifier rebuilds.
//!
//! The authored modifier identity and local graph content are the compatibility
//! boundary. Host stack order, unrelated modifiers, and generated labels are
//! deliberately absent from that boundary.

use std::collections::BTreeSet;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use sha2::{Digest, Sha256};

use crate::{graph::Graph, exec::effect_node::NodeInstanceId, state_store::StateStore};

use super::{SceneModifierExpandError, routes::SceneModifierNodeRoute};

#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct ModifierControlState {
    pub modifier_id: NodeId,
    pub graph_hash: String,
    pub nodes: Vec<ModifierControlNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct ModifierControlNode {
    pub generated_node_id: NodeId,
    pub runtime_node_id: NodeInstanceId,
    pub primitive_type: String,
}

/// Prepared CPU trigger-latch state for one authored scene-modifier stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedModifierControlState {
    #[doc(hidden)]
    pub modifiers: Vec<ModifierControlState>,
}

impl PreparedModifierControlState {
    /// Capture routed CPU trigger-latch nodes from a prepared graph.
    pub fn prepare(
        owner: &EffectGraphDef,
        routes: &[SceneModifierNodeRoute],
        graph: &Graph,
    ) -> Result<Self, SceneModifierExpandError> {
        Self::prepare_with_fusion(owner, routes, graph, &ahash::AHashMap::default())
    }

    pub fn prepare_with_fusion(
        owner: &EffectGraphDef,
        routes: &[SceneModifierNodeRoute],
        graph: &Graph,
        fused_members: &ahash::AHashMap<NodeId, NodeId>,
    ) -> Result<Self, SceneModifierExpandError> {
        let mut seen_modifiers = BTreeSet::new();
        let mut modifiers = Vec::with_capacity(owner.scene_modifiers.len());

        for instance in &owner.scene_modifiers {
            if instance.id.is_empty() || !seen_modifiers.insert(instance.id.to_string()) {
                return Err(SceneModifierExpandError::DuplicateIdentity {
                    path: format!("sceneModifiers[{}].id", instance.id),
                    detail: "modifier ids must be nonempty and unique".into(),
                });
            }
            let graph_hash = graph_hash(&instance.graph, &instance.id)?;
            let mut nodes = Vec::new();
            let mut seen_generated = BTreeSet::new();

            for route in routes
                .iter()
                .filter(|route| route.modifier_id == instance.id)
            {
                for copy in &route.copies {
                    if !seen_generated.insert(copy.node_id.to_string()) {
                        return Err(SceneModifierExpandError::DuplicateIdentity {
                            path: format!("sceneModifiers[{}].routes", instance.id),
                            detail: format!(
                                "generated node {} is listed more than once",
                                copy.node_id
                            ),
                        });
                    }
                    // Complete fusion membership proves these absent copies
                    // were absorbed into GPU kernels. CPU latches never fuse.
                    if fused_members.contains_key(&copy.node_id) {
                        continue;
                    }
                    let runtime_node_id =
                        graph.instance_by_node_id(&copy.node_id).ok_or_else(|| {
                            SceneModifierExpandError::MissingTarget {
                                path: format!("sceneModifiers[{}].routes", instance.id),
                                detail: format!(
                                    "generated node {} is absent from the prepared graph",
                                    copy.node_id
                                ),
                            }
                        })?;
                    let node = graph
                        .get_node(runtime_node_id)
                        .expect("instance_by_node_id returned a missing runtime node");
                    if !eligible(node) {
                        continue;
                    }
                    nodes.push(ModifierControlNode {
                        generated_node_id: copy.node_id.clone(),
                        runtime_node_id,
                        primitive_type: node.node.type_id().as_str().to_string(),
                    });
                }
            }

            modifiers.push(ModifierControlState {
                modifier_id: instance.id.clone(),
                graph_hash,
                nodes,
            });
        }

        for route in routes {
            if !seen_modifiers.contains(route.modifier_id.as_str()) {
                return Err(SceneModifierExpandError::MissingTarget {
                    path: "sceneModifiers.routes".into(),
                    detail: format!("route references unknown modifier {}", route.modifier_id),
                });
            }
        }

        Ok(Self { modifiers })
    }

    /// Carry unchanged CPU trigger-latch implementations and state buckets
    /// from `prior` into the current prepared graph.
    pub fn harvest_from(
        &self,
        prior: &Self,
        graph: &mut Graph,
        prior_graph: &mut Graph,
        state: &mut StateStore,
        prior_state: &mut StateStore,
    ) -> usize {
        let mut migrated = 0;
        for current_modifier in &self.modifiers {
            let Some(previous_modifier) = prior.modifiers.iter().find(|candidate| {
                candidate.modifier_id == current_modifier.modifier_id
                    && candidate.graph_hash == current_modifier.graph_hash
            }) else {
                continue;
            };

            for current_node in &current_modifier.nodes {
                let Some(previous_node) = previous_modifier.nodes.iter().find(|candidate| {
                    candidate.generated_node_id == current_node.generated_node_id
                }) else {
                    continue;
                };
                let Some(current) = graph.get_node(current_node.runtime_node_id) else {
                    continue;
                };
                let Some(previous) = prior_graph.get_node(previous_node.runtime_node_id) else {
                    continue;
                };
                if current.node.type_id().as_str() != current_node.primitive_type
                    || previous.node.type_id().as_str() != previous_node.primitive_type
                    || current.node.type_id() != previous.node.type_id()
                    || !eligible(current)
                    || !eligible(previous)
                {
                    continue;
                }

                let current = graph
                    .get_node_mut(current_node.runtime_node_id)
                    .expect("current node was checked above");
                let previous = prior_graph
                    .get_node_mut(previous_node.runtime_node_id)
                    .expect("previous node was checked above");
                std::mem::swap(&mut current.node, &mut previous.node);
                prior_state.migrate_node(
                    previous_node.runtime_node_id,
                    current_node.runtime_node_id,
                    state,
                );
                migrated += 1;
            }
        }
        migrated
    }
}

fn eligible(node: &crate::graph::NodeInstance) -> bool {
    node.node.is_trigger_latch() && !node.node.requires().gpu_encoder
}

fn graph_hash(
    graph: &EffectGraphDef,
    modifier_id: &NodeId,
) -> Result<String, SceneModifierExpandError> {
    let encoded =
        serde_json::to_vec(graph).map_err(|error| SceneModifierExpandError::InvalidRecipe {
            path: format!("sceneModifiers[{modifier_id}].graph"),
            detail: format!("cannot hash modifier graph: {error}"),
        })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}
