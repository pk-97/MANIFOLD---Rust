//! Pure planning for logical `Array<T>` resource allocation.
//!
//! The plan is deliberately independent of a GPU backend.  The loader can
//! consume its actions to create buffers, while tests and budget admission can
//! inspect the exact same sizing decisions without allocating GPU resources.

use ahash::{AHashMap, AHashSet};

use super::effect_node::NodeInstanceId;
use super::execution_plan::{ExecutionPlan, ResourceId};
use super::freeze::classify::BoundaryReason;
use super::graph::Graph;
use super::graph_loader::PreAllocationError;
use super::ports::PortType;

type ReusableKey = (PortType, u64);

/// Private scratch required by a family array output.
pub struct ArrayScratch {
    pub type_id: &'static str,
    pub bytes: fn(u64) -> Option<u64>,
}
inventory::collect!(ArrayScratch);

pub fn array_scratch(type_id: &str) -> Option<fn(u64) -> Option<u64>> {
    let mut entries = inventory::iter::<ArrayScratch>.into_iter()
        .filter(|entry| entry.type_id == type_id);
    let entry = entries.next()?;
    assert!(entries.next().is_none(), "duplicate array scratch provider");
    Some(entry.bytes)
}
type ReusableBuckets = AHashMap<ReusableKey, Vec<ResourceId>>;

/// Arrays whose size can change after planning because a provider hands in
/// storage of its own size. Derived once during preparation. All of them
/// keep dedicated storage so replacement cannot alter an unrelated array.
pub(crate) fn growing_array_resources(graph: &Graph, plan: &ExecutionPlan) -> Vec<bool> {
    capacity_lineage(graph, plan, |node, port| node.node.provides_array_output(port))
}

/// The seeded arrays, plus every array whose declared capacity moves when a
/// member's capacity moves. Walked to a fixed point so a feedback input,
/// produced later in the plan, carries the lineage too.
///
/// Membership follows capacity, not wires: an output sized from its params
/// (a lattice grid fed by particles) never changes when an input does, so it
/// stays an ordinary temporary. The capacity rule is opaque, so it is asked:
/// see [`capacity_follows`].
fn capacity_lineage(
    graph: &Graph,
    plan: &ExecutionPlan,
    seeded: impl Fn(&super::graph::NodeInstance, &str) -> bool,
) -> Vec<bool> {
    let mut members = vec![false; plan.resource_count()];
    let mut probe = Vec::with_capacity(8);
    loop {
        let mut changed = false;
        for step in plan.steps() {
            let Some(node) = graph.get_node(step.node) else { continue; };
            for (port, id) in &step.outputs {
                if members[id.0 as usize] || !matches!(plan.resource_type(*id), Some(PortType::Array(_))) {
                    continue;
                }
                if seeded(node, port) || capacity_follows(node, port, &step.inputs, plan, &members, &mut probe) {
                    members[id.0 as usize] = true;
                    changed = true;
                }
            }
        }
        if !changed { return members; }
    }
}

