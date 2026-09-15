//! Admission for additional prepared modifier buffers, using allocator actions.

use super::{SceneModifierExpandError, SceneModifierNodeRoute};
use crate::node_graph::resource_allocation::{ArrayAllocationAction, ArrayAllocationPlan};
use crate::node_graph::{Graph, NodeInstanceId};
use ahash::{AHashMap, AHashSet};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_modifier_preset::SceneNodeRef;
use std::collections::BTreeMap;

/// Optional aggregate byte ceiling override, expressed in MiB. This is read
/// and validated once at each structural admission boundary (never per frame).
pub const MODIFIER_MEMORY_OVERRIDE_ENV: &str = "MANIFOLD_MODIFIER_MEMORY_MIB";
const DEFAULT_WORKING_SET_FRACTION_NUMERATOR: u64 = 3;
const DEFAULT_WORKING_SET_FRACTION_DENOMINATOR: u64 = 4;

/// Physical prepared arrays shared by references or aliases are counted once.
/// Baseline excludes the union of all modifier-owned allocations in this owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifierBufferUsage {
    pub baseline_bytes: u64,
    pub modifier_bytes: BTreeMap<SceneNodeRef, u64>,
    /// Total bytes in fresh physical array roots in this candidate, including
    /// arrays belonging to the unmodified baseline graph.
    pub candidate_bytes: u64,
}

