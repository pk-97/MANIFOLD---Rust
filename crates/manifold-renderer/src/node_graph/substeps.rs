//! Bounded substep scheduling — types for the graph executor's repeat
//! regions. Contract: `docs/WATER_SIMULATION_DESIGN.md` section 4.
//!
//! A *substep boundary* node (e.g. `node.water_state`) declares its port
//! names via [`SubstepBoundaryPorts`]. At plan compile time the nodes between
//! its state outputs and its capture producers are contracted into a
//! [`SubstepRegion`]; the executor runs the boundary once, then repeats the
//! region body `step_count` times, capturing the candidate state into the
//! persistent accepted state after each iteration. Everything outside the
//! region — cameras, scene draws, post — runs once per output frame.

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::execution_plan::ResourceId;

/// Port names a substep boundary node declares to the plan compiler and
/// executor. `seed` is the one-shot init source; `capture`/`state` are the
/// primary back-edge pair (candidate in, accepted out); `count`/`delta`/
/// `time`/`index` are the per-iteration scalar outputs the boundary sets
/// before each repeated body run. `results` names additional typed
/// back-edge pairs (collider transform, sticky status) whose final values
/// escape the region alongside `state`.
#[derive(Clone, Copy, Debug)]
pub struct SubstepBoundaryPorts {
    pub seed: &'static str,
    pub capture: &'static str,
    pub state: &'static str,
    pub count: &'static str,
    pub delta: &'static str,
    pub time: &'static str,
    pub index: &'static str,
    pub results: &'static [SubstepResultPorts],
}

/// One typed capture→output back-edge pair beyond the primary state buffer.
#[derive(Clone, Copy, Debug)]
pub struct SubstepResultPorts {
    pub capture: &'static str,
    pub output: &'static str,
}

/// One contracted repeat region in an [`crate::node_graph::execution_plan::ExecutionPlan`].
/// `steps` are indices into `ExecutionPlan.steps` (boundary first, then the
/// body in topological order), fixed at compile time. `held_resources` are
/// the region-internal wires: they are excluded from per-step `free_after`
/// for the whole repeat, so the pool never recycles a slot between
/// iterations.
#[derive(Debug, Clone)]
pub struct SubstepRegion {
    pub boundary: NodeInstanceId,
    pub steps: Vec<usize>,
    pub held_resources: Vec<ResourceId>,
}

/// The host-supplied simulation clock for one output frame. The boundary
/// accumulates `delta * time_scale` in f64 Seconds and consumes integer
/// ticks of its configured `step_hz`. `epoch` changes on seek / project
/// replacement and resets seed, clock and event latches; a duplicate
/// `frame_id` must not advance the clock twice. `advancing` false means
/// render current state with zero steps; `exporting` upgrades an
/// overloaded clock from dropped-ticks to a hard error.
#[derive(Clone, Copy, Debug)]
pub struct SimulationFrame {
    pub frame_id: u64,
    pub delta: manifold_core::Seconds,
    pub epoch: u64,
    pub advancing: bool,
    pub exporting: bool,
}

/// Node-level region derivation result: the boundary plus its body nodes
/// in topological order. `compile` maps these to step indices after the
/// step table is built.
#[derive(Debug)]
pub(crate) struct RegionNodes {
    pub boundary: NodeInstanceId,
    pub body: Vec<NodeInstanceId>,
}