/// Whether `port`'s capacity follows any member input. Holds the other array
/// inputs at one capacity and moves the members together across values below
/// and above it: any change in the answer, or no answer, means it follows.
/// Canvas-sized outputs are sized from the canvas, never from inputs.
fn capacity_follows<'a>(
    node: &super::graph::NodeInstance,
    port: &str,
    inputs: &[(&'a str, ResourceId)],
    plan: &ExecutionPlan,
    members: &[bool],
    probe: &mut Vec<(&'a str, u32)>,
) -> bool {
    const HELD: u32 = 64;
    const MOVES: [u32; 4] = [1, HELD - 1, HELD + 1, 1 << 16];
    if node.node.canvas_sized_array_outputs().contains(&port)
        || !inputs.iter().any(|(_, id)| members[id.0 as usize])
    {
        return false;
    }
    let mut answer = |moved: u32| {
        probe.clear();
        probe.extend(inputs.iter().filter(|(_, id)| matches!(plan.resource_type(*id), Some(PortType::Array(_))))
            .map(|&(name, id)| (name, if members[id.0 as usize] { moved } else { HELD })));
        node.node.array_output_capacity(port, &node.params, probe)
    };
    let first = answer(MOVES[0]);
    first.is_none() || MOVES[1..].iter().any(|&moved| answer(moved) != first)
}

/// Each resource's storage class, indexed by resource: a declared in-place
/// alias (`aliased_array_io`) writes its input's storage, so both sides are
/// one array. The value is the class representative.
pub(crate) fn in_place_classes(graph: &Graph, plan: &ExecutionPlan) -> Vec<ResourceId> {
    fn find(class: &mut [ResourceId], resource: ResourceId) -> ResourceId {
        let mut root = resource;
        while class[root.0 as usize] != root { root = class[root.0 as usize]; }
        class[resource.0 as usize] = root;
        root
    }
    let mut class: Vec<ResourceId> = (0..plan.resource_count() as u32).map(ResourceId).collect();
    for step in plan.steps() {
        let Some(node) = graph.get_node(step.node) else { continue };
        for (input_port, output_port) in node.node.aliased_array_io() {
            let input = step.inputs.iter().find(|(name, _)| name == input_port);
            let output = step.outputs.iter().find(|(name, _)| name == output_port);
            if let (Some(&(_, input)), Some(&(_, output))) = (input, output) {
                let (a, b) = (find(&mut class, input), find(&mut class, output));
                if a != b { class[b.0 as usize] = a; }
            }
        }
    }
    for index in 0..class.len() {
        find(&mut class, ResourceId(index as u32));
    }
    class
}

/// Arrays whose storage a later step gives to a different array, so a host
/// reading them after the frame reads that other array. `storage_of` names
/// each array's physical storage (a backend slot, or a planned root). A
/// declared in-place alias writes the same array and does not count.
///
/// A host that reads an array after the frame declares it with
/// [`Graph::add_external_output`]; the planner then keeps it dedicated. The
/// whole-graph dump reads every array, so it snapshots these instead.
pub fn arrays_overwritten_later(
    graph: &Graph,
    plan: &ExecutionPlan,
    storage_of: impl Fn(ResourceId) -> Option<u32>,
) -> Vec<bool> {
    let class = in_place_classes(graph, plan);
    let mut writes: Vec<(u32, usize, ResourceId)> = Vec::new();
    for (index, step) in plan.steps().iter().enumerate() {
        for &(_, resource) in &step.outputs {
            if matches!(plan.resource_type(resource), Some(PortType::Array(_)))
                && let Some(storage) = storage_of(resource)
            {
                writes.push((storage, index, resource));
            }
        }
    }
    writes.sort_unstable_by_key(|&(storage, step, _)| (storage, step));
    let mut overwritten = vec![false; plan.resource_count()];
    for (at, &(storage, step, resource)) in writes.iter().enumerate() {
        overwritten[resource.0 as usize] = writes[at + 1..]
            .iter()
            .take_while(|&&(other, _, _)| other == storage)
            .any(|&(_, later, other)| later > step && class[other.0 as usize] != class[resource.0 as usize]);
    }
    overwritten
}

fn enqueue_reusable_root(reusable: &mut ReusableBuckets, key: ReusableKey, root: ResourceId) {
    let bucket = reusable.entry(key).or_default();
    if !bucket.contains(&root) {
        bucket.push(root);
    }
}

fn take_reusable_root(reusable: &mut ReusableBuckets, key: ReusableKey) -> Option<ResourceId> {
    let root = reusable.get_mut(&key)?.pop();
    if reusable.get(&key).is_some_and(Vec::is_empty) {
        reusable.remove(&key);
    }
    root
}

fn remove_reusable_root(reusable: &mut ReusableBuckets, root: ResourceId) {
    reusable.retain(|_, roots| {
        roots.retain(|candidate| *candidate != root);
        !roots.is_empty()
    });
}

/// Known storage for a logical array resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayStorage {
    pub root: ResourceId,
    pub bytes: u64,
}

/// One fresh array allocation requested by the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayAllocation {
    pub node: NodeInstanceId,
    pub resource: ResourceId,
    pub bytes: u64,
    pub zero_init: bool,
}

/// A fresh allocation, a lifetime-safe temporary reuse, or a declared
/// in-place alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayAllocationAction {
    Allocate(ArrayAllocation),
    /// Keep an existing physical root whose byte capacity exactly matches
    /// the new plan.  This is used by staged resize so a live simulation
    /// buffer is retained without CPU clearing or a transient replacement.
    Reuse {
        resource: ResourceId,
        root: ResourceId,
    },
    /// Bind to the same physical slot for either declared in-place IO or
    /// temporary reuse after the prior logical resource's `free_after` step.
    Alias {
        resource: ResourceId,
        input: ResourceId,
    },
}

/// Complete pure result of array resource planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayAllocationPlan {
    pub actions: Vec<ArrayAllocationAction>,
    pub storage: AHashMap<ResourceId, ArrayStorage>,
    pub warnings: Vec<String>,
}

