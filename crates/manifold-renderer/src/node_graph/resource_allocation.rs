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
type ReusableBuckets = AHashMap<ReusableKey, Vec<ResourceId>>;

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
    // Staged resize retains prebound physical roots. Keep canvas-sized array
    // families dedicated: two equally sized temporaries can require different
    // capacities after resize, while a prebound root still names the old shared
    // slot. Include connected array inputs/outputs (also across feedback edges)
    // so indirect capacity propagation cannot introduce that split.
    let mut canvas_arrays = AHashSet::default();
    for step in plan.steps() {
        if let Some(node) = graph.get_node(step.node) {
            for (port, resource) in &step.outputs {
                if node.node.canvas_sized_array_outputs().contains(port) {
                    canvas_arrays.insert(*resource);
                }
            }
        }
    }
    if !canvas_arrays.is_empty() {
        loop {
            let previous_count = canvas_arrays.len();
            for step in plan.steps() {
                if step.inputs.iter().chain(&step.outputs)
                    .any(|(_, resource)| canvas_arrays.contains(resource))
                {
                    for (_, resource) in step.inputs.iter().chain(&step.outputs) {
                        if matches!(plan.resource_type(*resource), Some(PortType::Array(_))) {
                            canvas_arrays.insert(*resource);
                        }
                    }
                }
            }
            if canvas_arrays.len() == previous_count { break; }
        }
        excluded_resources.extend(canvas_arrays);
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
    use crate::node_graph::primitives::{
        ArrayFeedback, ContainerBounds3D, GenerateCubeMesh, ResolveAccumulator, ScatterParticles,
        SceneObjectNode, SeedParticles, Value, WaveShearMesh,
    };

    struct FixedArrayNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        capacity: u32,
        boundary: Option<BoundaryReason>,
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

        fn array_output_capacity(
            &self,
            port: &str,
            _: &ParamValues,
            _: &[(&str, u32)],
        ) -> Option<u32> {
            self.outputs
                .iter()
                .any(|output| output.name == port && matches!(output.ty, PortType::Array(_)))
                .then_some(self.capacity)
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


    fn array_outputs(plan: &ExecutionPlan) -> Vec<(NodeInstanceId, ResourceId, u32)> {
        plan.steps()
            .iter()
            .flat_map(|step| {
                step.outputs.iter().filter_map(|(_, resource)| {
                    let PortType::Array(layout) = plan.resource_type(*resource)? else {
                        return None;
                    };
                    Some((step.node, *resource, layout.item_size))
                })
            })
            .collect()
    }

    fn wave_graph() -> (Graph, Vec<NodeInstanceId>) {
        let mut graph = Graph::new();
        let cube = graph.add_node(Box::new(GenerateCubeMesh::new()));
        let mut previous = cube;
        let mut previous_port = "vertices";
        let mut waves = Vec::new();
        for _ in 0..4 {
            let wave = graph.add_node(Box::new(WaveShearMesh::new()));
            graph.connect((previous, previous_port), (wave, "in")).unwrap();
            waves.push(wave);
            previous = wave;
            previous_port = "out";
        }
        let object = graph.add_node(Box::new(SceneObjectNode::new()));
        graph
            .connect((previous, previous_port), (object, "vertices"))
            .unwrap();
        (graph, waves)
    }

    #[test]
    fn array_allocation_plan_reuses_four_temporary_wave_stages() {
        let (graph, _) = wave_graph();
        let plan = compile(&graph).unwrap();
        let arrays = array_outputs(&plan);
        assert_eq!(arrays.len(), 5);
        let expected_bytes = 36_u64 * u64::from(arrays[0].2);
        let planned =
            plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
        let allocations: Vec<_> = planned
            .actions
            .iter()
            .filter_map(|action| match action {
                ArrayAllocationAction::Allocate(allocation) => Some(allocation),
                ArrayAllocationAction::Reuse { .. } | ArrayAllocationAction::Alias { .. } => None,
            })
            .collect();
        // The held cube and the final carried geometry stay dedicated; only
        // wave 3 reuses wave 1, after wave 2 has consumed it.
        assert_eq!(allocations.len(), 4);
        assert!(
            allocations
                .iter()
                .all(|allocation| allocation.bytes == expected_bytes)
        );
        assert_eq!(
            planned
                .actions
                .iter()
                .filter(|action| matches!(action, ArrayAllocationAction::Alias { .. }))
                .count(),
            1
        );
        for step in plan.steps() {
            for (_, output) in &step.outputs {
                let Some(ArrayAllocationAction::Alias { input, .. }) = planned
                    .actions
                    .iter()
                    .find(|action| matches!(action, ArrayAllocationAction::Alias { resource, .. } if resource == output))
                else {
                    continue;
                };
                assert!(
                    step.inputs.iter().all(|(_, input_resource)| {
                        planned.storage.get(input_resource).is_none_or(|storage| {
                            storage.root != planned.storage[output].root
                        })
                    }),
                    "temporary reuse must not alias a same-step input"
                );
                assert_eq!(plan.resource_type(*output), plan.resource_type(*input));
                assert_eq!(planned.storage[output].bytes, planned.storage[input].bytes);
            }
        }
        for &held in plan.held_resources() {
            assert_eq!(planned.storage[&held].root, held);
        }
    }

    #[test]
    fn temporary_reuse_preserves_prebound_roots_and_exact_capacity() {
        let (graph, waves) = wave_graph();
        let plan = compile(&graph).unwrap();
        let resource = |node| plan.steps().iter().find(|step| step.node == node)
            .unwrap().outputs[0].1;
        let first = resource(waves[0]);
        let third = resource(waves[2]);
        // An oversized borrowed buffer must not become an exact-size scratch
        // allocation even when its logical resource reaches its last reader.
        let prebound = AHashMap::from_iter([(first, ArrayStorage {
            root: first, bytes: 72 * 64,
        })]);
        let planned = plan_array_allocations(&graph, &plan, (64, 64), &prebound).unwrap();
        assert_ne!(planned.storage[&third].root, first);
        assert_eq!(prebound[&first].bytes, 72 * 64);
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn temporary_arrays_match_dedicated_storage_across_animated_and_repeat_frames() {
        use crate::gpu_encoder::GpuEncoder;
        use crate::node_graph::backend::Backend;
        use crate::node_graph::graph_loader::pre_allocate_resources;
        use crate::node_graph::parameters::ParamValue;
        use crate::node_graph::{Executor, FrameTime, MetalBackend};
        use manifold_core::{Beats, Seconds};
        use manifold_gpu::GpuTextureFormat;

        let device = crate::test_device();
        let make_runtime = |dedicated| {
            let (graph, waves) = wave_graph();
            let plan = compile(&graph).unwrap();
            let planned = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
            let final_resource = plan.steps().iter().find(|step| step.node == waves[3])
                .unwrap().outputs[0].1;
            let mut backend = MetalBackend::new(device.arc(), 64, 64, GpuTextureFormat::Rgba16Float);
            if dedicated {
                // Prebound dedicated storage disables planner reuse without
                // introducing a production toggle or a second planner.
                for (&resource, storage) in &planned.storage {
                    backend.pre_bind_array(resource, device.create_buffer_shared(storage.bytes));
                }
            }
            pre_allocate_resources(&graph, &plan, &device, &mut backend).unwrap();
            let unique: AHashSet<_> = planned.storage.keys()
                .map(|resource| backend.slot_for(*resource).unwrap()).collect();
            assert_eq!(unique.len(), if dedicated { 5 } else { 4 });
            (graph, waves, plan, Executor::new(Box::new(backend)), final_resource)
        };
        let mut shared = make_runtime(false);
        let mut dedicated = make_runtime(true);
        let mut prior = None;
        for (frame, phase) in [0.13, 0.13, 0.37, 0.62, 0.62].into_iter().enumerate() {
            let mut outputs = Vec::new();
            for (graph, waves, plan, executor, output) in [&mut shared, &mut dedicated] {
                for (index, node) in waves.iter().enumerate() {
                    graph.set_param(*node, "phase", ParamValue::Float(phase + index as f32 * 0.11)).unwrap();
                }
                let mut encoder = device.create_encoder("temporary-array-proof");
                {
                    let mut gpu = GpuEncoder::new(&mut encoder, &device);
                    executor.execute_frame_with_gpu(graph, plan, FrameTime {
                        beats: Beats(f64::from(phase)), seconds: Seconds(f64::from(phase)),
                        delta: Seconds(1.0 / 24.0), frame_count: frame as i64,
                    }, &mut gpu);
                }
                encoder.commit_and_wait_completed();
                let backend = executor.backend();
                let buffer = backend.array_buffer(backend.slot_for(*output).unwrap()).unwrap();
                let bytes = unsafe {
                    std::slice::from_raw_parts(buffer.mapped_ptr().unwrap(), buffer.size() as usize)
                }.to_vec();
                outputs.push(bytes);
            }
            assert_eq!(outputs[0], outputs[1], "shared output differs at frame {frame}");
            if frame == 2 {
                assert_ne!(prior.as_ref().unwrap(), &outputs[0], "animation must change geometry");
            }
            prior = Some(outputs.remove(0));
        }
    }

    #[test]
    fn array_allocation_plan_reuses_exact_live_storage() {
        let mut graph = Graph::new();
        let cube = graph.add_node(Box::new(GenerateCubeMesh::new()));
        let wave = graph.add_node(Box::new(WaveShearMesh::new()));
        graph.connect((cube, "vertices"), (wave, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let mut prebound = AHashMap::default();
        for (_, resource, item_size) in array_outputs(&plan) {
            prebound.insert(
                resource,
                ArrayStorage {
                    root: resource,
                    bytes: 36 * u64::from(item_size),
                },
            );
        }
        let planned = plan_array_allocations(&graph, &plan, (64, 64), &prebound).unwrap();
        assert!(planned.actions.iter().all(|action| matches!(
            action,
            ArrayAllocationAction::Reuse { .. } | ArrayAllocationAction::Alias { .. }
        )));
    }

    #[test]
    fn array_allocation_plan_alias_shares_physical_root() {
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(SeedParticles::new()));
        let alias = graph.add_node(Box::new(ContainerBounds3D::new()));
        graph.connect((source, "particles"), (alias, "in")).unwrap();
        let consumer = graph.add_node(Box::new(ContainerBounds3D::new()));
        graph.connect((alias, "out"), (consumer, "in")).unwrap();

        let plan = compile(&graph).unwrap();
        let arrays = array_outputs(&plan);
        assert_eq!(arrays.len(), 2);
        let planned =
            plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
        assert_eq!(planned.actions.len(), 2);
        let (resource, input) = match planned.actions[1] {
            ArrayAllocationAction::Alias { resource, input } => (resource, input),
            ArrayAllocationAction::Allocate(_) | ArrayAllocationAction::Reuse { .. } => {
                panic!("alias output allocated separately")
            }
        };
        assert_eq!(
            planned.storage[&resource].root,
            planned.storage[&input].root
        );
        assert!(planned
            .actions
            .iter()
            .all(|action| !matches!(action, ArrayAllocationAction::Allocate(allocation) if allocation.resource == resource)));
    }

    #[test]
    fn array_allocation_plan_prebound_input_supplies_capacity() {
        let mut graph = Graph::new();
        let feedback = graph.add_node(Box::new(ArrayFeedback::new()));
        let alias = graph.add_node(Box::new(ContainerBounds3D::new()));
        graph.connect((feedback, "out"), (alias, "in")).unwrap();
        graph.connect((alias, "out"), (feedback, "in")).unwrap();

        let plan = compile(&graph).unwrap();
        let arrays = array_outputs(&plan);
        let input_resource = arrays
            .iter()
            .find(|(node, _, _)| *node == feedback)
            .map(|(_, resource, _)| *resource)
            .expect("feedback output");
        let prebound_resource = arrays
            .iter()
            .find(|(node, _, _)| *node == alias)
            .map(|(_, resource, _)| *resource)
            .expect("alias output");
        let item_size = u64::from(arrays[0].2);
        let bytes = item_size * 7;
        let mut prebound = AHashMap::default();
        prebound.insert(
            prebound_resource,
            ArrayStorage {
                root: prebound_resource,
                bytes,
            },
        );

        let planned = plan_array_allocations(&graph, &plan, (64, 64), &prebound).unwrap();
        let allocation = planned
            .actions
            .iter()
            .find_map(|action| match action {
                ArrayAllocationAction::Allocate(allocation)
                    if allocation.resource == input_resource =>
                {
                    Some(allocation)
                }
                _ => None,
            })
            .expect("feedback allocation");
        assert_eq!(allocation.bytes, bytes);
        assert_eq!(prebound[&prebound_resource].bytes, bytes);
    }

    #[test]
    fn math_view_borrowed_arrays_keep_owner_storage_and_capacity() {
        let mut graph = Graph::new();
        let registry = crate::node_graph::persistence::PrimitiveRegistry::with_builtin();
        let source = graph.add_node(registry.construct("system.mesh_input").unwrap());
        let mask = graph.add_node(Box::new(crate::node_graph::primitives::MeshSpatialMask::new()));
        graph.connect((source, "vertices"), (mask, "in")).unwrap();
        let output = graph.add_node(registry.construct("system.mesh_output").unwrap());
        graph.connect((source, "vertices"), (output, "vertices")).unwrap();
        graph.connect((mask, "weights"), (output, "weights")).unwrap();
        let plan = compile(&graph).unwrap();
        let outputs = &plan.steps().iter().find(|step| step.node == source).unwrap().outputs;
        let mut prebound = AHashMap::default();
        for (port, resource) in outputs {
            let bytes = if *port == "vertices" { 1536 * 64 } else { 4 * 9000 };
            prebound.insert(*resource, ArrayStorage { root: *resource, bytes });
        }
        let planned = plan_array_allocations(&graph, &plan, (64, 64), &prebound).unwrap();
        for (resource, storage) in &prebound {
            assert_eq!(planned.storage[resource], *storage);
            assert!(planned.actions.iter().all(|action| !matches!(action,
                ArrayAllocationAction::Allocate(allocation) if allocation.resource == *resource)));
        }
        assert_eq!(planned.actions.len(), 1);
        assert!(matches!(planned.actions[0], ArrayAllocationAction::Allocate(allocation)
            if allocation.node == mask && allocation.bytes == 1536 * 4));
    }

    fn scatter_graph() -> (Graph, NodeInstanceId) {
        let mut graph = Graph::new();
        let seed = graph.add_node(Box::new(SeedParticles::new()));
        let width = graph.add_node(Box::new(Value::new()));
        let height = graph.add_node(Box::new(Value::new()));
        let scatter = graph.add_node(Box::new(ScatterParticles::new()));
        graph
            .connect((seed, "particles"), (scatter, "particles"))
            .unwrap();
        graph.connect((width, "out"), (scatter, "width")).unwrap();
        graph.connect((height, "out"), (scatter, "height")).unwrap();
        let resolve = graph.add_node(Box::new(ResolveAccumulator::new()));
        graph
            .connect((scatter, "accum"), (resolve, "accum"))
            .unwrap();
        (graph, scatter)
    }

    #[test]
    fn array_allocation_plan_canvas_atomic_output_is_zero_initialized() {
        let (graph, scatter) = scatter_graph();
        let plan = compile(&graph).unwrap();
        let accum = plan
            .steps()
            .iter()
            .find(|step| step.node == scatter)
            .and_then(|step| step.outputs.iter().find(|(name, _)| *name == "accum"))
            .map(|(_, resource)| *resource)
            .expect("scatter accumulator");
        let planned = plan_array_allocations(&graph, &plan, (8, 4), &AHashMap::default()).unwrap();
        let allocation = planned
            .actions
            .iter()
            .find_map(|action| match action {
                ArrayAllocationAction::Allocate(allocation) if allocation.resource == accum => {
                    Some(allocation)
                }
                _ => None,
            })
            .expect("canvas allocation");
        assert_eq!(allocation.bytes, 8 * 4 * 4);
        assert!(allocation.zero_init);
    }

    #[test]
    fn array_allocation_plan_zero_or_unsized_fails_without_mutation() {
        let (graph, scatter) = scatter_graph();
        let plan = compile(&graph).unwrap();
        let accum = plan
            .steps()
            .iter()
            .find(|step| step.node == scatter)
            .and_then(|step| step.outputs.iter().find(|(name, _)| *name == "accum"))
            .map(|(_, resource)| *resource)
            .unwrap();
        let mut prebound = AHashMap::default();
        prebound.insert(
            accum,
            ArrayStorage {
                root: accum,
                bytes: 16,
            },
        );
        let before = prebound.clone();
        let error = plan_array_allocations(&graph, &plan, (0, 4), &prebound).unwrap_err();
        assert!(matches!(
            error,
            PreAllocationError::UnboundArrayResource {
                cause: "canvas dimensions are zero",
                ..
            }
        ));
        assert_eq!(prebound, before);

        let mut unsized_graph = Graph::new();
        let feedback = unsized_graph.add_node(Box::new(ArrayFeedback::new()));
        unsized_graph
            .connect((feedback, "out"), (feedback, "in"))
            .unwrap();
        let unsized_plan = compile(&unsized_graph).unwrap();
        let error = plan_array_allocations(
            &unsized_graph,
            &unsized_plan,
            (64, 64),
            &AHashMap::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PreAllocationError::UnsizedArrayOutput { .. }
        ));
    }
}
