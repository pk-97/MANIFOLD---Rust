//! Substep repeat regions — the plan compiler's side of a bounded simulation
//! loop inside one frame. Contract: `docs/GPU_MPM_SOLVER_DESIGN.md` D7 (the
//! historical seam of `docs/WATER_SIMULATION_DESIGN.md` section 4 (Fixed
//! substeps and graph compiler seam), re-implemented on current main).
//!
//! A *substep boundary* node declares its port names through
//! [`SubstepBoundaryPorts`]. At compile time the nodes that are both
//! descendants of the boundary and ancestors of its capture producers are
//! contracted into a [`SubstepRegion`]. The executor runs the boundary once,
//! then the region body `count` times with per-iteration scalars; only the
//! boundary's final outputs escape. Regions are compile-time, never nested,
//! and a malformed region is a compile error naming NodeIds, never a
//! fallback to ordinary traversal.

use ahash::{AHashMap, AHashSet};

use crate::node_graph::boundary_nodes::FINAL_OUTPUT_TYPE_ID;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::execution_plan::ResourceId;
use crate::node_graph::freeze::classify::BoundaryReason;
use crate::node_graph::graph::{Graph, WireWalkMode};
use crate::node_graph::validation::GraphError;

/// Port names a substep boundary declares to the plan compiler and executor.
///
/// `seed` is the one-shot initial state; `capture`/`state` are the primary
/// back-edge pair (the region's final candidate in, the accepted state out).
/// `iteration_scalars` are scalar outputs the executor writes before EACH
/// body iteration, in this order, from
/// [`EffectNode::substep_iteration`](crate::node_graph::effect_node::EffectNode::substep_iteration).
/// `results` are further capture→output back-edge pairs whose final values
/// escape the region alongside `state` (per-tick statistics, coupling sums).
#[derive(Clone, Copy, Debug)]
pub struct SubstepBoundaryPorts {
    pub seed: &'static str,
    pub capture: &'static str,
    pub state: &'static str,
    pub iteration_scalars: &'static [&'static str],
    pub results: &'static [SubstepResultPorts],
}

impl SubstepBoundaryPorts {
    /// Every declared capture port: the primary one first, then each result.
    pub fn capture_ports(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.capture).chain(self.results.iter().map(|r| r.capture))
    }
}

/// One typed capture→output back-edge pair beyond the primary state.
#[derive(Clone, Copy, Debug)]
pub struct SubstepResultPorts {
    pub capture: &'static str,
    pub output: &'static str,
}

/// One contracted repeat region of an
/// [`ExecutionPlan`](crate::node_graph::execution_plan::ExecutionPlan).
///
/// `steps` index `ExecutionPlan::steps`: the boundary first, then the body in
/// topological order, contiguous by construction. `held_resources` are every
/// non-persistent wire whose last reader is a region step. They never appear
/// in any step's `free_after` — a free attached to a body step would fire per
/// iteration or, for the boundary, before the body reads it — so the executor
/// holds them for the whole repeat and releases them when the region ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstepRegion {
    pub boundary: NodeInstanceId,
    pub steps: Vec<usize>,
    pub held_resources: Vec<ResourceId>,
}

/// Node-level result of region derivation: the boundary first, then its body
/// in topological order. `compile` maps it to step indices.
#[derive(Debug, Clone)]
pub(crate) struct RegionNodes {
    pub boundary: NodeInstanceId,
    pub nodes: Vec<NodeInstanceId>,
}

