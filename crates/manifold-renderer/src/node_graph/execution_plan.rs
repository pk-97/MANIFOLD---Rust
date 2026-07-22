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

use crate::node_graph::effect_node::{intern_name, NodeInstanceId, NodeRequires, NodeWire};
use crate::node_graph::graph::Graph;
use crate::node_graph::ports::PortType;
use crate::node_graph::validation::{GraphError, topological_sort, validate};

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
    /// declared input ports. `&'static str` (interned from the port's
    /// `Cow` name via `intern_name` at plan-build) so the per-frame executor
    /// reads names with zero allocation.
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
    /// Producer-declared texture format for each `Texture2D` resource,
    /// queried at compile time from the producing node's
    /// [`EffectNode::output_format`](crate::node_graph::EffectNode::output_format).
    /// `None` means "use the backend's default format" (the common case).
    /// Indexed by `ResourceId`, parallel to `resource_types`.
    ///
    /// The runtime threads this through [`Backend::acquire`] / [`Backend::release`]
    /// so the backend's slot pool keys on `(PortType, GpuTextureFormat)` —
    /// preventing a freed rgba16float slot from aliasing into a fresh
    /// rgba32float acquire.
    resource_formats: Vec<Option<manifold_gpu::GpuTextureFormat>>,
    /// Resources whose producer declared
    /// [`EffectNode::output_mipmapped`](crate::node_graph::EffectNode::output_mipmapped)
    /// (`Texture2D` only). The executor installs this list into the
    /// backend via [`Backend::declare_mipmapped`] before any acquire, so
    /// the slot pool keys mip-chained slots apart from flat ones — a flat
    /// consumer recycled into a mipped slot would sample stale mip tails.
    /// Kept as an id list (almost always empty or tiny) rather than a
    /// parallel `Vec<bool>`. IMPORT_FIDELITY F-P6.
    mipmapped_resources: Vec<ResourceId>,
    /// Producer-declared texture dims for each `Texture2D` resource,
    /// queried at compile time. `None` means "use the backend's canvas
    /// dims at acquire time" — the common case for full-frame
    /// primitives. `Some((w, h))` is set when the producer explicitly
    /// downsamples (e.g. `node.downsample`) or when a default-policy
    /// propagation pulled a non-canvas dim from a parent texture
    /// input. Indexed by `ResourceId`, parallel to `resource_types`.
    ///
    /// The runtime resolves `None` against `backend.canvas_dims()`
    /// before calling `Backend::acquire`, so the slot pool key
    /// `(PortType, format, dims)` is fully concrete — canvas-sized
    /// slots pool together regardless of whether they were
    /// implicitly canvas (None) or explicitly so.
    resource_dims: Vec<Option<(u32, u32)>>,
    /// Canvas-relative dim hint per resource, as `(num, den)`. Only
    /// populated when [`resource_dims`] is `None` (the concrete dim
    /// couldn't be resolved at compile time — typically because the
    /// producing node fed from a state-capture back-edge). At slot
    /// acquire the executor resolves this to
    /// `(canvas_w * num / den, canvas_h * num / den)`, so primitives
    /// like `node.downsample` land their output at the right
    /// fraction of canvas even when their input dim is unknown at
    /// plan time. `None` here means "use the canvas-sized fallback"
    /// (the existing behaviour for textures with no producer hint).
    ///
    /// Read-priority order at slot acquire:
    ///   1. [`resource_dims`] — concrete `(w, h)` from the producer.
    ///   2. `resource_canvas_scales` — canvas-relative fraction.
    ///   3. `backend.canvas_dims()` — full canvas fallback.
    ///
    /// Sourced from
    /// [`EffectNode::output_canvas_scale`](crate::node_graph::EffectNode::output_canvas_scale)
    /// at compile time. Indexed by `ResourceId`, parallel to
    /// `resource_types` / `resource_formats` / `resource_dims`.
    resource_canvas_scales: Vec<Option<(u32, u32)>>,
    /// Union of every node's [`NodeRequires`] declaration. The
    /// executor's entry point checks this against what it can
    /// provide (encoder, state store) and panics with a clean
    /// message before evaluating any step.
    requires: NodeRequires,
    /// Resources whose lifetime spans frame boundaries — wires that
    /// terminate on a [`breaks_dependency_cycle`](crate::node_graph::effect_node::EffectNode::breaks_dependency_cycle)
    /// node carry next-frame state through the same physical buffer.
    /// They are excluded from every step's `free_after` (so the pool
    /// never recycles their slots), and must be acquired BEFORE step
    /// 0 each frame — the reading step (the marked node) runs first
    /// in topo order, and its `backend.slot_for` would otherwise panic
    /// because the producer that writes the resource runs LATER this
    /// frame. The executor walks this list and calls `Backend::acquire`
    /// (idempotent on existing bindings) before the step loop.
    persistent_resources: Vec<ResourceId>,
    /// Resources held across frames by the memoized-dataflow (constant-
    /// subgraph hoisting) path: every output of a hoistable step. The
    /// executor serves these from their latched slots on memo-skip frames
    /// instead of re-running the producer, so their lifetime is the
    /// executor's lifetime, NOT their last reader's step. Like
    /// [`persistent_resources`](Self::persistent_resources) they are
    /// excluded from every step's `free_after` at compile time — lifetime
    /// is decided ONCE here, and every plan consumer (the executor's pool
    /// release, the chain runtime's slot planner) inherits it.
    held_resources: Vec<ResourceId>,
    /// `hoistable_steps[i]` — step `i` is a pure node whose inputs are all
    /// produced by hoistable steps (the memoizable closure). Parallel to
    /// [`steps`](Self::steps). Outputs of these steps make up
    /// [`held_resources`](Self::held_resources).
    hoistable_steps: Vec<bool>,
    /// Indices into [`steps`] of nodes that declare non-empty
    /// [`state_capture_input_ports`](crate::node_graph::effect_node::EffectNode::state_capture_input_ports).
    /// The executor invokes
    /// [`EffectNode::late_capture`](crate::node_graph::effect_node::EffectNode::late_capture)
    /// on these — and ONLY these — after the main step loop completes,
    /// when their state-capture inputs hold THIS frame's producer
    /// output (the back-edge slot has just been written). Skipping
    /// non-stateful nodes here keeps the late pass cost proportional
    /// to the number of feedback / accumulator nodes in the graph.
    late_capture_steps: Vec<usize>,
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

    /// Producer-declared texture format for the given resource, or
    /// `None` if the producer didn't override (use backend default).
    /// Always `None` for non-`Texture2D` resources.
    pub fn resource_format(
        &self,
        id: ResourceId,
    ) -> Option<manifold_gpu::GpuTextureFormat> {
        self.resource_formats
            .get(id.0 as usize)
            .copied()
            .flatten()
    }

    /// Resources whose producer requested a mip-chained slot — the list
    /// the executor hands to [`Backend::declare_mipmapped`] each frame.
    /// Empty for every graph without a material-map source.
    pub fn mipmapped_resources(&self) -> &[ResourceId] {
        &self.mipmapped_resources
    }

    /// Producer-declared texture dims for the given resource, or
    /// `None` to use the backend's canvas dims at acquire time.
    /// Always `None` for non-`Texture2D` resources. See
    /// [`EffectNode::output_dims`](crate::node_graph::EffectNode::output_dims)
    /// for the compile-time resolution policy.
    pub fn resource_dims(&self, id: ResourceId) -> Option<(u32, u32)> {
        self.resource_dims.get(id.0 as usize).copied().flatten()
    }

    /// Canvas-relative dim hint for the given resource, as `(num, den)`.
    /// `None` means "no hint" — the executor falls back to canvas-sized
    /// allocation. See [`resource_canvas_scales`](Self::resource_canvas_scales)
    /// field docs for the full resolution order.
    pub fn resource_canvas_scale(&self, id: ResourceId) -> Option<(u32, u32)> {
        self.resource_canvas_scales
            .get(id.0 as usize)
            .copied()
            .flatten()
    }

    /// Aggregate runtime-service requirements across all nodes in
    /// the plan. The executor uses this to validate at the entry
    /// boundary rather than discovering mid-frame via `.expect()`.
    pub fn requires(&self) -> NodeRequires {
        self.requires
    }

    /// Resources that must be pre-acquired by the executor before
    /// the first step runs each frame, and never appear in any
    /// step's `free_after`. See the struct field docstring.
    pub fn persistent_resources(&self) -> &[ResourceId] {
        &self.persistent_resources
    }

    /// Resources latched by the memo/hoisting path — excluded from
    /// `free_after`, must never be aliased or recycled while the plan's
    /// executor lives. See the field docstring.
    pub fn held_resources(&self) -> &[ResourceId] {
        &self.held_resources
    }

    /// Whether step `idx` is in the memoizable (hoistable) closure.
    pub fn step_hoistable(&self, idx: usize) -> bool {
        self.hoistable_steps.get(idx).copied().unwrap_or(false)
    }

    /// Step indices that need a post-frame `late_capture` invocation —
    /// see [`late_capture_steps`](Self::late_capture_steps) field docs.
    pub fn late_capture_step_indices(&self) -> &[usize] {
        &self.late_capture_steps
    }

    /// Profiling-only: a sub-plan containing just the first `k` execution
    /// steps. Steps are topologically ordered, so `[0..k]` is always a valid
    /// executable prefix — every dependency of a kept step is also kept.
    /// Resource metadata is keyed by [`ResourceId`], not step index, so it
    /// carries over unchanged. Every `free_after` entry within the prefix stays
    /// correct: a resource freed at step `j < k` has no reader at or beyond `k`
    /// by construction (else its last reader, not `j`, would own the free), so
    /// truncating never orphans a still-live resource. `late_capture_steps` is
    /// filtered to the kept range so the post-frame pass can't index a dropped
    /// step. Persistent resources are kept as-is (pre-acquired, harmless if a
    /// producer that writes one falls outside the prefix).
    ///
    /// Used by the per-dispatch profiler to attribute GPU time to individual
    /// steps via the marginal `time[k] - time[k-1]`. NOT used on the live
    /// render path.
    pub fn truncated(&self, k: usize) -> ExecutionPlan {
        let k = k.min(self.steps.len());
        let mut p = self.clone();
        p.steps.truncate(k);
        p.hoistable_steps.truncate(k);
        p.late_capture_steps.retain(|&i| i < k);
        p
    }
}

