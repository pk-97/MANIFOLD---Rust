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
/// every wire whose LAST READER is a region step (persistent wires
/// excluded): they are excluded from per-step `free_after`, which would
/// never fire on steps that only run in the region path, and the executor
/// holds them for the whole repeat so the pool never recycles a slot
/// between iterations. This covers external inputs consumed by the region
/// as well as region-internal wires.
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

    // Sorted by NodeInstanceId: `graph.nodes()` is an AHashMap, so an
    // unsorted walk would make region order (and therefore the contracted
    // step order and ResourceIds of a multi-region graph) per-process
    // random.
    let mut boundaries: Vec<(NodeInstanceId, SubstepBoundaryPorts)> = graph
        .nodes()
        .filter_map(|inst| inst.node.substep_boundary().map(|p| (inst.id, p)))
        .collect();
    boundaries.sort_by_key(|(id, _)| id.0);
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
    //!
    //! The `Executor`-level synthetic proofs at the bottom of this module
    //! (`substeps_*` tests driving `Executor::with_mock`) prove the runtime
    //! half of the section 4 contract: order, count, duplicate-frame,
    //! per-iteration scalar slots, final-state visibility, zero steps,
    //! post-region liveness, pause/overload clock rules, resource
    //! hold/release, epoch reset, and the missing-`SimulationFrame` path.

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
        // legitimate reporter. The claimed node is deterministic: the
        // second walk's body scan is in topological order, so body_a
        // always trips first.
        assert!(b == b1 || b == b2, "boundary: {b:?}");
        assert_eq!(node, body_a);
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

    // ─── Executor-level synthetic proofs ───
    //
    // These drive the real `Executor` + `MockBackend` over a compiled plan
    // with one substep region. MockBackend stores scalars observably
    // (textures and arrays resolve to `None`), so the fixture models state
    // as scalar F32 values moving through slots:
    //
    // ```text
    // seed_src ──▶ boundary.seed
    // boundary.out ──▶ increment.in ──▶ increment.out ──▶ (capture) boundary.in
    // boundary.step_count/dt/time/index ──▶ increment
    // boundary.out ──▶ consumer.in   (outside reader of the accepted state)
    // ```
    //
    // The fixture boundary mirrors `node.feedback`'s shape: `in` is the
    // state-capture port, `out` is a persistent output (the accepted-state
    // buffer outside consumers read), `evaluate` resolves the tick clock
    // from the context's `SimulationFrame` and exposes the accepted state,
    // `substep_iteration` serves the per-tick scalars, and `late_capture`
    // accepts the captured candidate into the accepted state (feedback's
    // `late_capture` state move, with a scalar instead of a texture).

    use std::sync::{Arc, Mutex};

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::bindings::{NodeInputs, Slot};
    use crate::node_graph::effect_node::FrameTime;
    use crate::node_graph::execution::Executor;
    use crate::node_graph::execution_plan::ExecutionPlan;
    use crate::node_graph::parameters::ParamValue;

    /// Fixture clock rate: h = 1/32 s = 0.03125 s is exact in binary, so
    /// the fixture's tick math has no rounding hazard (0.09375 s is exactly
    /// 3 ticks, 0.109375 s is 3.5, 0.5 s is exactly 16).
    const FIXTURE_STEP_HZ: f64 = 32.0;
    const FIXTURE_H: f32 = 0.03125;

    /// Config + clock + accepted state + observation records for
    /// [`SimBoundary`]. Interior mutability because the graph owns the
    /// node — the same pattern as `execution.rs`'s `RecordingNode`.
    #[derive(Debug, Default)]
    struct BoundaryShared {
        /// Install-time constants (WaterState's `step_hz` / `max_substeps`)
        /// plus the bounded `time_scale`.
        step_hz: f64,
        max_substeps: u32,
        time_scale: f64,
        /// f64 accumulation clock: accumulate `delta * time_scale`, consume
        /// integer ticks of `1/step_hz`, keep the fractional remainder.
        accumulator: f64,
        sim_time: f64,
        frame_tick_base: f64,
        last_frame_id: Option<u64>,
        epoch: u64,
        /// Accepted state and the one-shot seed arm.
        accepted: f32,
        seeded: bool,
        /// Ticks resolved by this frame's `evaluate`, served by
        /// `substep_iteration`.
        ticks_pending: u32,
        /// Observation records.
        evals: u32,
        eval_out_writes: u32,
        iteration_calls: u32,
        capture_out_writes: u32,
        candidates: Vec<f32>,
        dropped_ticks: u32,
        missing_frame_errors: u32,
    }

    impl BoundaryShared {
        fn new() -> Self {
            Self {
                step_hz: FIXTURE_STEP_HZ,
                max_substeps: 8,
                time_scale: 1.0,
                ..Self::default()
            }
        }
    }

    /// Synthetic substep boundary with scalar state. The clock implements
    /// the section 4 rules: f64 accumulation, integer ticks, duplicate
    /// `frame_id` does not advance twice, epoch change re-arms the seed
    /// path and resets the clock, `advancing == false` or `time_scale == 0`
    /// yields zero ticks, and an overloaded clock drops WHOLE ticks at
    /// `max_substeps` rather than enlarging dt.
    struct SimBoundary {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        shared: Arc<Mutex<BoundaryShared>>,
    }

    impl SimBoundary {
        fn new(shared: Arc<Mutex<BoundaryShared>>) -> Self {
            Self {
                type_id: EffectNodeType::new("test.sim_boundary"),
                inputs: vec![
                    input("seed", PortType::Scalar(ScalarType::F32), true),
                    input("in", PortType::Scalar(ScalarType::F32), true),
                ],
                outputs: vec![
                    output("out", PortType::Scalar(ScalarType::F32)),
                    output("step_count", PortType::Scalar(ScalarType::F32)),
                    output("step_dt", PortType::Scalar(ScalarType::F32)),
                    output("step_time", PortType::Scalar(ScalarType::F32)),
                    output("step_index", PortType::Scalar(ScalarType::F32)),
                ],
                shared,
            }
        }

        /// Write the accepted state onto the persistent `out` slot.
        fn emit_out(ctx: &mut EffectNodeContext<'_, '_>, accepted: f32, writes: &mut u32) {
            *writes += 1;
            ctx.outputs.set_scalar("out", ParamValue::Float(accepted));
        }
    }

    impl EffectNode for SimBoundary {
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
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let mut s = self.shared.lock().unwrap();
            s.evals += 1;
            if let Some(frame) = ctx.simulation_frame
                && frame.epoch != s.epoch
            {
                s.epoch = frame.epoch;
                s.accumulator = 0.0;
                s.sim_time = 0.0;
                s.frame_tick_base = 0.0;
                s.seeded = false;
                s.dropped_ticks = 0;
                s.last_frame_id = None;
            }
            if !s.seeded {
                s.seeded = true;
                s.accepted = match ctx.inputs.scalar("seed") {
                    Some(ParamValue::Float(v)) => v,
                    _ => 0.0,
                };
            }
            let Some(frame) = ctx.simulation_frame else {
                // Host integration error (the executor reports it too and
                // runs the region zero times). The boundary must not panic
                // and still exposes its accepted state.
                s.missing_frame_errors += 1;
                ctx.error("SimBoundary: no SimulationFrame installed");
                s.ticks_pending = 0;
                Self::emit_out(ctx, s.accepted, &mut s.eval_out_writes);
                return;
            };
            if s.last_frame_id == Some(frame.frame_id) {
                // Duplicate frame: render the accepted state again, advance
                // nothing.
                s.ticks_pending = 0;
                Self::emit_out(ctx, s.accepted, &mut s.eval_out_writes);
                return;
            }
            s.last_frame_id = Some(frame.frame_id);
            if !frame.advancing || s.time_scale <= 0.0 {
                s.ticks_pending = 0;
                Self::emit_out(ctx, s.accepted, &mut s.eval_out_writes);
                return;
            }
            s.accumulator += frame.delta.0 * s.time_scale;
            let h = 1.0 / s.step_hz;
            let mut whole = (s.accumulator / h).floor();
            s.accumulator -= whole * h;
            let max = f64::from(s.max_substeps);
            if whole > max {
                s.dropped_ticks += (whole - max) as u32;
                whole = max;
            }
            s.frame_tick_base = s.sim_time;
            s.sim_time += whole * h;
            s.ticks_pending = whole as u32;
            // The count slot is the boundary's own declaration — the
            // executor writes only dt/time/index per iteration.
            ctx.outputs
                .set_scalar("step_count", ParamValue::Float(s.ticks_pending as f32));
            Self::emit_out(ctx, s.accepted, &mut s.eval_out_writes);
        }
        fn substep_iteration(&mut self, iteration: u32) -> Option<[f32; 3]> {
            let mut s = self.shared.lock().unwrap();
            s.iteration_calls += 1;
            if iteration >= s.ticks_pending {
                return None;
            }
            let h = 1.0 / s.step_hz;
            Some([
                h as f32,
                (s.frame_tick_base + (f64::from(iteration) + 1.0) * h) as f32,
                iteration as f32,
            ])
        }
        fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            // Per-iteration accept: the candidate the body produced this
            // iteration sits in the persistent `in` (capture) slot —
            // feedback's `late_capture` is the same shape with textures.
            let mut s = self.shared.lock().unwrap();
            let candidate = match ctx.inputs.scalar("in") {
                Some(ParamValue::Float(v)) => v,
                _ => 0.0,
            };
            s.candidates.push(candidate);
            s.accepted = candidate;
            // Land the accepted state in the persistent `out` slot so
            // outside consumers read the FINAL state this frame.
            s.capture_out_writes += 1;
            ctx.outputs
                .set_scalar("out", ParamValue::Float(candidate));
        }
        fn state_capture_input_ports(&self) -> &'static [&'static str] {
            &["in"]
        }
        fn persistent_output_ports(&self) -> &[&str] {
            // `out` IS the persistent accepted buffer — the emit half the
            // outside consumer reads, mirroring feedback's persistent `out`.
            &["out"]
        }
        fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
            Some(FAKE_BOUNDARY_PORTS)
        }
    }

    /// Scalar read helper shared by the fixture nodes.
    fn scalar_f32(inputs: &NodeInputs<'_>, port: &str) -> Option<f32> {
        match inputs.scalar(port) {
            Some(ParamValue::Float(v)) => Some(v),
            _ => None,
        }
    }

    /// One region-body observation.
    #[derive(Debug)]
    struct IncrementRecord {
        in_slot: Slot,
        out_slot: Slot,
        step_dt_slot: Slot,
        value_in: Option<f32>,
        step_count: Option<f32>,
        step_dt: Option<f32>,
        step_time: Option<f32>,
        step_index: Option<f32>,
    }

    /// Region body: `out = in + 1`, recording everything it saw. The
    /// evaluate count proves the iteration count; the recorded
    /// `step_index` sequence proves order; the recorded scalars prove the
    /// boundary's per-step outputs were live in the slots during the body
    /// run; `value_in` across iterations proves whether the region held
    /// state between iterations (accumulation) or recycled it.
    struct IncrementNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        log: Arc<Mutex<Vec<IncrementRecord>>>,
    }

    impl IncrementNode {
        fn new(log: Arc<Mutex<Vec<IncrementRecord>>>) -> Self {
            Self {
                type_id: EffectNodeType::new("test.substep_increment"),
                inputs: vec![
                    input("in", PortType::Scalar(ScalarType::F32), true),
                    input("step_count", PortType::Scalar(ScalarType::F32), false),
                    input("step_dt", PortType::Scalar(ScalarType::F32), false),
                    input("step_time", PortType::Scalar(ScalarType::F32), false),
                    input("step_index", PortType::Scalar(ScalarType::F32), false),
                ],
                outputs: vec![output("out", PortType::Scalar(ScalarType::F32))],
                log,
            }
        }
    }

    impl EffectNode for IncrementNode {
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
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let record = IncrementRecord {
                in_slot: ctx.inputs.slot("in").expect("in is wired"),
                out_slot: ctx.outputs.slot("out").expect("out is consumed"),
                step_dt_slot: ctx
                    .inputs
                    .slot("step_dt")
                    .expect("step_dt is wired"),
                value_in: scalar_f32(&ctx.inputs, "in"),
                step_count: scalar_f32(&ctx.inputs, "step_count"),
                step_dt: scalar_f32(&ctx.inputs, "step_dt"),
                step_time: scalar_f32(&ctx.inputs, "step_time"),
                step_index: scalar_f32(&ctx.inputs, "step_index"),
            };
            if let Some(v) = record.value_in {
                ctx.outputs.set_scalar("out", ParamValue::Float(v + 1.0));
            }
            self.log.lock().unwrap().push(record);
        }
    }

    /// Seed source: emits a configurable constant scalar so tests can
    /// change the seed between frames (the epoch-reset proof).
    struct ConstScalarNode {
        type_id: EffectNodeType,
        value: Arc<Mutex<f32>>,
    }

    impl EffectNode for ConstScalarNode {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &[]
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodeOutput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("out"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let v = *self.value.lock().unwrap();
            ctx.outputs.set_scalar("out", ParamValue::Float(v));
        }
    }

    /// Outside consumer of `boundary.out`: records the value it read and
    /// how many times it ran. Its texture output feeds a `FinalOutput` so
    /// the live-set keeps the post-region chain (a pure sink with no
    /// downstream root would be pruned like any dead mux branch).
    struct ConsumerNode {
        type_id: EffectNodeType,
        log: Arc<Mutex<Vec<Option<f32>>>>,
    }

    impl EffectNode for ConsumerNode {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            }];
            &INPUTS
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodeOutput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("out"),
                ty: PortType::Texture2D,
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            self.log.lock().unwrap().push(scalar_f32(&ctx.inputs, "in"));
        }
    }

    /// The compiled single-region graph plus every shared handle the
    /// tests need to drive frames and observe the fixture nodes.
    struct ExecGraph {
        graph: Graph,
        plan: ExecutionPlan,
        boundary_shared: Arc<Mutex<BoundaryShared>>,
        increment_log: Arc<Mutex<Vec<IncrementRecord>>>,
        consumer_log: Arc<Mutex<Vec<Option<f32>>>>,
        seed_value: Arc<Mutex<f32>>,
    }

    fn build_exec_graph(seed: f32) -> ExecGraph {
        let boundary_shared = Arc::new(Mutex::new(BoundaryShared::new()));
        let increment_log = Arc::new(Mutex::new(Vec::new()));
        let consumer_log = Arc::new(Mutex::new(Vec::new()));
        let seed_value = Arc::new(Mutex::new(seed));

        let mut graph = Graph::new();
        let seed_src = graph.add_node(Box::new(ConstScalarNode {
            type_id: EffectNodeType::new("test.const_scalar"),
            value: seed_value.clone(),
        }));
        let boundary = graph.add_node(Box::new(SimBoundary::new(boundary_shared.clone())));
        let increment = graph.add_node(Box::new(IncrementNode::new(increment_log.clone())));
        let consumer = graph.add_node(Box::new(ConsumerNode {
            type_id: EffectNodeType::new("test.substep_consumer"),
            log: consumer_log.clone(),
        }));
        let final_output = graph.add_node(Box::new(crate::node_graph::FinalOutput::new()));

        graph.connect((seed_src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (increment, "in")).unwrap();
        graph.connect((increment, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "in")).unwrap();
        graph.connect((consumer, "out"), (final_output, "in"))
            .unwrap();
        for port in ["step_count", "step_dt", "step_time", "step_index"] {
            graph.connect((boundary, port), (increment, port)).unwrap();
        }

        let plan = compile(&graph).unwrap();
        ExecGraph {
            graph,
            plan,
            boundary_shared,
            increment_log,
            consumer_log,
            seed_value,
        }
    }

    fn exec_frame_time(frame_count: u64) -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: frame_count as i64,
        }
    }

    fn sim_frame(frame_id: u64, delta_s: f64, epoch: u64, advancing: bool) -> SimulationFrame {
        SimulationFrame {
            frame_id,
            delta: Seconds(delta_s),
            epoch,
            advancing,
            exporting: false,
        }
    }

    #[test]
    fn substeps_count_order_and_duplicate_frame() {
        let mut fx = build_exec_graph(0.0);
        let mut exec = Executor::with_mock();
        // 0.09375 s at 32 Hz = exactly 3 ticks.
        exec.set_simulation_frame(sim_frame(1, 0.09375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(0));

        let log = fx.increment_log.lock().unwrap();
        assert_eq!(log.len(), 3, "3 ticks ⇒ the body runs exactly 3 times");
        assert_eq!(
            log.iter().map(|r| r.step_index).collect::<Vec<_>>(),
            vec![Some(0.0), Some(1.0), Some(2.0)],
            "the body must see step_index 0,1,2 in order"
        );
        assert!(
            log.iter().all(|r| r.step_dt == Some(FIXTURE_H)),
            "step_dt must hold the boundary-declared tick dt during every body run"
        );
        assert_eq!(
            log.iter().map(|r| r.step_time).collect::<Vec<_>>(),
            vec![Some(0.03125), Some(0.0625), Some(0.09375)],
            "step_time must advance one tick per iteration"
        );
        assert!(
            log.iter().all(|r| r.step_count == Some(3.0)),
            "step_count must hold the boundary-declared total during the body runs"
        );
        drop(log);

        // Re-executing the SAME frame_id renders again but must not
        // advance the clock: the body stays at 3 runs.
        exec.set_simulation_frame(sim_frame(1, 0.09375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(1));
        assert_eq!(
            fx.increment_log.lock().unwrap().len(),
            3,
            "duplicate frame_id must not advance the simulation again"
        );

        // A NEW frame_id advances: 3 more runs.
        exec.set_simulation_frame(sim_frame(2, 0.09375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(2));
        assert_eq!(
            fx.increment_log.lock().unwrap().len(),
            6,
            "a new frame_id runs 3 more iterations"
        );
    }

    #[test]
    fn substeps_final_state_and_zero_steps() {
        let mut fx = build_exec_graph(0.0);
        let mut exec = Executor::with_mock();

        // 3 ticks over a seed of 0: accepted state after the frame is 3.
        exec.set_simulation_frame(sim_frame(1, 0.09375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(0));

        // Contract (section 4): "outside consumers read the FINAL accepted
        // state, not the frame-start state."
        let reads = fx.consumer_log.lock().unwrap();
        assert_eq!(
            reads.len(),
            1,
            "the outside consumer runs once and reads boundary.out"
        );
        assert_eq!(
            reads[0], Some(3.0),
            "outside consumer must read the FINAL accepted state (seed 0 + \
             3 increments = 3.0), not the frame-start state — the \
             per-iteration accept must land in the persistent out slot \
             before the consumer reads it"
        );
        drop(reads);

        // Boundary-side diagnostics of the same contract.
        let shared = fx.boundary_shared.lock().unwrap();
        assert_eq!(
            shared.candidates,
            vec![1.0, 2.0, 3.0],
            "per-iteration capture must see the candidate ACCUMULATE: the \
             body reads the accepted state the previous iteration's capture \
             landed in the out slot"
        );
        assert_eq!(shared.accepted, 3.0, "accepted state after 3 increments");
        assert_eq!(
            shared.capture_out_writes, 3,
            "the boundary attempts the accept-write once per iteration"
        );
        drop(shared);

        // Zero-tick frame: the body runs zero more times and `out` exposes
        // the PRIOR accepted state.
        exec.set_simulation_frame(sim_frame(2, 0.09375, 0, false));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(1));
        assert_eq!(
            fx.increment_log.lock().unwrap().len(),
            3,
            "paused frame runs the body zero times"
        );
        let reads = fx.consumer_log.lock().unwrap();
        assert_eq!(reads.len(), 2);
        assert_eq!(
            reads[1], Some(3.0),
            "a zero-tick frame exposes the prior accepted state, not the seed"
        );
        drop(reads);
    }

    #[test]
    fn substeps_execute_post_once() {
        let mut fx = build_exec_graph(0.0);
        let mut exec = Executor::with_mock();
        for frame in 1..=3u64 {
            exec.set_simulation_frame(sim_frame(frame, 0.09375, 0, true));
            exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(frame));
            assert_eq!(
                fx.consumer_log.lock().unwrap().len(),
                frame as usize,
                "the post-region consumer executes exactly once per output frame"
            );
        }
        // 9 iterations across 3 frames, but the consumer ran 3 times.
        assert_eq!(
            fx.increment_log.lock().unwrap().len(),
            9,
            "3 frames × 3 ticks = 9 body runs"
        );
        assert_eq!(
            fx.consumer_log.lock().unwrap().len(),
            3,
            "iteration count must not leak into the once-per-frame pass"
        );
    }

    #[test]
    fn substeps_pause_gap_overload() {
        let mut fx = build_exec_graph(0.0);
        let mut exec = Executor::with_mock();

        // advancing=false: zero iterations.
        exec.set_simulation_frame(sim_frame(1, 0.09375, 0, false));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(0));
        assert!(
            fx.increment_log.lock().unwrap().is_empty(),
            "advancing=false freezes the clock: zero iterations"
        );

        // time_scale=0 driven through the fixture boundary: zero iterations.
        fx.boundary_shared.lock().unwrap().time_scale = 0.0;
        exec.set_simulation_frame(sim_frame(2, 0.09375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(1));
        assert!(
            fx.increment_log.lock().unwrap().is_empty(),
            "time_scale=0 freezes the clock: zero iterations"
        );
        fx.boundary_shared.lock().unwrap().time_scale = 1.0;

        // Overload: 0.5 s at 32 Hz = 16 whole ticks; max_substeps=4 drops
        // the excess 12 WHOLE ticks — the body never runs more than
        // max_substeps times and dt is never enlarged.
        fx.boundary_shared.lock().unwrap().max_substeps = 4;
        exec.set_simulation_frame(sim_frame(3, 0.5, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(2));
        let log = fx.increment_log.lock().unwrap();
        assert_eq!(
            log.len(),
            4,
            "overloaded clock drops whole ticks at the cap — the body must \
             never run more than max_substeps times in one frame"
        );
        assert!(
            log.iter().all(|r| r.step_dt == Some(FIXTURE_H)),
            "dropped ticks must never enlarge dt"
        );
        drop(log);
        assert_eq!(
            fx.boundary_shared.lock().unwrap().dropped_ticks, 12,
            "0.5 s at 32 Hz = 16 ticks, capped at 4 ⇒ 12 dropped"
        );

        // The dropped ticks leave no backlog: the next normal frame is a
        // plain 3-tick frame.
        exec.set_simulation_frame(sim_frame(4, 0.09375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(3));
        assert_eq!(
            fx.increment_log.lock().unwrap().len(),
            7,
            "no dropped-tick backlog may carry into the next frame"
        );
    }

    #[test]
    fn substeps_no_recycle_between_iterations() {
        let mut fx = build_exec_graph(0.0);
        let mut exec = Executor::with_mock();
        exec.set_simulation_frame(sim_frame(1, 0.09375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(0));
        let slots_after_frame_1 = exec.backend().slot_count();
        exec.set_simulation_frame(sim_frame(2, 0.09375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(1));

        let log = fx.increment_log.lock().unwrap();
        // Held resources keep their physical slots across ALL iterations.
        // (Slot identity alone is a necessary condition, not a sufficient
        // one — MockBackend's free list would hand back the same slot —
        // the value assertion below is the real proof.)
        let first = &log[0];
        assert!(
            log.iter()
                .all(|r| r.in_slot == first.in_slot
                    && r.out_slot == first.out_slot
                    && r.step_dt_slot == first.step_dt_slot),
            "held region resources must keep their physical slots across iterations"
        );
        // NOT recycled between iterations: iteration k reads the state
        // accepted after iteration k-1 — accumulation across 2 frames ×
        // 3 ticks.
        assert_eq!(
            log.iter().map(|r| r.value_in).collect::<Vec<_>>(),
            vec![
                Some(0.0),
                Some(1.0),
                Some(2.0),
                Some(3.0),
                Some(4.0),
                Some(5.0)
            ],
            "the body must see the previous iteration's accepted state — \
             region resources are held, not recycled, between iterations"
        );
        drop(log);

        // ARE released after the region completes: the slot pool stops
        // growing — frame 2 re-acquired cleanly from the pool.
        assert_eq!(
            exec.backend().slot_count(),
            slots_after_frame_1,
            "region resources must return to the pool when the region \
             completes — no slot growth across frames"
        );
    }

    #[test]
    fn substeps_reset_epoch() {
        let mut fx = build_exec_graph(0.0);
        let mut exec = Executor::with_mock();

        // Frame 1, epoch 0: 3 ticks, carrying a 0.5-tick remainder
        // (0.109375 s = 3.5 ticks at 32 Hz).
        exec.set_simulation_frame(sim_frame(1, 0.109375, 0, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(0));
        assert_eq!(fx.increment_log.lock().unwrap().len(), 3);

        // Epoch change with a delta SHORTER than the carried remainder
        // (0.015625 s = 0.5 tick): if the clock had NOT reset, the carried
        // 0.5 + 0.5 would tick once; a reset clock yields zero ticks.
        *fx.seed_value.lock().unwrap() = 10.0;
        exec.set_simulation_frame(sim_frame(2, 0.015625, 1, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(1));
        assert_eq!(
            fx.increment_log.lock().unwrap().len(),
            3,
            "epoch change resets the clock — the carried remainder must not produce a tick"
        );

        // The seed path re-armed: the next advancing frame seeds from the
        // NEW seed value (10), not from the carried-over accepted state.
        exec.set_simulation_frame(sim_frame(3, 0.09375, 1, true));
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(2));
        let log = fx.increment_log.lock().unwrap();
        assert_eq!(log.len(), 6);
        assert_eq!(
            log[3].value_in, Some(10.0),
            "epoch change re-arms the seed path — the body must read the re-seeded state"
        );
        drop(log);
        let shared = fx.boundary_shared.lock().unwrap();
        assert_eq!(
            shared.accepted, 11.0,
            "accepted state seeds from the new seed (10) and advances once"
        );
        assert_eq!(shared.epoch, 1);
    }

    #[test]
    fn substeps_missing_simulation_frame_is_error() {
        let mut fx = build_exec_graph(0.0);
        let mut exec = Executor::with_mock();
        // Host bug: a plan with substep regions executed without
        // set_simulation_frame. Reported (once), region runs zero
        // iterations, no panic.
        exec.execute_frame(&mut fx.graph, &fx.plan, exec_frame_time(0));
        assert!(
            fx.increment_log.lock().unwrap().is_empty(),
            "missing SimulationFrame ⇒ zero body iterations"
        );
        let shared = fx.boundary_shared.lock().unwrap();
        assert_eq!(shared.evals, 1, "the boundary itself still evaluates");
        assert_eq!(
            shared.missing_frame_errors, 1,
            "the boundary sees the missing frame instead of panicking"
        );
        assert_eq!(
            shared.iteration_calls, 0,
            "substep_iteration is never consulted without a SimulationFrame"
        );
        drop(shared);
        let reads = fx.consumer_log.lock().unwrap();
        assert_eq!(reads.len(), 1);
        assert_eq!(
            reads[0], Some(0.0),
            "zero steps still exposes the accepted/seed state on out"
        );
    }
}
