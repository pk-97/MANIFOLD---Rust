//! CPU trigger-latch state carried across prepared scene-modifier rebuilds.
//!
//! The authored modifier identity and local graph content are the compatibility
//! boundary. Host stack order, unrelated modifiers, and generated labels are
//! deliberately absent from that boundary.

use std::collections::BTreeSet;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use sha2::{Digest, Sha256};

use crate::node_graph::{Graph, NodeInstanceId, StateStore};

use super::{SceneModifierExpandError, routes::SceneModifierNodeRoute};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModifierControlState {
    modifier_id: NodeId,
    graph_hash: String,
    nodes: Vec<ModifierControlNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModifierControlNode {
    generated_node_id: NodeId,
    runtime_node_id: NodeInstanceId,
    primitive_type: String,
}

/// Prepared CPU trigger-latch state for one authored scene-modifier stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedModifierControlState {
    modifiers: Vec<ModifierControlState>,
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

fn eligible(node: &crate::node_graph::NodeInstance) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::{EffectGraphDefExt, NodeState, PrimitiveRegistry};
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::scene_modifier_preset::{
        SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };

    fn local_graph(enable: bool) -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [{
                "id": 0,
                "nodeId": "local-gate",
                "typeId": "node.trigger_gate",
                "params": {"enable": {"type": "Bool", "value": enable}}
            }],
            "wires": []
        }))
        .expect("local modifier graph parses")
    }

    fn owner(ids_and_enable: &[(&str, bool)]) -> EffectGraphDef {
        EffectGraphDef {
            version: 3,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: ids_and_enable
                .iter()
                .map(|(id, enable)| SceneModifierInstanceDef {
                    id: NodeId::new(*id),
                    scene: SceneNodeRef {
                        scope: Vec::new(),
                        node: NodeId::new("scene"),
                    },
                    targets: SceneTargetSelection::AllObjects,
                    mesh_frames: Vec::new(),
                    graph: Box::new(local_graph(*enable)),
                })
                .collect(),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    fn runtime_graph(nodes: &[(&str, &str)]) -> Graph {
        let nodes = nodes
            .iter()
            .enumerate()
            .map(|(id, (node_id, type_id))| {
                serde_json::json!({
                    "id": id,
                    "nodeId": node_id,
                    "typeId": type_id
                })
            })
            .collect::<Vec<_>>();
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": nodes,
            "wires": []
        }))
        .expect("runtime graph parses");
        def.into_graph(&PrimitiveRegistry::with_builtin(), &crate::node_graph::mesh_change::PreparedMeshRules::default())
            .expect("runtime graph builds")
    }

    fn routes(modifiers: &[(&str, &[&str])]) -> Vec<SceneModifierNodeRoute> {
        modifiers
            .iter()
            .map(|(modifier_id, generated)| SceneModifierNodeRoute {
                modifier_id: NodeId::new(*modifier_id),
                local: SceneNodeRef {
                    scope: Vec::new(),
                    node: NodeId::new("local-gate"),
                },
                copies: generated
                    .iter()
                    .map(|node_id| super::super::routes::SceneModifierNodeCopy {
                        object: None,
                        node_id: NodeId::new(*node_id),
                    })
                    .collect(),
            })
            .collect()
    }

    struct Probe;
    impl NodeState for Probe {}

    #[test]
    fn modifier_control_state_unchanged_copies_survive_unrelated_add_and_reorder() {
        let prior_owner = owner(&[("a", true), ("b", true)]);
        let current_owner = owner(&[("b", true), ("a", true), ("c", true)]);
        let prior_routes = routes(&[("a", &["gen-a"]), ("b", &["gen-b"])]);
        let current_routes = routes(&[("b", &["gen-b"]), ("a", &["gen-a"]), ("c", &["gen-c"])]);
        let mut prior_graph = runtime_graph(&[
            ("gen-a", "node.trigger_gate"),
            ("gen-b", "node.trigger_gate"),
        ]);
        let mut graph = runtime_graph(&[
            ("gen-a", "node.trigger_gate"),
            ("gen-b", "node.trigger_gate"),
            ("gen-c", "node.trigger_gate"),
        ]);
        let prior =
            PreparedModifierControlState::prepare(&prior_owner, &prior_routes, &prior_graph)
                .unwrap();
        let current =
            PreparedModifierControlState::prepare(&current_owner, &current_routes, &graph).unwrap();
        let a_old = prior_graph
            .instance_by_node_id(&NodeId::new("gen-a"))
            .unwrap();
        let b_old = prior_graph
            .instance_by_node_id(&NodeId::new("gen-b"))
            .unwrap();
        let a_new = graph.instance_by_node_id(&NodeId::new("gen-a")).unwrap();
        let b_new = graph.instance_by_node_id(&NodeId::new("gen-b")).unwrap();
        let mut prior_state = StateStore::new();
        prior_state.insert(a_old, 0, Probe);
        prior_state.insert(b_old, 0, Probe);
        let mut state = StateStore::new();

        assert_eq!(
            current.harvest_from(
                &prior,
                &mut graph,
                &mut prior_graph,
                &mut state,
                &mut prior_state
            ),
            2
        );
        assert!(state.get::<Probe>(a_new, 0).is_some());
        assert!(state.get::<Probe>(b_new, 0).is_some());
        assert!(prior_state.is_empty());
    }

    #[test]
    fn modifier_control_state_changed_recipe_and_different_instance_do_not_cross() {
        let prior_owner = owner(&[("a", true)]);
        let changed_owner = owner(&[("a", false)]);
        let other_owner = owner(&[("other", true)]);
        let route = routes(&[("a", &["gen-a"])]);
        let other_route = routes(&[("other", &["gen-a"])]);
        let mut prior_graph = runtime_graph(&[("gen-a", "node.trigger_gate")]);
        let mut graph = runtime_graph(&[("gen-a", "node.trigger_gate")]);
        let prior =
            PreparedModifierControlState::prepare(&prior_owner, &route, &prior_graph).unwrap();
        let changed =
            PreparedModifierControlState::prepare(&changed_owner, &route, &graph).unwrap();
        let other =
            PreparedModifierControlState::prepare(&other_owner, &other_route, &graph).unwrap();
        let old_id = prior_graph
            .instance_by_node_id(&NodeId::new("gen-a"))
            .unwrap();
        let new_id = graph.instance_by_node_id(&NodeId::new("gen-a")).unwrap();
        let mut prior_state = StateStore::new();
        prior_state.insert(old_id, 0, Probe);
        let mut state = StateStore::new();
        assert_eq!(
            changed.harvest_from(
                &prior,
                &mut graph,
                &mut prior_graph,
                &mut state,
                &mut prior_state
            ),
            0
        );
        assert!(prior_state.get::<Probe>(old_id, 0).is_some());
        assert_eq!(
            other.harvest_from(
                &prior,
                &mut graph,
                &mut prior_graph,
                &mut state,
                &mut prior_state
            ),
            0
        );
        assert!(state.get::<Probe>(new_id, 0).is_none());
    }

    #[test]
    fn modifier_control_state_only_cpu_latches_and_all_target_copies_are_captured() {
        let graph = runtime_graph(&[
            ("gen-a1", "node.trigger_gate"),
            ("gen-a2", "node.trigger_gate"),
            ("gpu", "node.scale_offset_image"),
            ("plain", "node.value"),
        ]);
        let owner = owner(&[("a", true)]);
        let routes = routes(&[("a", &["gen-a1", "gen-a2", "gpu", "plain"])]);
        let prepared = PreparedModifierControlState::prepare(&owner, &routes, &graph).unwrap();
        assert_eq!(prepared.modifiers[0].nodes.len(), 2);
        assert!(
            prepared.modifiers[0]
                .nodes
                .iter()
                .all(|node| node.primitive_type == "node.trigger_gate")
        );
    }
}