/// D6(a) (`docs/DEPTH_RELIGHT_DESIGN.md`): whether a materialized Texture2D
/// intermediate should be allocated `Rgba32Float` instead of the backend
/// default, given ALL of its consumers.
///
/// Promotes when at least one consumer names this wire's target port
/// `precision_critical` — AND no consumer (the same one or a different one)
/// reads it through a filtering sampler (`InputAccess::Coincident`/`Gather`).
/// The second condition is the mixed-consumer safety rule: `Rgba32Float` is
/// non-filterable on Apple GPUs, so a single filtering consumer anywhere in
/// the fan-out vetoes the promotion for everyone — the texel-exact consumer
/// stays on fp16 rather than break the filtering one. `test_mixed_consumer_stays_fp16`
/// covers this exact shape.
fn wants_fp32_intermediate(graph: &Graph, consumers: &[&NodeWire]) -> bool {
    let mut has_precision_critical_consumer = false;
    let mut has_filtering_consumer = false;
    for wire in consumers {
        let Some(consumer_inst) = graph.get_node(wire.to.0) else {
            continue;
        };
        let node = consumer_inst.node.as_ref();
        let port = wire.to.1;
        if node.precision_critical_inputs().contains(&port) {
            has_precision_critical_consumer = true;
        }
        if let Some(access) = crate::node_graph::freeze::classify::input_access_of(node, port)
            && access.is_filtering_sampler()
        {
            has_filtering_consumer = true;
        }
    }
    has_precision_critical_consumer && !has_filtering_consumer
}

