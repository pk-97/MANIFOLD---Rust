//! Graph validation — connection legality, structural integrity, and
//! topological order. Pure analysis on top of [`Graph`]; no mutation.

use std::collections::{HashSet, VecDeque};

use ahash::{AHashMap, AHashSet};

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
    let to_node = graph.get_node(to.0).ok_or(GraphError::NodeNotFound(to.0))?;

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

/// Whole-graph validation. Checks structural invariants:
///   1. Every required input is wired.
///   2. The graph is a DAG (no directed cycles).
///
/// Connection-time validation via [`Graph::connect`] guarantees the
/// second invariant under normal mutation paths, but programmatic
/// construction (composite presets, JSON load, undo / redo) bypasses
/// `connect()` and so this second check is the durable safety net.
pub fn validate(graph: &Graph) -> Result<(), GraphError> {
    let live = reachable_from_final_output(graph);
    // Reachability filtering only kicks in when a FinalOutput is
    // actually present. Graphs without one (most unit-test
    // fixtures, plus any caller that builds a graph for its side
    // effects rather than to render) fall back to validating every
    // node — there's no "what reaches the output?" to compute.
    let has_final_output = graph.nodes().any(|inst| {
        inst.node.type_id().as_str()
            == crate::node_graph::boundary_nodes::FINAL_OUTPUT_TYPE_ID
    });
    for inst in graph.nodes() {
        // Nodes that can't reach any FinalOutput don't run, so their
        // required inputs aren't actually required this frame.
        // Skipping them here makes editing-time graphs robust: the
        // user can drop a Sample into the canvas before wiring it
        // without the renderer falling back to catalog default.
        if has_final_output && !live.contains(&inst.id) {
            continue;
        }
        for input in inst.node.inputs() {
            if input.required {
                let wired = graph.wires().iter().any(|w| w.to == (inst.id, input.name));
                if wired {
                    continue;
                }
                // Port-shadows-param: a required scalar input with a
                // same-named backing param doesn't need a wire — the
                // inline param value drives the op. Constants embedded
                // in the graph live as param values on the consuming
                // node rather than as Value-node middlemen.
                let has_backing_param = inst
                    .node
                    .parameters()
                    .iter()
                    .any(|p| p.name == input.name);
                if has_backing_param {
                    continue;
                }
                return Err(GraphError::RequiredInputUnwired {
                    node: inst.id,
                    port: input.name.to_string(),
                });
            }
        }
    }
    // Cycle check — `topological_sort` returns `CycleDetected` if the
    // graph isn't a DAG. Done after the per-node sweep so the more
    // specific `RequiredInputUnwired` error wins when both apply.
    topological_sort(graph)?;
    Ok(())
}

