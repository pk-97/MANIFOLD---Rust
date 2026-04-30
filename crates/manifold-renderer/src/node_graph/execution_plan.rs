//! Execution plan compiler.
//!
//! Given a [`Graph`], [`compile`] produces an [`ExecutionPlan`] that the
//! runtime can iterate each frame: an ordered list of [`ExecutionStep`]s, one
//! per node, with input/output bindings expressed as [`ResourceId`]s and
//! per-step "free after" lists for pool recycling.
//!
//! The plan is built once when the graph is committed, not per frame.
//! Per-frame work in the runtime (step 4) reduces to: for each step, bind the
//! resources, call `EffectNode::evaluate`, return freed resources to the pool.
//!
//! ## Resource lifetime analysis
//!
//! Each node output port is assigned a fresh [`ResourceId`]. The compiler then
//! tracks the *last reader* of each resource — the latest step in topological
//! order that consumes it as an input. Resources whose last reader is step N
//! are added to step N's `free_after` list, signalling the runtime's pool
//! that the underlying physical buffer can be recycled.
//!
//! Resources that are produced but never read (a node's auxiliary output that
//! nobody wires) are freed immediately after the producing step.

use ahash::AHashMap;

use crate::node_graph::effect_node::{NodeInstanceId, NodeWire};
use crate::node_graph::graph::Graph;
use crate::node_graph::ports::PortType;
use crate::node_graph::validation::{topological_sort, validate, GraphError};

/// Identifier for one logical resource (texture, scalar) flowing on a wire.
///
/// Logical resources are abstract — the runtime maps them onto physical GPU
/// resources via a pool. Two resources with the same `PortType` may share the
/// same physical buffer if their lifetimes don't overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceId(pub u32);

/// One step in an [`ExecutionPlan`] — a node and its resource bindings.
#[derive(Debug, Clone)]
pub struct ExecutionStep {
    pub node: NodeInstanceId,

    /// `(input_port_name, resource_id)` for every wired input port. Optional
    /// inputs that aren't wired are omitted. Order follows the node's
    /// declared input ports.
    pub inputs: Vec<(&'static str, ResourceId)>,

    /// `(output_port_name, resource_id)` for every output port. Order follows
    /// the node's declared output ports.
    pub outputs: Vec<(&'static str, ResourceId)>,

    /// Resources whose last reader is this step. The runtime's pool may
    /// recycle the underlying physical buffers after this step completes.
    pub free_after: Vec<ResourceId>,
}

/// Pre-compiled evaluation order plus resource lifetime information for a
/// graph. Built once on commit, used every frame.
#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    steps: Vec<ExecutionStep>,
    /// `PortType` of each resource, indexed by `ResourceId`. The runtime pool
    /// uses these to size allocations (texture format, dimensions) or to
    /// store scalar values inline.
    resource_types: Vec<PortType>,
}

impl ExecutionPlan {
    pub fn steps(&self) -> &[ExecutionStep] {
        &self.steps
    }

    pub fn resource_count(&self) -> usize {
        self.resource_types.len()
    }

