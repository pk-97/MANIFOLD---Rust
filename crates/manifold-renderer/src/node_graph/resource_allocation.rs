//! Pure planning for logical `Array<T>` resource allocation.
//!
//! The plan is deliberately independent of a GPU backend.  The loader can
//! consume its actions to create buffers, while tests and budget admission can
//! inspect the exact same sizing decisions without allocating GPU resources.

use ahash::AHashMap;

use super::effect_node::NodeInstanceId;
use super::execution_plan::{ExecutionPlan, ResourceId};
use super::graph::Graph;
use super::graph_loader::PreAllocationError;
use super::ports::PortType;

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

/// A fresh allocation or a declared in-place alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayAllocationAction {
    Allocate(ArrayAllocation),
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
/// returned `storage` additionally records every allocation and declared alias
/// discovered while walking the plan.
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
    use crate::node_graph::primitives::{
        ArrayFeedback, ContainerBounds3D, GenerateCubeMesh, ResolveAccumulator, ScatterParticles,
        SceneObjectNode, SeedParticles, Value, WaveShearMesh,
    };

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

    #[test]
    fn array_allocation_plan_cube_wave_wave_propagates_capacity() {
        let mut graph = Graph::new();
        let cube = graph.add_node(Box::new(GenerateCubeMesh::new()));
        let wave_a = graph.add_node(Box::new(WaveShearMesh::new()));
        let wave_b = graph.add_node(Box::new(WaveShearMesh::new()));
        graph.connect((cube, "vertices"), (wave_a, "in")).unwrap();
        graph.connect((wave_a, "out"), (wave_b, "in")).unwrap();
        let object = graph.add_node(Box::new(SceneObjectNode::new()));
        graph
            .connect((wave_b, "out"), (object, "vertices"))
            .unwrap();

        let plan = compile(&graph).unwrap();
        let arrays = array_outputs(&plan);
        assert_eq!(arrays.len(), 3);
        let expected_bytes = 36_u64 * u64::from(arrays[0].2);
        let planned =
            plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
        let allocations: Vec<_> = planned
            .actions
            .iter()
            .filter_map(|action| match action {
                ArrayAllocationAction::Allocate(allocation) => Some(allocation),
                ArrayAllocationAction::Alias { .. } => None,
            })
            .collect();
        assert_eq!(allocations.len(), 3);
        assert!(
            allocations
                .iter()
                .all(|allocation| allocation.bytes == expected_bytes)
        );
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
            ArrayAllocationAction::Allocate(_) => panic!("alias output allocated separately"),
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