/// Compile a graph into an [`ExecutionPlan`].
///
/// Calls [`validate`] and [`topological_sort`] internally; errors propagate
/// as [`GraphError`]. The graph is not consumed.
pub fn compile(graph: &Graph) -> Result<ExecutionPlan, GraphError> {
    validate(graph)?;
    // Topological order, then filter to nodes the executor will
    // actually run: those whose output is (transitively) consumed by
    // any liveness root (FinalOutput, aliased_array_io primitives,
    // state-capture primitives — see [`EffectNode::is_liveness_root`]).
    // Anything outside that set is dead — the executor mustn't try to
    // evaluate it (its required inputs aren't bound), and validate()
    // already skipped its required-input check on the same reachability
    // grounds. Keeps editing-time graphs (orphan nodes in flight)
    // compilable instead of falling back to catalog. Filter only
    // applies when at least one root exists — graphs without any
    // (most unit-test fixtures) fall back to running every node.
    let full_order = topological_sort(graph)?;
    let has_root = graph.nodes().any(|inst| inst.node.is_liveness_root());
    let order: Vec<NodeInstanceId> = if has_root {
        let live = crate::node_graph::validation::reachable_from_liveness_roots(graph);
        full_order
            .into_iter()
            .filter(|id| live.contains(id))
            .collect()
    } else {
        full_order
    };

    // Index wires by their target (input) port for O(1) lookup during
    // input-binding construction.
    // Keyed by `Cow<'static, str>` port name so lookups by a variadic node's
    // owned port name (`mesh_37`) and by a wire's `&'static` name share one
    // key type. Wire names come in as `&'static str` → `Cow::Borrowed` (free).
    let mut wire_by_target: AHashMap<(NodeInstanceId, std::borrow::Cow<'static, str>), &NodeWire> =
        AHashMap::default();
    for w in graph.wires() {
        wire_by_target.insert((w.to.0, std::borrow::Cow::Borrowed(w.to.1)), w);
    }

    // Set of (node, output_port) pairs that have at least one downstream
    // consumer. Used to skip per-step output bindings (and therefore the
    // backing texture / array allocation + the primitive's per-pass work)
    // for outputs nobody reads. Optional outputs on multi-output primitives
    // (e.g. `render_3d_mesh`'s G-buffer attachments) only pay their cost
    // when actually wired. Primitive `run()` code that handles `texture_2d`
    // returning None for unused outputs gets the skip for free.
    let mut consumed_outputs: ahash::AHashSet<(NodeInstanceId, std::borrow::Cow<'static, str>)> =
        ahash::AHashSet::default();
    for w in graph.wires() {
        consumed_outputs.insert((w.from.0, std::borrow::Cow::Borrowed(w.from.1)));
    }
    // RAYTRACING_DESIGN.md D14: fold each node's param-driven forced
    // outputs (RT-enabled `render_scene`'s `depth`/`velocity`) into the
    // same consumed set a real wire would populate — see
    // `EffectNode::force_consumed_outputs`'s doc for why this is the
    // whole mechanism (nothing downstream needs to know the difference).
    for inst in graph.nodes() {
        for &port in inst.node.force_consumed_outputs(&inst.params) {
            consumed_outputs.insert((inst.id, std::borrow::Cow::Borrowed(port)));
        }
    }

    // Reverse of `wire_by_target`: every (producer node, producer output
    // port) → the wires reading it. Built for the D6(a) precision-aware
    // format seam below — deciding a materialized Texture2D intermediate's
    // format requires knowing ALL of its consumers (not just whether it has
    // one), so this is computed once up front rather than re-scanning
    // `graph.wires()` per output.
    let mut consumers_by_output: AHashMap<
        (NodeInstanceId, std::borrow::Cow<'static, str>),
        Vec<&NodeWire>,
    > = AHashMap::default();
    for w in graph.wires() {
        consumers_by_output
            .entry((w.from.0, std::borrow::Cow::Borrowed(w.from.1)))
            .or_default()
            .push(w);
    }

    // First pass: assign a fresh ResourceId to every output port of every
    // node, in topological order. Walking in topo order gives deterministic
    // resource IDs even when the underlying node map is unordered.
    //
    // Dims are also resolved here. The walk is topological, so by the
    // time a node's outputs are processed every wired Texture2D
    // input already has its dims recorded in `resource_dims`. We
    // gather those input dims, call the producer's
    // `EffectNode::output_dims`, and store the result. `None` from
    // the producer means "use the default policy" — max of texture
    // input dims, or canvas (left as `None` here, resolved by the
    // executor at acquire-time against the live backend's canvas).
    let mut output_resources: AHashMap<(NodeInstanceId, std::borrow::Cow<'static, str>), ResourceId> =
        AHashMap::default();
    let mut resource_types: Vec<PortType> = Vec::new();
    let mut resource_formats: Vec<Option<manifold_gpu::GpuTextureFormat>> = Vec::new();
    let mut mipmapped_resources: Vec<ResourceId> = Vec::new();
    let mut resource_dims: Vec<Option<(u32, u32)>> = Vec::new();
    // Parallel to `resource_dims`. Populated only when `resource_dims`
    // is `None` AND the producer overrides `output_canvas_scale` —
    // see the field doc on `ExecutionPlan::resource_canvas_scales`.
    // Non-Texture2D resources always get `None`.
    let mut resource_canvas_scales: Vec<Option<(u32, u32)>> = Vec::new();
    for &node_id in &order {
        let inst = graph
            .get_node(node_id)
            .expect("topo order references existing node");

        // Gather concrete dims for this node's wired texture inputs.
        // Producers that resolved to `None` (canvas-default) or whose
        // resource isn't yet assigned (state-capture back-edge to a
        // node later in topo order) are tracked separately as
        // `any_canvas_input` so the fallback below can propagate
        // canvas correctly. The old behaviour was to drop them from
        // the scratch entirely and take max-of-Some-only, which
        // silently picked a small dim when a chain mixed an explicit
        // downsample with a canvas-default feedback wire — the bug
        // that left oily-fluid's velocity-feedback writer at quarter-
        // res while feedback's state was canvas, so the per-frame
        // blit faulted with "source extent out of bounds" and state
        // never updated.
        // Fresh per node so the `&str` names can borrow this node's ports
        // (port names are `Cow`, not `&'static`, since variadic nodes format
        // them). Plan-build time only — the executor's hot path never sees it.
        let mut input_dims_scratch: Vec<(&str, (u32, u32))> = Vec::new();
        // Per-input categorization, mutually exclusive:
        //   - input_dims_scratch: input has a concrete (w, h)
        //   - input_canvas_scales: input has a canvas-relative scale
        //     (num, den) — e.g. a `node.downsample` producer landed
        //     its slot at canvas/factor
        //   - any_canvas_input: input is canvas-default (no hint at all)
        //     OR is a state-capture back-edge whose producer hasn't
        //     been processed yet
        // The split lets the output-dim policy propagate a
        // canvas-scaled chain (downsample → blur_h → blur_v → advect)
        // without falling back to full canvas at the first blur — the
        // bug that left OilyFluid's velocity blur dispatching at full
        // canvas even though the downsample's output was already
        // landed at quarter-res.
        let mut any_canvas_input = false;
        let mut input_canvas_scales: Vec<(u32, u32)> = Vec::new();
        for input_port in inst.node.inputs() {
            if !matches!(
                input_port.ty,
                PortType::Texture2D | PortType::Texture2DTyped(_) | PortType::Texture3D
            ) {
                continue;
            }
            let Some(wire) = wire_by_target.get(&(node_id, input_port.name.clone())) else {
                continue; // optional unwired input doesn't count
            };
            let Some(&src_res) =
                output_resources.get(&(wire.from.0, std::borrow::Cow::Borrowed(wire.from.1)))
            else {
                // Producer hasn't been processed yet — state-capture
                // back-edge. Its dim will be resolved when the writer
                // is visited later in topo; for the purposes of THIS
                // node's output dim, treat it as canvas (None).
                any_canvas_input = true;
                continue;
            };
            let dims = resource_dims.get(src_res.0 as usize).copied().flatten();
            let scale = resource_canvas_scales.get(src_res.0 as usize).copied().flatten();
            match (dims, scale) {
                (Some(d), _) => input_dims_scratch.push((input_port.name.as_ref(), d)),
                (None, Some(s)) => input_canvas_scales.push(s),
                (None, None) => any_canvas_input = true,
            }
        }

        for output_port in inst.node.outputs() {
            // Skip outputs with no downstream consumer: the resource
            // never appears in the plan, so no `pre_bind_*` allocation,
            // no per-step output binding, no post-build audit hit. The
            // primitive's `texture_2d` / `array` lookup for that port
            // returns None — gated passes inside the primitive's
            // `run()` are skipped naturally. This is the source-of-truth
            // for "don't pay for what nobody reads."
            if !consumed_outputs.contains(&(node_id, output_port.name.clone())) {
                continue;
            }
            let id = ResourceId(resource_types.len() as u32);
            output_resources.insert((node_id, output_port.name.clone()), id);
            resource_types.push(output_port.ty);
            // Format declaration is only meaningful for Texture2D
            // outputs; other port types ignore it. Query the producer
            // even for non-textures so the parallel arrays stay
            // aligned — the runtime normalizes non-texture formats to
            // `None` when constructing the pool key.
            //
            // D6(a) (`docs/DEPTH_RELIGHT_DESIGN.md`): an explicit producer
            // format (e.g. `node.edge_slope`'s per-instance fp32 opt-in)
            // always wins — this seam only fills in the "no opinion" case.
            // When the producer has no opinion AND the output is a
            // Texture2D-shaped intermediate, check whether any downstream
            // consumer needs the extra precision.
            let format = inst.node.output_format(output_port.name.as_ref()).or_else(|| {
                if !output_port.ty.is_texture_2d() {
                    return None;
                }
                let key = (node_id, output_port.name.clone());
                let wires = consumers_by_output.get(&key)?;
                if wants_fp32_intermediate(graph, wires) {
                    Some(manifold_gpu::GpuTextureFormat::Rgba32Float)
                } else {
                    None
                }
            });
            resource_formats.push(format);
            if output_port.ty.is_texture_2d()
                && inst.node.output_mipmapped(output_port.name.as_ref())
            {
                mipmapped_resources.push(id);
            }

            // Dims: only meaningful for Texture2D outputs. Query the
            // producer first; if it has no opinion, apply the default
            // policy: any canvas-default / unresolved input means the
            // output is canvas (None) because canvas is — by
            // construction — the largest dim in the chain and we
            // can't compute max(canvas, explicit) at compile time.
            // Otherwise, max of the explicit input dims.
            // Producer-declared canvas-relative scale, queried up
            // front: an explicit declaration ("my output is a canvas
            // fraction") beats the implicit max-of-input-dims
            // heuristic below. Rasterizers (render_scene /
            // render_*_mesh) declare (1, 1): their texture inputs
            // (envmap, base-color maps) are scene resources whose
            // dims say nothing about the render target — before this
            // priority flip, the import graph's render_scene color
            // inherited the envmap's fixed 1024², rendering the scene
            // square and stretching it to canvas (BUG-140).
            let declared_scale = if output_port.ty.is_texture_2d() {
                inst.node
                    .output_canvas_scale(output_port.name.as_ref(), &inst.params)
            } else {
                None
            };
            let dims = if output_port.ty.is_texture_2d() {
                // CANVAS dims aren't known at compile time. We pass
                // a sentinel (0, 0) here — primitives that need the
                // real canvas value should declare a width/height
                // scalar input instead. Mostly only downsample-style
                // primitives care, and they only need INPUT dims.
                inst.node
                    .output_dims(output_port.name.as_ref(), (0, 0), &input_dims_scratch, &inst.params)
                    .or_else(|| {
                        if declared_scale.is_some() || any_canvas_input {
                            // Propagate canvas: any None input means
                            // we can't be sure max is below canvas,
                            // so leave dims = None for the executor
                            // to resolve against the runtime canvas.
                            None
                        } else {
                            // Reduce explicit concrete dims; if none,
                            // leave dims = None and let
                            // `canvas_scale` propagation below pick
                            // it up from canvas-scaled inputs.
                            input_dims_scratch
                                .iter()
                                .map(|(_, d)| *d)
                                .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)))
                        }
                    })
            } else {
                None
            };
            // Canvas-relative dim: three sources, in priority order.
            //   1. Producer override (`output_canvas_scale`) — used by
            //      `node.downsample` to declare "my output is
            //      canvas/factor" when its input dim is unknown.
            //   2. Propagation from canvas-scaled inputs — when this
            //      node is pixel-local (no override, no concrete
            //      input dims) AND all of its inputs are themselves
            //      canvas-scaled, the output inherits the largest
            //      of those scales. This is what makes `downsample
            //      → blur_h → blur_v → advect` run end-to-end at
            //      quarter-res instead of just landing the
            //      downsample output at quarter-res and then
            //      dispatching every downstream node at full canvas.
            //   3. None — output is canvas-default OR concrete (and
            //      `dims` above has already been set).
            let canvas_scale = if output_port.ty.is_texture_2d()
                && dims.is_none()
            {
                declared_scale
                    .or_else(|| {
                        // Only propagate when ALL wired inputs are
                        // canvas-scaled (none canvas-default, none
                        // concrete). Mixed cases fall back to canvas
                        // — we can't compare a concrete dim against
                        // a canvas-relative one at compile time.
                        if any_canvas_input
                            || input_canvas_scales.is_empty()
                            || !input_dims_scratch.is_empty()
                        {
                            None
                        } else {
                            // Largest scale = largest num/den ratio.
                            // Compare via cross-multiply to avoid
                            // floating-point comparison.
                            input_canvas_scales.iter().copied().reduce(|a, b| {
                                let a_cmp = a.0 as u64 * b.1 as u64;
                                let b_cmp = b.0 as u64 * a.1 as u64;
                                if a_cmp >= b_cmp { a } else { b }
                            })
                        }
                    })
            } else {
                None
            };
            resource_dims.push(dims);
            resource_canvas_scales.push(canvas_scale);
        }
    }

    // Second pass: build steps, tracking last_reader for each resource.
    // last_reader starts at the producer's step (so unread resources are
    // freed immediately) and gets bumped each time a downstream node reads.
    //
    // Wires that terminate on a state-capture port are STATE CAPTURES,
    // not per-frame reads — the read happens at the consumer's step
    // (the consumer runs first relative to its state-capture wires
    // under the topo-sort exemption) but semantically picks up the
    // previous frame's producer write that still occupies the buffer.
    // They must NOT contribute to `last_reader` (else the resource
    // gets freed before the producer writes it this frame) and the
    // resource's slot must persist across frame boundaries. We collect
    // them in `persistent` and surface them on the plan so the executor
    // pre-acquires them.
    //
    // Per-PORT check: a stateful node can have a mix of state-capture
    // inputs (`in` on `node.feedback`) and regular per-frame inputs
    // (`seed`). Only the former get persistent-slot treatment; the
    // latter participate in `last_reader` like any other read so their
    // producers run upstream as normal.
    let mut last_reader: AHashMap<ResourceId, usize> = AHashMap::default();
    let mut persistent: Vec<ResourceId> = Vec::new();
    let mut persistent_seen: std::collections::HashSet<ResourceId> =
        std::collections::HashSet::new();
    let mut steps: Vec<ExecutionStep> = Vec::with_capacity(order.len());
    // Step indices whose nodes need a post-frame late_capture pass.
    // Populated alongside the step-building loop; the executor uses
    // this list after the main `evaluate` pass completes.
    let mut late_capture_steps: Vec<usize> = Vec::new();

    for (step_idx, &node_id) in order.iter().enumerate() {
        let inst = graph
            .get_node(node_id)
            .expect("topo order references existing node");
        let state_capture_ports = inst.node.state_capture_input_ports();
        if !state_capture_ports.is_empty() {
            late_capture_steps.push(step_idx);
        }

        let mut step_inputs = Vec::new();
        for input_port in inst.node.inputs() {
            if let Some(wire) = wire_by_target.get(&(node_id, input_port.name.clone())) {
                let res_id = *output_resources
                    .get(&(wire.from.0, std::borrow::Cow::Borrowed(wire.from.1)))
                    .expect("connect() guarantees the wire's source has a resource");
                step_inputs.push((intern_name(&input_port.name), res_id));
                if state_capture_ports.contains(&input_port.name.as_ref()) {
                    if persistent_seen.insert(res_id) {
                        persistent.push(res_id);
                    }
                } else {
                    last_reader.insert(res_id, step_idx);
                }
            }
            // Optional unwired inputs are omitted from the bindings.
        }

        let mut step_outputs = Vec::new();
        for output_port in inst.node.outputs() {
            // Skip outputs with no downstream consumer — the primitive's
            // `texture_2d("name")` / `array("name")` will return None and
            // any pass gated on that output won't run. Reclaims wasted
            // render passes / compute dispatches when a multi-output
            // primitive's optional outputs aren't wired (G-buffer outputs
            // on `render_3d_mesh` for shaded-only consumers, e.g.).
            if !consumed_outputs.contains(&(node_id, output_port.name.clone())) {
                continue;
            }
            let res_id = *output_resources
                .get(&(node_id, output_port.name.clone()))
                .expect("output resource was assigned in the first pass");
            step_outputs.push((intern_name(&output_port.name), res_id));
            // A node-declared persistent output (feedback's `out` — the
            // emit half of the zero-copy ping-pong) joins the persistent
            // set: pre-acquired before the step loop, never pool-released,
            // its texture survives across frames so a late-capture swap
            // with the back-edge slot carries state with no copies.
            if inst.node.persistent_output_ports().contains(&output_port.name.as_ref())
                && persistent_seen.insert(res_id)
            {
                persistent.push(res_id);
            }
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

    // Third pass — Part A: extend R(input) lifetimes for every node
    // that declares a skip-passthrough port pair. The slot runtime
    // installs an alias `borrowed_2d[out_slot] = clone(in_slot.texture)`
    // when the node skips; that alias points at the underlying
    // `MTLTexture` of the input slot, NOT a snapshot. Without
    // extension, the planner would free R(input)'s slot after the
    // skipping node (its only consumer in the planner's model), a
    // later step would acquire and write to that slot, and the
    // recycled MTLTexture would be visible through the alias —
    // silently corrupting downstream reads of R(output).
    //
    // The fix: for every node N with `skip_passthrough_ports() =
    // Some((in_port, out_port))`, extend `last_reader[R(in)]` to at
    // least `last_reader[R(out)]`. In linear chains this is a no-op
    // (R(out)'s sole reader is the step after N; R(in)'s last
    // reader is already N, which precedes that). In fan-out
    // topologies (V2 user composites) it's load-bearing.
    //
    // Done after step-building (so `last_reader` is fully
    // populated) and before bucketing (so the bumped lifetimes
    // land in the correct free_at_step bucket).
    for (step_idx, &node_id) in order.iter().enumerate() {
        let inst = graph
            .get_node(node_id)
            .expect("topo order references existing node");

        // Variadic router (mux): the runtime may alias ANY wired texture
        // input onto the declared output, so every one of them gets the
        // extension — the planner can't know which `in_N` the selector
        // picks at runtime.
        if let Some(out_port) = inst.node.variadic_skip_passthrough_out() {
            let Some(&r_out) = output_resources.get(&(node_id, std::borrow::Cow::Borrowed(out_port)))
            else {
                continue;
            };
            let r_out_last = *last_reader.get(&r_out).unwrap_or(&step_idx);
            for input in inst.node.inputs() {
                if !matches!(input.ty, PortType::Texture2D) {
                    continue;
                }
                let r_in = wire_by_target
                    .get(&(node_id, input.name.clone()))
                    .and_then(|w| {
                        output_resources
                            .get(&(w.from.0, std::borrow::Cow::Borrowed(w.from.1)))
                            .copied()
                    });
                let Some(r_in) = r_in else {
                    continue;
                };
                let entry = last_reader.entry(r_in).or_insert(step_idx);
                if *entry < r_out_last {
                    *entry = r_out_last;
                }
            }
            continue;
        }

        // `carries_resources` (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D2): a
        // node whose output forwards resource `Slot`s inside a CPU struct
        // field (`node.scene_object`'s `SceneObject`) needs the same
        // lifetime extension the mux branch above gives its aliased
        // texture inputs — the struct-carried resource is invisible to the
        // planner's normal wire walk, so without this every `Texture2D`/
        // `Array` input wired into the node would free right after the
        // node itself runs, even though the struct forwarding it is read
        // much later (by `render_scene`, through the Object wire).
        if inst.node.carries_resources() {
            // This node's own output(s) determine how long its inputs must
            // stay alive — take the latest last-reader across every output
            // port the node declares (in practice `node.scene_object` has
            // exactly one: `object`).
            let mut out_last = step_idx;
            for output_port in inst.node.outputs() {
                if let Some(&r_out) =
                    output_resources.get(&(node_id, output_port.name.clone()))
                {
                    let r_out_last = *last_reader.get(&r_out).unwrap_or(&step_idx);
                    if r_out_last > out_last {
                        out_last = r_out_last;
                    }
                }
            }
            for input in inst.node.inputs() {
                if !matches!(input.ty, PortType::Texture2D | PortType::Texture2DTyped(_))
                    && !matches!(input.ty, PortType::Array(_))
                {
                    continue;
                }
                let r_in = wire_by_target
                    .get(&(node_id, input.name.clone()))
                    .and_then(|w| {
                        output_resources
                            .get(&(w.from.0, std::borrow::Cow::Borrowed(w.from.1)))
                            .copied()
                    });
                let Some(r_in) = r_in else {
                    continue;
                };
                let entry = last_reader.entry(r_in).or_insert(step_idx);
                if *entry < out_last {
                    *entry = out_last;
                }
            }
            continue;
        }

        let Some((in_port, out_port)) = inst.node.skip_passthrough_ports() else {
            continue;
        };
        // R(in_port): the resource wired into N's input. If the
        // port is unwired (optional), skip_passthrough can't fire
        // at runtime either, so no extension needed.
        let r_in = wire_by_target
            .get(&(node_id, std::borrow::Cow::Borrowed(in_port)))
            .and_then(|w| {
                output_resources
                    .get(&(w.from.0, std::borrow::Cow::Borrowed(w.from.1)))
                    .copied()
            });
        let Some(r_in) = r_in else {
            continue;
        };
        // R(out_port): the resource N produces on the output port.
        let Some(&r_out) =
            output_resources.get(&(node_id, std::borrow::Cow::Borrowed(out_port)))
        else {
            continue;
        };
        // Last reader of R(out) — the deepest step that consumes it.
        // Falls back to the producer step if nobody reads R(out),
        // which is the no-op case for the extension.
        let r_out_last = *last_reader.get(&r_out).unwrap_or(&step_idx);
        let entry = last_reader.entry(r_in).or_insert(step_idx);
        if *entry < r_out_last {
            *entry = r_out_last;
        }
    }

    // Third pass — Part A2: the memoizable (hoistable) closure, in topo
    // order: a step is hoistable when its node is pure AND every input is
    // produced by an already-hoistable step. An input with no producing
    // step (host-prebound, e.g. the chain `source`) breaks the chain — it
    // has no change-epoch, so caching across it would serve stale content.
    // Outputs of hoistable steps are HELD: the executor's memo skip serves
    // their latched slots on later frames without re-running the producer,
    // so they must outlive their last reader. Classify them here — the one
    // place lifetimes are decided — so `free_after` (below) never frees
    // them and downstream slot planners can't alias them.
    let mut held: Vec<ResourceId> = Vec::new();
    let mut hoistable_steps: Vec<bool> = vec![false; steps.len()];
    {
        let mut res_hoistable: AHashMap<ResourceId, bool> =
            AHashMap::with_capacity(resource_types.len());
        for (idx, step) in steps.iter().enumerate() {
            let pure = graph
                .get_node(step.node)
                .is_some_and(|inst| inst.node.is_pure());
            let hoistable = pure
                && step
                    .inputs
                    .iter()
                    .all(|&(_, res)| res_hoistable.get(&res).copied().unwrap_or(false));
            hoistable_steps[idx] = hoistable;
            for &(_, res) in &step.outputs {
                res_hoistable.insert(res, hoistable);
                if hoistable && !persistent_seen.contains(&res) {
                    held.push(res);
                }
            }
        }
    }
    for res_id in &held {
        last_reader.remove(res_id);
    }

    // Third pass — Part B: bucket resources by their last_reader
    // step (now reflecting any skip-passthrough extensions) and
    // attach to the corresponding step's free_after list. Sort
    // within each bucket for deterministic iteration order in
    // tests. Persistent resources are explicitly skipped — they
    // were never added to `last_reader` in pass 2, but a producer's
    // `or_insert` could have planted a default entry for them in the
    // outputs walk. Drop it before bucketing so the slot survives
    // across frames.
    for res_id in &persistent {
        last_reader.remove(res_id);
    }
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

    // Roll up the per-node runtime-service requirements so the
    // executor can validate at its entry point. Walking the live
    // graph here (not the topological order) is sufficient because
    // `requires()` is a static per-node declaration.
    let requires = graph.nodes().fold(NodeRequires::default(), |acc, inst| {
        acc.union(inst.node.requires())
    });

    persistent.sort();
    held.sort();
    held.dedup();
    Ok(ExecutionPlan {
        steps,
        resource_types,
        resource_formats,
        mipmapped_resources,
        resource_dims,
        resource_canvas_scales,
        requires,
        persistent_resources: persistent,
        held_resources: held,
        hoistable_steps,
        late_capture_steps,
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
        // D6(a) test hooks — empty by default (every existing test using
        // `TestNode::new` keeps its prior behavior: Coincident access,
        // nothing precision_critical). Set via `with_input_access` /
        // `with_precision_critical`. `'static` to match the trait's const-
        // array-shaped return type; test call sites pass array literals of
        // Copy/const-constructible values, which Rust promotes to 'static.
        input_access: &'static [crate::node_graph::freeze::classify::InputAccess],
        precision_critical: &'static [&'static str],
    }

    impl TestNode {
        fn new(name: &'static str, inputs: Vec<NodeInput>, outputs: Vec<NodeOutput>) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs,
                outputs,
                input_access: &[],
                precision_critical: &[],
            }
        }

        fn with_input_access(
            mut self,
            access: &'static [crate::node_graph::freeze::classify::InputAccess],
        ) -> Self {
            self.input_access = access;
            self
        }

        fn with_precision_critical(mut self, names: &'static [&'static str]) -> Self {
            self.precision_critical = names;
            self
        }
    }

    impl crate::node_graph::EffectNode for TestNode {
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
        fn input_access(&self) -> &'static [crate::node_graph::freeze::classify::InputAccess] {
            self.input_access
        }
        fn precision_critical_inputs(&self) -> &'static [&'static str] {
            self.precision_critical
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
    fn unread_outputs_are_not_allocated_resources() {
        // A has two outputs, neither wired. The planner skips both —
        // unread outputs cost nothing: no resource, no per-step binding,
        // no backing texture / array allocation downstream. Replaces
        // the old "create-then-immediately-free" semantics.
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
        assert_eq!(plan.resource_count(), 0);
        assert!(plan.steps()[0].outputs.is_empty());
        assert!(plan.steps()[0].free_after.is_empty());
    }

    #[test]
    fn resource_types_match_consumed_output_port_types() {
        // Mix Texture2D and Texture3D outputs, both consumed downstream.
        // (Unconsumed outputs are skipped by the planner — see
        // `unread_outputs_are_not_allocated_resources` for that contract.)
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![
                output("color", PortType::Texture2D),
                output("volume", PortType::Texture3D),
            ],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![
                input("color_in", PortType::Texture2D, true),
                input("volume_in", PortType::Texture3D, true),
            ],
            vec![],
        )));
        g.connect((a, "color"), (b, "color_in")).unwrap();
        g.connect((a, "volume"), (b, "volume_in")).unwrap();
        let plan = compile(&g).unwrap();
        // Step 0 is `a` (topological order). Resources are listed in
        // declaration order on the producer.
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

    /// Test node that declares a static skip-passthrough port pair.
    /// The dynamic `skip_passthrough(params)` decision isn't
    /// exercised here — the planner only consults the static
    /// `skip_passthrough_ports()` declaration.
    struct SkippableNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl SkippableNode {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("skippable"),
                inputs: vec![input("in", PortType::Texture2D, true)],
                outputs: vec![output("out", PortType::Texture2D)],
            }
        }
    }

    impl crate::node_graph::EffectNode for SkippableNode {
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
        fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
            Some(("in", "out"))
        }
    }

    #[test]
    fn fan_out_from_skip_passthrough_node_extends_input_lifetime() {
        // Topology: A → B(skippable) → C
        //                          \→ D
        // R(A.out) = R_a, R(B.out) = R_b, R(C.out), R(D.out).
        //
        // Without the planner extension: last_reader(R_a) = step B
        // (B is R_a's only direct reader), so R_a frees after B.
        // A later step (C or D) could then recycle R_a's slot and
        // write to it — silently corrupting D's read through B's
        // alias (which points at R_a's underlying MTLTexture).
        //
        // With the extension: B declares skip_passthrough_ports =
        // ("in", "out"), so the planner extends last_reader(R_a) to
        // cover last_reader(R_b). R_b is read by both C and D —
        // whichever is later (step 3) becomes R_b's last reader,
        // and R_a's last_reader is bumped to match.
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(SkippableNode::new()));
        let c = g.add_node(Box::new(TestNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        let d = g.add_node(Box::new(TestNode::new(
            "d",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((b, "out"), (c, "in")).unwrap();
        g.connect((b, "out"), (d, "in")).unwrap();

        let plan = compile(&g).unwrap();

        // 4 steps: A, B, C, D in some valid topo order.
        assert_eq!(plan.steps().len(), 4);
        let r_a = plan.steps()[0].outputs[0].1;
        let r_b = plan.steps()[1].outputs[0].1;

        // Find which step is the LAST reader of R_b — that's where
        // R_a's lifetime extension is forced to land.
        let mut last_reader_of_r_b = 0;
        for (idx, step) in plan.steps().iter().enumerate() {
            if step.inputs.iter().any(|(_, r)| *r == r_b) {
                last_reader_of_r_b = idx;
            }
        }
        // Sanity: R_b is read by step 2 and step 3.
        assert!(last_reader_of_r_b >= 2, "R_b should be read after step 1");

        // Without the extension, R_a would be in free_after at step
        // 1 (B). With the extension, R_a moves to free_after at
        // step `last_reader_of_r_b`.
        assert!(
            !plan.steps()[1].free_after.contains(&r_a),
            "R_a must NOT be freed at step 1 (skippable B) — that would let \
             a later step recycle the slot and corrupt the alias"
        );
        assert!(
            plan.steps()[last_reader_of_r_b].free_after.contains(&r_a),
            "R_a must be freed at the step that's the last reader of R_b \
             (skip-passthrough alias lifetime extension)"
        );
    }

    #[test]
    fn linear_chain_with_skip_passthrough_unchanged() {
        // A → B(skippable) → C
        // R_a is only read by B (= step 1). R_b is only read by C
        // (= step 2). The extension bumps last_reader(R_a) to
        // last_reader(R_b) = step 2 — so R_a moves from "free
        // after step 1" to "free after step 2".
        //
        // Semantically correct: R_a's MTLTexture must stay alive
        // until C has read R_b's alias.
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(SkippableNode::new()));
        let c = g.add_node(Box::new(TestNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((b, "out"), (c, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let r_a = plan.steps()[0].outputs[0].1;

        assert!(
            !plan.steps()[1].free_after.contains(&r_a),
            "R_a must not be freed at B's step"
        );
        assert!(
            plan.steps()[2].free_after.contains(&r_a),
            "R_a must be freed at C's step (the alias's last reader)"
        );
    }

    /// Test node mirroring `node.scene_object`'s shape for
    /// `carries_resources_extends_lifetimes`: a texture-typed input
    /// forwarded inside a struct-carried output rather than aliased
    /// directly.
    struct CarriesResourcesNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl CarriesResourcesNode {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("carries_resources"),
                inputs: vec![input("in", PortType::Texture2D, false)],
                outputs: vec![output("object", PortType::Object)],
            }
        }
    }

    impl crate::node_graph::EffectNode for CarriesResourcesNode {
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
        fn carries_resources(&self) -> bool {
            true
        }
    }

    #[test]
    fn carries_resources_extends_lifetimes() {
        // Topology: A → N(carries_resources) → C
        // R(A.out) = R_a is forwarded inside the `SceneObject`-shaped
        // struct N emits on `object`, not aliased directly onto a wire —
        // so the planner's normal wire-walk sees only "N reads R_a", and
        // without the extension would free R_a right after N's step, even
        // though C reads N's `object` output (and, through it, the struct
        // field carrying R_a) afterward. Mirrors
        // `fan_out_from_skip_passthrough_node_extends_input_lifetime`.
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let n = g.add_node(Box::new(CarriesResourcesNode::new()));
        let c = g.add_node(Box::new(TestNode::new(
            "c",
            vec![input("in", PortType::Object, true)],
            vec![],
        )));
        g.connect((a, "out"), (n, "in")).unwrap();
        g.connect((n, "object"), (c, "in")).unwrap();

        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 3);
        let r_a = plan.steps()[0].outputs[0].1;

        assert!(
            !plan.steps()[1].free_after.contains(&r_a),
            "R_a must NOT be freed at N's step — carries_resources forwards it \
             inside the struct N emits, read later by C"
        );
        assert!(
            plan.steps()[2].free_after.contains(&r_a),
            "R_a must be freed at C's step (the last reader of N's own output)"
        );
    }

    /// Regression: a node with one explicit-dim input AND one
    /// canvas-default input must NOT pick up the explicit dim via
    /// max-of-Some-only — that's the bug that left oily-fluid's
    /// velocity-feedback writer at quarter-res while feedback's state
    /// was canvas, causing the per-frame blit to fault. Any canvas
    /// input means "we can't bound the output below canvas," so the
    /// output dim must propagate as None (= canvas at runtime).
    #[test]
    fn output_dims_canvas_input_overrides_explicit_input() {
        struct ExplicitDimNode {
            type_id: EffectNodeType,
        }
        impl crate::node_graph::EffectNode for ExplicitDimNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodePort] {
                &[]
            }
            fn outputs(&self) -> &[NodePort] {
                static OUTPUTS: [NodePort; 1] = [NodePort {
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
            fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
            fn output_dims(
                &self,
                _port: &str,
                _canvas: (u32, u32),
                _inputs: &[(&str, (u32, u32))],
                _params: &crate::node_graph::effect_node::ParamValues,
            ) -> Option<(u32, u32)> {
                Some((480, 270))
            }
        }

        // A_explicit (quarter-res) ─┐
        //                            ├─→ B (mix)
        // A_canvas (None)        ─┘
        // B's output should be None (canvas), NOT quarter-res.
        let mut g = Graph::new();
        let a_explicit = g.add_node(Box::new(ExplicitDimNode {
            type_id: EffectNodeType::new("a_explicit"),
        }));
        let a_canvas = g.add_node(Box::new(TestNode::new(
            "a_canvas",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, true),
            ],
            vec![output("out", PortType::Texture2D)],
        )));
        g.connect((a_explicit, "out"), (b, "a")).unwrap();
        g.connect((a_canvas, "out"), (b, "b")).unwrap();
        // Sink consuming `b.out` so the planner allocates a resource
        // for it (post-A unread outputs are skipped entirely).
        let sink = g.add_node(Box::new(TestNode::new(
            "sink",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((b, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();
        // The explicit-dim node should still resolve to quarter-res...
        let r_explicit = plan.steps().iter()
            .find(|s| s.node == a_explicit).unwrap()
            .outputs[0].1;
        assert_eq!(plan.resource_dims(r_explicit), Some((480, 270)));
        // ...but B's mix output, because one input is canvas-default,
        // must NOT inherit the quarter-res — it must be None (canvas).
        let r_b = plan.steps().iter()
            .find(|s| s.node == b).unwrap()
            .outputs[0].1;
        assert_eq!(
            plan.resource_dims(r_b),
            None,
            "B mixes a quarter-res input with a canvas-default input; \
             output must be canvas (None), not the quarter-res input's dim. \
             The old max-of-Some-only fallback silently picked quarter, \
             which caused oily-fluid's feedback blit to fault on dim mismatch."
        );
    }

    /// `node.downsample`-style producer declares a canvas-relative
    /// output scale. The propagation must carry that scale through
    /// every downstream pixel-local node (blur, gain, mix, …) so the
    /// whole chain runs at the reduced resolution. Without
    /// propagation, only the immediate downsample slot is quarter-res
    /// and every blur/advect downstream dispatches at full canvas —
    /// the perf regression that left OilyFluid's velocity chain
    /// running at full-res even though we'd added the downsample back
    /// to the JSON.
    #[test]
    fn canvas_scale_propagates_through_pixel_local_chain() {
        // A node that declares `output_canvas_scale = (1, 4)` and
        // has no concrete output_dims (the runtime would land it at
        // canvas/4). Models `node.downsample` with a back-edge
        // input.
        struct QuarterCanvasSource {
            type_id: EffectNodeType,
        }
        impl crate::node_graph::EffectNode for QuarterCanvasSource {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodePort] { &[] }
            fn outputs(&self) -> &[NodePort] {
                static OUTPUTS: [NodePort; 1] = [NodePort {
                    name: std::borrow::Cow::Borrowed("out"),
                    ty: PortType::Texture2D,
                    kind: PortKind::Output,
                    required: false,
                }];
                &OUTPUTS
            }
            fn parameters(&self) -> &[ParamDef] { &[] }
            fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
            fn output_canvas_scale(
                &self,
                _port: &str,
                _params: &crate::node_graph::effect_node::ParamValues,
            ) -> Option<(u32, u32)> {
                Some((1, 4))
            }
        }

        // Quarter-canvas source → pixel-local A → pixel-local B.
        // Both A and B should inherit the (1, 4) scale.
        let mut g = Graph::new();
        let q = g.add_node(Box::new(QuarterCanvasSource {
            type_id: EffectNodeType::new("quarter_src"),
        }));
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
        g.connect((q, "out"), (a, "in")).unwrap();
        g.connect((a, "out"), (b, "in")).unwrap();
        let sink = g.add_node(Box::new(TestNode::new(
            "sink",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((b, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_q = plan.steps().iter().find(|s| s.node == q).unwrap().outputs[0].1;
        let r_a = plan.steps().iter().find(|s| s.node == a).unwrap().outputs[0].1;
        let r_b = plan.steps().iter().find(|s| s.node == b).unwrap().outputs[0].1;

        assert_eq!(plan.resource_dims(r_q), None, "Q has no concrete dim");
        assert_eq!(plan.resource_canvas_scale(r_q), Some((1, 4)), "Q declared (1, 4)");
        assert_eq!(
            plan.resource_canvas_scale(r_a),
            Some((1, 4)),
            "A inherits Q's canvas scale (pixel-local, no output_dims override)",
        );
        assert_eq!(
            plan.resource_canvas_scale(r_b),
            Some((1, 4)),
            "B inherits A's canvas scale — propagation reaches arbitrary depth",
        );
    }

    /// Mixing a canvas-scaled input with a canvas-default input must
    /// fall back to canvas (the larger). We can't statically compare
    /// `canvas/4` against `canvas` — at runtime canvas is canvas — so
    /// the safe choice is to not propagate the scale through a node
    /// whose other input would have run at full canvas anyway. This
    /// is exactly the `vel_advect` shape in OilyFluid: blurred
    /// velocity (quarter-res) + unblurred velocity (canvas) → output
    /// must be canvas so the advect dispatch covers the full frame.
    #[test]
    fn mixed_scaled_and_canvas_input_falls_back_to_canvas() {
        struct QuarterCanvasSource {
            type_id: EffectNodeType,
        }
        impl crate::node_graph::EffectNode for QuarterCanvasSource {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
            fn type_id(&self) -> &EffectNodeType { &self.type_id }
            fn inputs(&self) -> &[NodePort] { &[] }
            fn outputs(&self) -> &[NodePort] {
                static OUTPUTS: [NodePort; 1] = [NodePort {
                    name: std::borrow::Cow::Borrowed("out"),
                    ty: PortType::Texture2D,
                    kind: PortKind::Output,
                    required: false,
                }];
                &OUTPUTS
            }
            fn parameters(&self) -> &[ParamDef] { &[] }
            fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
            fn output_canvas_scale(
                &self,
                _port: &str,
                _params: &crate::node_graph::effect_node::ParamValues,
            ) -> Option<(u32, u32)> {
                Some((1, 4))
            }
        }

        let mut g = Graph::new();
        let q = g.add_node(Box::new(QuarterCanvasSource {
            type_id: EffectNodeType::new("quarter_src"),
        }));
        let canvas_src = g.add_node(Box::new(TestNode::new(
            "canvas_src",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let mix = g.add_node(Box::new(TestNode::new(
            "mix",
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, true),
            ],
            vec![output("out", PortType::Texture2D)],
        )));
        g.connect((q, "out"), (mix, "a")).unwrap();
        g.connect((canvas_src, "out"), (mix, "b")).unwrap();
        let sink = g.add_node(Box::new(TestNode::new(
            "sink",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((mix, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_mix = plan.steps().iter().find(|s| s.node == mix).unwrap().outputs[0].1;
        assert_eq!(plan.resource_dims(r_mix), None);
        assert_eq!(
            plan.resource_canvas_scale(r_mix),
            None,
            "mix has one canvas-default input — output must be canvas, not the \
             quarter-scaled input's (1, 4). Otherwise vel_advect would dispatch \
             at quarter-res and miss writing the canvas-sized destination.",
        );
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

    // D6(a) (`docs/DEPTH_RELIGHT_DESIGN.md`): the format-selection seam
    // promotes a materialized Texture2D intermediate to Rgba32Float when a
    // consumer needs it, gated by the mixed-consumer sampler-safety rule.

    #[test]
    fn precision_critical_consumer_gets_fp32_intermediate() {
        use crate::node_graph::freeze::classify::InputAccess;

        // A smooth-gradient Source producer feeding a GatherTexel consumer
        // that names its input precision_critical (the `surface_bumps`/
        // `ssao_gtao` shape: a finite-difference / horizon-test read).
        let mut g = Graph::new();
        let producer = g.add_node(Box::new(TestNode::new(
            "grad",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let consumer = g.add_node(Box::new(
            TestNode::new(
                "depth_consumer",
                vec![input("depth", PortType::Texture2D, true)],
                vec![output("out", PortType::Texture2D)],
            )
            .with_input_access(&[InputAccess::GatherTexel])
            .with_precision_critical(&["depth"]),
        ));
        g.connect((producer, "out"), (consumer, "depth")).unwrap();

        let plan = compile(&g).unwrap();
        let r_producer_out = plan.steps()[0].outputs[0].1;
        assert_eq!(
            plan.resource_format(r_producer_out),
            Some(manifold_gpu::GpuTextureFormat::Rgba32Float),
            "a Texture2D intermediate feeding a precision_critical, texel-exact \
             consumer must be allocated fp32",
        );
    }

    #[test]
    fn plain_color_chain_stays_fp16() {
        // A → B → C, all default (Coincident) access, nothing
        // precision_critical: every intermediate stays at the backend
        // default (None = fp16), same as before D6(a).
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
        let r_a = plan.steps()[0].outputs[0].1;
        let r_b = plan.steps()[1].outputs[0].1;
        assert_eq!(plan.resource_format(r_a), None);
        assert_eq!(plan.resource_format(r_b), None);
    }

    #[test]
    fn mixed_consumer_stays_fp16() {
        use crate::node_graph::freeze::classify::InputAccess;

        // One producer, two consumers of the SAME output: one texel-exact
        // consumer marking it precision_critical (wants fp32), one filtering
        // (Coincident) consumer that would break on a non-filterable
        // Rgba32Float source. The mixed-consumer safety rule must veto the
        // promotion — the filtering consumer's correctness wins, and the
        // texel-exact consumer stays on fp16 rather than break the other.
        let mut g = Graph::new();
        let producer = g.add_node(Box::new(TestNode::new(
            "depth_source",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let texel_consumer = g.add_node(Box::new(
            TestNode::new(
                "shadow",
                vec![input("height", PortType::Texture2D, true)],
                vec![],
            )
            .with_input_access(&[InputAccess::GatherTexel])
            .with_precision_critical(&["height"]),
        ));
        let filtering_consumer = g.add_node(Box::new(TestNode::new(
            "blur",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((producer, "out"), (texel_consumer, "height"))
            .unwrap();
        g.connect((producer, "out"), (filtering_consumer, "in"))
            .unwrap();

        let plan = compile(&g).unwrap();
        let r_producer_out = plan.steps()[0].outputs[0].1;
        assert_eq!(
            plan.resource_format(r_producer_out),
            None,
            "a mixed-consumer intermediate (one texel-exact + one filtering \
             reader) must stay fp16 — promoting would break the filtering \
             consumer's sampler read",
        );
    }

    /// D6(a) follow-up (`docs/DEPTH_RELIGHT_DESIGN.md`): end-to-end proof
    /// using the REAL registered primitives, now that `node.edge_slope`
    /// (aliased "gradient" — a gradient-producing central-difference atom)
    /// and `node.surface_bumps` have both been converted to `GatherTexel` +
    /// `precision_critical`. A gradient-producer → surface_bumps chain
    /// (source → edge_slope → surface_bumps) must promote edge_slope's own
    /// output to Rgba32Float — closing the loop the synthetic TestNode
    /// tests above only approximated.
    #[test]
    fn real_gradient_producer_into_surface_bumps_gets_fp32_intermediate() {
        use crate::node_graph::PrimitiveRegistry;

        let registry = PrimitiveRegistry::with_builtin();
        let mut g = Graph::new();
        let source = g.add_node(Box::new(TestNode::new(
            "source",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let edge_slope = g.add_node(
            registry
                .construct("node.edge_slope")
                .expect("node.edge_slope registered"),
        );
        let surface_bumps = g.add_node(
            registry
                .construct("node.surface_bumps")
                .expect("node.surface_bumps registered"),
        );
        g.connect((source, "out"), (edge_slope, "in")).unwrap();
        g.connect((edge_slope, "out"), (surface_bumps, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let r_edge_slope_out = plan
            .steps()
            .iter()
            .find(|s| s.node == edge_slope)
            .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
            .map(|&(_, r)| r)
            .expect("edge_slope's out resource is bound (surface_bumps wires it)");

        assert_eq!(
            plan.resource_format(r_edge_slope_out),
            Some(manifold_gpu::GpuTextureFormat::Rgba32Float),
            "node.edge_slope's output feeding node.surface_bumps's precision_critical \
             `in` (both now GatherTexel, no filtering consumer here) must be \
             allocated fp32",
        );
    }
}