    pub fn resource_type(&self, id: ResourceId) -> Option<PortType> {
        self.resource_types.get(id.0 as usize).copied()
    }
}

/// Compile a graph into an [`ExecutionPlan`].
///
/// Calls [`validate`] and [`topological_sort`] internally; errors propagate
/// as [`GraphError`]. The graph is not consumed.
pub fn compile(graph: &Graph) -> Result<ExecutionPlan, GraphError> {
    validate(graph)?;
    let order = topological_sort(graph)?;

    // Index wires by their target (input) port for O(1) lookup during
    // input-binding construction.
    let mut wire_by_target: AHashMap<(NodeInstanceId, &'static str), &NodeWire> =
        AHashMap::default();
    for w in graph.wires() {
        wire_by_target.insert(w.to, w);
    }

    // First pass: assign a fresh ResourceId to every output port of every
    // node, in topological order. Walking in topo order gives deterministic
    // resource IDs even when the underlying node map is unordered.
    let mut output_resources: AHashMap<(NodeInstanceId, &'static str), ResourceId> =
        AHashMap::default();
    let mut resource_types: Vec<PortType> = Vec::new();
    for &node_id in &order {
        let inst = graph
            .get_node(node_id)
            .expect("topo order references existing node");
        for output_port in inst.node.outputs() {
            let id = ResourceId(resource_types.len() as u32);
            output_resources.insert((node_id, output_port.name), id);
            resource_types.push(output_port.ty);
        }
    }

    // Second pass: build steps, tracking last_reader for each resource.
    // last_reader starts at the producer's step (so unread resources are
    // freed immediately) and gets bumped each time a downstream node reads.
    let mut last_reader: AHashMap<ResourceId, usize> = AHashMap::default();
    let mut steps: Vec<ExecutionStep> = Vec::with_capacity(order.len());

    for (step_idx, &node_id) in order.iter().enumerate() {
        let inst = graph
            .get_node(node_id)
            .expect("topo order references existing node");

        let mut step_inputs = Vec::new();
        for input_port in inst.node.inputs() {
            if let Some(wire) = wire_by_target.get(&(node_id, input_port.name)) {
                let res_id = *output_resources
                    .get(&wire.from)
                    .expect("connect() guarantees the wire's source has a resource");
                step_inputs.push((input_port.name, res_id));
                last_reader.insert(res_id, step_idx);
            }
            // Optional unwired inputs are omitted from the bindings.
        }

        let mut step_outputs = Vec::new();
        for output_port in inst.node.outputs() {
            let res_id = *output_resources
                .get(&(node_id, output_port.name))
                .expect("output resource was assigned in the first pass");
            step_outputs.push((output_port.name, res_id));
            // Default last_reader to the producer step — handles "never read"
            // outputs by freeing them immediately.
            last_reader.entry(res_id).or_insert(step_idx);
        }

        steps.push(ExecutionStep {
            node: node_id,
            inputs: step_inputs,
            outputs: step_outputs,
            free_after: Vec::new(), // populated in the next loop
        });
    }

    // Third pass: bucket resources by their last_reader step and attach.
    // Sort within each bucket for deterministic iteration order in tests.
    let mut free_at_step: AHashMap<usize, Vec<ResourceId>> = AHashMap::default();
    for (&res_id, &step_idx) in &last_reader {
        free_at_step.entry(step_idx).or_default().push(res_id);
    }
    for (step_idx, step) in steps.iter_mut().enumerate() {
        if let Some(mut frees) = free_at_step.remove(&step_idx) {
            frees.sort();
            step.free_after = frees;
        }
    }

    Ok(ExecutionPlan {
        steps,
        resource_types,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::effect_node::{EffectNodeContext, EffectNodeType};
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind};

    struct TestNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl TestNode {
        fn new(name: &'static str, inputs: Vec<NodeInput>, outputs: Vec<NodeOutput>) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs,
                outputs,
            }
        }
    }

    impl crate::node_graph::EffectNode for TestNode {
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
        fn evaluate(&mut self, _: &mut EffectNodeContext) {}
    }

    fn input(name: &'static str, ty: PortType, required: bool) -> NodeInput {
        NodePort {
            name,
            ty,
            kind: PortKind::Input,
            required,
        }
    }

    fn output(name: &'static str, ty: PortType) -> NodeOutput {
        NodePort {
            name,
            ty,
            kind: PortKind::Output,
            required: false,
        }
    }

    #[test]
    fn linear_chain_resources_and_freeing() {
        // A → B → C
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let c = g.add_node(Box::new(TestNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((b, "out"), (c, "in")).unwrap();

        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 3);
        assert_eq!(plan.resource_count(), 2); // a.out + b.out

        let r_a = plan.steps()[0].outputs[0].1;
        let r_b = plan.steps()[1].outputs[0].1;

        // A produces, no inputs, no frees yet (its output is read by B at step 1).
        assert_eq!(plan.steps()[0].node, a);
        assert!(plan.steps()[0].inputs.is_empty());
        assert!(plan.steps()[0].free_after.is_empty());

        // B reads R_a, produces R_b. R_a is free after B (its last reader).
        assert_eq!(plan.steps()[1].node, b);
        assert_eq!(plan.steps()[1].inputs, vec![("in", r_a)]);
        assert_eq!(plan.steps()[1].free_after, vec![r_a]);

        // C reads R_b, no outputs. R_b is freed at step 2 (its last reader).
        assert_eq!(plan.steps()[2].node, c);
        assert_eq!(plan.steps()[2].inputs, vec![("in", r_b)]);
        assert!(plan.steps()[2].free_after.contains(&r_b));
    }

    #[test]
    fn diamond_shared_resource_freed_after_last_reader() {
        // A → B, A → C, (B, C) → D
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let c = g.add_node(Box::new(TestNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let d = g.add_node(Box::new(TestNode::new(
            "d",
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, true),
            ],
            vec![],
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((a, "out"), (c, "in")).unwrap();
        g.connect((b, "out"), (d, "a")).unwrap();
        g.connect((c, "out"), (d, "b")).unwrap();

        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 4);
        assert_eq!(plan.resource_count(), 3); // a.out + b.out + c.out

        // A is first, D is last; B and C order is unspecified.
        assert_eq!(plan.steps()[0].node, a);
        assert_eq!(plan.steps()[3].node, d);
        let r_a = plan.steps()[0].outputs[0].1;

        // R_a is read by both B and C. Whichever is later (step 2) is its
        // last reader, so R_a should appear in step-2's free_after.
        assert!(plan.steps()[2].free_after.contains(&r_a));
        assert!(!plan.steps()[1].free_after.contains(&r_a));
    }

    #[test]
    fn unread_output_freed_at_producing_step() {
        // A has two outputs, neither wired. Both should free immediately.
        let mut g = Graph::new();
        let _ = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![
                output("main", PortType::Texture2D),
                output("aux", PortType::Texture2D),
            ],
        )));
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 1);
        assert_eq!(plan.resource_count(), 2);
        // Both resources free after step 0.
        assert_eq!(plan.steps()[0].free_after.len(), 2);
    }

    #[test]
    fn resource_types_match_output_port_types() {
        // Mix Texture2D and Texture3D outputs; ensure resource_types is correct.
        let mut g = Graph::new();
        let _ = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![
                output("color", PortType::Texture2D),
                output("volume", PortType::Texture3D),
            ],
        )));
        let plan = compile(&g).unwrap();
        let color_id = plan.steps()[0].outputs[0].1;
        let volume_id = plan.steps()[0].outputs[1].1;
        assert_eq!(plan.resource_type(color_id), Some(PortType::Texture2D));
        assert_eq!(plan.resource_type(volume_id), Some(PortType::Texture3D));
    }

    #[test]
    fn compile_propagates_validation_errors() {
        // Required input not wired → compile() should error before topo sort.
        let mut g = Graph::new();
        g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        let r = compile(&g);
        assert!(matches!(r, Err(GraphError::RequiredInputUnwired { .. })));
    }

    #[test]
    fn optional_unwired_input_omitted_from_bindings() {
        // B has one required input (wired) and one optional input (unwired).
        // The optional input should not appear in the step's bindings.
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![
                input("required", PortType::Texture2D, true),
                input("optional", PortType::Texture2D, false),
            ],
            vec![],
        )));
        g.connect((a, "out"), (b, "required")).unwrap();
        let plan = compile(&g).unwrap();
        let step_b = &plan.steps()[1];
        assert_eq!(step_b.inputs.len(), 1);
        assert_eq!(step_b.inputs[0].0, "required");
    }
}
