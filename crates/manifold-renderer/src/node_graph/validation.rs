//! Graph validation — connection legality, structural integrity, and
//! topological order. Pure analysis on top of [`Graph`]; no mutation.

use std::collections::{HashSet, VecDeque};

use ahash::AHashMap;

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::ports::{PortKind, PortType};

/// Errors produced by graph mutation and validation.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphError {
    NodeNotFound(NodeInstanceId),
    PortNotFound {
        node: NodeInstanceId,
        port: String,
    },
    /// The named port exists but has the wrong direction (e.g. trying to wire
    /// from an input, or to an output).
    PortKindMismatch {
        node: NodeInstanceId,
        port: String,
        expected: PortKind,
    },
    PortTypeMismatch {
        from: PortType,
        to: PortType,
    },
    /// A required input has no incoming wire.
    RequiredInputUnwired {
        node: NodeInstanceId,
        port: String,
    },
    ParamNotFound {
        node: NodeInstanceId,
        param: String,
    },
    /// Adding the connection would form a directed cycle. V1 graphs are pure
    /// DAGs; explicit feedback edges are deferred to a later phase.
    CycleDetected {
        involves: Vec<NodeInstanceId>,
    },
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeNotFound(id) => write!(f, "node {:?} not found", id),
            Self::PortNotFound { node, port } => {
                write!(f, "port `{port}` not found on node {node:?}")
            }
            Self::PortKindMismatch {
                node,
                port,
                expected,
            } => write!(
                f,
                "port `{port}` on node {node:?} is wrong kind (expected {expected:?})"
            ),
            Self::PortTypeMismatch { from, to } => {
                write!(f, "port type mismatch: {from:?} -> {to:?}")
            }
            Self::RequiredInputUnwired { node, port } => write!(
                f,
                "required input `{port}` on node {node:?} has no incoming wire"
            ),
            Self::ParamNotFound { node, param } => {
                write!(f, "parameter `{param}` not found on node {node:?}")
            }
            Self::CycleDetected { involves } => {
                write!(f, "cycle detected involving nodes {involves:?}")
            }
        }
    }
}

impl std::error::Error for GraphError {}