/// Plan every `Array<T>` output in execution order.
///
/// `prebound` describes storage already supplied by a caller (for example a
/// persistent input).  It is copied into the result and never mutated.  The
/// returned `storage` additionally records every allocation and alias,
/// including temporary reuse discovered while walking the plan.
pub fn plan_array_allocations(
    graph: &Graph,
    plan: &ExecutionPlan,
    canvas: (u32, u32),
    prebound: &AHashMap<ResourceId, ArrayStorage>,
) -> Result<ArrayAllocationPlan, PreAllocationError> {
    let handle_by_node: AHashMap<NodeInstanceId, &'static str> = graph
        .handles()
        .map(|(handle, node)| (node, handle))
        .collect();
    let mut storage = prebound.clone();
    let mut actions = Vec::new();
    let mut warnings = Vec::new();
    let mut input_capacities = Vec::with_capacity(8);
    // These resources either escape the ordinary step-local lifetime model or
    // participate in an intentional alias. Their physical roots must remain
    // dedicated even when a free_after entry would otherwise make them look
    // reusable.
    let mut excluded_resources = AHashSet::default();
    for &resource in plan
        .persistent_resources()
        .iter()
        .chain(plan.held_resources())
    {
        excluded_resources.insert(resource);
    }
    let mut excluded_roots = AHashSet::default();
    for (&resource, storage_entry) in prebound {
        excluded_resources.insert(resource);
        excluded_roots.insert(storage_entry.root);
    }
    for step in plan.steps() {
        let Some(node_inst) = graph.get_node(step.node) else {
            continue;
        };
        // A CPU step finishes while previously encoded GPU commands may not
        // have started. Its reads/writes therefore do not share the GPU's
        // ordered lifetime model. Keep both sides of CPU/IO boundaries out
        // of scratch reuse; Metal hazard tracking cannot order mapped writes.
        if matches!(node_inst.node.boundary_reason(),
            Some(BoundaryReason::NonGpu | BoundaryReason::IoBridge))
        {
            for (_, resource) in step.inputs.iter().chain(&step.outputs) {
                if matches!(plan.resource_type(*resource), Some(PortType::Array(_))) {
                    excluded_resources.insert(*resource);
                }
            }
        }
        for (input_port, output_port) in node_inst.node.aliased_array_io() {
            if let Some((_, resource)) = step.inputs.iter().find(|(name, _)| *name == *input_port)
            {
                excluded_resources.insert(*resource);
            }
            if let Some((_, resource)) = step.outputs.iter().find(|(name, _)| *name == *output_port)
            {
                excluded_resources.insert(*resource);
            }
        }
        for (port_name, resource) in &step.outputs {
            if node_inst.node.atomic_outputs().contains(port_name) {
                excluded_resources.insert(*resource);
            }
        }
        if node_inst.node.carries_resources() {
            for (_, resource) in &step.inputs {
                if matches!(plan.resource_type(*resource), Some(PortType::Array(_))) {
                    excluded_resources.insert(*resource);
                }
            }
        }
    }
    // Staged resize retains prebound physical roots. Keep every array whose
    // capacity follows the canvas dedicated: after a resize it needs a new
    // size while a prebound root still names the old shared slot. Growth must
    // likewise never enlarge an unrelated array sharing a scratch slot.
    let canvas_sized = capacity_lineage(graph, plan, |node, port| {
        node.node.canvas_sized_array_outputs().contains(&port)
    });
    let growing = growing_array_resources(graph, plan);
    for (index, (resized, grows)) in canvas_sized.into_iter().zip(growing).enumerate() {
        if resized || grows { excluded_resources.insert(ResourceId(index as u32)); }
    }
    let mut reusable: ReusableBuckets = AHashMap::default();

    for step in plan.steps() {
        let Some(node_inst) = graph.get_node(step.node) else {
            continue;
        };
        let node_type = node_inst.node.type_id().as_str();
        let aliased_pairs = node_inst.node.aliased_array_io();
        let canvas_sized_outputs = node_inst.node.canvas_sized_array_outputs();
        let atomic_outputs = node_inst.node.atomic_outputs();

        input_capacities.clear();
        for (port_name, resource) in &step.inputs {
            let Some(PortType::Array(layout)) = plan.resource_type(*resource) else {
                continue;
            };
            if layout.item_size == 0 {
                return Err(unbound_error(
                    node_type,
                    port_name,
                    &handle_by_node,
                    step.node,
                    "input Array layout has zero item stride",
                ));
            }
            let Some(input_storage) = storage.get(resource) else {
                continue;
            };
            let count = input_storage
                .bytes
                .checked_div(layout.item_size as u64)
                .and_then(|count| u32::try_from(count).ok())
                .ok_or_else(|| {
                    unbound_error(
                        node_type,
                        port_name,
                        &handle_by_node,
                        step.node,
                        "input Array capacity exceeds u32",
                    )
                })?;
            input_capacities.push((*port_name, count));
        }

        let mut current_output_roots = AHashSet::default();
        for (port_name, resource) in &step.outputs {
            let Some(PortType::Array(layout)) = plan.resource_type(*resource) else {
                continue;
            };
            if layout.item_size == 0 {
                return Err(unbound_error(
                    node_type,
                    port_name,
                    &handle_by_node,
                    step.node,
                    "output Array layout has zero item stride",
                ));
            }

            // Explicit host inputs borrow already allocated scene resources.
            // Never replace that shared storage with an independently owned
            // allocation. Ordinary produced outputs retain their sizing rules.
            if node_type == "system.mesh_input" && prebound.contains_key(resource) {
                continue;
            }

            let zero_init = atomic_outputs.contains(port_name);
            let alias_input_port = aliased_pairs
                .iter()
                .find(|(_, output_port)| *output_port == *port_name)
                .map(|(input_port, _)| *input_port);
            if let Some(input_port) = alias_input_port {
                let input = step
                    .inputs
                    .iter()
                    .find(|(name, _)| *name == input_port)
                    .map(|(_, resource)| *resource);
                if let Some(input) = input
                    && let Some(input_storage) = storage.get(&input).copied()
                {
                    actions.push(ArrayAllocationAction::Alias {
                        resource: *resource,
                        input,
                    });
                    storage.insert(*resource, input_storage);
                    current_output_roots.insert(input_storage.root);
                    excluded_roots.insert(input_storage.root);
                    remove_reusable_root(&mut reusable, input_storage.root);
                    continue;
                }
                warnings.push(format!(
                    "node `{node_type}` declared aliased pair `{input_port}` -> `{port_name}` without known input storage; using a fresh allocation"
                ));
            }

            let bytes = if canvas_sized_outputs.contains(port_name) {
                if canvas.0 == 0 || canvas.1 == 0 {
                    return Err(unbound_error(
                        node_type,
                        port_name,
                        &handle_by_node,
                        step.node,
                        "canvas dimensions are zero",
                    ));
                }
                let capacity = (canvas.0 as u64)
                    .checked_mul(canvas.1 as u64)
                    .ok_or_else(|| {
                        unbound_error(
                            node_type,
                            port_name,
                            &handle_by_node,
                            step.node,
                            "canvas area overflows u64",
                        )
                    })?;
                capacity
                    .checked_mul(layout.item_size as u64)
                    .ok_or_else(|| {
                        unbound_error(
                            node_type,
                            port_name,
                            &handle_by_node,
                            step.node,
                            "canvas array byte size overflows u64",
                        )
                    })?
            } else {
                let Some(capacity) = node_inst.node.array_output_capacity(
                    port_name,
                    &node_inst.params,
                    &input_capacities,
                ) else {
                    return Err(PreAllocationError::UnsizedArrayOutput {
                        node_type: node_type.to_string(),
                        port: port_name.to_string(),
                        handle: handle_by_node
                            .get(&step.node)
                            .map(|handle| (*handle).to_string()),
                    });
                };
                if capacity == 0 {
                    return Err(unbound_error(
                        node_type,
                        port_name,
                        &handle_by_node,
                        step.node,
                        "array output capacity is zero",
                    ));
                }
                (capacity as u64)
                    .checked_mul(layout.item_size as u64)
                    .ok_or_else(|| {
                        unbound_error(
                            node_type,
                            port_name,
                            &handle_by_node,
                            step.node,
                            "array output byte size overflows u64",
                        )
                    })?
            };

            if bytes == 0 {
                return Err(unbound_error(
                    node_type,
                    port_name,
                    &handle_by_node,
                    step.node,
                    "array output resolves to zero bytes",
                ));
            }
            if let Some(existing) = prebound.get(resource)
                && existing.bytes == bytes
                && !zero_init
            {
                actions.push(ArrayAllocationAction::Reuse {
                    resource: *resource,
                    root: existing.root,
                });
                storage.insert(*resource, *existing);
                current_output_roots.insert(existing.root);
            } else {
                let storage_type = PortType::Array(layout);
                if !zero_init
                    && !excluded_resources.contains(resource)
                    && let Some(root) = take_reusable_root(&mut reusable, (storage_type, bytes))
                {
                    actions.push(ArrayAllocationAction::Alias {
                        resource: *resource,
                        input: root,
                    });
                    storage.insert(
                        *resource,
                        ArrayStorage {
                            root,
                            bytes,
                        },
                    );
                    current_output_roots.insert(root);
                } else {
                    actions.push(ArrayAllocationAction::Allocate(ArrayAllocation {
                        node: step.node,
                        resource: *resource,
                        bytes,
                        zero_init,
                    }));
                    storage.insert(
                        *resource,
                        ArrayStorage {
                            root: *resource,
                            bytes,
                        },
                    );
                    current_output_roots.insert(*resource);
                    if excluded_resources.contains(resource) {
                        excluded_roots.insert(*resource);
                    }
                }
            }
        }

        // Return only ordinary temporary roots after all outputs of this step
        // have been assigned. This ordering prevents same-step input/output
        // reuse, and roots still used by a current output remain unavailable.
        for &resource in &step.free_after {
            let Some(entry) = storage.get(&resource).copied() else {
                continue;
            };
            if excluded_resources.contains(&resource)
                || current_output_roots.contains(&entry.root)
                || excluded_roots.contains(&entry.root)
            {
                continue;
            }
            let Some(resource_type) = plan.resource_type(resource) else {
                continue;
            };
            if !matches!(resource_type, PortType::Array(_)) {
                continue;
            }
            if plan.resource_type(entry.root) != Some(resource_type) {
                continue;
            }
            enqueue_reusable_root(&mut reusable, (resource_type, entry.bytes), entry.root);
        }
    }

    Ok(ArrayAllocationPlan {
        actions,
        storage,
        warnings,
    })
}

