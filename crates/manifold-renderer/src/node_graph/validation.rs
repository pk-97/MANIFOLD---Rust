//! Graph validation — connection legality, structural integrity, and
//! topological order. Pure analysis on top of [`Graph`]; no mutation.

use std::collections::{HashSet, VecDeque};

use ahash::{AHashMap, AHashSet};

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::ports::{ItemKind, PortKind, PortType};

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
    /// Producer's declared output format isn't in the consumer's
    /// accepted-format list. Fires when both sides declare formats
    /// and they disagree — the silent format-mismatch class (e.g.
    /// fp32 producer wired into an fp16 consumer that saturates) that
    /// otherwise produces wrong-but-not-panicking output. When either
    /// side is unconstrained the wire is accepted; this only fires on
    /// the both-declared-and-incompatible case.
    PortFormatMismatch {
        from_node: NodeInstanceId,
        from_port: String,
        to_node: NodeInstanceId,
        to_port: String,
        producer_format: manifold_gpu::GpuTextureFormat,
        accepted: Vec<manifold_gpu::GpuTextureFormat>,
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
            Self::PortFormatMismatch {
                from_node,
                from_port,
                to_node,
                to_port,
                producer_format,
                accepted,
            } => write!(
                f,
                "format mismatch: {from_node:?}.{from_port} emits {producer_format:?}, \
                 but {to_node:?}.{to_port} only accepts {accepted:?}. \
                 Match the formats, change the consumer's accepted list, or \
                 set the producer's `outputFormats` override to one of the \
                 accepted formats."
            ),
        }
    }
}

impl std::error::Error for GraphError {}