/// Derive and validate the substep regions of the live graph.
///
/// `active_order` is the liveness-filtered topological order. Membership is
/// computed over forward wires only (capture wires are the back-edges): the
/// body is every node that is BOTH a descendant of the boundary AND an
/// ancestor of a producer wired into one of the boundary's capture ports.
/// External inputs (seed, controls, lattice parameters) are ancestors but not
/// descendants, so they stay outside and evaluate once before the region.
///
/// Every failure is [`GraphError::MalformedSubstepRegion`] naming the boundary
/// and the offending node: an undeclared port, an unwired capture port, a
/// capture producer the boundary does not reach, a nested boundary, a
/// state-capture node, a draw call, the final output or a coupled-scene
/// participant inside a body, a body wire read outside the region, a node
/// claimed by two regions, or one region feeding another.
pub(crate) fn derive_regions(
    graph: &Graph,
    active_order: &[NodeInstanceId],
) -> Result<Vec<RegionNodes>, GraphError> {
    // Sorted by id: `Graph::nodes` walks a hash map, and region order decides
    // step order and ResourceIds in multi-region graphs.
    let mut boundaries: Vec<(NodeInstanceId, SubstepBoundaryPorts)> = graph
        .nodes()
        .filter_map(|inst| inst.node.substep_boundary().map(|p| (inst.id, p)))
        .collect();
    if boundaries.is_empty() {
        return Ok(Vec::new());
    }
    boundaries.sort_by_key(|(id, _)| id.0);

    let active: AHashSet<NodeInstanceId> = active_order.iter().copied().collect();
    let mut fwd: AHashMap<NodeInstanceId, Vec<NodeInstanceId>> = AHashMap::default();
    let mut rev: AHashMap<NodeInstanceId, Vec<NodeInstanceId>> = AHashMap::default();
    for w in graph.walk_wires(WireWalkMode::ForwardOnly) {
        fwd.entry(w.from.0).or_default().push(w.to.0);
        rev.entry(w.to.0).or_default().push(w.from.0);
    }
    let coupled: AHashSet<NodeInstanceId> = graph
        .coupled_scenes()
        .iter()
        .flat_map(|pair| [pair.fluid, pair.rigid])
        .collect();

    let mut regions: Vec<RegionNodes> = Vec::with_capacity(boundaries.len());
    let mut claimed: AHashSet<NodeInstanceId> = AHashSet::default();
    for &(boundary, ports) in &boundaries {
        if !active.contains(&boundary) {
            continue;
        }
        let inst = graph.get_node(boundary).expect("boundary exists");
        let has_input = |name: &str| inst.node.inputs().iter().any(|p| p.name == name);
        let has_output = |name: &str| inst.node.outputs().iter().any(|p| p.name == name);
        let declared_inputs = std::iter::once(ports.seed).chain(ports.capture_ports());
        let declared_outputs = std::iter::once(ports.state)
            .chain(ports.iteration_scalars.iter().copied())
            .chain(ports.results.iter().map(|r| r.output));
        if let Some(port) = declared_inputs
            .filter(|p| !has_input(p))
            .chain(declared_outputs.filter(|p| !has_output(p)))
            .next()
        {
            return Err(malformed(
                boundary,
                boundary,
                format!("declared substep port `{port}` is not a port of the boundary"),
            ));
        }
        let captures = inst.node.state_capture_input_ports();
        if let Some(port) = ports.capture_ports().find(|p| !captures.contains(p)) {
            return Err(malformed(
                boundary,
                boundary,
                format!("capture port `{port}` is not declared as a state-capture input"),
            ));
        }

        let mut producers: Vec<NodeInstanceId> = Vec::new();
        for cap in ports.capture_ports() {
            let Some(wire) = graph
                .wires_into(boundary)
                .find(|w| w.to.1 == cap)
            else {
                return Err(malformed(
                    boundary,
                    boundary,
                    format!("capture port `{cap}` has no wire"),
                ));
            };
            producers.push(wire.from.0);
        }

        let forward = reach(boundary, &fwd);
        let backward = {
            let mut seen = AHashSet::default();
            let mut stack = producers.clone();
            while let Some(n) = stack.pop() {
                if n == boundary || !seen.insert(n) {
                    continue;
                }
                if let Some(prev) = rev.get(&n) {
                    stack.extend(prev.iter().copied());
                }
            }
            seen
        };
        let body: Vec<NodeInstanceId> = active_order
            .iter()
            .copied()
            .filter(|id| *id != boundary && forward.contains(id) && backward.contains(id))
            .collect();

        for &producer in &producers {
            if !body.contains(&producer) {
                return Err(malformed(
                    boundary,
                    producer,
                    "capture producer is not reachable from the boundary's outputs — \
                     the region is not connected"
                        .to_string(),
                ));
            }
        }

        let members: AHashSet<NodeInstanceId> = body.iter().copied().collect();
        for &node in &body {
            let inst = graph.get_node(node).expect("body node exists");
            if inst.node.substep_boundary().is_some() {
                return Err(malformed(
                    boundary,
                    node,
                    "nested substep boundary inside a region".to_string(),
                ));
            }
            if !inst.node.state_capture_input_ports().is_empty() {
                return Err(malformed(
                    boundary,
                    node,
                    "feedback/state-capture node inside a substep region".to_string(),
                ));
            }
            if inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID
                || inst.node.boundary_reason() == Some(BoundaryReason::DrawCall)
            {
                return Err(malformed(
                    boundary,
                    node,
                    "render/IO node inside a substep region".to_string(),
                ));
            }
            if coupled.contains(&node) {
                return Err(malformed(
                    boundary,
                    node,
                    "coupled-scene participant inside a substep region".to_string(),
                ));
            }
            if let Some(&target) = fwd
                .get(&node)
                .and_then(|targets| targets.iter().find(|t| **t != boundary && !members.contains(t)))
            {
                return Err(malformed(
                    boundary,
                    node,
                    format!(
                        "region intermediate wire escapes to outside reader {target:?} — \
                         only the boundary's outputs may leave the region"
                    ),
                ));
            }
            if !claimed.insert(node) {
                return Err(malformed(
                    boundary,
                    node,
                    "node belongs to two substep regions (overlap)".to_string(),
                ));
            }
        }
        if !claimed.insert(boundary) {
            return Err(malformed(
                boundary,
                boundary,
                "boundary belongs to two substep regions (overlap)".to_string(),
            ));
        }
        let mut nodes = Vec::with_capacity(body.len() + 1);
        nodes.push(boundary);
        nodes.extend(body);
        regions.push(RegionNodes { boundary, nodes });
    }

    // One region feeding another (chaining) is not supported: a member whose
    // forward predecessor belongs to a different region.
    let region_of: AHashMap<NodeInstanceId, NodeInstanceId> = regions
        .iter()
        .flat_map(|r| r.nodes.iter().map(move |&n| (n, r.boundary)))
        .collect();
    for region in &regions {
        for &node in &region.nodes {
            for &pred in rev.get(&node).map(Vec::as_slice).unwrap_or(&[]) {
                if region_of.get(&pred).is_some_and(|&b| b != region.boundary) {
                    return Err(malformed(
                        region.boundary,
                        pred,
                        "one substep region feeds another — region chaining is not supported"
                            .to_string(),
                    ));
                }
            }
        }
    }

    Ok(regions)
}