fn unbound_error(
    node_type: &str,
    port: &str,
    handle_by_node: &AHashMap<NodeInstanceId, &'static str>,
    node: NodeInstanceId,
    cause: &'static str,
) -> PreAllocationError {
    PreAllocationError::UnboundArrayResource {
        producer_node_type: node_type.to_string(),
        producer_port: port.to_string(),
        producer_handle: handle_by_node
            .get(&node)
            .map(|handle| (*handle).to_string()),
        cause,
    }
}

/// Test oracle for temporary reuse: no physical root is ever live for two
/// logical arrays at once.
#[cfg(test)]
pub(crate) mod lifetimes {
    use super::*;

    /// Steps over which each array must keep its contents: producer to last
    /// reader. A substep region repeats its steps, so an array any of them
    /// touches lives across the whole outermost region. Persistent and held
    /// arrays live the whole frame.
    pub(crate) fn live_ranges(plan: &ExecutionPlan) -> AHashMap<ResourceId, (usize, usize)> {
        let mut live: AHashMap<ResourceId, (usize, usize)> = AHashMap::default();
        let mut widen = |resource: ResourceId, lo: usize, hi: usize| {
            let range = live.entry(resource).or_insert((lo, hi));
            *range = (range.0.min(lo), range.1.max(hi));
        };
        for (index, step) in plan.steps().iter().enumerate() {
            for (_, resource) in step.inputs.iter().chain(&step.outputs) {
                widen(*resource, index, index);
            }
        }
        for region in plan.substep_regions() {
            let (lo, hi) = (region.steps[0], region.steps[region.steps.len() - 1]);
            for &index in &region.steps {
                let step = &plan.steps()[index];
                for (_, resource) in step.inputs.iter().chain(&step.outputs) {
                    widen(*resource, lo, hi);
                }
            }
        }
        let last = plan.steps().len().saturating_sub(1);
        for &resource in plan.persistent_resources().iter().chain(plan.held_resources()) {
            widen(resource, 0, last);
        }
        live
    }