/// Derive and validate substep regions, then contract each region to a
/// contiguous block in `order` (boundary first, body in topological
/// sequence) so region members sort as one vertex for the outer plan.
///
/// Membership: the body is the nodes that are BOTH descendants of the
/// boundary's per-frame outputs AND ancestors of the producers wired into
/// the boundary's capture ports — computed over per-frame wires only
/// (wires into state-capture ports are the back-edges, cut as for
/// feedback). External inputs (seed, camera, controls, collider targets)
/// are ancestors but not descendants, so they stay outside and evaluate
/// once per frame before the region.
///
/// Validation (each failure is a `MalformedSubstepRegion` compile error
/// with NodeIds, never a fallback): every capture port is wired; each
/// capture producer is reachable from the boundary (region connected);
/// no nested boundary, feedback/state-capture node, or render/IO node
/// inside a body; no body wire escapes to an outside reader (only the
/// boundary's final outputs may leave the region); no two regions share
/// a node; and one region's members may not appear in another's external
/// ancestor set (region chaining is deferred, not supported).
pub(crate) fn derive_regions(
    graph: &crate::node_graph::graph::Graph,
    order: &mut Vec<NodeInstanceId>,
) -> Result<Vec<RegionNodes>, crate::node_graph::validation::GraphError> {
    use crate::node_graph::boundary_nodes::FINAL_OUTPUT_TYPE_ID;
    use crate::node_graph::validation::GraphError;

    let boundaries: Vec<(NodeInstanceId, SubstepBoundaryPorts)> = graph
        .nodes()
        .filter_map(|inst| inst.node.substep_boundary().map(|p| (inst.id, p)))
        .collect();
    if boundaries.is_empty() {
        return Ok(Vec::new());
    }

    // Per-frame adjacency: wires whose target port is NOT a declared
    // state-capture port. State-capture wires are back-edges and take no
    // part in membership walks.
    let mut fwd: ahash::AHashMap<NodeInstanceId, Vec<NodeInstanceId>> =
        ahash::AHashMap::default();
    let mut rev: ahash::AHashMap<NodeInstanceId, Vec<NodeInstanceId>> =
        ahash::AHashMap::default();
    for w in graph.wires() {
        let is_capture = graph
            .get_node(w.to.0)
            .is_some_and(|inst| inst.node.state_capture_input_ports().contains(&w.to.1));
        if is_capture {
            continue;
        }
        fwd.entry(w.from.0).or_default().push(w.to.0);
        rev.entry(w.to.0).or_default().push(w.from.0);
    }

    let malformed = |boundary: NodeInstanceId, node: NodeInstanceId, reason: String| {
        GraphError::MalformedSubstepRegion {
            boundary,
            node,
            reason,
        }
    };

    let mut regions: Vec<RegionNodes> = Vec::with_capacity(boundaries.len());
    let mut claimed: ahash::AHashSet<NodeInstanceId> = ahash::AHashSet::default();
    for (boundary, ports) in &boundaries {
        let (boundary, ports) = (*boundary, *ports);

        // Every capture port (primary + results) must be wired; its
        // producer anchors the backward walk.
        let capture_ports: Vec<&'static str> = std::iter::once(ports.capture)
            .chain(ports.results.iter().map(|r| r.capture))
            .collect();
        let mut producers: Vec<NodeInstanceId> = Vec::new();
        for cap in &capture_ports {
            let wire = graph
                .wires()
                .iter()
                .find(|w| w.to.0 == boundary && w.to.1 == *cap);
            let Some(wire) = wire else {
                return Err(malformed(
                    boundary,
                    boundary,
                    format!("capture port `{cap}` has no wire"),
                ));
            };
            producers.push(wire.from.0);
        }

        // Forward from the boundary over per-frame wires.
        let mut forward: ahash::AHashSet<NodeInstanceId> = ahash::AHashSet::default();
        let mut stack = vec![boundary];
        while let Some(n) = stack.pop() {
            if !forward.insert(n) {
                continue;
            }
            if let Some(next) = fwd.get(&n) {
                stack.extend(next.iter().copied());
            }
        }

        // Backward from each capture producer, never expanding the
        // boundary itself.
        let mut backward: ahash::AHashSet<NodeInstanceId> = ahash::AHashSet::default();
        let mut stack = producers.clone();
        while let Some(n) = stack.pop() {
            if n == boundary || !backward.insert(n) {
                continue;
            }
            if let Some(prev) = rev.get(&n) {
                stack.extend(prev.iter().copied());
            }
        }

        // Body in topological sequence (order's relative ordering).
        let mut body: Vec<NodeInstanceId> = order
            .iter()
            .copied()
            .filter(|id| *id != boundary && forward.contains(id) && backward.contains(id))
            .collect();

        for &producer in &producers {
            if !body.contains(&producer) {
                return Err(malformed(
                    boundary,
                    producer,
                    "capture producer is not reachable from the boundary's \
                     state/step outputs — the region is not connected"
                        .to_string(),
                ));
            }
        }

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
            if inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID || inst.node.io_pending() {
                return Err(malformed(
                    boundary,
                    node,
                    "render/IO node inside a substep region".to_string(),
                ));
            }
            if let Some(targets) = fwd.get(&node) {
                for &target in targets {
                    if target != boundary && !body.contains(&target) {
                        return Err(malformed(
                            boundary,
                            node,
                            format!(
                                "region intermediate wire escapes to outside reader {target:?} — \
                                 only the boundary's final outputs may leave the region"
                            ),
                        ));
                    }
                }
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
        // Boundary leads its block.
        body.insert(0, boundary);
        regions.push(RegionNodes {
            boundary,
            body,
        });
    }

    // Ancestors of every region (external inputs: seed, camera, controls),
    // never expanding region members. Reaching another region's member
    // means one region feeds another — deferred, so malformed.
    let region_members: ahash::AHashSet<NodeInstanceId> = regions
        .iter()
        .flat_map(|r| r.body.iter().copied())
        .collect();
    let mut ancestors: ahash::AHashSet<NodeInstanceId> = ahash::AHashSet::default();
    let mut stack: Vec<NodeInstanceId> = region_members.iter().copied().collect();
    while let Some(n) = stack.pop() {
        if let Some(prev) = rev.get(&n) {
            for &p in prev {
                if region_members.contains(&p) {
                    if !regions.iter().any(|r| r.body.contains(&n) && r.body.contains(&p)) {
                        return Err(malformed(
                            p,
                            n,
                            "one substep region feeds another — region chaining is \
                             deferred, not supported"
                                .to_string(),
                        ));
                    }
                    continue;
                }
                if ancestors.insert(p) {
                    stack.push(p);
                }
            }
        }
    }

    // Contract: ancestors first, then each region block in the order its
    // boundary appears, then everything else. Each subsequence keeps the
    // topological relative order, and no edge can point from a later
    // class to an earlier one (an edge into an ancestor makes the source
    // an ancestor; an edge from a region into an ancestor would make the
    // region node both descendant and ancestor, i.e. a body member).
    let mut new_order: Vec<NodeInstanceId> = order
        .iter()
        .copied()
        .filter(|id| ancestors.contains(id))
        .collect();
    for region in &regions {
        new_order.extend(region.body.iter().copied());
    }
    new_order.extend(
        order
            .iter()
            .copied()
            .filter(|id| !ancestors.contains(id) && !region_members.contains(id)),
    );
    *order = new_order;

    Ok(regions)
}

#[cfg(test)]
mod tests {
    //! Compile-level tests for the substep region contract
    //! (`docs/WATER_SIMULATION_DESIGN.md` section 4): membership, the
    //! disallowed shapes, and truncation. All graphs are synthetic — every
    //! node is a `TestNode`, with one fake boundary declaring the WaterState
    //! port names — so no GPU or executor is involved.

    use super::*;
    use crate::node_graph::boundary_nodes::FINAL_OUTPUT_TYPE_ID;
    use crate::node_graph::effect_node::{EffectNodeContext, EffectNodeType};
    use crate::node_graph::execution_plan::compile;
    use crate::node_graph::graph::Graph;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{
        NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };
    use crate::node_graph::validation::GraphError;
    use crate::node_graph::EffectNode;

    /// Port names mirroring how `WaterState` will declare itself
    /// (`docs/WATER_SIMULATION_DESIGN.md` section 4).
    const FAKE_BOUNDARY_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
        seed: "seed",
        capture: "in",
        state: "out",
        count: "step_count",
        delta: "step_dt",
        time: "step_time",
        index: "step_index",
        results: &[],
    };

    /// Synthetic node. `boundary` declares the fake substep boundary ports;
    /// `capture_inputs` mirrors a feedback-style state-capture declaration.
    struct TestNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        boundary: bool,
        capture_inputs: &'static [&'static str],
    }

    impl TestNode {
        fn new(name: &'static str, inputs: Vec<NodeInput>, outputs: Vec<NodeOutput>) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs,
                outputs,
                boundary: false,
                capture_inputs: &[],
            }
        }

        /// The fake substep boundary: `seed`/`in` texture inputs (`in` is
        /// the state-capture port), `out` texture plus the four per-step
        /// scalar outputs.
        fn boundary() -> Self {
            Self {
                boundary: true,
                capture_inputs: &["in"],
                ..Self::new(
                    "test.substep_boundary",
                    vec![
                        input("seed", PortType::Texture2D, false),
                        input("in", PortType::Texture2D, false),
                    ],
                    vec![
                        output("out", PortType::Texture2D),
                        output("step_count", PortType::Scalar(ScalarType::F32)),
                        output("step_dt", PortType::Scalar(ScalarType::F32)),
                        output("step_time", PortType::Scalar(ScalarType::F32)),
                        output("step_index", PortType::Scalar(ScalarType::F32)),
                    ],
                )
            }
        }

        /// Feedback-style node: declares a state-capture input port without
        /// being a substep boundary.
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
            if self.boundary {
                Some(FAKE_BOUNDARY_PORTS)
            } else {
                None
            }
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

    /// The happy-path graph: an external source feeding both the boundary
    /// seed and `body_a`; `body_a → body_b`; `body_b.out` wired back to the
    /// boundary's capture port; the boundary's final outputs read by an
    /// outside consumer:
    ///
    /// ```text
    /// src ─┬─▶ boundary.seed
    ///      └─▶ body_a.tex ─▶ body_b.a ─▶ (capture) boundary.in
    /// boundary.out ─┬─▶ body_a.state
    ///               └─▶ consumer.tex
    /// boundary.step_dt / step_time / step_index ─▶ consumer
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
        let src = graph.add_node(Box::new(TestNode::new(
            "src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let boundary = graph.add_node(Box::new(TestNode::boundary()));
        let body_a = graph.add_node(Box::new(TestNode::new(
            "body_a",
            vec![
                input("tex", PortType::Texture2D, true),
                input("state", PortType::Texture2D, true),
            ],
            vec![output("out", PortType::Texture2D)],
        )));
        let body_b = graph.add_node(Box::new(TestNode::new(
            "body_b",
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, false),
            ],
            vec![output("out", PortType::Texture2D)],
        )));
        let consumer = graph.add_node(Box::new(TestNode::new(
            "consumer",
            vec![
                input("tex", PortType::Texture2D, true),
                input("dt", PortType::Scalar(ScalarType::F32), false),
                input("t", PortType::Scalar(ScalarType::F32), false),
                input("i", PortType::Scalar(ScalarType::F32), false),
            ],
            vec![],
        )));
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((src, "out"), (body_a, "tex")).unwrap();
        graph.connect((boundary, "out"), (body_a, "state")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();
        graph.connect((boundary, "step_dt"), (consumer, "dt")).unwrap();
        graph.connect((boundary, "step_time"), (consumer, "t")).unwrap();
        graph.connect((boundary, "step_index"), (consumer, "i")).unwrap();
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

    /// Unpack the expected compile error, panicking with the actual error
    /// on any other failure mode.
    fn malformed(err: GraphError) -> (NodeInstanceId, NodeInstanceId, String) {
        match err {
            GraphError::MalformedSubstepRegion {
                boundary,
                node,
                reason,
            } => (boundary, node, reason),
            other => panic!("expected MalformedSubstepRegion, got {other:?}"),
        }
    }

    #[test]
    fn substeps_region_happy_path_contract() {
        let hp = happy_path();
        let plan = compile(&hp.graph).unwrap();
        let steps = plan.steps();
        assert_eq!(steps.len(), 5);

        // External source's step first, then the contracted region block
        // (boundary first, body in topological order), then the outside
        // consumer.
        assert_eq!(steps[0].node, hp.src);
        assert_eq!(steps[1].node, hp.boundary);
        assert_eq!(steps[2].node, hp.body_a);
        assert_eq!(steps[3].node, hp.body_b);
        assert_eq!(steps[4].node, hp.consumer);

        let r_body_a = steps[2].outputs[0].1;
        let r_body_b = steps[3].outputs[0].1;

        // Exactly one region; its steps are contiguous, boundary first.
        let regions = plan.substep_regions();
        assert_eq!(regions.len(), 1);
        let region = &regions[0];
        assert_eq!(region.boundary, hp.boundary);
        assert_eq!(region.steps, vec![1, 2, 3]);

        // Held: EVERY wire whose last reader is a region step (excluding
        // persistent wires) — see the held-resource computation in
        // `execution_plan::compile`. That includes src.out: its last
        // reader is the boundary step, which never runs in the ordinary
        // pass, so a free_after attached there would never fire. The
        // region path holds the slot for the repeat and releases it at
        // region end. body_a.out is read by body_b (a region step), so
        // it is held too. The boundary's final outputs escape to the
        // outside consumer — their last reader is an outside step, so
        // they keep ordinary lifetimes and are NOT held.
        let r_src = steps[0].outputs[0].1;
        let mut expected_held = vec![r_src, r_body_a];
        expected_held.sort();
        assert_eq!(region.held_resources, expected_held);

        // Pass 2 marks state-capture input resources persistent, so the
        // capture producer's wire (body_b.out) survives across frames as
        // well as across iterations. Persistent wires are excluded from
        // the region's held list — their slots are pre-acquired and
        // never released.
        assert_eq!(plan.persistent_resources(), &[r_body_b]);

        // No region resource (held or persistent) appears in any step's
        // free_after — the pool must not recycle a slot mid-repeat.
        let region_resources: [ResourceId; 3] = [r_src, r_body_a, r_body_b];
        for step in steps {
            for res in region_resources {
                assert!(!step.free_after.contains(&res));
            }
        }

        // The boundary declares state-capture ports but is excluded from
        // the frame-end late capture pass — its capture runs once per
        // iteration instead.
        assert!(plan.late_capture_step_indices().is_empty());
    }

    #[test]
    fn substeps_region_unwired_capture_port_rejected() {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(TestNode::new(
            "src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let boundary = graph.add_node(Box::new(TestNode::boundary()));
        let consumer = graph.add_node(Box::new(TestNode::new(
            "consumer",
            vec![input("tex", PortType::Texture2D, true)],
            vec![],
        )));
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed(compile(&graph).unwrap_err());
        assert_eq!(b, boundary);
        assert_eq!(node, boundary);
        assert!(reason.contains("capture port"), "reason: {reason}");
    }

    #[test]
    fn substeps_region_disconnected_capture_producer_rejected() {
        // The capture producer is an unrelated chain — nothing reaches it
        // from the boundary's outputs, so there is no connected region.
        let mut graph = Graph::new();
        let boundary = graph.add_node(Box::new(TestNode::boundary()));
        let consumer = graph.add_node(Box::new(TestNode::new(
            "consumer",
            vec![input("tex", PortType::Texture2D, true)],
            vec![],
        )));
        let src2 = graph.add_node(Box::new(TestNode::new(
            "src2",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let rogue = graph.add_node(Box::new(TestNode::new(
            "rogue",
            vec![input("tex", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        graph.connect((boundary, "out"), (consumer, "tex")).unwrap();
        graph.connect((src2, "out"), (rogue, "tex")).unwrap();
        graph.connect((rogue, "out"), (boundary, "in")).unwrap();

        let (b, node, reason) = malformed(compile(&graph).unwrap_err());
        assert_eq!(b, boundary);
        assert_eq!(node, rogue);
        assert!(reason.contains("not reachable"), "reason: {reason}");
    }

    #[test]
    fn substeps_region_nested_boundary_rejected() {
        // A second boundary node inside the first region's body. The inner
        // boundary has its own well-formed body (inner_body feeds its
        // capture), so both boundary processing orders reach the nested
        // rule on the outer boundary — `Graph::nodes` iterates a hash map,
        // so the order is not under test control.
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(TestNode::new(
            "src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let outer = graph.add_node(Box::new(TestNode::boundary()));
        let body_a = graph.add_node(Box::new(TestNode::new(
            "body_a",
            vec![input("tex", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let inner = graph.add_node(Box::new(TestNode::boundary()));
        let inner_body = graph.add_node(Box::new(TestNode::new(
            "inner_body",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let body_b = graph.add_node(Box::new(TestNode::new(
            "body_b",
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, false),
            ],
            vec![output("out", PortType::Texture2D)],
        )));
        graph.connect((src, "out"), (outer, "seed")).unwrap();
        graph.connect((outer, "out"), (body_a, "tex")).unwrap();
        graph.connect((body_a, "out"), (body_b, "a")).unwrap();
        graph.connect((body_a, "out"), (inner, "seed")).unwrap();
        graph.connect((inner, "out"), (body_b, "b")).unwrap();
        graph.connect((inner, "out"), (inner_body, "in")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((body_b, "out"), (outer, "in")).unwrap();

        let (b, node, reason) = malformed(compile(&graph).unwrap_err());
        assert_eq!(b, outer);
        assert_eq!(node, inner);
        assert!(reason.contains("nested"), "reason: {reason}");
    }

    #[test]
    fn substeps_region_feedback_node_in_body_rejected() {
        // A feedback-style node (state-capture inputs, not a boundary)
        // inside the region body.
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(TestNode::new(
            "src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let boundary = graph.add_node(Box::new(TestNode::boundary()));
        let body_a = graph.add_node(Box::new(TestNode::new(
            "body_a",
            vec![input("tex", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let fb = graph.add_node(Box::new(TestNode::feedback_style()));
        let body_b = graph.add_node(Box::new(TestNode::new(
            "body_b",
            vec![input("a", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (body_a, "tex")).unwrap();
        graph.connect((body_a, "out"), (fb, "tex")).unwrap();
        graph.connect((body_a, "out"), (fb, "prev")).unwrap();
        graph.connect((fb, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (boundary, "in")).unwrap();

        let (b, node, reason) = malformed(compile(&graph).unwrap_err());
        assert_eq!(b, boundary);
        assert_eq!(node, fb);
        assert!(reason.contains("state-capture"), "reason: {reason}");
    }

    #[test]
    fn substeps_region_render_output_in_body_rejected() {
        // The final-output node inside the region body. Its type id makes
        // it the graph's liveness root, so every node here is live.
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(TestNode::new(
            "src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let boundary = graph.add_node(Box::new(TestNode::boundary()));
        let body_a = graph.add_node(Box::new(TestNode::new(
            "body_a",
            vec![input("tex", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let fin = graph.add_node(Box::new(TestNode::new(
            "system.final_output",
            vec![input("tex", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let body_b = graph.add_node(Box::new(TestNode::new(
            "body_b",
            vec![input("a", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (body_a, "tex")).unwrap();
        graph.connect((body_a, "out"), (fin, "tex")).unwrap();
        graph.connect((fin, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (boundary, "in")).unwrap();

        let (b, node, reason) = malformed(compile(&graph).unwrap_err());
        assert_eq!(b, boundary);
        assert_eq!(node, fin);
        assert!(reason.contains("render/IO"), "reason: {reason}");
    }

    #[test]
    fn substeps_region_escaping_body_wire_rejected() {
        // body_b feeds the boundary's capture AND an outside reader — only
        // the boundary's final outputs may leave the region.
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(TestNode::new(
            "src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let boundary = graph.add_node(Box::new(TestNode::boundary()));
        let body_a = graph.add_node(Box::new(TestNode::new(
            "body_a",
            vec![input("tex", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let body_b = graph.add_node(Box::new(TestNode::new(
            "body_b",
            vec![input("a", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let peek = graph.add_node(Box::new(TestNode::new(
            "peek",
            vec![input("tex", PortType::Texture2D, true)],
            vec![],
        )));
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (body_a, "tex")).unwrap();
        graph.connect((body_a, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (boundary, "in")).unwrap();
        graph.connect((body_b, "out"), (peek, "tex")).unwrap();

        let (b, node, reason) = malformed(compile(&graph).unwrap_err());
        assert_eq!(b, boundary);
        assert_eq!(node, body_b);
        assert!(reason.contains("escapes"), "reason: {reason}");
    }

    #[test]
    fn substeps_region_overlapping_boundaries_rejected() {
        // Two boundaries whose descendant/ancestor walks both claim
        // body_a and body_b.
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(TestNode::new(
            "src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b1 = graph.add_node(Box::new(TestNode::boundary()));
        let b2 = graph.add_node(Box::new(TestNode::boundary()));
        let body_a = graph.add_node(Box::new(TestNode::new(
            "body_a",
            vec![
                input("tex", PortType::Texture2D, true),
                input("state", PortType::Texture2D, true),
            ],
            vec![output("out", PortType::Texture2D)],
        )));
        let body_b = graph.add_node(Box::new(TestNode::new(
            "body_b",
            vec![input("a", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        graph.connect((src, "out"), (b1, "seed")).unwrap();
        graph.connect((src, "out"), (b2, "seed")).unwrap();
        graph.connect((b1, "out"), (body_a, "tex")).unwrap();
        graph.connect((b2, "out"), (body_a, "state")).unwrap();
        graph.connect((body_a, "out"), (body_b, "a")).unwrap();
        graph.connect((body_b, "out"), (b1, "in")).unwrap();
        graph.connect((body_b, "out"), (b2, "in")).unwrap();

        let (b, node, reason) = malformed(compile(&graph).unwrap_err());
        // `Graph::nodes` iterates a hash map, so the boundary whose walk
        // trips the claim is order-dependent — either boundary is a
        // legitimate reporter.
        assert!(b == b1 || b == b2, "boundary: {b:?}");
        assert!(node == body_a || node == body_b, "node: {node:?}");
        assert!(
            reason.contains("two substep regions"),
            "reason: {reason}"
        );
    }

    #[test]
    fn substeps_region_absent_when_no_boundaries() {
        // Ordinary chain: no boundary → no regions, plan shape unchanged.
        let mut graph = Graph::new();
        let a = graph.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = graph.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let c = graph.add_node(Box::new(TestNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        graph.connect((a, "out"), (b, "in")).unwrap();
        graph.connect((b, "out"), (c, "in")).unwrap();

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
        assert_eq!(plan.steps().len(), 5);
        assert_eq!(plan.substep_regions().len(), 1);

        // Full prefix: the region survives whole.
        let full = plan.truncated(5);
        assert_eq!(full.steps().len(), 5);
        assert_eq!(full.substep_regions().len(), 1);
        assert_eq!(full.substep_regions()[0].steps, vec![1, 2, 3]);

        // Prefix covering the whole region (region steps are 1..=3).
        let covered = plan.truncated(4);
        assert_eq!(covered.steps().len(), 4);
        assert_eq!(covered.substep_regions().len(), 1);
        assert_eq!(covered.substep_regions()[0].steps, vec![1, 2, 3]);

        // A prefix cutting the body drops the region — truncated mid-body
        // steps have no repeat semantics and revert to ordinary linear
        // steps.
        let cut = plan.truncated(3);
        assert_eq!(cut.steps().len(), 3);
        assert!(cut.substep_regions().is_empty());

        let cut_at_body = plan.truncated(2);
        assert_eq!(cut_at_body.steps().len(), 2);
        assert!(cut_at_body.substep_regions().is_empty());
    }
}