pub struct PreparedModifierBufferBudget {
    scenes: BTreeMap<SceneNodeRef, AHashSet<NodeInstanceId>>,
    cutters: AHashSet<NodeInstanceId>,
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
        // Generated appearance masks and real-face samples belong to the
        // modifier even though they have no authored parameter route.
        for modifier in &owner.scene_modifiers {
            let nodes = scenes
                .get_mut(&modifier.scene)
                .expect("scene inserted above");
            for frame in &modifier.mesh_frames {
                for role in ["weights", "samples"] {
                    let id = super::compiler::math_events::resource_node_id(
                        &modifier.id,
                        &frame.target,
                        role,
                    );
                    let target = fused_members.get(&id).unwrap_or(&id);
                    if let Some(runtime) = graph.instance_by_node_id(target) {
                        nodes.insert(runtime);
                    }
                }
            }
        }
        // Cut-map and remap nodes are structural children generated after
        // authored routes are built. Attribute them by following their output
        // to the first authored route node, so independent scenes do not pay
        // for one another's fixed reserve.
        let mut route_scene = AHashMap::new();
        for scene in scenes.keys() {
            if let Some(runtime) = graph.instance_by_node_id(&scene.node) {
                route_scene.insert(runtime, scene.clone());
            }
        }
        for route in routes {
            let scene = owner
                .scene_modifiers
                .iter()
                .find(|instance| instance.id == route.modifier_id)
                .map(|instance| instance.scene.clone())
                .expect("route modifier exists");
            for copy in &route.copies {
                let target = fused_members.get(&copy.node_id).unwrap_or(&copy.node_id);
                if let Some(runtime) = graph.instance_by_node_id(target) {
                    route_scene.insert(runtime, scene.clone());
                }
            }
        }
        let mut generated: AHashSet<_> = graph
            .nodes()
            .filter(|node| {
                matches!(
                    node.node.type_id().as_str(),
                    "node.cut_mesh_bands"
                        | "node.cut_mesh_cells"
                        | "node.remap_mesh_cut"
                        | "node.remap_cut_weights"
                )
            })
            .map(|node| node.id)
            .collect();
        let prefix = super::namespace::namespace_node_id(&["fragment_cut"]);
        for (original, fused) in fused_members {
            if original.as_str().starts_with(prefix.as_str())
                && let Some(runtime) = graph.instance_by_node_id(fused)
            {
                generated.insert(runtime);
            }
        }
        for generated_id in generated {
            let mut frontier = vec![generated_id];
            let mut visited = AHashSet::from_iter([generated_id]);
            while let Some(id) = frontier.pop() {
                if let Some(scene) = route_scene.get(&id) {
                    scenes
                        .get_mut(scene)
                        .expect("route scene exists")
                        .insert(generated_id);
                    break;
                }
                for wire in graph.wires_from(id) {
                    if visited.insert(wire.to.0) {
                        frontier.push(wire.to.0);
                    }
                }
            }
        }
        let cutters = graph
            .nodes()
            .filter(|node| {
                matches!(
                    node.node.type_id().as_str(),
                    "node.cut_mesh_bands" | "node.cut_mesh_cells"
                )
            })
            .map(|node| node.id)
            .collect();
        Ok(Self { scenes, cutters })
    }

    /// Account the candidate's physical arrays without applying a device
    /// ceiling. This is useful to diagnostics and tests that intentionally
    /// have no GPU snapshot. Admission callers must use
    /// [`Self::check_with_snapshot`] so current device allocations are part of
    /// the projected peak.
    pub fn account(
        &self,
        allocation: &ArrayAllocationPlan,
    ) -> Result<ModifierBufferUsage, SceneModifierExpandError> {
        self.account_impl(allocation)
    }

    /// Compatibility alias for callers that only need pure accounting.
    #[doc(hidden)]
    pub fn check(
        &self,
        allocation: &ArrayAllocationPlan,
    ) -> Result<ModifierBufferUsage, SceneModifierExpandError> {
        self.account(allocation)
    }

    /// Admit the candidate against one point-in-time device memory snapshot.
    /// The current allocation is intentionally retained in the sum: replacing
    /// a live scene can overlap old and new resources until GPU retirement.
    pub fn check_with_snapshot(
        &self,
        allocation: &ArrayAllocationPlan,
        snapshot: Option<manifold_gpu::GpuMemorySnapshot>,
    ) -> Result<ModifierBufferUsage, SceneModifierExpandError> {
        let snapshot = snapshot
            .ok_or_else(|| Self::memory_unavailable("device memory limits are unavailable"))?;
        let (allowed, policy) = configured_limit(snapshot.recommended_max_working_set_bytes)?;
        let usage = self.account(allocation)?;
        let candidate = usage.candidate_bytes;
        admit_candidate_bytes_with_limit(snapshot, candidate, allowed, &policy)?;
        Ok(usage)
    }

    fn account_impl(
        &self,
        allocation: &ArrayAllocationPlan,
    ) -> Result<ModifierBufferUsage, SceneModifierExpandError> {
        let mut modifiers = AHashSet::default();
        let mut usage = BTreeMap::new();
        let mut candidate_bytes = 0u64;
        let mut candidate_roots = AHashSet::default();
        for (scene, nodes) in &self.scenes {
            let mut roots = AHashSet::default();
            let mut bytes = 0u64;
            for action in &allocation.actions {
                if let ArrayAllocationAction::Allocate(item) = action
                    && nodes.contains(&item.node)
                    && roots.insert(item.resource)
                {
                    bytes = bytes
                        .checked_add(self.allocation_bytes(item)?)
                        .ok_or_else(|| {
                            Self::memory_exceeded(
                                0,
                                u64::MAX,
                                u64::MAX,
                                "checked arithmetic overflow",
                            )
                        })?;
                    modifiers.insert(item.resource);
                }
            }
            usage.insert(scene.clone(), bytes);
        }
        // Count every newly planned physical root, including baseline arrays.
        // Aliases do not allocate and therefore do not increase the peak.
        for action in &allocation.actions {
            if let ArrayAllocationAction::Allocate(item) = action
                && candidate_roots.insert(item.resource)
            {
                candidate_bytes = candidate_bytes
                    .checked_add(self.allocation_bytes(item)?)
                    .ok_or_else(|| {
                        Self::memory_exceeded(0, u64::MAX, u64::MAX, "checked arithmetic overflow")
                    })?;
            }
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
            candidate_bytes,
        })
    }

    fn memory_unavailable(detail: impl Into<String>) -> SceneModifierExpandError {
        SceneModifierExpandError::CapacityExceeded {
            path: "modifierBufferBudget".into(),
            detail: detail.into(),
        }
    }

    fn allocation_bytes(
        &self,
        item: &crate::node_graph::resource_allocation::ArrayAllocation,
    ) -> Result<u64, SceneModifierExpandError> {
        if !self.cutters.contains(&item.node) {
            return Ok(item.bytes);
        }
        crate::node_graph::primitives::cut_map_scratch_bytes(item.bytes / 16)
            .and_then(|scratch| item.bytes.checked_add(scratch))
            .ok_or_else(|| {
                Self::memory_exceeded(0, u64::MAX, u64::MAX, "cut scratch arithmetic overflow")
            })
    }

    fn memory_exceeded(
        current: u64,
        candidate: u64,
        allowed: u64,
        policy: &str,
    ) -> SceneModifierExpandError {
        let available = allowed.saturating_sub(current);
        SceneModifierExpandError::CapacityExceeded {
            path: "modifierBufferBudget".into(),
            detail: format!(
                "projected GPU memory peak {projected} bytes (current {current} + candidate {candidate}) exceeds allowed {allowed} bytes; available for this candidate: {available} bytes ({policy})",
                projected = current.saturating_add(candidate),
            ),
        }
    }
}