    /// Panics naming both arrays when two of them share a root while both
    /// are live. A declared in-place alias is one array for this check: its
    /// input and output are the same storage by contract. Returns how many
    /// arrays took a root another array had released.
    pub(crate) fn assert_shared_roots_never_overlap(
        graph: &Graph,
        plan: &ExecutionPlan,
        allocation: &ArrayAllocationPlan,
    ) -> usize {
        let live = live_ranges(plan);
        let class = in_place_classes(graph, plan);
        let mut ranges: AHashMap<(ResourceId, ResourceId), (usize, usize)> = AHashMap::default();
        for (&resource, storage) in &allocation.storage {
            let Some(&(lo, hi)) = live.get(&resource) else { continue };
            let key = (storage.root, class[resource.0 as usize]);
            let range = ranges.entry(key).or_insert((lo, hi));
            *range = (range.0.min(lo), range.1.max(hi));
        }
        let mut by_root: AHashMap<ResourceId, Vec<(usize, usize, ResourceId)>> = AHashMap::default();
        for (&(root, member), &(lo, hi)) in &ranges {
            by_root.entry(root).or_default().push((lo, hi, member));
        }
        let mut reused = 0;
        for (root, mut owners) in by_root {
            owners.sort();
            reused += owners.len() - 1;
            let (_, mut latest, mut holder) = owners[0];
            for &(lo, hi, owner) in &owners[1..] {
                assert!(
                    latest < lo,
                    "root {root:?}: {holder:?} lives to step {latest} but {owner:?} takes it at step {lo}"
                );
                if hi > latest { (latest, holder) = (hi, owner); }
            }
        }
        reused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::compile;
    use crate::node_graph::effect_node::{
        EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
    };
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{
        ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };



    struct FixedArrayNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        capacity: u32,
        boundary: Option<BoundaryReason>,
        /// Publishes its own output storage, like a solver or fill.
        provides: bool,
        /// Output capacity copies this input's instead of `capacity`.
        follows: Option<&'static str>,
        /// Keeps its upstream live when the graph has other liveness roots.
        root: bool,
    }

