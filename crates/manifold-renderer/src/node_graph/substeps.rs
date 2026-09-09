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
