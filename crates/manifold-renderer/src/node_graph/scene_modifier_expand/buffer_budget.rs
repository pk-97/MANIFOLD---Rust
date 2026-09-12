//! Admission for additional prepared modifier buffers, using allocator actions.

use super::{SceneModifierExpandError, SceneModifierNodeRoute};
use crate::node_graph::resource_allocation::{ArrayAllocationAction, ArrayAllocationPlan};
use crate::node_graph::{Graph, NodeInstanceId};
use ahash::{AHashMap, AHashSet};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_modifier_preset::SceneNodeRef;
use std::collections::BTreeMap;

pub const MODIFIER_BUFFER_LIMIT_BYTES: u64 = 256 * 1024 * 1024;

/// Physical prepared arrays shared by references or aliases are counted once.
/// Baseline excludes the union of all modifier-owned allocations in this owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifierBufferUsage {
    pub baseline_bytes: u64,
    pub modifier_bytes: BTreeMap<SceneNodeRef, u64>,
}

pub struct PreparedModifierBufferBudget {
    scenes: BTreeMap<SceneNodeRef, AHashSet<NodeInstanceId>>,
}

fn invalid(detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::MissingTarget {
        path: "modifierBufferBudget".into(),
        detail: detail.into(),
    }
}

impl PreparedModifierBufferBudget {
    /// Resolve attribution before installation, including parameterless atoms
    /// represented by the compiler's complete fused-member map.
    pub fn prepare(
        owner: &EffectGraphDef,
        routes: &[SceneModifierNodeRoute],
        graph: &Graph,
        fused_members: &AHashMap<NodeId, NodeId>,
    ) -> Result<Self, SceneModifierExpandError> {
        let mut scenes: BTreeMap<_, AHashSet<_>> = owner
            .scene_modifiers
            .iter()
            .map(|instance| (instance.scene.clone(), AHashSet::default()))
            .collect();
        for route in routes {
            let instance = owner
                .scene_modifiers
                .iter()
                .find(|instance| instance.id == route.modifier_id)
                .ok_or_else(|| invalid(format!("unknown modifier {}", route.modifier_id)))?;
            let nodes = scenes
                .get_mut(&instance.scene)
                .expect("scene inserted above");
            for copy in &route.copies {
                let target = fused_members.get(&copy.node_id).unwrap_or(&copy.node_id);
                let runtime = graph
                    .instance_by_node_id(target)
                    .ok_or_else(|| invalid(format!("missing prepared node {target}")))?;
                nodes.insert(runtime);
            }
        }
        Ok(Self { scenes })
    }

    pub fn check(
        &self,
        allocation: &ArrayAllocationPlan,
    ) -> Result<ModifierBufferUsage, SceneModifierExpandError> {
        self.check_limit(allocation, MODIFIER_BUFFER_LIMIT_BYTES)
    }

    fn check_limit(
        &self,
        allocation: &ArrayAllocationPlan,
        limit: u64,
    ) -> Result<ModifierBufferUsage, SceneModifierExpandError> {
        let mut modifiers = AHashSet::default();
        let mut usage = BTreeMap::new();
        for (scene, nodes) in &self.scenes {
            let mut roots = AHashSet::default();
            let mut bytes = 0u64;
            for action in &allocation.actions {
                if let ArrayAllocationAction::Allocate(item) = action
                    && nodes.contains(&item.node)
                    && roots.insert(item.resource)
                {
                    bytes = bytes
                        .checked_add(item.bytes)
                        .ok_or_else(|| Self::exceeded(scene, u64::MAX, limit))?;
                    modifiers.insert(item.resource);
                }
            }
            if bytes > limit {
                return Err(Self::exceeded(scene, bytes, limit));
            }
            usage.insert(scene.clone(), bytes);
        }
        let mut baseline_bytes = 0u64;
        let mut roots = AHashSet::default();
        for storage in allocation.storage.values() {
            if !modifiers.contains(&storage.root) && roots.insert(storage.root) {
                baseline_bytes = baseline_bytes.checked_add(storage.bytes).ok_or_else(|| {
                    SceneModifierExpandError::CapacityExceeded {
                        path: "baselineBuffers".into(),
                        detail: "prepared baseline byte count overflow".into(),
                    }
                })?;
            }
        }
        Ok(ModifierBufferUsage {
            baseline_bytes,
            modifier_bytes: usage,
        })
    }

    fn exceeded(scene: &SceneNodeRef, requested: u64, allowed: u64) -> SceneModifierExpandError {
        SceneModifierExpandError::CapacityExceeded {
            path: format!("scene {:?} modifierBuffers", scene),
            detail: format!(
                "requested {requested} additional prepared buffer bytes; allowed {allowed}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::resource_allocation::{ArrayAllocation, ArrayStorage};

    #[test]
    fn scene_modifier_buffer_budget_counts_shared_storage_once_and_separates_baseline() {
        let scene = SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("scene"),
        };
        let budget = PreparedModifierBufferBudget {
            scenes: BTreeMap::from([(scene.clone(), AHashSet::from_iter([NodeInstanceId(2)]))]),
        };
        let allocation = ArrayAllocationPlan {
            actions: vec![
                ArrayAllocationAction::Allocate(ArrayAllocation {
                    node: NodeInstanceId(1),
                    resource: ResourceId(0),
                    bytes: 100,
                    zero_init: false,
                }),
                ArrayAllocationAction::Allocate(ArrayAllocation {
                    node: NodeInstanceId(2),
                    resource: ResourceId(1),
                    bytes: 80,
                    zero_init: false,
                }),
                ArrayAllocationAction::Alias {
                    resource: ResourceId(2),
                    input: ResourceId(0),
                },
                ArrayAllocationAction::Alias {
                    resource: ResourceId(3),
                    input: ResourceId(1),
                },
            ],
            storage: AHashMap::from_iter([
                (
                    ResourceId(0),
                    ArrayStorage {
                        root: ResourceId(0),
                        bytes: 100,
                    },
                ),
                (
                    ResourceId(1),
                    ArrayStorage {
                        root: ResourceId(1),
                        bytes: 80,
                    },
                ),
                (
                    ResourceId(2),
                    ArrayStorage {
                        root: ResourceId(0),
                        bytes: 100,
                    },
                ),
                (
                    ResourceId(3),
                    ArrayStorage {
                        root: ResourceId(1),
                        bytes: 80,
                    },
                ),
            ]),
            warnings: Vec::new(),
        };
        let unchanged = allocation.clone();
        let usage = budget.check_limit(&allocation, 80).unwrap();
        assert_eq!(usage.baseline_bytes, 100);
        assert_eq!(usage.modifier_bytes[&scene], 80);
        let error = budget.check_limit(&allocation, 79).unwrap_err();
        assert!(error.to_string().contains("requested 80"));
        assert!(error.to_string().contains("allowed 79"));
        assert_eq!(allocation, unchanged);
    }
}