/// Validate a single proposed connection. Called by [`Graph::connect`] before
/// the wire is committed. Internally delegates the per-wire checks
/// (kind, type, format) to [`validate_wire_endpoints`] so the same
/// checks run from [`validate`]'s post-construction audit — both
/// paths share one definition of "is this wire well-formed?" and
/// can't drift.
pub(super) fn validate_connection(
    graph: &Graph,
    from: (NodeInstanceId, &'static str),
    to: (NodeInstanceId, &'static str),
) -> Result<(), GraphError> {
    validate_wire_endpoints(graph, from, to)?;

    // Cycle check is per-wire only at connect time (whole-graph cycle
    // check at validate time goes through `topological_sort`). State-
    // capture wires close a per-frame loop through the StateStore, so
    // they're allowed back-edges and don't trigger this check.
    let to_is_state_capture_port = graph
        .get_node(to.0)
        .map(|inst| inst.node.state_capture_input_ports().contains(&to.1))
        .unwrap_or(false);
    if !to_is_state_capture_port && would_create_cycle(graph, from.0, to.0) {
        return Err(GraphError::CycleDetected {
            involves: vec![from.0, to.0],
        });
    }

    Ok(())
}

/// Shared per-wire endpoint check: nodes exist, ports exist, kinds
/// match (output→input), texture types match, format contract holds.
/// Used by both connect-time validation (when adding one wire) and
/// `validate`'s post-construction audit (walking every wire). Having
/// one implementation collapses the connect-vs-compile drift surface
/// for per-wire properties — the only per-wire check NOT here is
/// cycle detection (which is whole-graph for `validate` via topo
/// sort, and per-edge for `validate_connection` since "would adding
/// THIS wire form a cycle?" is the natural shape at connect time).
pub(super) fn validate_wire_endpoints(
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

    if !port_types_compatible(from_port.ty, to_port.ty) {
        return Err(GraphError::PortTypeMismatch {
            from: from_port.ty,
            to: to_port.ty,
        });
    }

    if let Some(err) = check_wire_format_compatibility(
        from.0,
        from.1,
        &*from_node.node,
        to.0,
        to.1,
        &*to_node.node,
    ) {
        return Err(err);
    }

    Ok(())
}

/// Port-type compatibility for the wire validator. Exact equality
/// for most cases, with one relaxation: an `Array(Anonymous)` matches
/// a typed `Array(KnownKind)` of the same `(item_size, item_align)`,
/// in either direction. This is what makes `node.wgsl_compute`'s raw
/// byte outputs flow into typed cast atoms (`cast_as_particle`,
/// `cast_as_mesh_vertex`, …) and what lets typed upstream producers
/// (`seed_particles` → `Array<Particle>`) feed into a wgsl_compute
/// shader that declared its storage as a generic struct.
///
/// The point of the Anonymous boundary is to be **explicitly a
/// byte-level escape hatch** — the wgsl_compute owns the per-byte
/// interpretation inside the shader, and the cast atom on the other
/// side carries the type label downstream. Two typed kinds (Particle
/// ↔ MeshVertex) STILL don't connect — those are semantic mismatches
/// the validator must catch.
fn port_types_compatible(from: PortType, to: PortType) -> bool {
    if from == to {
        return true;
    }
    if let (PortType::Array(a), PortType::Array(b)) = (from, to)
        && a.item_size == b.item_size
        && a.item_align == b.item_align
        && (a.item_kind == ItemKind::Anonymous || b.item_kind == ItemKind::Anonymous)
    {
        return true;
    }
    false
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
    let live = reachable_from_liveness_roots(graph);
    // Reachability filtering only kicks in when at least one liveness
    // root is present. Graphs without any (most unit-test fixtures,
    // plus any caller that builds a graph for its side effects rather
    // than to render) fall back to validating every node — there's no
    // "what does the executor run?" to compute.
    let has_root = graph.nodes().any(|inst| inst.node.is_liveness_root());
    for inst in graph.nodes() {
        // Nodes the executor won't run don't have to satisfy required-
        // input rules — skipping them here makes editing-time graphs
        // robust: the user can drop a Sample into the canvas before
        // wiring it without the renderer falling back to catalog.
        if has_root && !live.contains(&inst.id) {
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

    // Per-wire catch-all — replay `validate_connection`'s endpoint
    // checks (kind, type, format) on every wire in the existing
    // graph. Connect-time validation rejects each wire as it's added,
    // but `validate` is the durable safety net for programmatic
    // construction (JSON load, composite expansion, undo/redo) and
    // sequential mutations. Because both paths invoke
    // `validate_wire_endpoints`, the per-wire checks can't drift
    // between connect-time and compile-time — same source-of-truth
    // function, same rules. The only check not duplicated here is
    // cycle detection (already covered above by `topological_sort`
    // at the whole-graph level).
    for w in graph.wires() {
        validate_wire_endpoints(graph, w.from, w.to)?;
    }

    Ok(())
}

/// Set of nodes whose output is (transitively) consumed by any
/// liveness root. Built by BFS backward across `wires` from every
/// `EffectNode::is_liveness_root` node. Anything outside this set is
/// dead — the executor won't run it and the validator shouldn't
/// reject it.
///
/// Liveness roots include `system.final_output`, primitives declaring
/// `aliased_array_io`, and primitives declaring
/// `state_capture_input_ports` (see [`EffectNode::is_liveness_root`]).
/// Seeding the BFS from only FinalOutput silently strips simulators
/// at the bottom of a scatter-first chain (their output never wires
/// into a FinalOutput-reachable consumer — next frame's read happens
/// through the persistent aliased slot), so this set must match the
/// runtime pruner in [`Executor::compute_live_steps`] one-for-one or
/// the chain compiler filters away nodes the executor would have run.
pub(crate) fn reachable_from_liveness_roots(graph: &Graph) -> AHashSet<NodeInstanceId> {
    let mut live: AHashSet<NodeInstanceId> = AHashSet::default();
    let mut frontier: Vec<NodeInstanceId> = graph
        .nodes()
        .filter(|inst| inst.node.is_liveness_root())
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
///
/// Walks the graph in `ForwardOnly` mode — state-capture wires close
/// per-frame loops through the StateStore rather than this-frame's
/// dependency graph, so they don't contribute to in-degree and don't
/// form cycles in the causality sense. The decision is uniform with
/// [`would_create_cycle`] via the shared
/// [`crate::node_graph::WireWalkMode`] API; no more per-pass
/// `is_state_capture_wire` closures to drift apart.
pub fn topological_sort(graph: &Graph) -> Result<Vec<NodeInstanceId>, GraphError> {
    use crate::node_graph::WireWalkMode;

    let mut in_degree: AHashMap<NodeInstanceId, u32> = AHashMap::default();
    for inst in graph.nodes() {
        in_degree.insert(inst.id, 0);
    }
    for w in graph.walk_wires(WireWalkMode::ForwardOnly) {
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
        for w in graph.walk_wires_from(id, WireWalkMode::ForwardOnly) {
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
/// Format-as-contract check. Returns `Some(PortFormatMismatch)` only
/// when BOTH the producer declares a concrete output format AND the
/// consumer declares a non-empty accepted-format list AND the
/// producer's format is missing from that list. When either side is
/// unconstrained (default `None` return from `output_format` or
/// `accepted_input_formats`), the wire is accepted — the
/// unconstrained side accepts the relationship by not declaring.
///
/// Only applies to `Texture2D` wires; other port types don't carry a
/// texture format. Type compatibility was already checked by the
/// caller via `from_port.ty != to_port.ty`.
fn check_wire_format_compatibility(
    from_node_id: NodeInstanceId,
    from_port: &'static str,
    from_node: &dyn crate::node_graph::effect_node::EffectNode,
    to_node_id: NodeInstanceId,
    to_port: &'static str,
    to_node: &dyn crate::node_graph::effect_node::EffectNode,
) -> Option<GraphError> {
    let producer_format = from_node.output_format(from_port)?;
    let accepted = to_node.accepted_input_formats(to_port)?;
    if accepted.is_empty() {
        // An empty accept-list defaults to "any" — declaring the
        // method exists but accepting nothing would be unreachable.
        return None;
    }
    if accepted.contains(&producer_format) {
        return None;
    }
    Some(GraphError::PortFormatMismatch {
        from_node: from_node_id,
        from_port: from_port.to_string(),
        to_node: to_node_id,
        to_port: to_port.to_string(),
        producer_format,
        accepted: accepted.to_vec(),
    })
}

fn would_create_cycle(graph: &Graph, from: NodeInstanceId, to: NodeInstanceId) -> bool {
    if from == to {
        return true; // self-loop
    }
    // Skip wires that terminate on a state-capture port during the
    // traversal — they're next-frame captures, not this-frame
    // dependencies, so they don't contribute to a closeable cycle.
    // Matches the topological_sort logic via the shared
    // `WireWalkMode::ForwardOnly` API — both passes now share one
    // definition of "forward dependency only" so the two can't drift
    // (they did before this API existed; that's the bug this whole
    // unification PR closes).
    use crate::node_graph::WireWalkMode;
    let mut visited: HashSet<NodeInstanceId> = HashSet::new();
    let mut stack = vec![to];
    while let Some(n) = stack.pop() {
        if !visited.insert(n) {
            continue;
        }
        if n == from {
            return true;
        }
        for w in graph.walk_wires_from(n, WireWalkMode::ForwardOnly) {
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
        // Tests construct stand-ins for the FinalOutput boundary by
        // its type_id string. The real FinalOutput impl overrides
        // `is_liveness_root` to true; mirror that here so tests don't
        // have to wrap in a separate fixture struct.
        fn is_liveness_root(&self) -> bool {
            self.type_id.as_str() == crate::node_graph::FINAL_OUTPUT_TYPE_ID
        }
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

    /// Test scaffold for the format contract — extends `TestNode`
    /// with declared output formats and accepted input formats per
    /// port name. Default behaviour (empty maps) matches the
    /// production default: unconstrained on every port.
    struct FormatTestNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        output_formats: ahash::AHashMap<&'static str, manifold_gpu::GpuTextureFormat>,
        accepted_inputs: ahash::AHashMap<&'static str, &'static [manifold_gpu::GpuTextureFormat]>,
    }

    impl FormatTestNode {
        fn new(name: &'static str, inputs: Vec<NodeInput>, outputs: Vec<NodeOutput>) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs,
                outputs,
                output_formats: ahash::AHashMap::default(),
                accepted_inputs: ahash::AHashMap::default(),
            }
        }
        fn with_output_format(
            mut self,
            port: &'static str,
            fmt: manifold_gpu::GpuTextureFormat,
        ) -> Self {
            self.output_formats.insert(port, fmt);
            self
        }
        fn with_accepted_inputs(
            mut self,
            port: &'static str,
            accepted: &'static [manifold_gpu::GpuTextureFormat],
        ) -> Self {
            self.accepted_inputs.insert(port, accepted);
            self
        }
    }

    impl crate::node_graph::EffectNode for FormatTestNode {
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
        fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
            self.output_formats.get(port).copied()
        }
        fn accepted_input_formats(
            &self,
            port: &str,
        ) -> Option<&'static [manifold_gpu::GpuTextureFormat]> {
            self.accepted_inputs.get(port).copied()
        }
    }

    #[test]
    fn format_contract_accepts_when_producer_unconstrained() {
        // Producer doesn't declare a format → wire goes through
        // regardless of what the consumer accepts.
        let mut g = Graph::new();
        let a = g.add_node(Box::new(FormatTestNode::new(
            "producer",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(
            FormatTestNode::new(
                "consumer",
                vec![input("in", PortType::Texture2D, true)],
                vec![],
            )
            .with_accepted_inputs("in", &[manifold_gpu::GpuTextureFormat::Rgba16Float]),
        ));
        g.connect((a, "out"), (b, "in"))
            .expect("unconstrained producer accepts any consumer");
    }

    #[test]
    fn format_contract_accepts_when_consumer_unconstrained() {
        // Consumer doesn't declare accepted formats → wire goes
        // through regardless of what the producer emits.
        let mut g = Graph::new();
        let a = g.add_node(Box::new(
            FormatTestNode::new(
                "producer",
                vec![],
                vec![output("out", PortType::Texture2D)],
            )
            .with_output_format("out", manifold_gpu::GpuTextureFormat::Rgba32Float),
        ));
        let b = g.add_node(Box::new(FormatTestNode::new(
            "consumer",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((a, "out"), (b, "in"))
            .expect("unconstrained consumer accepts any producer");
    }

    #[test]
    fn format_contract_accepts_when_producer_format_in_accept_list() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(
            FormatTestNode::new(
                "producer",
                vec![],
                vec![output("out", PortType::Texture2D)],
            )
            .with_output_format("out", manifold_gpu::GpuTextureFormat::Rgba16Float),
        ));
        let b = g.add_node(Box::new(
            FormatTestNode::new(
                "consumer",
                vec![input("in", PortType::Texture2D, true)],
                vec![],
            )
            .with_accepted_inputs(
                "in",
                &[
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                    manifold_gpu::GpuTextureFormat::Rgba32Float,
                ],
            ),
        ));
        g.connect((a, "out"), (b, "in"))
            .expect("format in accept list passes");
    }

    #[test]
    fn format_contract_rejects_at_connect_when_mismatch() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(
            FormatTestNode::new(
                "producer",
                vec![],
                vec![output("out", PortType::Texture2D)],
            )
            .with_output_format("out", manifold_gpu::GpuTextureFormat::Rgba32Float),
        ));
        let b = g.add_node(Box::new(
            FormatTestNode::new(
                "consumer",
                vec![input("in", PortType::Texture2D, true)],
                vec![],
            )
            .with_accepted_inputs(
                "in",
                &[manifold_gpu::GpuTextureFormat::Rgba16Float],
            ),
        ));
        let err = g
            .connect((a, "out"), (b, "in"))
            .expect_err("mismatched formats must error at connect");
        match err {
            GraphError::PortFormatMismatch {
                producer_format,
                accepted,
                ..
            } => {
                assert_eq!(producer_format, manifold_gpu::GpuTextureFormat::Rgba32Float);
                assert_eq!(accepted, vec![manifold_gpu::GpuTextureFormat::Rgba16Float]);
            }
            other => panic!("expected PortFormatMismatch, got {other:?}"),
        }
    }

    /// Connect-vs-compile convergence: the per-wire checks in
    /// `validate_connection` (kind, type, format) must produce the
    /// same verdict as `validate`'s catch-all sweep — they share the
    /// `validate_wire_endpoints` helper, so this is a structural
    /// invariant. The test exercises a representative spread of
    /// well-formed and malformed wires and asserts identity. A
    /// future refactor that lets the two paths drift fails here.
    #[test]
    fn validate_subsumes_connect_per_wire_checks_for_every_endpoint_failure_mode() {
        use crate::node_graph::ports::PortType;

        // Every malformed-wire scenario we care about: each is built
        // by hand-constructing a graph through the public API (which
        // routes through `connect`), and the test confirms that
        // either (a) connect rejects upfront (the connect path
        // returning a specific error), or (b) connect would have
        // rejected if we tried — `validate_wire_endpoints` called
        // directly returns the same error.
        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, fn() -> (Graph, (NodeInstanceId, &'static str), (NodeInstanceId, &'static str), &'static str))> = vec![
            ("type mismatch", || {
                let mut g = Graph::new();
                let a = g.add_node(Box::new(TestNode::new(
                    "tex_out",
                    vec![],
                    vec![output("out", PortType::Texture2D)],
                )));
                let b = g.add_node(Box::new(TestNode::new(
                    "tex3d_in",
                    vec![input("in", PortType::Texture3D, true)],
                    vec![],
                )));
                (g, (a, "out"), (b, "in"), "PortTypeMismatch")
            }),
            ("unknown from-port", || {
                let mut g = Graph::new();
                let a = g.add_node(Box::new(TestNode::new(
                    "src",
                    vec![],
                    vec![output("out", PortType::Texture2D)],
                )));
                let b = g.add_node(Box::new(TestNode::new(
                    "sink",
                    vec![input("in", PortType::Texture2D, true)],
                    vec![],
                )));
                (g, (a, "nonexistent"), (b, "in"), "PortNotFound")
            }),
            ("unknown to-port", || {
                let mut g = Graph::new();
                let a = g.add_node(Box::new(TestNode::new(
                    "src",
                    vec![],
                    vec![output("out", PortType::Texture2D)],
                )));
                let b = g.add_node(Box::new(TestNode::new(
                    "sink",
                    vec![input("in", PortType::Texture2D, true)],
                    vec![],
                )));
                (g, (a, "out"), (b, "nonexistent"), "PortNotFound")
            }),
        ];

        for (label, build) in cases {
            let (g, from, to, expected_variant) = build();

            // connect-time rejection
            let connect_err = validate_connection(&g, from, to).expect_err(
                "connect-time validation should reject this case",
            );
            let connect_label = error_variant_label(&connect_err);
            assert_eq!(
                connect_label, expected_variant,
                "[{label}] connect returned {connect_label}, expected {expected_variant}",
            );

            // validate_wire_endpoints called directly should return
            // the SAME error class.
            let endpoint_err = validate_wire_endpoints(&g, from, to).expect_err(
                "validate_wire_endpoints should reject this case (it's what connect calls)",
            );
            let endpoint_label = error_variant_label(&endpoint_err);
            assert_eq!(
                connect_label, endpoint_label,
                "[{label}] connect and validate_wire_endpoints diverged: \
                 connect={connect_label} endpoint={endpoint_label}",
            );
        }
    }

    fn error_variant_label(e: &GraphError) -> &'static str {
        match e {
            GraphError::NodeNotFound(_) => "NodeNotFound",
            GraphError::PortNotFound { .. } => "PortNotFound",
            GraphError::PortKindMismatch { .. } => "PortKindMismatch",
            GraphError::PortTypeMismatch { .. } => "PortTypeMismatch",
            GraphError::RequiredInputUnwired { .. } => "RequiredInputUnwired",
            GraphError::ParamNotFound { .. } => "ParamNotFound",
            GraphError::CycleDetected { .. } => "CycleDetected",
            GraphError::PortFormatMismatch { .. } => "PortFormatMismatch",
        }
    }

    /// Regression: when a state-capture back-edge is added BEFORE the
    /// forward edges that close the loop through it, the cycle
    /// detector must NOT false-positive. Before the `WireWalkMode`
    /// unification (and before `would_create_cycle` learned to skip
    /// state-capture wires in its DFS), this scenario was a silent
    /// footgun — adding edges in the "wrong" order rejected a valid
    /// feedback-loop graph that adding them in a different order
    /// would accept.
    #[test]
    fn cycle_detector_ignores_state_capture_back_edges_regardless_of_wire_order() {
        // Graph: producer → mid → consumer, with consumer → producer
        // wired into a state-capture port. The legitimate feedback
        // loop is `producer.out → mid.in → consumer.in (forward)`
        // closed by `consumer.out → producer.capture_in` (back-edge).
        let mut g = Graph::new();
        let producer = g.add_node(Box::new(
            TestNodeWithCapturePort::new("producer", vec![
                input("capture_in", PortType::Texture2D, false),
            ], vec![output("out", PortType::Texture2D)]),
        ));
        let mid = g.add_node(Box::new(TestNode::new(
            "mid",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let consumer = g.add_node(Box::new(TestNode::new(
            "consumer",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));

        // Add the BACK-edge first — this used to break the cycle
        // detector.
        g.connect((consumer, "out"), (producer, "capture_in"))
            .expect("back-edge wire onto state-capture port is allowed");

        // Then the forward chain that closes the loop.
        g.connect((producer, "out"), (mid, "in"))
            .expect("forward edge producer → mid should not trip cycle detector");
        g.connect((mid, "out"), (consumer, "in"))
            .expect("forward edge mid → consumer should not trip cycle detector");

        // Whole-graph validation also accepts (topological_sort
        // returns an order rather than CycleDetected).
        topological_sort(&g).expect("graph with state-capture back-edge has a valid topo order");
    }

    /// Local test scaffold: like `TestNode` but declares its first
    /// input port as a state-capture port via
    /// `state_capture_input_ports`.
    struct TestNodeWithCapturePort {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        capture_ports: &'static [&'static str],
    }

    impl TestNodeWithCapturePort {
        fn new(name: &'static str, inputs: Vec<NodeInput>, outputs: Vec<NodeOutput>) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs,
                outputs,
                capture_ports: &["capture_in"],
            }
        }
    }

    impl crate::node_graph::EffectNode for TestNodeWithCapturePort {
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
        fn state_capture_input_ports(&self) -> &'static [&'static str] {
            self.capture_ports
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

    /// State-capture is per-PORT, not per-NODE. A stateful node can have
    /// `in` as a cycle-break input (exempt from in-degree, runs first)
    /// while a sibling input like `seed` is a normal per-frame dependency
    /// whose producer must run upstream. Regression for the `node.feedback`
    /// seed bug where the planner treated every incoming wire to a
    /// cycle-breaker as a state capture, so the seed source's output slot
    /// was pre-cleared to black and the seed-on-first-allocation contract
    /// silently failed.
    #[test]
    fn topo_sort_state_capture_is_per_port_not_per_node() {
        struct StatefulNode {
            type_id: EffectNodeType,
        }
        impl crate::node_graph::EffectNode for StatefulNode {
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                static INPUTS: [NodeInput; 2] = [
                    NodePort {
                        name: "in",
                        ty: PortType::Texture2D,
                        kind: PortKind::Input,
                        required: true,
                    },
                    NodePort {
                        name: "seed",
                        ty: PortType::Texture2D,
                        kind: PortKind::Input,
                        required: false,
                    },
                ];
                &INPUTS
            }
            fn outputs(&self) -> &[NodeOutput] {
                static OUTPUTS: [NodeOutput; 1] = [NodePort {
                    name: "out",
                    ty: PortType::Texture2D,
                    kind: PortKind::Output,
                    required: false,
                }];
                &OUTPUTS
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
            fn state_capture_input_ports(&self) -> &'static [&'static str] {
                &["in"]
            }
        }

        // Graph shape:
        //   src         (produces -> stateful.in via loop_back)
        //   seed_src    (produces -> stateful.seed)
        //   stateful    (in = state capture, seed = per-frame)
        //   loop_back   (consumes stateful.out, produces back to stateful.in)
        //
        // Because `in` is a state capture, the wire loop_back -> stateful.in
        // doesn't impose an in-degree on stateful. But the wire
        // seed_src -> stateful.seed MUST impose in-degree, so seed_src
        // runs BEFORE stateful.
        let mut g = Graph::new();
        let stateful = g.add_node(Box::new(StatefulNode {
            type_id: EffectNodeType::new("stateful"),
        }));
        let seed_src = g.add_node(Box::new(TestNode::new(
            "seed_src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let loop_back = g.add_node(Box::new(TestNode::new(
            "loop_back",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        // stateful.out -> loop_back.in (regular wire)
        g.connect((stateful, "out"), (loop_back, "in")).unwrap();
        // loop_back.out -> stateful.in (state capture; would be a cycle
        // without the per-port exemption)
        g.connect((loop_back, "out"), (stateful, "in")).unwrap();
        // seed_src.out -> stateful.seed (per-frame dependency)
        g.connect((seed_src, "out"), (stateful, "seed")).unwrap();

        let order = topological_sort(&g).unwrap();
        let index_of = |id: NodeInstanceId| order.iter().position(|&n| n == id).unwrap();
        assert!(
            index_of(seed_src) < index_of(stateful),
            "seed producer must run before the stateful consumer — its `seed` \
             wire is a regular per-frame dependency, not a state capture. \
             order: {order:?}"
        );
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