/// Apply the same aggregate ceiling to candidates from multiple graph owners.
/// Each owner is accounted independently, then this function is called once so
/// all fresh physical roots consume one shared device allowance.
pub fn admit_candidate_bytes(
    snapshot: Option<manifold_gpu::GpuMemorySnapshot>,
    candidate: u64,
) -> Result<(), SceneModifierExpandError> {
    let snapshot = snapshot.ok_or_else(|| {
        PreparedModifierBufferBudget::memory_unavailable("device memory limits are unavailable")
    })?;
    let (allowed, policy) = configured_limit(snapshot.recommended_max_working_set_bytes)?;
    admit_candidate_bytes_with_limit(snapshot, candidate, allowed, &policy)
}

fn admit_candidate_bytes_with_limit(
    snapshot: manifold_gpu::GpuMemorySnapshot,
    candidate: u64,
    allowed: u64,
    policy: &str,
) -> Result<(), SceneModifierExpandError> {
    let _projected = snapshot
        .current_allocated_bytes
        .checked_add(candidate)
        .ok_or_else(|| {
            PreparedModifierBufferBudget::memory_exceeded(
                snapshot.current_allocated_bytes,
                candidate,
                u64::MAX,
                "checked arithmetic overflow",
            )
        })?;
    if _projected > allowed {
        return Err(PreparedModifierBufferBudget::memory_exceeded(
            snapshot.current_allocated_bytes,
            candidate,
            allowed,
            policy,
        ));
    }
    Ok(())
}

fn configured_limit(recommended: u64) -> Result<(u64, String), SceneModifierExpandError> {
    configured_limit_from(
        recommended,
        std::env::var(MODIFIER_MEMORY_OVERRIDE_ENV).ok().as_deref(),
    )
}