/// Validate a single proposed connection. Called by [`Graph::connect`] before
/// the wire is committed.
pub(super) fn validate_connection(
    graph: &Graph,
    from: (NodeInstanceId, &'static str),
    to: (NodeInstanceId, &'static str),
) -> Result<(), GraphError> {
    let from_node = graph
        .get_node(from.0)
        .ok_or(GraphError::NodeNotFound(from.0))?;
    let to_node = graph
        .get_node(to.0)
        .ok_or(GraphError::NodeNotFound(to.0))?;

    let from_port = from_node
        .node
        .outputs()
        .iter()
        .find(|p| p.name == from.1)
        .ok_or_else(|| GraphError::PortNotFound {
            node: from.0,
            port: from.1.to_string(),
        })?;

    let to_port = to_node
        .node
        .inputs()
        .iter()
        .find(|p| p.name == to.1)
        .ok_or_else(|| GraphError::PortNotFound {
            node: to.0,
            port: to.1.to_string(),
        })?;

    if from_port.kind != PortKind::Output {
        return Err(GraphError::PortKindMismatch {
            node: from.0,
            port: from.1.to_string(),
            expected: PortKind::Output,
        });
    }
    if to_port.kind != PortKind::Input {
        return Err(GraphError::PortKindMismatch {
            node: to.0,
            port: to.1.to_string(),
            expected: PortKind::Input,
        });
    }

    if from_port.ty != to_port.ty {
        return Err(GraphError::PortTypeMismatch {
            from: from_port.ty,
            to: to_port.ty,
        });
    }

    if would_create_cycle(graph, from.0, to.0) {
        return Err(GraphError::CycleDetected {
            involves: vec![from.0, to.0],
        });
    }

    Ok(())
}

/// Whole-graph validation. Currently checks that every required input is
/// wired. Extend as new structural invariants are introduced.
pub fn validate(graph: &Graph) -> Result<(), GraphError> {
    for inst in graph.nodes() {
        for input in inst.node.inputs() {
            if input.required {
                let wired = graph.wires().iter().any(|w| w.to == (inst.id, input.name));
                if !wired {
                    return Err(GraphError::RequiredInputUnwired {
                        node: inst.id,
                        port: input.name.to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Return nodes in evaluation order (dependencies before dependents).
/// Errors with [`GraphError::CycleDetected`] if the graph contains a cycle.
pub fn topological_sort(graph: &Graph) -> Result<Vec<NodeInstanceId>, GraphError> {
    let mut in_degree: AHashMap<NodeInstanceId, u32> = AHashMap::default();
    for inst in graph.nodes() {
        in_degree.insert(inst.id, 0);
    }
    for w in graph.wires() {
        if let Some(d) = in_degree.get_mut(&w.to.0) {
            *d += 1;
        }
    }

    let mut queue: VecDeque<NodeInstanceId> = in_degree
        .iter()
        .filter_map(|(id, d)| if *d == 0 { Some(*id) } else { None })
        .collect();

    let mut order = Vec::with_capacity(graph.node_count());
    while let Some(id) = queue.pop_front() {
        order.push(id);
        for w in graph.wires_from(id) {
            if let Some(d) = in_degree.get_mut(&w.to.0) {
                *d -= 1;
                if *d == 0 {
                    queue.push_back(w.to.0);
                }
            }
        }
    }

    if order.len() != graph.node_count() {
        let unreached: Vec<_> = in_degree
            .iter()
            .filter_map(|(id, d)| if *d > 0 { Some(*id) } else { None })
            .collect();
        return Err(GraphError::CycleDetected {
            involves: unreached,
        });
    }

    Ok(order)
}

/// Would adding `from -> to` introduce a cycle into the graph as it stands?
///
/// True iff a directed path already exists from `to` back to `from`. DFS from
/// `to`; if we reach `from`, a cycle would form.
fn would_create_cycle(graph: &Graph, from: NodeInstanceId, to: NodeInstanceId) -> bool {
    if from == to {
        return true; // self-loop
    }
    let mut visited: HashSet<NodeInstanceId> = HashSet::new();
    let mut stack = vec![to];
    while let Some(n) = stack.pop() {
        if !visited.insert(n) {
            continue;
        }
        if n == from {
            return true;
        }
        for w in graph.wires_from(n) {
            stack.push(w.to.0);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::effect_node::EffectNodeContext;
    use crate::node_graph::effect_node::EffectNodeType;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

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
    fn rejects_type_mismatch() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture3D, true)],
            vec![],
        )));
        let r = g.connect((a, "out"), (b, "in"));
        assert!(matches!(r, Err(GraphError::PortTypeMismatch { .. })));
    }

    #[test]
    fn rejects_unknown_port_name() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        let r = g.connect((a, "missing"), (b, "in"));
        assert!(matches!(r, Err(GraphError::PortNotFound { .. })));
    }

    #[test]
    fn rejects_simple_cycle() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        let r = g.connect((b, "out"), (a, "in"));
        assert!(matches!(r, Err(GraphError::CycleDetected { .. })));
    }

    #[test]
    fn rejects_self_loop() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let r = g.connect((a, "out"), (a, "in"));
        assert!(matches!(r, Err(GraphError::CycleDetected { .. })));
    }

    #[test]
    fn topo_sort_linear_chain() {
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
        let order = topological_sort(&g).unwrap();
        assert_eq!(order, vec![a, b, c]);
    }

    #[test]
    fn topo_sort_diamond() {
        // a -> b, a -> c, b+c -> d (two-input node d)
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
        let order = topological_sort(&g).unwrap();
        // a must come first; d must come last; b and c order is unspecified.
        assert_eq!(order[0], a);
        assert_eq!(order[3], d);
        assert!(order[1..3].contains(&b) && order[1..3].contains(&c));
    }

    #[test]
    fn validate_required_input_unwired() {
        let mut g = Graph::new();
        g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        assert!(matches!(
            validate(&g),
            Err(GraphError::RequiredInputUnwired { .. })
        ));
    }

    #[test]
    fn validate_optional_input_unwired_is_ok() {
        let mut g = Graph::new();
        g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, false)],
            vec![],
        )));
        assert!(validate(&g).is_ok());
    }
}