fn malformed(boundary: NodeInstanceId, node: NodeInstanceId, reason: String) -> GraphError {
    GraphError::MalformedSubstepRegion {
        boundary,
        node,
        reason,
    }
}

fn reach(
    from: NodeInstanceId,
    edges: &AHashMap<NodeInstanceId, Vec<NodeInstanceId>>,
) -> AHashSet<NodeInstanceId> {
    let mut seen = AHashSet::default();
    let mut stack = vec![from];
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        if let Some(next) = edges.get(&n) {
            stack.extend(next.iter().copied());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    //! Compile-level tests of the region contract: membership, contraction,
    //! held resources, the rejected shapes and truncation. Synthetic graphs
    //! only — no GPU, no executor.

    use super::*;
    use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
    use crate::node_graph::execution_plan::compile;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::physics::RigidImpulseTargets;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};

    const STATS: &[SubstepResultPorts] = &[SubstepResultPorts {
        capture: "stats_in",
        output: "stats",
    }];

    const PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
        seed: "seed",
        capture: "in",
        state: "out",
        iteration_scalars: &["step_dt", "step_index"],
        results: &[],
    };

    const PORTS_WITH_STATS: SubstepBoundaryPorts = SubstepBoundaryPorts {
        results: STATS,
        ..PORTS
    };

    struct TestNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        boundary: Option<SubstepBoundaryPorts>,
        capture_inputs: &'static [&'static str],
        draw_call: bool,
    }

    impl TestNode {
        fn new(name: &'static str, inputs: Vec<NodeInput>, outputs: Vec<NodeOutput>) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs,
                outputs,
                boundary: None,
                capture_inputs: &[],
                draw_call: false,
            }
        }

        fn boundary(ports: SubstepBoundaryPorts) -> Self {
            let mut inputs = vec![
                input("seed", PortType::Texture2D, false),
                input("in", PortType::Texture2D, false),
            ];
            let mut outputs = vec![
                output("out", PortType::Texture2D),
                output("step_dt", PortType::Scalar(ScalarType::F32)),
                output("step_index", PortType::Scalar(ScalarType::F32)),
            ];
            let capture_inputs: &'static [&'static str] = if ports.results.is_empty() {
                &["in"]
            } else {
                inputs.push(input("stats_in", PortType::Texture2D, false));
                outputs.push(output("stats", PortType::Texture2D));
                &["in", "stats_in"]
            };
            Self {
                boundary: Some(ports),
                capture_inputs,
                ..Self::new("test.substep_boundary", inputs, outputs)
            }
        }

        fn feedback_style() -> Self {
            Self {
                capture_inputs: &["prev"],
                ..Self::new(
                    "test.feedback_style",
                    vec![
                        input("tex", PortType::Texture2D, true),
                        input("prev", PortType::Texture2D, false),
                    ],
                    vec![output("out", PortType::Texture2D)],
                )
            }
        }
    }

    impl EffectNode for TestNode {
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
        fn state_capture_input_ports(&self) -> &[&str] {
            self.capture_inputs
        }
        fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
            self.boundary
        }
        fn boundary_reason(&self) -> Option<BoundaryReason> {
            self.draw_call.then_some(BoundaryReason::DrawCall)
        }
        fn is_liveness_root(&self) -> bool {
            self.type_id.as_str() == FINAL_OUTPUT_TYPE_ID
        }
    }

    fn input(name: &'static str, ty: PortType, required: bool) -> NodeInput {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind: PortKind::Input,
            required,
        }
    }

    fn output(name: &'static str, ty: PortType) -> NodeOutput {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind: PortKind::Output,
            required: false,
        }
    }

    fn pass(graph: &mut Graph, name: &'static str) -> NodeInstanceId {
        graph.add_node(Box::new(TestNode::new(
            name,
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, false),
                input("dt", PortType::Scalar(ScalarType::F32), false),
            ],
            vec![output("out", PortType::Texture2D)],
        )))
    }

    fn source(graph: &mut Graph, name: &'static str) -> NodeInstanceId {
        graph.add_node(Box::new(TestNode::new(
            name,
            vec![],
            vec![output("out", PortType::Texture2D)],
        )))
    }

    fn sink(graph: &mut Graph, name: &'static str) -> NodeInstanceId {
        graph.add_node(Box::new(TestNode::new(
            name,
            vec![
                input("tex", PortType::Texture2D, true),
                input("aux", PortType::Texture2D, false),
            ],
            vec![],
        )))
    }

    /// ```text
    /// src ─┬─▶ boundary.seed
    ///      └─▶ body_a.b
    /// boundary.out ─┬─▶ body_a.a ─▶ body_b.a ─▶ (capture) boundary.in
    ///               └─▶ consumer.tex
    /// boundary.step_dt ─▶ body_b.dt
    /// ```
    struct HappyPath {
        graph: Graph,
        src: NodeInstanceId,
        boundary: NodeInstanceId,
        body_a: NodeInstanceId,
        body_b: NodeInstanceId,
        consumer: NodeInstanceId,
    }

    fn happy_path() -> HappyPath {
        let mut graph = Graph::new();
        let src = source(&mut graph, "src");
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body_a = pass(&mut graph, "body_a");
        let body_b = pass(&mut graph, "body_b");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((src, "out"), (body_a, "b")).unwrap();
        graph.connect((boundary, "out"), (body_a, "a")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();
        graph.connect((boundary, "step_dt"), (body_b, "dt")).unwrap();
        graph.connect((body_a, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (boundary, "in")).unwrap();
        HappyPath {
            graph,
            src,
            boundary,
            body_a,
            body_b,
            consumer,
        }
    }

    fn malformed_parts(err: GraphError) -> (NodeInstanceId, NodeInstanceId, String) {
        match err {
            GraphError::MalformedSubstepRegion {
                boundary,
                node,
                reason,
            } => (boundary, node, reason),
            other => panic!("expected MalformedSubstepRegion, got {other:?}"),
        }
    }

    fn step_output(plan: &crate::node_graph::ExecutionPlan, step: usize, port: &str) -> ResourceId {
        plan.steps()[step]
            .outputs
            .iter()
            .find(|(p, _)| *p == port)
            .map(|(_, r)| *r)
            .expect("output bound")
    }

    #[test]
    fn substeps_region_happy_path_contract() {
        let hp = happy_path();
        let plan = compile(&hp.graph).unwrap();
        let steps = plan.steps();
        let order: Vec<NodeInstanceId> = steps.iter().map(|s| s.node).collect();
        assert_eq!(
            order,
            vec![hp.src, hp.boundary, hp.body_a, hp.body_b, hp.consumer]
        );

        let regions = plan.substep_regions();
        assert_eq!(regions.len(), 1);
        let region = &regions[0];
        assert_eq!(region.boundary, hp.boundary);
        assert_eq!(region.steps, vec![1, 2, 3]);

        // Held: every non-persistent wire whose last reader is a region
        // step — src.out (read by the boundary and body_a), body_a.out,
        // and the step_dt scalar. boundary.out escapes to the consumer and
        // keeps its ordinary lifetime.
        let r_src = step_output(&plan, 0, "out");
        let r_dt = step_output(&plan, 1, "step_dt");
        let r_body_a = step_output(&plan, 2, "out");
        let r_body_b = step_output(&plan, 3, "out");
        let r_state = step_output(&plan, 1, "out");
        let mut expected = vec![r_src, r_dt, r_body_a];
        expected.sort();
        assert_eq!(region.held_resources, expected);
        assert_eq!(plan.persistent_resources(), &[r_body_b]);
        assert!(!region.held_resources.contains(&r_state));

        for step in steps {
            for res in [r_src, r_dt, r_body_a, r_body_b] {
                assert!(!step.free_after.contains(&res), "{res:?} freed mid-region");
            }
        }
        assert!(steps[4].free_after.contains(&r_state));

        // The boundary's capture runs per iteration, never in the frame-end
        // late pass; region steps are never memoized.
        assert!(plan.late_capture_step_indices().is_empty());
        assert!((1..=3).all(|i| !plan.step_hoistable(i)));
    }

    #[test]
    fn substeps_region_result_ports_escape_and_are_captured() {
        let mut graph = Graph::new();
        let src = source(&mut graph, "src");
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS_WITH_STATS)));
        let body = pass(&mut graph, "body");
        let stats = pass(&mut graph, "stats");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (body, "a")).unwrap();
        graph.connect((body, "out"), (boundary, "in")).unwrap();
        graph.connect((body, "out"), (stats, "a")).unwrap();
        graph.connect((stats, "out"), (boundary, "stats_in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();
        graph.connect((boundary, "stats"), (consumer, "aux")).unwrap();

        let plan = compile(&graph).unwrap();
        let region = &plan.substep_regions()[0];
        let nodes: Vec<NodeInstanceId> =
            region.steps.iter().map(|&i| plan.steps()[i].node).collect();
        assert_eq!(nodes, vec![boundary, body, stats]);
        let r_body = step_output(&plan, region.steps[1], "out");
        let r_stats = step_output(&plan, region.steps[2], "out");
        assert_eq!(plan.persistent_resources(), &[r_body, r_stats]);
    }

    #[test]
    fn substeps_region_unwired_capture_port_rejected() {
        let mut graph = Graph::new();
        let src = source(&mut graph, "src");
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let consumer = sink(&mut graph, "consumer");
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (boundary, boundary));
        assert!(reason.contains("capture port `in` has no wire"), "{reason}");
    }

    #[test]
    fn substeps_region_unwired_result_capture_rejected() {
        let mut graph = Graph::new();
        let src = source(&mut graph, "src");
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS_WITH_STATS)));
        let body = pass(&mut graph, "body");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (body, "a")).unwrap();
        graph.connect((body, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();

        let (b, _, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!(b, boundary);
        assert!(reason.contains("`stats_in` has no wire"), "{reason}");
    }

    #[test]
    fn substeps_region_undeclared_port_rejected() {
        const BAD: SubstepBoundaryPorts = SubstepBoundaryPorts {
            iteration_scalars: &["step_dt", "tick_start"],
            ..PORTS
        };
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary(BAD)));
        let body = pass(&mut graph, "body");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((boundary, "out"), (body, "a")).unwrap();
        graph.connect((body, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();

        let (_, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!(node, boundary);
        assert!(reason.contains("`tick_start`"), "{reason}");
    }

    #[test]
    fn substeps_region_disconnected_capture_producer_rejected() {
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let consumer = sink(&mut graph, "consumer");
        let src2 = source(&mut graph, "src2");
        let rogue = pass(&mut graph, "rogue");
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();
        graph.connect((src2, "out"), (rogue, "a")).unwrap();
        graph.connect((rogue, "out"), (boundary, "in")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (boundary, rogue));
        assert!(reason.contains("not reachable"), "{reason}");
    }

    #[test]
    fn substeps_region_nested_boundary_rejected() {
        let mut graph = Graph::new();
        let src = source(&mut graph, "src");
        let outer = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body_a = pass(&mut graph, "body_a");
        let inner = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner_body = pass(&mut graph, "inner_body");
        let body_b = pass(&mut graph, "body_b");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((src, "out"), (outer, "seed")).unwrap();
        graph.connect((outer, "out"), (body_a, "a")).unwrap();
        graph.connect((body_a, "out"), (body_b, "a")).unwrap();
        graph.connect((body_a, "out"), (inner, "seed")).unwrap();
        graph.connect((inner, "out"), (body_b, "b")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((body_b, "out"), (outer, "in")).unwrap();
        graph.connect((outer, "out"), (consumer, "tex")).unwrap();

        // Boundaries are walked in id order; `outer` was added first.
        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (outer, inner));
        assert!(reason.contains("nested"), "{reason}");
    }

    #[test]
    fn substeps_region_feedback_node_in_body_rejected() {
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body_a = pass(&mut graph, "body_a");
        let fb = graph.add_node(Box::new(TestNode::feedback_style()));
        let body_b = pass(&mut graph, "body_b");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((boundary, "out"), (body_a, "a")).unwrap();
        graph.connect((body_a, "out"), (fb, "tex")).unwrap();
        graph.connect((body_a, "out"), (fb, "prev")).unwrap();
        graph.connect((fb, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (boundary, fb));
        assert!(reason.contains("state-capture"), "{reason}");
    }

    #[test]
    fn substeps_region_render_node_in_body_rejected() {
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let draw = graph.add_node(Box::new(TestNode {
            draw_call: true,
            ..TestNode::new(
                "test.draw",
                vec![input("a", PortType::Texture2D, true)],
                vec![output("out", PortType::Texture2D)],
            )
        }));
        let consumer = sink(&mut graph, "consumer");
        graph.connect((boundary, "out"), (draw, "a")).unwrap();
        graph.connect((draw, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (boundary, draw));
        assert!(reason.contains("render/IO"), "{reason}");
    }

    #[test]
    fn substeps_region_final_output_in_body_rejected() {
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let fin = graph.add_node(Box::new(TestNode::new(
            FINAL_OUTPUT_TYPE_ID,
            vec![input("a", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        graph.connect((boundary, "out"), (fin, "a")).unwrap();
        graph.connect((fin, "out"), (boundary, "in")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (boundary, fin));
        assert!(reason.contains("render/IO"), "{reason}");
    }

    #[test]
    fn substeps_region_coupled_participant_in_body_rejected() {
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let fluid = pass(&mut graph, "fluid");
        let rigid = pass(&mut graph, "rigid");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((boundary, "out"), (fluid, "a")).unwrap();
        graph.connect((fluid, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();
        graph.connect((rigid, "out"), (consumer, "aux")).unwrap();
        let src = source(&mut graph, "src");
        graph.connect((src, "out"), (rigid, "a")).unwrap();
        graph
            .add_coupled_scene(
                fluid,
                rigid,
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            )
            .unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (boundary, fluid));
        assert!(reason.contains("coupled-scene"), "{reason}");
    }

    #[test]
    fn substeps_region_escaping_body_wire_rejected() {
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body_a = pass(&mut graph, "body_a");
        let body_b = pass(&mut graph, "body_b");
        let peek = sink(&mut graph, "peek");
        graph.connect((boundary, "out"), (body_a, "a")).unwrap();
        graph.connect((body_a, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (boundary, "in")).unwrap();
        graph.connect((body_b, "out"), (peek, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (boundary, body_b));
        assert!(reason.contains("escapes"), "{reason}");
    }

    #[test]
    fn substeps_region_overlapping_boundaries_rejected() {
        let mut graph = Graph::new();
        let b1 = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let b2 = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body_a = pass(&mut graph, "body_a");
        let body_b = pass(&mut graph, "body_b");
        graph.connect((b1, "out"), (body_a, "a")).unwrap();
        graph.connect((b2, "out"), (body_a, "b")).unwrap();
        graph.connect((body_a, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (b1, "in")).unwrap();
        graph.connect((body_b, "out"), (b2, "in")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (b2, body_a));
        assert!(reason.contains("two substep regions"), "{reason}");
    }

    #[test]
    fn substeps_region_chaining_rejected() {
        let mut graph = Graph::new();
        let b1 = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body1 = pass(&mut graph, "body1");
        let b2 = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body2 = pass(&mut graph, "body2");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((b1, "out"), (body1, "a")).unwrap();
        graph.connect((body1, "out"), (b1, "in")).unwrap();
        graph.connect((b1, "out"), (b2, "seed")).unwrap();
        graph.connect((b2, "out"), (body2, "a")).unwrap();
        graph.connect((body2, "out"), (b2, "in")).unwrap();
        graph.connect((b2, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (b2, b1));
        assert!(reason.contains("chaining"), "{reason}");
    }

    #[test]
    fn substeps_region_two_independent_regions_contract_separately() {
        let mut graph = Graph::new();
        let b1 = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body1 = pass(&mut graph, "body1");
        let b2 = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body2 = pass(&mut graph, "body2");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((b1, "out"), (body1, "a")).unwrap();
        graph.connect((body1, "out"), (b1, "in")).unwrap();
        graph.connect((b2, "out"), (body2, "a")).unwrap();
        graph.connect((body2, "out"), (b2, "in")).unwrap();
        graph.connect((b1, "out"), (consumer, "tex")).unwrap();
        graph.connect((b2, "out"), (consumer, "aux")).unwrap();

        let plan = compile(&graph).unwrap();
        let regions = plan.substep_regions();
        assert_eq!(regions.len(), 2);
        for (region, (b, body)) in regions.iter().zip([(b1, body1), (b2, body2)]) {
            let nodes: Vec<NodeInstanceId> =
                region.steps.iter().map(|&i| plan.steps()[i].node).collect();
            assert_eq!(nodes, vec![b, body]);
            assert_eq!(region.steps[1], region.steps[0] + 1, "contiguous block");
        }
    }

    #[test]
    fn substeps_region_contraction_orders_outside_readers_after_the_block() {
        // `late` is added before the body and only reads the boundary; plain
        // id order would interleave it. Contraction keeps the block whole.
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let late = sink(&mut graph, "late");
        let body_a = pass(&mut graph, "body_a");
        let body_b = pass(&mut graph, "body_b");
        graph.connect((boundary, "out"), (late, "tex")).unwrap();
        graph.connect((boundary, "out"), (body_a, "a")).unwrap();
        graph.connect((body_a, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (boundary, "in")).unwrap();

        let plan = compile(&graph).unwrap();
        let order: Vec<NodeInstanceId> = plan.steps().iter().map(|s| s.node).collect();
        assert_eq!(order, vec![boundary, body_a, body_b, late]);
        assert_eq!(plan.substep_regions()[0].steps, vec![0, 1, 2]);
    }

    #[test]
    fn substeps_region_absent_when_no_boundaries() {
        let mut graph = Graph::new();
        let a = source(&mut graph, "a");
        let b = pass(&mut graph, "b");
        let c = sink(&mut graph, "c");
        graph.connect((a, "out"), (b, "a")).unwrap();
        graph.connect((b, "out"), (c, "tex")).unwrap();

        let plan = compile(&graph).unwrap();
        assert_eq!(plan.steps().len(), 3);
        assert!(plan.substep_regions().is_empty());
        assert!(plan.late_capture_step_indices().is_empty());
        assert!(plan.held_resources().is_empty());
    }

    #[test]
    fn substeps_region_truncation_keeps_or_drops_region() {
        let hp = happy_path();
        let plan = compile(&hp.graph).unwrap();
        assert_eq!(plan.truncated(5).substep_regions()[0].steps, vec![1, 2, 3]);
        assert_eq!(plan.truncated(4).substep_regions()[0].steps, vec![1, 2, 3]);
        assert!(plan.truncated(3).substep_regions().is_empty());
        assert!(plan.truncated(2).substep_regions().is_empty());
    }
}
