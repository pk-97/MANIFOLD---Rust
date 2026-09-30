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
//! boundary's final outputs escape. A malformed region is a compile error
//! naming NodeIds, never a fallback to ordinary traversal.
//!
//! Regions nest at most [`MAX_REGION_DEPTH`] deep
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` D10): an outer body may hold whole
//! inner regions, which hold none. An inner region lies wholly inside one
//! outer body, names no clock, and only its boundary's outputs leave it, into
//! the outer body or the outer capture. The executor runs an inner region its
//! own count times on every outer iteration.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

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
/// `clock` opts the region into host syncs: it names the boundary input
/// wired from the region's clock owner, which offline may ask for a GPU sync
/// and a host step between iterations
/// ([`EffectNode::substep_host_sync`](crate::node_graph::effect_node::EffectNode::substep_host_sync)).
/// `None`, the default for every other region, means the executor never
/// commits or waits inside the region, live or offline.
#[derive(Clone, Copy, Debug)]
pub struct SubstepBoundaryPorts {
    pub seed: &'static str,
    pub capture: &'static str,
    pub state: &'static str,
    pub iteration_scalars: &'static [&'static str],
    pub results: &'static [SubstepResultPorts],
    pub clock: Option<&'static str>,
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

/// How deep substep regions nest: an outer region and the inner regions in
/// its body.
pub const MAX_REGION_DEPTH: usize = 2;

/// One contracted repeat region of an
/// [`ExecutionPlan`](crate::node_graph::execution_plan::ExecutionPlan).
///
/// `steps` index `ExecutionPlan::steps`: the boundary first, then the body in
/// topological order, contiguous by construction. `held_resources` are every
/// non-persistent wire whose last reader is a region step. They never appear
/// in any step's `free_after` — a free attached to a body step would fire per
/// iteration or, for the boundary, before the body reads it — so the executor
/// holds them for the whole repeat and releases them when the region ends.
/// `clock` is the clock owner of a boundary that opted into host syncs.
///
/// `inner` are the regions nested in this body, in step order, each a
/// contiguous run of `steps`. An inner region has no `inner`, no `clock` and
/// no `held_resources`: its outer region holds everything an inner step
/// reads for the whole outer repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstepRegion {
    pub boundary: NodeInstanceId,
    pub steps: Vec<usize>,
    pub held_resources: Vec<ResourceId>,
    pub clock: Option<NodeInstanceId>,
    pub inner: Vec<SubstepRegion>,
}

/// Node-level result of region derivation: the boundary first, then its body
/// in execution order, every inner region a contiguous run inside it.
/// `compile` maps it to step indices.
#[derive(Debug, Clone)]
pub(crate) struct RegionNodes {
    pub boundary: NodeInstanceId,
    pub nodes: Vec<NodeInstanceId>,
    pub clock: Option<NodeInstanceId>,
    /// The outer region's boundary when this region is nested.
    pub parent: Option<NodeInstanceId>,
}

/// A region's place in the nest. The plan compiler and the freeze finder
/// both derive it here, so a fused kernel and the executor always agree on
/// which side of every border a node sits.
pub(crate) struct RegionNest<N> {
    pub boundary: N,
    /// Every body node, each inner region whole (boundary and body).
    pub body: AHashSet<N>,
    pub parent: Option<N>,
    pub children: Vec<N>,
}

/// A nest the compiler refuses, before any per-node check.
pub(crate) enum NestError<N> {
    /// `inner` sits in `middle`'s body, which sits in `outer`'s.
    TooDeep { outer: N, middle: N, inner: N },
    /// `inner` sits in the bodies of two regions, neither inside the other.
    TwoOuters { inner: N, first: N, second: N },
}

/// Nest the regions of `boundaries` (each boundary with its capture
/// producers); the result is parallel to the input.
///
/// A region is inner when its boundary lies in another region's forward body
/// (a descendant of that boundary and an ancestor of its capture producers,
/// capture wires excluded). That outer body then also takes every node
/// between the outer boundary and the inner capture producers, so the inner
/// region and whatever feeds it per outer iteration lie wholly inside. `fwd`
/// and `rev` carry forward wires only.
pub(crate) fn nest_regions<N>(
    boundaries: &[(N, Vec<N>)],
    fwd: &AHashMap<N, Vec<N>>,
    rev: &AHashMap<N, Vec<N>>,
) -> Result<Vec<RegionNest<N>>, NestError<N>>
where
    N: Copy + Eq + std::hash::Hash,
{
    let raw: Vec<AHashSet<N>> = boundaries
        .iter()
        .map(|(boundary, producers)| region_body(*boundary, producers, fwd, rev))
        .collect();
    let children: Vec<Vec<usize>> = (0..boundaries.len())
        .map(|outer| {
            (0..boundaries.len())
                .filter(|&inner| inner != outer && raw[outer].contains(&boundaries[inner].0))
                .collect()
        })
        .collect();
    for (outer, inners) in children.iter().enumerate() {
        for &middle in inners {
            if let Some(&inner) = children[middle].first() {
                return Err(NestError::TooDeep {
                    outer: boundaries[outer].0,
                    middle: boundaries[middle].0,
                    inner: boundaries[inner].0,
                });
            }
        }
    }
    let mut parent: Vec<Option<usize>> = vec![None; boundaries.len()];
    for (outer, inners) in children.iter().enumerate() {
        for &inner in inners {
            if let Some(first) = parent[inner] {
                return Err(NestError::TwoOuters {
                    inner: boundaries[inner].0,
                    first: boundaries[first].0,
                    second: boundaries[outer].0,
                });
            }
            parent[inner] = Some(outer);
        }
    }
    Ok(raw
        .into_iter()
        .enumerate()
        .map(|(index, raw_body)| {
            let (boundary, producers) = &boundaries[index];
            let body = if children[index].is_empty() {
                raw_body
            } else {
                let mut all_producers = producers.clone();
                for &inner in &children[index] {
                    all_producers.extend(boundaries[inner].1.iter().copied());
                }
                region_body(*boundary, &all_producers, fwd, rev)
            };
            RegionNest {
                boundary: *boundary,
                body,
                parent: parent[index].map(|outer| boundaries[outer].0),
                children: children[index].iter().map(|&inner| boundaries[inner].0).collect(),
            }
        })
        .collect())
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
/// capture producer the boundary does not reach, a nest deeper than
/// [`MAX_REGION_DEPTH`], an inner region inside two outer ones or only partly
/// inside one, a clock on an inner region, a state-capture node, a draw call,
/// the final output or a coupled-scene participant inside a body, a body wire
/// read outside the region, an inner intermediate captured by the outer
/// boundary, a node claimed by two regions, or one region feeding another.
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

    let mut declared: Vec<(NodeInstanceId, SubstepBoundaryPorts, Vec<NodeInstanceId>)> =
        Vec::with_capacity(boundaries.len());
    for &(boundary, ports) in &boundaries {
        if !active.contains(&boundary) {
            continue;
        }
        let inst = graph.get_node(boundary).expect("boundary exists");
        let has_input = |name: &str| inst.node.inputs().iter().any(|p| p.name == name);
        let has_output = |name: &str| inst.node.outputs().iter().any(|p| p.name == name);
        let declared_inputs = std::iter::once(ports.seed).chain(ports.capture_ports()).chain(ports.clock);
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
        declared.push((boundary, ports, producers));
    }

    let shapes: Vec<(NodeInstanceId, Vec<NodeInstanceId>)> = declared
        .iter()
        .map(|(boundary, _, producers)| (*boundary, producers.clone()))
        .collect();
    let nest = nest_regions(&shapes, &fwd, &rev).map_err(|error| match error {
        NestError::TooDeep { outer, middle, inner } => malformed(
            outer,
            inner,
            format!(
                "substep regions nest at most {MAX_REGION_DEPTH} deep: region {inner:?} sits \
                 inside {middle:?}, which sits inside {outer:?}"
            ),
        ),
        NestError::TwoOuters { inner, first, second } => malformed(
            second,
            inner,
            format!(
                "partial overlap: inner region {inner:?} sits inside two outer regions, \
                 {first:?} and {second:?}; an inner region lies wholly inside one outer body"
            ),
        ),
    })?;
    for (region, (boundary, ports, _)) in nest.iter().zip(&declared) {
        if let (Some(outer), Some(port)) = (region.parent, ports.clock) {
            return Err(malformed(
                outer,
                *boundary,
                format!(
                    "inner region {boundary:?} names clock port `{port}` — only an outer \
                     region may own host syncs"
                ),
            ));
        }
    }
    // Capture wires by producer, for the inner-intermediate check; only a
    // nest needs them.
    let mut captured_by: AHashMap<NodeInstanceId, Vec<NodeInstanceId>> = AHashMap::default();
    if nest.iter().any(|region| region.parent.is_some()) {
        for w in graph.walk_wires(WireWalkMode::CaptureOnly) {
            captured_by.entry(w.from.0).or_default().push(w.to.0);
        }
    }

    let mut regions: Vec<RegionNodes> = Vec::with_capacity(declared.len());
    let mut claimed: AHashSet<NodeInstanceId> = AHashSet::default();
    for (region, (boundary, ports, producers)) in nest.iter().zip(&declared) {
        let (boundary, ports) = (*boundary, *ports);
        let body: Vec<NodeInstanceId> = active_order
            .iter()
            .copied()
            .filter(|id| region.body.contains(id))
            .collect();

        for &producer in producers {
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
        // Inner regions' nodes: their own region claims them.
        let inner_members: AHashSet<NodeInstanceId> = nest
            .iter()
            .filter(|inner| region.children.contains(&inner.boundary))
            .flat_map(|inner| inner.body.iter().copied().chain([inner.boundary]))
            .collect();
        for &node in &body {
            let inst = graph.get_node(node).expect("body node exists");
            let inner_boundary = region.children.contains(&node);
            if !inner_boundary && inst.node.substep_boundary().is_some() {
                return Err(malformed(
                    boundary,
                    node,
                    format!(
                        "partial overlap: substep boundary {node:?} lies in this region's body \
                         but its own body does not; an inner region lies wholly inside one \
                         outer body and feeds it through its boundary's outputs"
                    ),
                ));
            }
            if !inner_boundary && !inst.node.state_capture_input_ports().is_empty() {
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
                let reason = if inner_boundary {
                    format!(
                        "inner region {node:?}'s output escapes to outside reader {target:?} — \
                         an inner region's outputs feed only its outer body or the outer capture"
                    )
                } else {
                    format!(
                        "region intermediate wire escapes to outside reader {target:?} — \
                         only the boundary's outputs may leave the region"
                    )
                };
                return Err(malformed(boundary, node, reason));
            }
            if let Some(outer) = region.parent
                && captured_by.get(&node).is_some_and(|targets| targets.contains(&outer))
            {
                return Err(malformed(
                    boundary,
                    node,
                    format!(
                        "inner region intermediate is captured by the outer boundary {outer:?} — \
                         only the inner boundary's outputs may leave the inner region"
                    ),
                ));
            }
            if !inner_members.contains(&node) && !claimed.insert(node) {
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
        let clock = match ports.clock {
            None => None,
            Some(port) => {
                let Some(wire) = graph.wires_into(boundary).find(|w| w.to.1 == port) else {
                    return Err(malformed(boundary, boundary, format!("clock port `{port}` has no wire")));
                };
                let owner = wire.from.0;
                if members.contains(&owner) {
                    return Err(malformed(
                        boundary,
                        owner,
                        "the region's clock owner sits inside the region".to_string(),
                    ));
                }
                Some(owner)
            }
        };
        let mut nodes = Vec::with_capacity(body.len() + 1);
        nodes.push(boundary);
        if region.children.is_empty() {
            nodes.extend(body);
        } else {
            let inner_blocks: Vec<Vec<NodeInstanceId>> = nest
                .iter()
                .filter(|inner| region.children.contains(&inner.boundary))
                .map(|inner| {
                    std::iter::once(inner.boundary)
                        .chain(active_order.iter().copied().filter(|id| inner.body.contains(id)))
                        .collect()
                })
                .collect();
            nodes.extend(order_outer_body(&body, &inner_blocks, &fwd)?);
        }
        regions.push(RegionNodes {
            boundary,
            nodes,
            clock,
            parent: region.parent,
        });
    }

    // One region feeding another (chaining) is not supported: a member whose
    // forward predecessor belongs to a different region. Nesting is not
    // chaining: an inner region reads its outer body, and the outer body
    // reads the inner boundary's outputs.
    let mut region_of: AHashMap<NodeInstanceId, NodeInstanceId> = AHashMap::default();
    for region in regions.iter().filter(|r| r.parent.is_none()) {
        region_of.extend(region.nodes.iter().map(|&n| (n, region.boundary)));
    }
    for region in regions.iter().filter(|r| r.parent.is_some()) {
        region_of.extend(region.nodes.iter().map(|&n| (n, region.boundary)));
    }
    let parent_of: AHashMap<NodeInstanceId, NodeInstanceId> = regions
        .iter()
        .filter_map(|r| r.parent.map(|outer| (r.boundary, outer)))
        .collect();
    for region in &regions {
        let own = region.nodes.iter().filter(|n| region_of.get(n) == Some(&region.boundary));
        for &node in own {
            for &pred in rev.get(&node).map(Vec::as_slice).unwrap_or(&[]) {
                if region_of.get(&pred).is_some_and(|&b| {
                    b != region.boundary
                        && Some(b) != region.parent
                        && parent_of.get(&b) != Some(&region.boundary)
                }) {
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

/// An outer body in execution order: topological, each inner region one
/// contiguous run, boundary first. `body` is in topological order; each
/// `inner` block is its boundary then its body in topological order.
fn order_outer_body(
    body: &[NodeInstanceId],
    inner: &[Vec<NodeInstanceId>],
    fwd: &AHashMap<NodeInstanceId, Vec<NodeInstanceId>>,
) -> Result<Vec<NodeInstanceId>, GraphError> {
    let block_of: AHashMap<NodeInstanceId, usize> = inner
        .iter()
        .enumerate()
        .flat_map(|(block, members)| members.iter().map(move |&n| (n, block)))
        .collect();
    // (first topological position, members)
    let mut groups: Vec<(usize, Vec<NodeInstanceId>)> = Vec::new();
    let mut group_of: AHashMap<NodeInstanceId, usize> = AHashMap::default();
    for (position, &node) in body.iter().enumerate() {
        if group_of.contains_key(&node) {
            continue;
        }
        let members = match block_of.get(&node) {
            Some(&block) => inner[block].clone(),
            None => vec![node],
        };
        for &member in &members {
            group_of.insert(member, groups.len());
        }
        groups.push((position, members));
    }
    let mut incoming = vec![0usize; groups.len()];
    let mut outgoing = vec![Vec::<usize>::new(); groups.len()];
    let mut edges = AHashSet::<(usize, usize)>::default();
    for &node in body {
        for target in fwd.get(&node).map(Vec::as_slice).unwrap_or(&[]) {
            let (Some(&from), Some(&to)) = (group_of.get(&node), group_of.get(target)) else {
                continue;
            };
            if from != to && edges.insert((from, to)) {
                outgoing[from].push(to);
                incoming[to] += 1;
            }
        }
    }
    let mut ready: BinaryHeap<Reverse<(usize, usize)>> = incoming
        .iter()
        .enumerate()
        .filter(|(_, degree)| **degree == 0)
        .map(|(group, _)| Reverse((groups[group].0, group)))
        .collect();
    let mut order = Vec::with_capacity(body.len());
    let mut placed = 0;
    while let Some(Reverse((_, group))) = ready.pop() {
        placed += 1;
        order.extend(groups[group].1.iter().copied());
        for &next in &outgoing[group] {
            incoming[next] -= 1;
            if incoming[next] == 0 {
                ready.push(Reverse((groups[next].0, next)));
            }
        }
    }
    if placed != groups.len() {
        return Err(GraphError::CycleDetected {
            involves: body.to_vec(),
        });
    }
    Ok(order)
}

fn malformed(boundary: NodeInstanceId, node: NodeInstanceId, reason: String) -> GraphError {
    GraphError::MalformedSubstepRegion {
        boundary,
        node,
        reason,
    }
}

/// A region's body: every node that is both a forward descendant of
/// `boundary` and a forward ancestor of one of `producers` (the nodes wired
/// into its capture ports), boundary excluded. `fwd`/`rev` carry forward
/// wires only — state-capture back edges excluded. The plan compiler and the
/// freeze finder both call this, so a fused kernel and the executor always
/// agree on which side of the border a node sits.
pub(crate) fn region_body<N>(
    boundary: N,
    producers: &[N],
    fwd: &AHashMap<N, Vec<N>>,
    rev: &AHashMap<N, Vec<N>>,
) -> AHashSet<N>
where
    N: Copy + Eq + std::hash::Hash,
{
    let forward = reach(&[boundary], fwd);
    let mut body = AHashSet::default();
    let mut stack = producers.to_vec();
    while let Some(n) = stack.pop() {
        if n == boundary || !body.insert(n) {
            continue;
        }
        if let Some(prev) = rev.get(&n) {
            stack.extend(prev.iter().copied());
        }
    }
    body.retain(|n| forward.contains(n));
    body
}

fn reach<N>(from: &[N], edges: &AHashMap<N, Vec<N>>) -> AHashSet<N>
where
    N: Copy + Eq + std::hash::Hash,
{
    let mut seen = AHashSet::default();
    let mut stack = from.to_vec();
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

/// Test-only nodes for region proofs that go through the registry (the
/// freeze finder and the `gpu_proofs` binary build graphs from definitions):
/// array sources, a particle substep boundary and a texture sink that makes
/// the region live. Compiled for unit tests and the `gpu-proofs` feature only.
#[cfg(any(test, feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod test_nodes {
    use std::borrow::Cow;

    use crate::generators::compute_common::Particle;
    use crate::node_graph::PrimitiveRegistry;
    use crate::node_graph::effect_node::{
        EffectNode, EffectNodeContext, EffectNodeType, NodeRequires, ParamValues,
    };
    use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
    use crate::node_graph::ports::{
        ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };

    use super::SubstepBoundaryPorts;

    pub const PARTICLE_BOUNDARY_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
        seed: "seed",
        capture: "in",
        state: "out",
        iteration_scalars: &["step_dt", "step_index"],
        results: &[],
        clock: None,
    };

    /// The `step_dt` the particle boundary serves at iteration `i`: distinct
    /// per iteration so a shared uniform would be visible in the result.
    pub fn particle_step_dt(iteration: u32) -> f32 {
        0.25 * (iteration + 1) as f32
    }

    fn port(name: &'static str, ty: PortType, kind: PortKind, required: bool) -> NodePort {
        NodePort {
            name: Cow::Borrowed(name),
            ty,
            kind,
            required,
        }
    }

    fn int_param(name: &'static str, default: f32) -> ParamDef {
        ParamDef {
            name: Cow::Borrowed(name),
            label: name,
            ty: ParamType::Int,
            default: ParamValue::Float(default),
            range: Some((0.0, 1.0e6)),
            enum_values: &[],
        }
    }

    macro_rules! node_basics {
        () => {
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
                &self.params
            }
        };
    }

    /// An array whose contents the test writes after pre-allocation;
    /// sized by `max_capacity`.
    struct ArraySource {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        params: Vec<ParamDef>,
    }

    impl ArraySource {
        fn new(type_id: &'static str, item: ArrayType) -> Self {
            Self {
                type_id: EffectNodeType::new(type_id),
                inputs: Vec::new(),
                outputs: vec![port("out", PortType::Array(item), PortKind::Output, false)],
                params: vec![int_param("max_capacity", 256.0)],
            }
        }
    }

    impl EffectNode for ArraySource {
        node_basics!();
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    }

    /// A particle substep boundary with the same shape the MPM state node
    /// takes: `seed` copied in once, `out` the persistent state the body
    /// mutates, `in` the capture. `iterations` per frame, `step_dt` from
    /// [`particle_step_dt`], `step_index` the iteration. The inner variant
    /// (`test.particle_inner_boundary`) copies `seed` in on every evaluate:
    /// nested in an outer body, it restarts from that body each outer
    /// iteration, the way a Krylov boundary restarts from its start vector.
    struct ParticleBoundary {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        params: Vec<ParamDef>,
        seeded: bool,
        reseed: bool,
        pending: u32,
    }

    impl ParticleBoundary {
        fn inner() -> Self {
            Self {
                type_id: EffectNodeType::new("test.particle_inner_boundary"),
                reseed: true,
                ..Self::new()
            }
        }

        fn new() -> Self {
            let particles = PortType::Array(ArrayType::of_known::<Particle>());
            let f32_ty = PortType::Scalar(ScalarType::F32);
            Self {
                type_id: EffectNodeType::new("test.particle_boundary"),
                inputs: vec![
                    port("seed", particles, PortKind::Input, true),
                    port("in", particles, PortKind::Input, true),
                ],
                outputs: vec![
                    port("out", particles, PortKind::Output, false),
                    port("step_dt", f32_ty, PortKind::Output, false),
                    port("step_index", f32_ty, PortKind::Output, false),
                ],
                params: vec![int_param("iterations", 4.0)],
                seeded: false,
                reseed: false,
                pending: 0,
            }
        }
    }

    impl EffectNode for ParticleBoundary {
        node_basics!();
        fn requires(&self) -> NodeRequires {
            NodeRequires {
                gpu_encoder: true,
                state_store: false,
            }
        }
        fn array_output_capacity(
            &self,
            port_name: &str,
            _params: &ParamValues,
            input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            (port_name == "out")
                .then(|| input_capacities.iter().find(|(p, _)| *p == "seed").map(|&(_, n)| n))
                .flatten()
        }
        fn state_capture_input_ports(&self) -> &[&str] {
            &["in"]
        }
        fn persistent_output_ports(&self) -> &[&str] {
            &["out"]
        }
        fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
            Some(PARTICLE_BOUNDARY_PORTS)
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            self.pending = ctx
                .params
                .get("iterations")
                .and_then(|v| v.as_u32_clamped(0))
                .unwrap_or(0);
            let (Some(seed), Some(out)) = (ctx.inputs.array("seed"), ctx.outputs.array("out"))
            else {
                return;
            };
            if !self.seeded || self.reseed {
                self.seeded = true;
                let size = seed.size.min(out.size);
                let gpu = ctx.gpu.as_deref_mut().expect("particle boundary needs a GpuEncoder");
                gpu.native_enc.copy_buffer_to_buffer(seed, out, size);
            }
        }
        fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
            if iteration >= self.pending {
                return false;
            }
            scalars[0] = particle_step_dt(iteration);
            scalars[1] = iteration as f32;
            true
        }
        fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            // An in-place body already wrote `out`; a fresh-output body is
            // accepted by copy.
            let (Some(candidate), Some(out)) = (ctx.inputs.array("in"), ctx.outputs.array("out"))
            else {
                return;
            };
            if !candidate.ptr_eq(out) {
                let size = candidate.size.min(out.size);
                let gpu = ctx.gpu.as_deref_mut().expect("particle boundary needs a GpuEncoder");
                gpu.native_enc.copy_buffer_to_buffer(candidate, out, size);
            }
        }
    }

    /// Copies `in` into a fresh `out` of the same capacity: an ordinary
    /// temporary, which the array planner may place in storage another array
    /// has released.
    struct ParticleCopy {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        params: Vec<ParamDef>,
    }

    impl EffectNode for ParticleCopy {
        node_basics!();
        fn requires(&self) -> NodeRequires {
            NodeRequires {
                gpu_encoder: true,
                state_store: false,
            }
        }
        fn array_output_capacity(
            &self,
            port_name: &str,
            _params: &ParamValues,
            input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            (port_name == "out")
                .then(|| input_capacities.iter().find(|(p, _)| *p == "in").map(|&(_, n)| n))
                .flatten()
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let (Some(source), Some(copy)) = (ctx.inputs.array("in"), ctx.outputs.array("out")) else {
                return;
            };
            let size = source.size.min(copy.size);
            let gpu = ctx.gpu.as_deref_mut().expect("particle copy needs a GpuEncoder");
            gpu.native_enc.copy_buffer_to_buffer(source, copy, size);
        }
    }

    /// Consumes particles and yields a texture so the region reaches a final
    /// output; draws nothing.
    struct ParticleSink {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        params: Vec<ParamDef>,
    }

    impl EffectNode for ParticleSink {
        node_basics!();
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    }

    pub fn register_substep_test_nodes(registry: &mut PrimitiveRegistry) {
        registry.register("test.particle_source", || {
            Box::new(ArraySource::new(
                "test.particle_source",
                ArrayType::of_known::<Particle>(),
            ))
        });
        registry.register("test.force_source", || {
            Box::new(ArraySource::new(
                "test.force_source",
                ArrayType::of_known::<[f32; 3]>(),
            ))
        });
        registry.register("test.particle_boundary", || Box::new(ParticleBoundary::new()));
        registry.register("test.particle_inner_boundary", || Box::new(ParticleBoundary::inner()));
        registry.register("test.particle_copy", || {
            let particles = PortType::Array(ArrayType::of_known::<Particle>());
            Box::new(ParticleCopy {
                type_id: EffectNodeType::new("test.particle_copy"),
                inputs: vec![port("in", particles, PortKind::Input, true)],
                outputs: vec![port("out", particles, PortKind::Output, false)],
                params: Vec::new(),
            })
        });
        registry.register("test.particle_sink", || {
            Box::new(ParticleSink {
                type_id: EffectNodeType::new("test.particle_sink"),
                inputs: vec![port(
                    "particles",
                    PortType::Array(ArrayType::of_known::<Particle>()),
                    PortKind::Input,
                    true,
                )],
                outputs: vec![port("out", PortType::Texture2D, PortKind::Output, false)],
                params: Vec::new(),
            })
        });
    }
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
        clock: None,
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
            if let Some(clock) = ports.clock {
                inputs.push(input(clock, PortType::Texture2D, false));
            }
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

    // ─── Nested regions: the compiler (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P5) ───

    fn region_nodes(plan: &crate::node_graph::ExecutionPlan, region: &SubstepRegion) -> Vec<NodeInstanceId> {
        region.steps.iter().map(|&i| plan.steps()[i].node).collect()
    }

    /// ```text
    /// src ─▶ outer.seed
    /// outer.out ─┬─▶ pre.a ─┬─▶ inner.seed
    ///            │          ├─▶ z.a ──────▶ inner_body.b
    ///            │          └─▶ side.a ───▶ post.b
    ///            └─▶ consumer.tex
    /// inner.out ─┬─▶ inner_body.a ─▶ (capture) inner.in
    ///            └─▶ post.a ─▶ (capture) outer.in
    /// ```
    /// `z` feeds only the inner body, so the outer region reaches it only
    /// through the inner region. Plain topological order puts `z` and `side`
    /// between the inner boundary and its body.
    #[test]
    fn nested_region_contracts_inner_whole() {
        let mut graph = Graph::new();
        let src = source(&mut graph, "src");
        let outer = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let pre = pass(&mut graph, "pre");
        let inner = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let z = pass(&mut graph, "z");
        let side = pass(&mut graph, "side");
        let inner_body = pass(&mut graph, "inner_body");
        let post = pass(&mut graph, "post");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((src, "out"), (outer, "seed")).unwrap();
        graph.connect((outer, "out"), (pre, "a")).unwrap();
        graph.connect((pre, "out"), (inner, "seed")).unwrap();
        graph.connect((pre, "out"), (z, "a")).unwrap();
        graph.connect((pre, "out"), (side, "a")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((z, "out"), (inner_body, "b")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((inner, "out"), (post, "a")).unwrap();
        graph.connect((side, "out"), (post, "b")).unwrap();
        graph.connect((post, "out"), (outer, "in")).unwrap();
        graph.connect((outer, "out"), (consumer, "tex")).unwrap();

        let topo = crate::node_graph::validation::topological_sort(&graph).unwrap();
        let at = |node| topo.iter().position(|&n| n == node).unwrap();
        assert!(
            at(inner) < at(z) && at(z) < at(inner_body),
            "premise: plain topological order splits the inner region: {topo:?}"
        );

        let plan = compile(&graph).unwrap();
        let order: Vec<NodeInstanceId> = plan.steps().iter().map(|s| s.node).collect();
        assert_eq!(
            order,
            vec![src, outer, pre, z, inner, inner_body, side, post, consumer],
            "the inner region is one run inside the outer block"
        );
        let regions = plan.substep_regions();
        assert_eq!(regions.len(), 1, "only the outer region runs at the top level");
        let outer_region = &regions[0];
        assert_eq!(outer_region.boundary, outer);
        assert_eq!(outer_region.steps, (1..=7).collect::<Vec<_>>());
        assert_eq!(outer_region.inner.len(), 1);
        let inner_region = &outer_region.inner[0];
        assert_eq!(region_nodes(&plan, inner_region), vec![inner, inner_body]);
        assert_eq!(inner_region.steps, vec![4, 5]);
        assert!(inner_region.inner.is_empty() && inner_region.clock.is_none());

        // The outer region holds everything its steps read, inner ones
        // included, for the whole outer repeat; the inner region holds nothing.
        assert!(inner_region.held_resources.is_empty());
        let held = |node, port| step_output(&plan, order.iter().position(|&n| n == node).unwrap(), port);
        for resource in [held(src, "out"), held(pre, "out"), held(z, "out"), held(side, "out"), held(inner, "out")] {
            assert!(outer_region.held_resources.contains(&resource), "{resource:?} not held");
        }
        for step in &plan.steps()[1..=7] {
            for resource in &outer_region.held_resources {
                assert!(!step.free_after.contains(resource), "{resource:?} freed mid-region");
            }
        }
        assert!(plan.late_capture_step_indices().is_empty(), "both boundaries capture per iteration");
        assert!((1..=7).all(|i| !plan.step_hoistable(i)));
    }

    /// The inner boundary's output may be the outer region's candidate
    /// directly: the outer capture.
    #[test]
    fn nested_region_inner_output_feeds_outer_capture() {
        let mut graph = Graph::new();
        let outer = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner_body = pass(&mut graph, "inner_body");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((outer, "out"), (inner, "seed")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((inner, "out"), (outer, "in")).unwrap();
        graph.connect((outer, "out"), (consumer, "tex")).unwrap();

        let plan = compile(&graph).unwrap();
        let regions = plan.substep_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(region_nodes(&plan, &regions[0]), vec![outer, inner, inner_body]);
        assert_eq!(region_nodes(&plan, &regions[0].inner[0]), vec![inner, inner_body]);
    }

    /// `side`'s boundary sits in the outer body (its output feeds the inner
    /// body) but its own body does not.
    #[test]
    fn nested_region_rejects_partial_overlap() {
        let mut graph = Graph::new();
        let outer = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let pre = pass(&mut graph, "pre");
        let inner = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let side = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let side_body = pass(&mut graph, "side_body");
        let inner_body = pass(&mut graph, "inner_body");
        let post = pass(&mut graph, "post");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((outer, "out"), (pre, "a")).unwrap();
        graph.connect((pre, "out"), (inner, "seed")).unwrap();
        graph.connect((pre, "out"), (side, "seed")).unwrap();
        graph.connect((side, "out"), (side_body, "a")).unwrap();
        graph.connect((side_body, "out"), (side, "in")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((side, "out"), (inner_body, "b")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((inner, "out"), (post, "a")).unwrap();
        graph.connect((post, "out"), (outer, "in")).unwrap();
        graph.connect((outer, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (outer, side));
        assert!(reason.contains("partial overlap"), "{reason}");
    }

    /// One inner region in the bodies of two outer regions.
    #[test]
    fn nested_region_rejects_two_outer_regions() {
        let mut graph = Graph::new();
        let outer_a = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let outer_b = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let merge = pass(&mut graph, "merge");
        let inner = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner_body = pass(&mut graph, "inner_body");
        let post_a = pass(&mut graph, "post_a");
        let post_b = pass(&mut graph, "post_b");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((outer_a, "out"), (merge, "a")).unwrap();
        graph.connect((outer_b, "out"), (merge, "b")).unwrap();
        graph.connect((merge, "out"), (inner, "seed")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((inner, "out"), (post_a, "a")).unwrap();
        graph.connect((inner, "out"), (post_b, "a")).unwrap();
        graph.connect((post_a, "out"), (outer_a, "in")).unwrap();
        graph.connect((post_b, "out"), (outer_b, "in")).unwrap();
        graph.connect((outer_a, "out"), (consumer, "tex")).unwrap();
        graph.connect((outer_b, "out"), (consumer, "aux")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (outer_b, inner));
        assert!(reason.contains("two outer regions") && reason.contains(&format!("{outer_a:?}")), "{reason}");
    }

    #[test]
    fn nested_region_rejects_depth_three() {
        let mut graph = Graph::new();
        let outer = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let middle = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner_body = pass(&mut graph, "inner_body");
        let middle_post = pass(&mut graph, "middle_post");
        let outer_post = pass(&mut graph, "outer_post");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((outer, "out"), (middle, "seed")).unwrap();
        graph.connect((middle, "out"), (inner, "seed")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((inner, "out"), (middle_post, "a")).unwrap();
        graph.connect((middle_post, "out"), (middle, "in")).unwrap();
        graph.connect((middle, "out"), (outer_post, "a")).unwrap();
        graph.connect((outer_post, "out"), (outer, "in")).unwrap();
        graph.connect((outer, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (outer, inner));
        assert!(
            reason.contains("at most 2 deep") && reason.contains(&format!("{middle:?}")),
            "{reason}"
        );
    }

    #[test]
    fn nested_region_rejects_inner_clock() {
        const CLOCKED: SubstepBoundaryPorts = SubstepBoundaryPorts {
            clock: Some("clock"),
            ..PORTS
        };
        let mut graph = Graph::new();
        let clock = source(&mut graph, "clock");
        let outer = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner = graph.add_node(Box::new(TestNode::boundary(CLOCKED)));
        let inner_body = pass(&mut graph, "inner_body");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((clock, "out"), (inner, "clock")).unwrap();
        graph.connect((outer, "out"), (inner, "seed")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((inner, "out"), (outer, "in")).unwrap();
        graph.connect((outer, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (outer, inner));
        assert!(reason.contains("clock port `clock`") && reason.contains("only an outer"), "{reason}");
    }

    #[test]
    fn nested_region_rejects_inner_output_escaping_outer() {
        let mut graph = Graph::new();
        let outer = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner_body = pass(&mut graph, "inner_body");
        let post = pass(&mut graph, "post");
        let peek = sink(&mut graph, "peek");
        graph.connect((outer, "out"), (inner, "seed")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((inner, "out"), (post, "a")).unwrap();
        graph.connect((inner, "out"), (peek, "tex")).unwrap();
        graph.connect((post, "out"), (outer, "in")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (outer, inner));
        assert!(reason.contains("inner region") && reason.contains("escapes"), "{reason}");
    }

    /// An inner intermediate wired straight into the outer capture escapes
    /// the inner region past its boundary.
    #[test]
    fn nested_region_rejects_inner_intermediate_in_outer_capture() {
        let mut graph = Graph::new();
        let outer = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let inner_body = pass(&mut graph, "inner_body");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((outer, "out"), (inner, "seed")).unwrap();
        graph.connect((inner, "out"), (inner_body, "a")).unwrap();
        graph.connect((inner_body, "out"), (inner, "in")).unwrap();
        graph.connect((inner_body, "out"), (outer, "in")).unwrap();
        graph.connect((outer, "out"), (consumer, "tex")).unwrap();

        let (b, node, reason) = malformed_parts(compile(&graph).unwrap_err());
        assert_eq!((b, node), (inner, inner_body));
        assert!(reason.contains("captured by the outer boundary"), "{reason}");
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

    // ─── Executor repeat proofs ───
    //
    // The real `Executor` over `MockBackend`, which stores scalars
    // observably, so state is one scalar moving through slots:
    //
    // ```text
    // src ──▶ boundary.seed
    // boundary.out ──▶ add_dt.a ──▶ add_index.a ──▶ (capture) boundary.in
    // boundary.step_dt ──▶ add_dt.b      boundary.step_index ──▶ add_index.b
    // aux ──▶ add_index.c                boundary.out ──▶ consumer.a
    // ```
    //
    // Per iteration i the body computes `state + 0.5 + i`; the boundary's
    // `late_capture` accepts it onto its persistent `out`.

    use std::sync::{Arc, Mutex};

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::effect_node::FrameTime;
    use crate::node_graph::execution::Executor;
    use crate::node_graph::parameters::ParamValue;

    type Log = Arc<Mutex<Vec<String>>>;

    const SIM_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
        seed: "seed",
        capture: "in",
        state: "out",
        iteration_scalars: &["step_dt", "step_index"],
        results: &[],
        clock: None,
    };

    /// The same region, opted into host syncs through its clock owner.
    const SIM_CLOCK_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts { clock: Some("clock"), ..SIM_PORTS };

    fn scalar_in(ctx: &EffectNodeContext<'_, '_>, port: &str) -> Option<f32> {
        match ctx.inputs.scalar(port) {
            Some(ParamValue::Float(v)) => Some(v),
            _ => None,
        }
    }

    struct SimBoundary {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        log: Log,
        /// Iterations the next frame runs.
        count: Arc<Mutex<u32>>,
        pending: u32,
        accepted: f32,
        seeded: bool,
        ports: SubstepBoundaryPorts,
        /// What evaluate logs; captures log `<capture_label> <value>`.
        name: &'static str,
        capture_label: &'static str,
        /// Take `seed` on every evaluate, as an inner region restarting from
        /// its outer body does.
        reseed: bool,
        step_dt: f32,
    }

    impl SimBoundary {
        /// A labelled boundary for nests: its own log lines and `step_dt`,
        /// reseeded on every evaluate when it is the inner one.
        fn labelled(
            log: Log,
            count: Arc<Mutex<u32>>,
            name: &'static str,
            step_dt: f32,
            reseed: bool,
        ) -> Self {
            Self {
                name,
                capture_label: if reseed { "inner capture" } else { "outer capture" },
                step_dt,
                reseed,
                ..Self::new(log, count)
            }
        }

        fn new(log: Log, count: Arc<Mutex<u32>>) -> Self {
            let f32_ty = PortType::Scalar(ScalarType::F32);
            Self {
                type_id: EffectNodeType::new("test.sim_boundary"),
                inputs: vec![
                    input("seed", f32_ty, true),
                    input("in", f32_ty, true),
                    input("clock", f32_ty, false),
                ],
                outputs: vec![
                    output("out", f32_ty),
                    output("step_dt", f32_ty),
                    output("step_index", f32_ty),
                ],
                log,
                count,
                pending: 0,
                accepted: 0.0,
                seeded: false,
                ports: SIM_PORTS,
                name: "boundary",
                capture_label: "capture",
                reseed: false,
                step_dt: 0.5,
            }
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
            self.log.lock().unwrap().push(self.name.into());
            if !self.seeded || self.reseed {
                self.seeded = true;
                self.accepted = scalar_in(ctx, "seed").unwrap_or(0.0);
            }
            self.pending = *self.count.lock().unwrap();
            ctx.outputs.set_scalar("out", ParamValue::Float(self.accepted));
        }
        fn state_capture_input_ports(&self) -> &[&str] {
            &["in"]
        }
        fn persistent_output_ports(&self) -> &[&str] {
            &["out"]
        }
        fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
            Some(self.ports)
        }
        fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
            if iteration >= self.pending {
                return false;
            }
            scalars[0] = self.step_dt;
            scalars[1] = iteration as f32;
            true
        }
        fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let candidate = scalar_in(ctx, "in").expect("capture slot bound");
            self.log.lock().unwrap().push(format!("{} {candidate}", self.capture_label));
            self.accepted = candidate;
            ctx.outputs.set_scalar("out", ParamValue::Float(candidate));
        }
    }

    /// `out = a + b + c`, logging what it read; `c` unbound logs `c=none`.
    struct Adder {
        type_id: EffectNodeType,
        name: &'static str,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        log: Log,
        root: bool,
        value: Option<f32>,
    }

    impl Adder {
        fn new(name: &'static str, log: Log) -> Self {
            let f32_ty = PortType::Scalar(ScalarType::F32);
            Self {
                type_id: EffectNodeType::new("test.adder"),
                name,
                inputs: vec![
                    input("a", f32_ty, false),
                    input("b", f32_ty, false),
                    input("c", f32_ty, false),
                ],
                outputs: vec![output("out", f32_ty)],
                log,
                root: false,
                value: None,
            }
        }

        fn constant(name: &'static str, log: Log, value: f32) -> Self {
            Self {
                value: Some(value),
                ..Self::new(name, log)
            }
        }

        fn root(name: &'static str, log: Log) -> Self {
            Self {
                root: true,
                ..Self::new(name, log)
            }
        }
    }

    impl EffectNode for Adder {
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
        fn is_liveness_root(&self) -> bool {
            self.root
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let out = if let Some(v) = self.value {
                self.log.lock().unwrap().push(self.name.to_string());
                v
            } else {
                let a = scalar_in(ctx, "a").unwrap_or(0.0);
                let b = scalar_in(ctx, "b").unwrap_or(0.0);
                let c = scalar_in(ctx, "c");
                let c_text = c.map_or("none".to_string(), |c| c.to_string());
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("{} a={a} b={b} c={c_text}", self.name));
                a + b + c.unwrap_or(0.0)
            };
            ctx.outputs.set_scalar("out", ParamValue::Float(out));
        }
    }

    /// A clock owner that asks for a host sync before every iteration it is
    /// asked about and logs each host step.
    struct EagerClock {
        type_id: EffectNodeType,
        outputs: Vec<NodeOutput>,
        log: Log,
    }

    impl EffectNode for EagerClock {
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
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            ctx.outputs.set_scalar("out", ParamValue::Float(0.0));
        }
        fn substep_host_sync(&self, _iteration: u32) -> bool {
            true
        }
        fn substep_host_step(
            &mut self,
            iteration: u32,
            _gpu: Option<&mut crate::gpu_encoder::GpuEncoder<'_>>,
        ) -> Result<(), String> {
            self.log.lock().unwrap().push(format!("host {iteration}"));
            Ok(())
        }
    }

    struct SimFixture {
        graph: Graph,
        plan: crate::node_graph::ExecutionPlan,
        log: Log,
        count: Arc<Mutex<u32>>,
        aux: NodeInstanceId,
    }

    /// `sim_fixture` with an eager clock owner wired into the boundary's
    /// `clock` input; `opted` says whether the boundary names it.
    fn clock_fixture(opted: bool) -> SimFixture {
        let log: Log = Arc::default();
        let count = Arc::new(Mutex::new(3));
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Adder::constant("src", log.clone(), 1.0)));
        let aux = graph.add_node(Box::new(Adder::constant("aux", log.clone(), 0.0)));
        let clock = graph.add_node(Box::new(EagerClock {
            type_id: EffectNodeType::new("test.eager_clock"),
            outputs: vec![output("out", PortType::Scalar(ScalarType::F32))],
            log: log.clone(),
        }));
        let mut sim = SimBoundary::new(log.clone(), count.clone());
        if opted {
            sim.ports = SIM_CLOCK_PORTS;
        }
        let boundary = graph.add_node(Box::new(sim));
        let add_dt = graph.add_node(Box::new(Adder::new("add_dt", log.clone())));
        let add_index = graph.add_node(Box::new(Adder::new("add_index", log.clone())));
        let consumer = graph.add_node(Box::new(Adder::root("consumer", log.clone())));
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((clock, "out"), (boundary, "clock")).unwrap();
        graph.connect((boundary, "out"), (add_dt, "a")).unwrap();
        graph.connect((boundary, "step_dt"), (add_dt, "b")).unwrap();
        graph.connect((add_dt, "out"), (add_index, "a")).unwrap();
        graph.connect((boundary, "step_index"), (add_index, "b")).unwrap();
        graph.connect((aux, "out"), (add_index, "c")).unwrap();
        graph.connect((add_index, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "a")).unwrap();
        let plan = compile(&graph).unwrap();
        assert_eq!(plan.substep_regions()[0].clock, opted.then_some(clock));
        SimFixture { graph, plan, log, count, aux }
    }

    fn sim_fixture() -> SimFixture {
        let log: Log = Arc::default();
        let count = Arc::new(Mutex::new(3));
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Adder::constant("src", log.clone(), 1.0)));
        let aux = graph.add_node(Box::new(Adder::constant("aux", log.clone(), 0.0)));
        let boundary =
            graph.add_node(Box::new(SimBoundary::new(log.clone(), count.clone())));
        let add_dt = graph.add_node(Box::new(Adder::new("add_dt", log.clone())));
        let add_index = graph.add_node(Box::new(Adder::new("add_index", log.clone())));
        let consumer = graph.add_node(Box::new(Adder::root("consumer", log.clone())));
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (add_dt, "a")).unwrap();
        graph.connect((boundary, "step_dt"), (add_dt, "b")).unwrap();
        graph.connect((add_dt, "out"), (add_index, "a")).unwrap();
        graph.connect((boundary, "step_index"), (add_index, "b")).unwrap();
        graph.connect((aux, "out"), (add_index, "c")).unwrap();
        graph.connect((add_index, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "a")).unwrap();
        let plan = compile(&graph).unwrap();
        assert_eq!(plan.substep_regions().len(), 1);
        SimFixture {
            graph,
            plan,
            log,
            count,
            aux,
        }
    }

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn run_frame(fx: &mut SimFixture, exec: &mut Executor, count: u32) -> Vec<String> {
        *fx.count.lock().unwrap() = count;
        fx.log.lock().unwrap().clear();
        exec.execute_frame(&mut fx.graph, &fx.plan, frame_time());
        fx.log.lock().unwrap().clone()
    }

    #[test]
    fn substeps_count_order_and_zero_steps() {
        let mut fx = sim_fixture();
        let mut exec = Executor::with_mock();
        let log = run_frame(&mut fx, &mut exec, 3);
        let body: Vec<&str> = log
            .iter()
            .map(String::as_str)
            .filter(|e| !matches!(*e, "src" | "aux"))
            .collect();
        assert_eq!(
            body,
            vec![
                "boundary",
                "add_dt a=1 b=0.5 c=none",
                "add_index a=1.5 b=0 c=0",
                "capture 1.5",
                "add_dt a=1.5 b=0.5 c=none",
                "add_index a=2 b=1 c=0",
                "capture 3",
                "add_dt a=3 b=0.5 c=none",
                "add_index a=3.5 b=2 c=0",
                "capture 5.5",
                "consumer a=5.5 b=0 c=none",
            ]
        );

        // Zero iterations: the body never runs and the boundary's own
        // publication of the accepted state reaches the consumer.
        let log = run_frame(&mut fx, &mut exec, 0);
        let body: Vec<&str> = log
            .iter()
            .map(String::as_str)
            .filter(|e| !matches!(*e, "src" | "aux"))
            .collect();
        assert_eq!(body, vec!["boundary", "consumer a=5.5 b=0 c=none"]);
    }

    #[test]
    fn substeps_final_state_escapes() {
        let mut fx = sim_fixture();
        let mut exec = Executor::with_mock();
        let first = run_frame(&mut fx, &mut exec, 3);
        assert_eq!(first.last().unwrap(), "consumer a=5.5 b=0 c=none");
        // The next frame starts from the accepted final state:
        // 5.5 → 6.0 → 7.5 → 10.0.
        let second = run_frame(&mut fx, &mut exec, 3);
        assert_eq!(second.last().unwrap(), "consumer a=10 b=0 c=none");
    }

    #[test]
    fn substeps_no_recycle_between_iterations() {
        let mut fx = sim_fixture();
        let mut exec = Executor::with_mock();
        let aux_out = fx
            .plan
            .steps()
            .iter()
            .find(|s| s.node == fx.aux)
            .and_then(|s| s.outputs.first())
            .map(|&(_, r)| r)
            .unwrap();
        let region = fx.plan.substep_regions()[0].clone();
        assert!(region.held_resources.contains(&aux_out));
        let mut slot_counts = Vec::new();
        for _ in 0..3 {
            let log = run_frame(&mut fx, &mut exec, 4);
            // An outside input read inside the body stays bound for every
            // iteration: a mid-region free would unbind it (`c=none`).
            let reads: Vec<&String> =
                log.iter().filter(|e| e.starts_with("add_index")).collect();
            assert_eq!(reads.len(), 4);
            assert!(reads.iter().all(|e| e.ends_with("c=0")), "{reads:?}");
            // Released once the region ends.
            assert!(exec.backend().slot_for(aux_out).is_none());
            slot_counts.push(exec.backend().slot_count());
        }
        assert!(
            slot_counts.windows(2).all(|w| w[0] == w[1]),
            "slot count grew across frames: {slot_counts:?}"
        );
    }

    #[test]
    fn substeps_execute_post_once() {
        let mut fx = sim_fixture();
        let mut exec = Executor::with_mock();
        let log = run_frame(&mut fx, &mut exec, 5);
        let count = |prefix: &str| log.iter().filter(|e| e.starts_with(prefix)).count();
        assert_eq!(count("boundary"), 1);
        assert_eq!(count("consumer"), 1);
        assert_eq!(count("src"), 1);
        assert_eq!(count("aux"), 1);
        assert_eq!(count("add_dt"), 5);
        assert_eq!(count("add_index"), 5);
        assert_eq!(count("capture"), 5);
    }

    #[test]
    fn substeps_physics_sample_never_advances_region() {
        let mut fx = sim_fixture();
        let mut exec = Executor::with_mock();
        run_frame(&mut fx, &mut exec, 3);
        fx.log.lock().unwrap().clear();
        let n = fx.plan.steps().len();
        let params: Vec<Option<crate::node_graph::effect_node::ParamValues>> =
            (0..n).map(|_| Some(Default::default())).collect();
        exec.execute_physics_sample_frame(
            &mut fx.graph,
            &fx.plan,
            frame_time(),
            &vec![true; n],
            &params,
        );
        let log = fx.log.lock().unwrap().clone();
        assert!(
            log.iter()
                .all(|e| !e.starts_with("boundary") && !e.starts_with("add_") && !e.starts_with("capture")),
            "{log:?}"
        );
        // The live state is untouched by the sample.
        let next = run_frame(&mut fx, &mut exec, 0);
        assert_eq!(next.last().unwrap(), "consumer a=5.5 b=0 c=none");
    }

    // ─── Host syncs: opt-in per boundary, offline only ───

    fn region_events(log: &[String]) -> Vec<&str> {
        log.iter()
            .map(String::as_str)
            .filter(|e| e.starts_with("capture") || e.starts_with("host") || e.starts_with("add_index"))
            .collect()
    }

    #[test]
    fn substeps_host_sync_runs_offline_between_iterations() {
        let _export = crate::node_graph::physics::PhysicsStepScope::for_render(true);
        let mut fx = clock_fixture(true);
        let mut exec = Executor::with_mock();
        let log = run_frame(&mut fx, &mut exec, 3);
        assert_eq!(
            region_events(&log),
            vec![
                "add_index a=1.5 b=0 c=0",
                "capture 1.5",
                "host 1",
                "add_index a=2 b=1 c=0",
                "capture 3",
                "host 2",
                "add_index a=3.5 b=2 c=0",
                "capture 5.5",
            ]
        );
        assert_eq!(exec.substep_host_syncs(), 2);
    }

    /// Live never waits: an opted-in region with an eager clock owner
    /// encodes the whole frame without a mid-region commit.
    #[test]
    fn substeps_host_sync_never_runs_live() {
        let _live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
        let mut fx = clock_fixture(true);
        let mut exec = Executor::with_mock();
        let log = run_frame(&mut fx, &mut exec, 3);
        assert!(log.iter().all(|e| !e.starts_with("host")), "{log:?}");
        assert_eq!(log.last().unwrap(), "consumer a=5.5 b=0 c=none");
        assert_eq!(exec.substep_host_syncs(), 0);
    }

    /// A boundary that has not opted in never commits or waits mid-region,
    /// even during export and with an eager clock node wired to it.
    #[test]
    fn substeps_host_sync_off_by_default_in_export() {
        let _export = crate::node_graph::physics::PhysicsStepScope::for_render(true);
        let mut fx = clock_fixture(false);
        let mut exec = Executor::with_mock();
        let log = run_frame(&mut fx, &mut exec, 3);
        assert!(log.iter().all(|e| !e.starts_with("host")), "{log:?}");
        assert_eq!(log.last().unwrap(), "consumer a=5.5 b=0 c=none");
        assert_eq!(exec.substep_host_syncs(), 0);
    }

    #[test]
    fn substeps_region_unwired_clock_port_rejected() {
        let log: Log = Arc::default();
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Adder::constant("src", log.clone(), 1.0)));
        let mut sim = SimBoundary::new(log.clone(), Arc::new(Mutex::new(1)));
        sim.ports = SIM_CLOCK_PORTS;
        let boundary = graph.add_node(Box::new(sim));
        let body = graph.add_node(Box::new(Adder::new("body", log.clone())));
        let consumer = graph.add_node(Box::new(Adder::root("consumer", log)));
        graph.connect((src, "out"), (boundary, "seed")).unwrap();
        graph.connect((boundary, "out"), (body, "a")).unwrap();
        graph.connect((body, "out"), (boundary, "in")).unwrap();
        graph.connect((boundary, "out"), (consumer, "a")).unwrap();
        let err = compile(&graph).unwrap_err();
        assert!(
            matches!(&err, GraphError::MalformedSubstepRegion { reason, .. } if reason.contains("clock port")),
            "{err:?}"
        );
    }

    // ─── Freeze never fuses across the border ───

    #[test]
    fn substeps_freeze_never_fuses_across_border() {
        use crate::node_graph::PrimitiveRegistry;
        use crate::node_graph::freeze::region::partition_regions;
        use super::test_nodes::register_substep_test_nodes;

        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        // `outside` (a force atom on the seed particles) feeds `inner_a`'s
        // forces through a coincident array wire; both are fusable and the
        // merge is convex, so only the border gate keeps them apart.
        // `inner_a → inner_b` is inside the body.
        let def: manifold_core::effect_graph_def::EffectGraphDef =
            serde_json::from_value(serde_json::json!({
                "version": 3,
                "nodes": [
                    {"id": 0, "nodeId": "seed", "typeId": "test.particle_source"},
                    {"id": 1, "nodeId": "forces", "typeId": "test.force_source"},
                    {"id": 2, "nodeId": "boundary", "typeId": "test.particle_boundary"},
                    {"id": 3, "nodeId": "inner_a", "typeId": "node.move_particles_3d"},
                    {"id": 4, "nodeId": "inner_b", "typeId": "node.move_particles_3d"},
                    {"id": 5, "nodeId": "outside", "typeId": "node.push_from_walls_3d"},
                    {"id": 6, "nodeId": "sink", "typeId": "test.particle_sink"},
                    {"id": 7, "nodeId": "output", "typeId": "system.final_output"}
                ],
                "wires": [
                    {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "seed"},
                    {"fromNode": 1, "fromPort": "out", "toNode": 5, "toPort": "in"},
                    {"fromNode": 0, "fromPort": "out", "toNode": 5, "toPort": "particles"},
                    {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in"},
                    {"fromNode": 5, "fromPort": "out", "toNode": 3, "toPort": "forces"},
                    {"fromNode": 2, "fromPort": "step_dt", "toNode": 3, "toPort": "speed"},
                    {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"},
                    {"fromNode": 1, "fromPort": "out", "toNode": 4, "toPort": "forces"},
                    {"fromNode": 2, "fromPort": "step_index", "toNode": 4, "toPort": "speed"},
                    {"fromNode": 4, "fromPort": "out", "toNode": 2, "toPort": "in"},
                    {"fromNode": 2, "fromPort": "out", "toNode": 6, "toPort": "particles"},
                    {"fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "in"}
                ]
            }))
            .unwrap();
        let regions = partition_regions(&def, &registry);
        let region_of = |id: u32| regions.iter().position(|r| r.members.iter().any(|m| m.doc_id == id));
        // The body pair fuses; the outside atom never joins it.
        assert!(region_of(3).is_some(), "the body pair should fuse");
        assert_eq!(region_of(3), region_of(4));
        assert_ne!(region_of(5), region_of(3));

        // Control: the same force atom → mover wire with no boundary fuses,
        // so the border gate is what kept them apart above.
        let control: manifold_core::effect_graph_def::EffectGraphDef =
            serde_json::from_value(serde_json::json!({
                "version": 3,
                "nodes": [
                    {"id": 0, "nodeId": "seed", "typeId": "test.particle_source"},
                    {"id": 1, "nodeId": "forces", "typeId": "test.force_source"},
                    {"id": 3, "nodeId": "mover", "typeId": "node.move_particles_3d"},
                    {"id": 5, "nodeId": "outside", "typeId": "node.push_from_walls_3d"},
                    {"id": 6, "nodeId": "sink", "typeId": "test.particle_sink"},
                    {"id": 7, "nodeId": "output", "typeId": "system.final_output"}
                ],
                "wires": [
                    {"fromNode": 1, "fromPort": "out", "toNode": 5, "toPort": "in"},
                    {"fromNode": 0, "fromPort": "out", "toNode": 5, "toPort": "particles"},
                    {"fromNode": 0, "fromPort": "out", "toNode": 3, "toPort": "in"},
                    {"fromNode": 5, "fromPort": "out", "toNode": 3, "toPort": "forces"},
                    {"fromNode": 3, "fromPort": "out", "toNode": 6, "toPort": "particles"},
                    {"fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "in"}
                ]
            }))
            .unwrap();
        let regions = partition_regions(&control, &registry);
        let fused_together = regions.iter().any(|r| {
            r.members.iter().any(|m| m.doc_id == 3) && r.members.iter().any(|m| m.doc_id == 5)
        });
        assert!(fused_together, "control: the force atom and mover should fuse");
    }

    // ─── Nested regions: executor and freeze (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P6) ───
    //
    // ```text
    // src ─▶ outer.seed                     outer.step_index ─▶ pre.b
    // outer.out ─▶ pre.a ─▶ inner.seed      outer.step_dt ─▶ z.a     aux ─▶ z.b
    // inner.out ─▶ in_a.a ─▶ in_b.a ─▶ (capture) inner.in
    // inner.step_index ─▶ in_a.b    z.out ─▶ in_a.c    inner.step_dt ─▶ in_b.b
    // inner.out ─▶ post.a ─▶ (capture) outer.in       outer.step_dt ─▶ post.b
    // outer.out ─▶ consumer.a
    // ```
    //
    // Per outer iteration o: `pre = s + o`, `z = 0.5`; the inner region
    // restarts from `pre` and runs `c = c + i + z + 0.25` per inner iteration
    // i; `post = c + 0.5` becomes the outer state. The outer boundary serves
    // step_dt 0.5, the inner one 0.25, so a level reading the other's
    // scalars shows in the log.

    struct NestFixture {
        graph: Graph,
        plan: crate::node_graph::ExecutionPlan,
        log: Log,
        outer_count: Arc<Mutex<u32>>,
        inner_count: Arc<Mutex<u32>>,
        aux: NodeInstanceId,
        z: NodeInstanceId,
    }

    fn nest_fixture(outer_clock: bool) -> NestFixture {
        let log: Log = Arc::default();
        let outer_count = Arc::new(Mutex::new(2));
        let inner_count = Arc::new(Mutex::new(3));
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Adder::constant("src", log.clone(), 1.0)));
        let aux = graph.add_node(Box::new(Adder::constant("aux", log.clone(), 0.0)));
        let mut outer_node = SimBoundary::labelled(log.clone(), outer_count.clone(), "outer", 0.5, false);
        if outer_clock {
            outer_node.ports = SIM_CLOCK_PORTS;
        }
        let outer = graph.add_node(Box::new(outer_node));
        let pre = graph.add_node(Box::new(Adder::new("pre", log.clone())));
        let z = graph.add_node(Box::new(Adder::new("z", log.clone())));
        let inner = graph.add_node(Box::new(SimBoundary::labelled(
            log.clone(),
            inner_count.clone(),
            "inner",
            0.25,
            true,
        )));
        let in_a = graph.add_node(Box::new(Adder::new("in_a", log.clone())));
        let in_b = graph.add_node(Box::new(Adder::new("in_b", log.clone())));
        let post = graph.add_node(Box::new(Adder::new("post", log.clone())));
        let consumer = graph.add_node(Box::new(Adder::root("consumer", log.clone())));
        if outer_clock {
            let clock = graph.add_node(Box::new(EagerClock {
                type_id: EffectNodeType::new("test.eager_clock"),
                outputs: vec![output("out", PortType::Scalar(ScalarType::F32))],
                log: log.clone(),
            }));
            graph.connect((clock, "out"), (outer, "clock")).unwrap();
        }
        graph.connect((src, "out"), (outer, "seed")).unwrap();
        graph.connect((outer, "out"), (pre, "a")).unwrap();
        graph.connect((outer, "step_index"), (pre, "b")).unwrap();
        graph.connect((pre, "out"), (inner, "seed")).unwrap();
        graph.connect((outer, "step_dt"), (z, "a")).unwrap();
        graph.connect((aux, "out"), (z, "b")).unwrap();
        graph.connect((inner, "out"), (in_a, "a")).unwrap();
        graph.connect((inner, "step_index"), (in_a, "b")).unwrap();
        graph.connect((z, "out"), (in_a, "c")).unwrap();
        graph.connect((in_a, "out"), (in_b, "a")).unwrap();
        graph.connect((inner, "step_dt"), (in_b, "b")).unwrap();
        graph.connect((in_b, "out"), (inner, "in")).unwrap();
        graph.connect((inner, "out"), (post, "a")).unwrap();
        graph.connect((outer, "step_dt"), (post, "b")).unwrap();
        graph.connect((post, "out"), (outer, "in")).unwrap();
        graph.connect((outer, "out"), (consumer, "a")).unwrap();
        let plan = compile(&graph).unwrap();
        let regions = plan.substep_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].boundary, outer);
        assert_eq!(regions[0].inner.len(), 1);
        assert_eq!(regions[0].inner[0].boundary, inner);
        NestFixture {
            graph,
            plan,
            log,
            outer_count,
            inner_count,
            aux,
            z,
        }
    }

    fn run_nest(fx: &mut NestFixture, exec: &mut Executor, outer: u32, inner: u32) -> Vec<String> {
        *fx.outer_count.lock().unwrap() = outer;
        *fx.inner_count.lock().unwrap() = inner;
        fx.log.lock().unwrap().clear();
        exec.execute_frame(&mut fx.graph, &fx.plan, frame_time());
        fx.log.lock().unwrap().clone()
    }

    /// The outer state after `frames` frames, computed on the CPU.
    fn nest_expected(frames: u32, outer: u32, inner: u32) -> f32 {
        let mut state = 1.0f32;
        for _ in 0..frames {
            for o in 0..outer {
                let mut c = state + o as f32;
                for i in 0..inner {
                    c = c + i as f32 + 0.5;
                    c += 0.25;
                }
                state = c + 0.5;
            }
        }
        state
    }

    fn without(log: &[String], prefixes: &[&str]) -> Vec<String> {
        log.iter()
            .filter(|e| !prefixes.iter().any(|p| e.starts_with(p)))
            .cloned()
            .collect()
    }

    #[test]
    fn nested_region_runs_inner_per_outer_iteration() {
        let mut fx = nest_fixture(false);
        let mut exec = Executor::with_mock();
        let log = run_nest(&mut fx, &mut exec, 2, 3);
        assert_eq!(
            without(&log, &["src", "aux", "z "]),
            vec![
                "outer",
                "pre a=1 b=0 c=none",
                "inner",
                "in_a a=1 b=0 c=0.5",
                "in_b a=1.5 b=0.25 c=none",
                "inner capture 1.75",
                "in_a a=1.75 b=1 c=0.5",
                "in_b a=3.25 b=0.25 c=none",
                "inner capture 3.5",
                "in_a a=3.5 b=2 c=0.5",
                "in_b a=6 b=0.25 c=none",
                "inner capture 6.25",
                "post a=6.25 b=0.5 c=none",
                "outer capture 6.75",
                "pre a=6.75 b=1 c=none",
                "inner",
                "in_a a=7.75 b=0 c=0.5",
                "in_b a=8.25 b=0.25 c=none",
                "inner capture 8.5",
                "in_a a=8.5 b=1 c=0.5",
                "in_b a=10 b=0.25 c=none",
                "inner capture 10.25",
                "in_a a=10.25 b=2 c=0.5",
                "in_b a=12.75 b=0.25 c=none",
                "inner capture 13",
                "post a=13 b=0.5 c=none",
                "outer capture 13.5",
                "consumer a=13.5 b=0 c=none",
            ]
        );
        let z_runs = log.iter().filter(|e| e.starts_with("z ")).count();
        assert_eq!(z_runs, 2, "z runs once per outer iteration: {log:?}");
        assert_eq!(nest_expected(1, 2, 3), 13.5);

        // The outer state carries across frames.
        let log = run_nest(&mut fx, &mut exec, 2, 3);
        assert_eq!(log.last().unwrap(), &format!("consumer a={} b=0 c=none", nest_expected(2, 2, 3)));

        // No inner iterations: the inner boundary still restarts from `pre`
        // each outer iteration and `post` reads that.
        let log = run_nest(&mut fx, &mut exec, 1, 0);
        let s = nest_expected(2, 2, 3);
        assert_eq!(
            without(&log, &["src", "aux", "z "]),
            vec![
                "outer".to_string(),
                format!("pre a={s} b=0 c=none"),
                "inner".to_string(),
                format!("post a={s} b=0.5 c=none"),
                format!("outer capture {}", s + 0.5),
                format!("consumer a={} b=0 c=none", s + 0.5),
            ]
        );

        // No outer iterations: nothing in the outer body runs.
        let log = run_nest(&mut fx, &mut exec, 0, 3);
        assert_eq!(
            without(&log, &["src", "aux"]),
            vec!["outer".to_string(), format!("consumer a={} b=0 c=none", s + 0.5)]
        );
    }

    /// What the inner body reads from the outer body, and what the outer body
    /// reads from outside, stays bound through every iteration of both
    /// levels and is released once the outer region ends.
    #[test]
    fn nested_region_no_recycle_across_iterations() {
        let mut fx = nest_fixture(false);
        let mut exec = Executor::with_mock();
        let output_of = |fx: &NestFixture, node| {
            fx.plan
                .steps()
                .iter()
                .find(|s| s.node == node)
                .and_then(|s| s.outputs.first())
                .map(|&(_, r)| r)
                .unwrap()
        };
        let (aux_out, z_out) = (output_of(&fx, fx.aux), output_of(&fx, fx.z));
        let region = &fx.plan.substep_regions()[0];
        assert!(region.held_resources.contains(&aux_out) && region.held_resources.contains(&z_out));
        let mut slot_counts = Vec::new();
        for _ in 0..3 {
            let log = run_nest(&mut fx, &mut exec, 3, 4);
            let inner_reads: Vec<&String> = log.iter().filter(|e| e.starts_with("in_a")).collect();
            assert_eq!(inner_reads.len(), 12);
            assert!(inner_reads.iter().all(|e| e.ends_with("c=0.5")), "{inner_reads:?}");
            let outer_reads: Vec<&String> = log.iter().filter(|e| e.starts_with("z ")).collect();
            assert_eq!(outer_reads.len(), 3);
            assert!(outer_reads.iter().all(|e| e.as_str() == "z a=0.5 b=0 c=none"), "{outer_reads:?}");
            assert!(exec.backend().slot_for(aux_out).is_none());
            assert!(exec.backend().slot_for(z_out).is_none());
            slot_counts.push(exec.backend().slot_count());
        }
        assert!(
            slot_counts.windows(2).all(|w| w[0] == w[1]),
            "slot count grew across frames: {slot_counts:?}"
        );
    }

    #[test]
    fn nested_region_host_sync_only_between_outer_iterations() {
        let _export = crate::node_graph::physics::PhysicsStepScope::for_render(true);
        let mut fx = nest_fixture(true);
        let mut exec = Executor::with_mock();
        let log = run_nest(&mut fx, &mut exec, 3, 3);
        let events: Vec<&str> = log
            .iter()
            .map(String::as_str)
            .filter(|e| e.starts_with("host") || e.starts_with("pre") || e.contains("capture"))
            .collect();
        let mut expected = Vec::new();
        for o in 0..3 {
            if o > 0 {
                expected.push(format!("host {o}"));
            }
            expected.push("pre".to_string());
            expected.extend(["inner capture"; 3].map(String::from));
            expected.push("outer capture".to_string());
        }
        let shapes: Vec<String> = events
            .iter()
            .map(|e| {
                if e.starts_with("host") {
                    e.to_string()
                } else {
                    e.rsplit_once(' ').map_or(e.to_string(), |(head, _)| head.to_string())
                }
            })
            .map(|e| if e.starts_with("pre") { "pre".to_string() } else { e })
            .collect();
        assert_eq!(shapes, expected);
        assert_eq!(exec.substep_host_syncs(), 2);
        // The same state as a run without syncs.
        assert_eq!(log.last().unwrap(), &format!("consumer a={} b=0 c=none", nest_expected(1, 3, 3)));
        drop(_export);

        let _live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
        let log = run_nest(&mut fx, &mut exec, 3, 3);
        assert!(log.iter().all(|e| !e.starts_with("host")), "{log:?}");
        assert_eq!(exec.substep_host_syncs(), 2, "live never syncs");
    }

    #[test]
    fn nested_region_truncation_keeps_inner_with_outer() {
        let fx = nest_fixture(false);
        let outer = &fx.plan.substep_regions()[0];
        let last = *outer.steps.last().unwrap();
        let whole = fx.plan.truncated(last + 1);
        assert_eq!(whole.substep_regions(), fx.plan.substep_regions());
        // A prefix past the inner region but inside the outer body drops
        // both: the inner region never survives without its outer one.
        let past_inner = *outer.inner[0].steps.last().unwrap() + 1;
        assert!(past_inner <= last);
        assert!(fx.plan.truncated(past_inner).substep_regions().is_empty());
    }

    /// ```text
    /// seed ─▶ outside ─▶ outer_a.forces
    /// outer.out ─▶ outer_a ─▶ outer_b ─▶ inner.seed
    /// outer.out ─▶ z.particles, z.out ─▶ inner_a.forces
    /// inner.out ─▶ inner_a ─▶ inner_b ─▶ (capture) inner.in
    /// inner.out ─▶ post_a ─▶ post_b ─▶ (capture) outer.in
    /// ```
    /// `outside` (no region), `z` (outer body) and `inner_a` (inner body)
    /// each meet a fusable neighbour across a border through a coincident
    /// array wire; only the border gates keep them apart.
    fn nested_particle_def() -> manifold_core::effect_graph_def::EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [
                {"id": 0, "nodeId": "seed", "typeId": "test.particle_source"},
                {"id": 1, "nodeId": "forces", "typeId": "test.force_source"},
                {"id": 2, "nodeId": "outer", "typeId": "test.particle_boundary"},
                {"id": 3, "nodeId": "outer_a", "typeId": "node.move_particles_3d"},
                {"id": 4, "nodeId": "outer_b", "typeId": "node.move_particles_3d"},
                {"id": 5, "nodeId": "inner", "typeId": "test.particle_inner_boundary"},
                {"id": 6, "nodeId": "z", "typeId": "node.push_from_walls_3d"},
                {"id": 7, "nodeId": "inner_a", "typeId": "node.move_particles_3d"},
                {"id": 8, "nodeId": "inner_b", "typeId": "node.move_particles_3d"},
                {"id": 9, "nodeId": "post_a", "typeId": "node.move_particles_3d"},
                {"id": 10, "nodeId": "post_b", "typeId": "node.move_particles_3d"},
                {"id": 11, "nodeId": "outside", "typeId": "node.push_from_walls_3d"},
                {"id": 12, "nodeId": "sink", "typeId": "test.particle_sink"},
                {"id": 13, "nodeId": "output", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "seed"},
                {"fromNode": 1, "fromPort": "out", "toNode": 11, "toPort": "in"},
                {"fromNode": 0, "fromPort": "out", "toNode": 11, "toPort": "particles"},
                {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in"},
                {"fromNode": 11, "fromPort": "out", "toNode": 3, "toPort": "forces"},
                {"fromNode": 2, "fromPort": "step_dt", "toNode": 3, "toPort": "speed"},
                {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"},
                {"fromNode": 1, "fromPort": "out", "toNode": 4, "toPort": "forces"},
                {"fromNode": 2, "fromPort": "step_index", "toNode": 4, "toPort": "speed"},
                {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "seed"},
                {"fromNode": 1, "fromPort": "out", "toNode": 6, "toPort": "in"},
                {"fromNode": 2, "fromPort": "out", "toNode": 6, "toPort": "particles"},
                {"fromNode": 5, "fromPort": "out", "toNode": 7, "toPort": "in"},
                {"fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "forces"},
                {"fromNode": 5, "fromPort": "step_dt", "toNode": 7, "toPort": "speed"},
                {"fromNode": 7, "fromPort": "out", "toNode": 8, "toPort": "in"},
                {"fromNode": 1, "fromPort": "out", "toNode": 8, "toPort": "forces"},
                {"fromNode": 5, "fromPort": "step_index", "toNode": 8, "toPort": "speed"},
                {"fromNode": 8, "fromPort": "out", "toNode": 5, "toPort": "in"},
                {"fromNode": 5, "fromPort": "out", "toNode": 9, "toPort": "in"},
                {"fromNode": 1, "fromPort": "out", "toNode": 9, "toPort": "forces"},
                {"fromNode": 2, "fromPort": "step_dt", "toNode": 9, "toPort": "speed"},
                {"fromNode": 9, "fromPort": "out", "toNode": 10, "toPort": "in"},
                {"fromNode": 1, "fromPort": "out", "toNode": 10, "toPort": "forces"},
                {"fromNode": 2, "fromPort": "step_index", "toNode": 10, "toPort": "speed"},
                {"fromNode": 10, "fromPort": "out", "toNode": 2, "toPort": "in"},
                {"fromNode": 2, "fromPort": "out", "toNode": 12, "toPort": "particles"},
                {"fromNode": 12, "fromPort": "out", "toNode": 13, "toPort": "in"}
            ]
        }))
        .unwrap()
    }

    fn particle_registry() -> crate::node_graph::PrimitiveRegistry {
        let mut registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        super::test_nodes::register_substep_test_nodes(&mut registry);
        registry
    }

    /// I15: fusion never crosses a region border, nested or not.
    #[test]
    fn nested_region_fusion_stays_inside() {
        use crate::node_graph::freeze::region::partition_regions;

        let registry = particle_registry();
        let def = nested_particle_def();
        // The def compiles as a nest: the border the finder must respect.
        let graph = crate::node_graph::EffectGraphDefExt::into_graph(def.clone(), &registry, &Default::default())
            .expect("nested def builds");
        let plan = compile(&graph).expect("nested def compiles");
        assert_eq!(plan.substep_regions().len(), 1);
        assert_eq!(plan.substep_regions()[0].inner.len(), 1);

        let regions = partition_regions(&def, &registry);
        let region_of = |id: u32| regions.iter().position(|r| r.members.iter().any(|m| m.doc_id == id));
        for pair in [(3, 4), (7, 8), (9, 10)] {
            assert!(region_of(pair.0).is_some(), "{pair:?} should fuse");
            assert_eq!(region_of(pair.0), region_of(pair.1), "{pair:?} should fuse together");
        }
        assert_ne!(region_of(6), region_of(7), "an outer-body atom joined the inner body");
        assert_ne!(region_of(11), region_of(3), "an outside atom joined the outer body");
        // Every fused kernel lies on one side of every border.
        let side = |id: u32| match id {
            3 | 4 | 6 | 9 | 10 => Some(2),
            7 | 8 => Some(5),
            _ => None,
        };
        for region in &regions {
            let sides: AHashSet<Option<u32>> = region.members.iter().map(|m| side(m.doc_id)).collect();
            assert_eq!(sides.len(), 1, "a fused kernel crosses a border: {:?}", region.members.iter().map(|m| m.doc_id).collect::<Vec<_>>());
        }

        // Control: the same force atom → mover wire with no boundaries
        // fuses, so the inner border is what kept `z` and `inner_a` apart.
        let control: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [
                {"id": 0, "nodeId": "seed", "typeId": "test.particle_source"},
                {"id": 1, "nodeId": "forces", "typeId": "test.force_source"},
                {"id": 6, "nodeId": "z", "typeId": "node.push_from_walls_3d"},
                {"id": 7, "nodeId": "mover", "typeId": "node.move_particles_3d"},
                {"id": 12, "nodeId": "sink", "typeId": "test.particle_sink"},
                {"id": 13, "nodeId": "output", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 1, "fromPort": "out", "toNode": 6, "toPort": "in"},
                {"fromNode": 0, "fromPort": "out", "toNode": 6, "toPort": "particles"},
                {"fromNode": 0, "fromPort": "out", "toNode": 7, "toPort": "in"},
                {"fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "forces"},
                {"fromNode": 7, "fromPort": "out", "toNode": 12, "toPort": "particles"},
                {"fromNode": 12, "fromPort": "out", "toNode": 13, "toPort": "in"}
            ]
        }))
        .unwrap();
        let regions = partition_regions(&control, &registry);
        assert!(
            regions.iter().any(|r| r.members.iter().any(|m| m.doc_id == 6) && r.members.iter().any(|m| m.doc_id == 7)),
            "control: the force atom and mover should fuse"
        );
    }

    /// [`nested_particle_def`] with three-copy chains of same-size
    /// temporaries before the nest (`seed` into `outer.seed`), in the outer
    /// body (`outer_b` into `inner.seed`) and after it (`outer.out` into the
    /// sink).
    pub(crate) fn nested_copy_chains_def() -> manifold_core::effect_graph_def::EffectGraphDef {
        let mut def = serde_json::to_value(nested_particle_def()).unwrap();
        let mut chain = |ids: [u64; 3], from: u64, to: (u64, &str)| {
            let mut previous = from;
            for id in ids {
                def["nodes"].as_array_mut().unwrap().push(
                    serde_json::json!({"id": id, "nodeId": format!("copy_{id}"), "typeId": "test.particle_copy"}),
                );
                def["wires"].as_array_mut().unwrap().push(
                    serde_json::json!({"fromNode": previous, "fromPort": "out", "toNode": id, "toPort": "in"}),
                );
                previous = id;
            }
            let wires = def["wires"].as_array_mut().unwrap();
            wires.retain(|w| !(w["toNode"] == to.0 && w["toPort"] == to.1));
            wires.push(serde_json::json!({"fromNode": previous, "fromPort": "out", "toNode": to.0, "toPort": to.1}));
        };
        chain([20, 21, 22], 0, (2, "seed"));
        chain([25, 26, 27], 4, (5, "seed"));
        chain([30, 31, 32], 2, (12, "particles"));
        serde_json::from_value(def).unwrap()
    }

    /// Temporary array reuse around and inside a nest. Every array a region
    /// step touches keeps its storage for the whole outer region, every
    /// iteration of both levels. Arrays before and after the nest still reuse
    /// each other.
    #[test]
    fn nested_region_arrays_keep_storage_across_iterations() {
        use crate::node_graph::resource_allocation::{lifetimes, plan_array_allocations, ArrayAllocationAction};

        let graph = crate::node_graph::EffectGraphDefExt::into_graph(nested_copy_chains_def(), &particle_registry(), &Default::default())
            .expect("nest with chains builds");
        let plan = compile(&graph).expect("nest with chains compiles");
        let region = &plan.substep_regions()[0];
        assert_eq!(region.inner.len(), 1);

        let planned = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).expect("plan allocates");
        let reused = lifetimes::assert_shared_roots_never_overlap(&graph, &plan, &planned);
        assert!(reused >= 3, "the chains before, inside and after the nest reuse: {reused}");
        // No array a region step touches ever hands its storage on.
        let (first, last) = (region.steps[0], *region.steps.last().unwrap());
        let touched: AHashSet<_> = region.steps.iter()
            .flat_map(|&s| plan.steps()[s].inputs.iter().chain(&plan.steps()[s].outputs).map(|(_, r)| *r))
            .filter(|r| planned.storage.contains_key(r))
            .collect();
        for step in &plan.steps()[last + 1..] {
            for (_, resource) in &step.outputs {
                let Some(storage) = planned.storage.get(resource) else { continue };
                assert!(
                    touched.iter().all(|t| planned.storage[t].root != storage.root),
                    "{resource:?} after the nest takes storage a region array still names"
                );
            }
        }
        // A region array that reuses storage takes it from before the region.
        let mut inside = 0;
        for step in &plan.steps()[first..=last] {
            let declared = graph.get_node(step.node).unwrap().node.aliased_array_io();
            for (port, resource) in &step.outputs {
                if declared.iter().any(|(_, out)| out == port) {
                    continue;
                }
                if let Some(ArrayAllocationAction::Alias { input, .. }) = planned.actions.iter()
                    .find(|a| matches!(a, ArrayAllocationAction::Alias { resource: r, .. } if r == resource))
                {
                    let owner = plan.steps().iter().position(|s| s.outputs.iter().any(|(_, r)| r == input)).unwrap();
                    assert!(owner < first, "{resource:?} reuses storage released inside the region");
                    inside += 1;
                }
            }
        }
        assert!(inside > 0, "the outer body's copies take storage the pre-nest chain released");

        // The oracle has teeth: a body copy placed in the storage of the array
        // the outer boundary seeds from is live across every iteration.
        let output = |id: u32| {
            let node = graph.nodes().find(|n| n.node_id.as_str() == format!("copy_{id}")).unwrap().id;
            plan.steps().iter().find(|s| s.node == node).unwrap().outputs[0].1
        };
        let mut broken = planned.clone();
        let seed_root = broken.storage[&output(22)].root;
        broken.storage.get_mut(&output(26)).unwrap().root = seed_root;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lifetimes::assert_shared_roots_never_overlap(&graph, &plan, &broken)
        }));
        assert!(caught.is_err(), "a body array in storage the region still reads must be refused");
    }

    /// The fused-def cache is keyed by the def's content, and the nest is a
    /// function of the def: the same atoms arranged as two sibling regions
    /// instead of a nest key differently and fuse differently.
    #[test]
    fn nested_region_freeze_key_includes_nesting() {
        use crate::node_graph::freeze::install::def_content_key;
        use crate::node_graph::freeze::region::partition_regions;

        let registry = particle_registry();
        let nested = nested_particle_def();
        // Siblings: the inner region seeds from the particle source instead
        // of the outer body, and `post_a` reads `outer_b` instead of the
        // inner state, so the inner region leaves the outer body.
        let mut siblings = nested.clone();
        for wire in &mut siblings.wires {
            match (wire.from_node, wire.to_node, wire.to_port.as_str()) {
                (4, 5, "seed") => wire.from_node = 0,
                (5, 9, "in") => wire.from_node = 4,
                _ => {}
            }
        }
        assert_ne!(def_content_key(&nested), def_content_key(&siblings));
        let members = |def| {
            let mut regions: Vec<Vec<u32>> = partition_regions(def, &registry)
                .iter()
                .map(|r| {
                    let mut ids: Vec<u32> = r.members.iter().map(|m| m.doc_id).collect();
                    ids.sort();
                    ids
                })
                .collect();
            regions.sort();
            regions
        };
        assert_ne!(members(&nested), members(&siblings));
    }
}