/// Set of nodes whose output is (transitively) consumed by a
/// `system.final_output` boundary node. Built by BFS backward across
/// `wires` from every FinalOutput. Anything outside this set is dead
/// — the executor won't run it and the validator shouldn't reject it.
pub(crate) fn reachable_from_final_output(graph: &Graph) -> AHashSet<NodeInstanceId> {
    use crate::node_graph::boundary_nodes::FINAL_OUTPUT_TYPE_ID;
    let mut live: AHashSet<NodeInstanceId> = AHashSet::default();
    let mut frontier: Vec<NodeInstanceId> = graph
        .nodes()
        .filter(|inst| inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)
        .map(|inst| inst.id)
        .collect();
    while let Some(id) = frontier.pop() {
        if !live.insert(id) {
            continue;
        }
        for w in graph.wires() {
            if w.to.0 == id {
                frontier.push(w.from.0);
            }
        }
    }
    live
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
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
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
    fn connects_array_ports_when_item_layout_matches() {
        // Two Array ports declared with the same (size, align, kind)
        // connect cleanly. The wire validator compares PortType via
        // derived Eq — equivalent ArrayType descriptors match
        // regardless of the macro-side type-name origin.
        use crate::node_graph::ports::ArrayType;
        let layout = ArrayType::of_known::<crate::generators::compute_common::Particle>();
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "producer",
            vec![],
            vec![output("particles", PortType::Array(layout))],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "consumer",
            vec![input("particles", PortType::Array(layout), true)],
            vec![],
        )));
        g.connect((a, "particles"), (b, "particles"))
            .expect("matching-layout Array ports should connect");
    }

    #[test]
    fn rejects_array_ports_with_mismatched_item_layout() {
        // Two Array ports with different item_size are different
        // PortType values — validate must reject the connection
        // rather than let mismatched layouts flow downstream.
        use crate::node_graph::ports::ArrayType;
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "particle_producer",
            vec![],
            vec![output(
                "out",
                PortType::Array(ArrayType::of_known::<
                    crate::generators::compute_common::Particle,
                >()),
            )],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "vertex_consumer",
            vec![input(
                "in",
                PortType::Array(ArrayType::of_known::<
                    crate::generators::mesh_common::MeshVertex,
                >()),
                true,
            )],
            vec![],
        )));
        let r = g.connect((a, "out"), (b, "in"));
        assert!(matches!(r, Err(GraphError::PortTypeMismatch { .. })));
    }

    /// Regression for the recurring "coordinate-space contract" bug
    /// class. Two `Array` ports with byte-identical layouts but
    /// different [`ItemKind`](crate::node_graph::ports::ItemKind)
    /// tags MUST NOT connect — that's the whole point of carrying
    /// the kind on the wire. `CurvePoint` (origin-centered 2D, what
    /// `render_lines` consumes) and `EdgePair` (two u32 indices)
    /// are both 8 bytes / 4-aligned, so under a pure size/align
    /// check they would connect silently. The kind tag forces the
    /// validator to refuse the wire.
    #[test]
    fn rejects_array_ports_with_matching_layout_but_mismatched_kind() {
        use crate::generators::mesh_common::{CurvePoint, EdgePair};
        use crate::node_graph::ports::ArrayType;
        // Sanity: same byte layout, different kinds.
        let curve = ArrayType::of_known::<CurvePoint>();
        let edge = ArrayType::of_known::<EdgePair>();
        assert_eq!((curve.item_size, curve.item_align), (8, 4));
        assert_eq!((edge.item_size, edge.item_align), (8, 4));
        assert_ne!(curve, edge, "kinds must distinguish the ArrayTypes");

        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "curve_producer",
            vec![],
            vec![output("out", PortType::Array(curve))],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "edge_consumer",
            vec![input("in", PortType::Array(edge), true)],
            vec![],
        )));
        let r = g.connect((a, "out"), (b, "in"));
        assert!(
            matches!(r, Err(GraphError::PortTypeMismatch { .. })),
            "wiring CurvePoint into an EdgePair port must fail \
             validation — byte layouts match but the kinds don't",
        );
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

    /// Regression: an orphan node (a Sample dropped into the canvas
    /// before its source input is wired) must NOT fail validation
    /// once a FinalOutput exists in the graph. The orphan isn't
    /// reachable from FinalOutput so the executor will skip it; the
    /// validator should agree. Without this, hydrate falls back to
    /// catalog default mid-edit and the user loses all their other
    /// per-card param changes.
    #[test]
    fn unreachable_node_with_required_input_does_not_break_validate() {
        use crate::node_graph::FINAL_OUTPUT_TYPE_ID;
        let mut g = Graph::new();
        // Live chain: source → final_output.
        let source = g.add_node(Box::new(TestNode::new(
            "source",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let final_out = g.add_node(Box::new(TestNode::new(
            FINAL_OUTPUT_TYPE_ID,
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((source, "out"), (final_out, "in")).unwrap();

        // Orphan node — required input unwired, output not consumed
        // by anything reaching FinalOutput. Pre-fix this would
        // poison validate(); post-fix, it's a silent no-op.
        let _orphan = g.add_node(Box::new(TestNode::new(
            "orphan_sample",
            vec![input("source", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        assert!(validate(&g).is_ok());
    }

    #[test]
    fn validate_runs_cycle_detection_via_topo_sort() {
        // Whole-graph `validate()` delegates the cycle check to
        // `topological_sort` which is exercised by the dedicated
        // cycle tests (`rejects_simple_cycle`, `rejects_self_loop`).
        // This test only verifies the wiring — `validate()` succeeds
        // on a clean DAG.
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
        g.connect((a, "out"), (b, "in")).unwrap();
        assert!(validate(&g).is_ok());
    }
}
