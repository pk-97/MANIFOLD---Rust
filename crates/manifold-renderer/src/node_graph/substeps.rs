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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstepRegion {
    pub boundary: NodeInstanceId,
    pub steps: Vec<usize>,
    pub held_resources: Vec<ResourceId>,
    pub clock: Option<NodeInstanceId>,
}

/// Node-level result of region derivation: the boundary first, then its body
/// in topological order. `compile` maps it to step indices.
#[derive(Debug, Clone)]
pub(crate) struct RegionNodes {
    pub boundary: NodeInstanceId,
    pub nodes: Vec<NodeInstanceId>,
    pub clock: Option<NodeInstanceId>,
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

        let members = region_body(boundary, &producers, &fwd, &rev);
        let body: Vec<NodeInstanceId> = active_order
            .iter()
            .copied()
            .filter(|id| members.contains(id))
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
        nodes.extend(body);
        regions.push(RegionNodes { boundary, nodes, clock });
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
    /// [`particle_step_dt`], `step_index` the iteration.
    struct ParticleBoundary {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        params: Vec<ParamDef>,
        seeded: bool,
        pending: u32,
    }

    impl ParticleBoundary {
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
            if !self.seeded {
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
        registry.register("test.value_source", || {
            Box::new(ArraySource::new("test.value_source", ArrayType::of_known::<f32>()))
        });
        registry.register("test.value_sink", || {
            Box::new(ParticleSink {
                type_id: EffectNodeType::new("test.value_sink"),
                inputs: vec![port(
                    "values",
                    PortType::Array(ArrayType::of_known::<f32>()),
                    PortKind::Input,
                    true,
                )],
                outputs: vec![port("out", PortType::Texture2D, PortKind::Output, false)],
                params: Vec::new(),
            })
        });
        registry.register("test.liquid_sink", || {
            Box::new(ParticleSink {
                type_id: EffectNodeType::new("test.liquid_sink"),
                inputs: vec![port(
                    "particles",
                    PortType::Array(ArrayType::of_known::<crate::node_graph::fluid_particles::FluidParticle>()),
                    PortKind::Input,
                    true,
                )],
                outputs: vec![port("out", PortType::Texture2D, PortKind::Output, false)],
                params: Vec::new(),
            })
        });
        registry.register("test.particle_boundary", || Box::new(ParticleBoundary::new()));
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

    /// Two simulation steps per frame, each with its own Krylov region
    /// (`docs/FFT_WATER_SOLVER_DESIGN.md` D8): the second region is seeded
    /// through a node outside both regions, which is not chaining.
    #[test]
    fn substeps_region_two_regions_in_sequence_through_an_outside_node() {
        let mut graph = Graph::new();
        let b1 = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body1 = pass(&mut graph, "body1");
        let between = pass(&mut graph, "between");
        let b2 = graph.add_node(Box::new(TestNode::boundary(PORTS)));
        let body2 = pass(&mut graph, "body2");
        let consumer = sink(&mut graph, "consumer");
        graph.connect((b1, "out"), (body1, "a")).unwrap();
        graph.connect((body1, "out"), (b1, "in")).unwrap();
        graph.connect((b1, "out"), (between, "a")).unwrap();
        graph.connect((between, "out"), (b2, "seed")).unwrap();
        graph.connect((between, "out"), (body2, "b")).unwrap();
        graph.connect((b2, "out"), (body2, "a")).unwrap();
        graph.connect((body2, "out"), (b2, "in")).unwrap();
        graph.connect((b2, "out"), (consumer, "tex")).unwrap();

        let plan = compile(&graph).unwrap();
        let order: Vec<NodeInstanceId> = plan.steps().iter().map(|s| s.node).collect();
        assert_eq!(order, vec![b1, body1, between, b2, body2, consumer]);
        let regions = plan.substep_regions();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].steps, vec![0, 1]);
        assert_eq!(regions[1].steps, vec![3, 4]);
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
    }

    impl SimBoundary {
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
            self.log.lock().unwrap().push("boundary".into());
            if !self.seeded {
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
            scalars[0] = 0.5;
            scalars[1] = iteration as f32;
            true
        }
        fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let candidate = scalar_in(ctx, "in").expect("capture slot bound");
            self.log.lock().unwrap().push(format!("capture {candidate}"));
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
}