    impl FixedArrayNode {
        fn new(
            type_name: &'static str,
            inputs: Vec<NodeInput>,
            outputs: Vec<NodeOutput>,
            capacity: u32,
        ) -> Self {
            Self {
                type_id: EffectNodeType::new(type_name),
                inputs,
                outputs,
                capacity,
                boundary: None,
                provides: false,
                follows: None,
                root: false,
            }
        }
    }

    impl EffectNode for FixedArrayNode {
        fn boundary_reason(&self) -> Option<BoundaryReason> {
            self.boundary
        }

        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }

        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }

        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }

        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }

        fn parameters(&self) -> &[ParamDef] {
            &[]
        }

        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}

        fn provides_array_output(&self, _: &str) -> bool {
            self.provides
        }

        fn is_liveness_root(&self) -> bool {
            self.root
        }

        fn array_output_capacity(
            &self,
            port: &str,
            _: &ParamValues,
            inputs: &[(&str, u32)],
        ) -> Option<u32> {
            let is_array = self.outputs.iter().any(|output| output.name == port && matches!(output.ty, PortType::Array(_)));
            match self.follows {
                Some(input) => inputs.iter().find(|(name, _)| *name == input).map(|&(_, n)| n),
                None => is_array.then_some(self.capacity),
            }
        }
    }

    fn mock_port(name: &'static str, ty: PortType, kind: PortKind, required: bool) -> NodePort {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind,
            required,
        }
    }

    #[test]
    fn reusable_root_buckets_deduplicate_and_remove_across_keys() {
        let key = (PortType::Array(ArrayType::of::<u32>()), 16);
        let other_key = (PortType::Array(ArrayType::of::<u32>()), 32);
        let first = ResourceId(10);
        let second = ResourceId(11);
        let mut reusable = ReusableBuckets::default();

        enqueue_reusable_root(&mut reusable, key, first);
        enqueue_reusable_root(&mut reusable, key, first);
        enqueue_reusable_root(&mut reusable, key, second);
        enqueue_reusable_root(&mut reusable, other_key, first);
        assert_eq!(reusable[&key], vec![first, second]);
        assert_eq!(reusable[&other_key], vec![first]);

        remove_reusable_root(&mut reusable, first);
        assert_eq!(reusable[&key], vec![second]);
        assert!(!reusable.contains_key(&other_key));
        assert_eq!(take_reusable_root(&mut reusable, key), Some(second));
        assert!(!reusable.contains_key(&key));
    }

    #[test]
    fn cpu_and_io_array_boundaries_keep_dedicated_storage() {
        let array = PortType::Array(ArrayType::of::<u32>());
        for boundary in [BoundaryReason::NonGpu, BoundaryReason::IoBridge] {
            let mut graph = Graph::new();
            let mut nodes = Vec::new();
            for index in 0..5 {
                let mut node = FixedArrayNode::new(
                    "test.array_stage",
                    if index == 0 { vec![] } else {
                        vec![mock_port("in", array, PortKind::Input, true)]
                    },
                    vec![mock_port("out", array, PortKind::Output, false)],
                    4,
                );
                if index == 2 { node.boundary = Some(boundary); }
                let id = graph.add_node(Box::new(node));
                if let Some(&previous) = nodes.last() {
                    graph.connect((previous, "out"), (id, "in")).unwrap();
                }
                nodes.push(id);
            }
            let plan = compile(&graph).unwrap();
            let allocated = plan_array_allocations(
                &graph, &plan, (64, 64), &AHashMap::default(),
            ).unwrap();
            let output = |node| plan.steps().iter().find(|step| step.node == node)
                .unwrap().outputs[0].1;
            for boundary_resource in [output(nodes[1]), output(nodes[2])] {
                assert_eq!(allocated.storage[&boundary_resource].root, boundary_resource);
                assert!(allocated.storage.iter().all(|(resource, storage)| {
                    *resource == boundary_resource || storage.root != boundary_resource
                }), "CPU/IO input and output storage cannot be overwritten by another step");
            }
            assert_eq!(allocated.storage[&output(nodes[3])].root, output(nodes[0]),
                "ordinary GPU lifetimes remain eligible for reuse");
        }
    }

    #[test]
    fn array_allocation_plan_reuses_two_same_key_roots_at_multi_output_step() {
        let array = PortType::Array(ArrayType::of::<u32>());
        let scalar = PortType::Scalar(ScalarType::F32);
        let mut graph = Graph::new();
        let source_a = graph.add_node(Box::new(FixedArrayNode::new(
            "test.source_a",
            vec![],
            vec![mock_port("out", array, PortKind::Output, false)],
            4,
        )));
        let source_b = graph.add_node(Box::new(FixedArrayNode::new(
            "test.source_b",
            vec![],
            vec![mock_port("out", array, PortKind::Output, false)],
            4,
        )));
        let fan_in = graph.add_node(Box::new(FixedArrayNode::new(
            "test.fan_in",
            vec![
                mock_port("left", array, PortKind::Input, true),
                mock_port("right", array, PortKind::Input, true),
            ],
            vec![mock_port("trigger", scalar, PortKind::Output, false)],
            4,
        )));
        let fan_out = graph.add_node(Box::new(FixedArrayNode::new(
            "test.fan_out",
            vec![mock_port("trigger", scalar, PortKind::Input, true)],
            vec![
                mock_port("left", array, PortKind::Output, false),
                mock_port("right", array, PortKind::Output, false),
            ],
            4,
        )));
        let sink = graph.add_node(Box::new(FixedArrayNode::new(
            "test.sink",
            vec![
                mock_port("left", array, PortKind::Input, true),
                mock_port("right", array, PortKind::Input, true),
            ],
            vec![],
            4,
        )));
        graph.connect((source_a, "out"), (fan_in, "left")).unwrap();
        graph.connect((source_b, "out"), (fan_in, "right")).unwrap();
        graph
            .connect((fan_in, "trigger"), (fan_out, "trigger"))
            .unwrap();
        graph.connect((fan_out, "left"), (sink, "left")).unwrap();
        graph.connect((fan_out, "right"), (sink, "right")).unwrap();

        let plan = compile(&graph).unwrap();
        let planned =
            plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
        let fan_out_outputs: Vec<_> = plan
            .steps()
            .iter()
            .find(|step| step.node == fan_out)
            .unwrap()
            .outputs
            .iter()
            .map(|(_, resource)| *resource)
            .collect();
        assert_eq!(fan_out_outputs.len(), 2);
        let roots: Vec<_> = fan_out_outputs
            .iter()
            .map(|resource| planned.storage[resource].root)
            .collect();
        assert_ne!(roots[0], roots[1]);
        assert!(fan_out_outputs.iter().all(|resource| {
            planned.actions.iter().any(|action| {
                matches!(
                    action,
                    ArrayAllocationAction::Alias { resource: aliased, input }
                        if aliased == resource && *input == planned.storage[resource].root
                )
            })
        }));
        assert_eq!(
            planned
                .actions
                .iter()
                .filter(|action| matches!(action, ArrayAllocationAction::Allocate(_)))
                .count(),
            2,
            "both fan-in roots should satisfy the later outputs"
        );
    }


























    fn output_of(plan: &ExecutionPlan, node: NodeInstanceId) -> ResourceId {
        let step = plan.steps().iter().find(|step| step.node == node).unwrap_or_else(|| {
            panic!("{node:?} has no step; plan runs {:?}", plan.steps().iter().map(|s| s.node).collect::<Vec<_>>())
        });
        step.outputs[0].1
    }

    /// A provider's particles feeding a chain of lattice grids sized from
    /// params, as SWASH's particles feed its face grids: only arrays whose
    /// capacity follows the provider grow, and the grids reuse each other.
    #[test]
    fn param_sized_arrays_below_a_provider_are_temporaries() {
        let array = PortType::Array(ArrayType::of::<u32>());
        let input = || vec![mock_port("in", array, PortKind::Input, true)];
        let output = || vec![mock_port("out", array, PortKind::Output, false)];
        let mut graph = Graph::new();
        let mut provider = FixedArrayNode::new("test.provider", vec![], output(), 4);
        provider.provides = true;
        let provider = graph.add_node(Box::new(provider));
        let mut follower = FixedArrayNode::new("test.follower", input(), output(), 0);
        follower.follows = Some("in");
        let follower = graph.add_node(Box::new(follower));
        graph.connect((provider, "out"), (follower, "in")).unwrap();
        let mut grids = vec![graph.add_node(Box::new(FixedArrayNode::new("test.grid", input(), output(), 8)))];
        graph.connect((provider, "out"), (grids[0], "in")).unwrap();
        for _ in 0..3 {
            let grid = graph.add_node(Box::new(FixedArrayNode::new("test.grid", input(), output(), 8)));
            graph.connect((*grids.last().unwrap(), "out"), (grid, "in")).unwrap();
            grids.push(grid);
        }
        for source in [*grids.last().unwrap(), follower] {
            let sink = graph.add_node(Box::new(FixedArrayNode::new("test.sink", input(), vec![], 1)));
            graph.connect((source, "out"), (sink, "in")).unwrap();
        }

        let plan = compile(&graph).unwrap();
        let growing = growing_array_resources(&graph, &plan);
        let grows = |node| growing[output_of(&plan, node).0 as usize];
        assert!(grows(provider) && grows(follower), "provided storage and its followers grow");
        assert!(grids.iter().all(|&grid| !grows(grid)), "a grid sized from params never grows");

        let planned = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
        let root = |node| planned.storage[&output_of(&plan, node)].root;
        assert_eq!(root(grids[2]), root(grids[0]), "the third grid takes the first's storage");
        assert_eq!(root(grids[3]), root(grids[1]), "the fourth grid takes the second's storage");
        assert_eq!(root(follower), output_of(&plan, follower), "a growing array keeps its own storage");
        assert_eq!(lifetimes::assert_shared_roots_never_overlap(&graph, &plan, &planned), 2);
    }

    /// A host reading an array after the frame reads whatever a later array
    /// left in its storage, unless the host declares the read: an external
    /// output keeps its own storage.
    #[test]
    fn declared_host_reads_keep_their_storage() {
        let array = PortType::Array(ArrayType::of::<u32>());
        let input = || vec![mock_port("in", array, PortKind::Input, true)];
        let output = || vec![mock_port("out", array, PortKind::Output, false)];
        let first_grid_overwritten = |declared: bool| {
            let mut graph = Graph::new();
            let mut grids = vec![graph.add_node(Box::new(FixedArrayNode::new("test.grid", vec![], output(), 8)))];
            for _ in 0..3 {
                let grid = graph.add_node(Box::new(FixedArrayNode::new("test.grid", input(), output(), 8)));
                graph.connect((*grids.last().unwrap(), "out"), (grid, "in")).unwrap();
                grids.push(grid);
            }
            let mut sink = FixedArrayNode::new("test.sink", input(), vec![], 1);
            sink.root = true;
            let sink = graph.add_node(Box::new(sink));
            graph.connect((grids[3], "out"), (sink, "in")).unwrap();
            if declared {
                graph.add_external_output(grids[0], "out").unwrap();
            }
            let plan = compile(&graph).unwrap();
            let planned = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
            lifetimes::assert_shared_roots_never_overlap(&graph, &plan, &planned);
            let overwritten = arrays_overwritten_later(&graph, &plan, |r| planned.storage.get(&r).map(|s| s.root.0));
            let later = |grid: usize| overwritten[output_of(&plan, grids[grid]).0 as usize];
            assert!(!later(3), "nothing follows the last grid");
            later(0)
        };
        assert!(first_grid_overwritten(false), "undeclared, the third grid takes the first's storage");
        assert!(!first_grid_overwritten(true), "declared, the first grid keeps its storage");
    }

    /// A feedback node reads an array produced later in the plan. Growth
    /// still reaches it and everything that follows its capacity.
    #[test]
    fn growth_crosses_a_feedback_edge() {
        let mut graph = Graph::new();
        let feedback = graph.add_node(Box::new(crate::testkit::graph::GraphFixture::array_feedback()));
        let array = graph.get_node(feedback).unwrap().node.outputs()[0].ty;
        let mut provider = FixedArrayNode::new("test.provider", vec![], vec![mock_port("out", array, PortKind::Output, false)], 4);
        provider.provides = true;
        let provider = graph.add_node(Box::new(provider));
        let follower = |graph: &mut Graph| {
            let mut node = FixedArrayNode::new(
                "test.follower",
                vec![mock_port("in", array, PortKind::Input, true)],
                vec![mock_port("out", array, PortKind::Output, false)],
                0,
            );
            node.follows = Some("in");
            graph.add_node(Box::new(node))
        };
        let upstream = follower(&mut graph);
        let downstream = follower(&mut graph);
        graph.connect((provider, "out"), (upstream, "in")).unwrap();
        graph.connect((upstream, "out"), (feedback, "in")).unwrap();
        graph.connect((feedback, "out"), (downstream, "in")).unwrap();
        let mut sink = FixedArrayNode::new("test.sink", vec![mock_port("in", array, PortKind::Input, true)], vec![], 1);
        sink.root = true;
        let sink = graph.add_node(Box::new(sink));
        graph.connect((downstream, "out"), (sink, "in")).unwrap();

        let plan = compile(&graph).unwrap();
        let step = |node| plan.steps().iter().position(|step| step.node == node).unwrap();
        assert!(step(feedback) < step(upstream), "premise: the feedback input is produced later");
        let growing = growing_array_resources(&graph, &plan);
        for node in [provider, upstream, feedback, downstream] {
            assert!(growing[output_of(&plan, node).0 as usize], "{node:?} must grow");
        }
    }




}

#[cfg(test)]
mod testkit;