fn configured_limit_from(
    recommended: u64,
    override_mib: Option<&str>,
) -> Result<(u64, String), SceneModifierExpandError> {
    if let Some(raw) = override_mib {
        let mib = raw
            .parse::<u64>()
            .map_err(|_| SceneModifierExpandError::CapacityExceeded {
                path: MODIFIER_MEMORY_OVERRIDE_ENV.into(),
                detail: format!("expected a non-negative integer MiB ceiling, got `{raw}`"),
            })?;
        let bytes = mib.checked_mul(1024 * 1024).ok_or_else(|| {
            SceneModifierExpandError::CapacityExceeded {
                path: MODIFIER_MEMORY_OVERRIDE_ENV.into(),
                detail: format!("MiB ceiling `{mib}` overflows byte arithmetic"),
            }
        })?;
        return Ok((bytes, format!("explicit {mib} MiB ceiling")));
    }
    let bytes = recommended
        .checked_mul(DEFAULT_WORKING_SET_FRACTION_NUMERATOR)
        .and_then(|bytes| bytes.checked_div(DEFAULT_WORKING_SET_FRACTION_DENOMINATOR))
        .ok_or_else(|| SceneModifierExpandError::CapacityExceeded {
            path: "modifierBufferBudget".into(),
            detail: format!("recommended working set {recommended} overflows the 75% ceiling"),
        })?;
    Ok((
        bytes,
        format!("75% of recommended working set {recommended} bytes"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::resource_allocation::{ArrayAllocation, ArrayStorage};

    #[test]
    fn cut_budget_includes_private_scan_scratch_and_readbacks() {
        let scene = SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("scene"),
        };
        let cutter = NodeInstanceId(7);
        let budget = PreparedModifierBufferBudget {
            scenes: BTreeMap::from([(scene.clone(), AHashSet::from_iter([cutter]))]),
            cutters: AHashSet::from_iter([cutter]),
        };
        let map_bytes = (196_608 + 3 * 257) * 16;
        let private_bytes = (257 + 2) * 4 * 2 + 16 + 48;
        let allocation = ArrayAllocationPlan {
            actions: vec![ArrayAllocationAction::Allocate(ArrayAllocation {
                node: cutter,
                resource: ResourceId(0),
                bytes: map_bytes,
                zero_init: false,
            })],
            storage: AHashMap::from_iter([(
                ResourceId(0),
                ArrayStorage {
                    root: ResourceId(0),
                    bytes: map_bytes,
                },
            )]),
            warnings: Vec::new(),
        };
        let usage = budget.account(&allocation).unwrap();
        assert_eq!(usage.candidate_bytes, map_bytes + private_bytes);
        assert_eq!(usage.modifier_bytes[&scene], map_bytes + private_bytes);
        assert_eq!(usage.baseline_bytes, 0);
    }

    #[test]
    fn scene_modifier_buffer_budget_counts_shared_storage_once_and_separates_baseline() {
        let scene = SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("scene"),
        };
        let budget = PreparedModifierBufferBudget {
            scenes: BTreeMap::from([(scene.clone(), AHashSet::from_iter([NodeInstanceId(2)]))]),
            cutters: AHashSet::default(),
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
        let usage = budget.account(&allocation).unwrap();
        assert_eq!(usage.baseline_bytes, 100);
        assert_eq!(usage.modifier_bytes[&scene], 80);
        assert_eq!(usage.candidate_bytes, 180);
        let error = admit_candidate_bytes(
            Some(manifold_gpu::GpuMemorySnapshot {
                current_allocated_bytes: 0,
                recommended_max_working_set_bytes: 100,
            }),
            usage.candidate_bytes,
        )
        .unwrap_err();
        assert!(error.to_string().contains("candidate 180"));
        assert!(error.to_string().contains("allowed 75"));
        assert_eq!(allocation, unchanged);
    }

    #[test]
    fn aggregate_admission_includes_current_device_bytes() {
        let error = admit_candidate_bytes(
            Some(manifold_gpu::GpuMemorySnapshot {
                current_allocated_bytes: 10,
                recommended_max_working_set_bytes: 240,
            }),
            180,
        )
        .unwrap_err();
        let detail = error.to_string();
        assert!(detail.contains("current 10 + candidate 180"));
        assert!(detail.contains("allowed 180"));
        assert!(detail.contains("available for this candidate: 170"));
    }

    #[test]
    fn aggregate_admission_reports_unavailable_snapshot() {
        let error = admit_candidate_bytes(None, 1).unwrap_err();
        assert!(error.to_string().contains("memory limits are unavailable"));
    }

    #[test]
    fn configured_limit_override_is_pure_and_checked() {
        let (bytes, detail) = configured_limit_from(4_000, Some("12")).unwrap();
        assert_eq!(bytes, 12 * 1024 * 1024);
        assert!(detail.contains("explicit 12 MiB"));

        let invalid = configured_limit_from(4_000, Some("nope")).unwrap_err();
        assert!(
            invalid
                .to_string()
                .contains("expected a non-negative integer MiB")
        );

        let overflow = configured_limit_from(4_000, Some("18446744073709551615")).unwrap_err();
        assert!(overflow.to_string().contains("overflows byte arithmetic"));
    }

    #[test]
    fn aggregate_admission_reports_current_plus_candidate_overflow() {
        let error = admit_candidate_bytes(
            Some(manifold_gpu::GpuMemorySnapshot {
                current_allocated_bytes: u64::MAX,
                recommended_max_working_set_bytes: 4 * 1024 * 1024 * 1024,
            }),
            1,
        )
        .unwrap_err();
        assert!(error.to_string().contains("checked arithmetic overflow"));
    }
}
