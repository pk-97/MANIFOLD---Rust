//! Region partition — the pointwise-fusion finder (design §3).
//!
//! This is the "other half" of the freeze compiler. [`super::codegen`] already
//! chains a region of atom bodies into one kernel; [`super::install`] already
//! rewrites a def around a region and retargets its bindings. What was missing
//! was the *finder*: until now [`super::install::fuse_canonical_def`] only fused
//! a card when its **entire** worker body was one pointwise region rooted at the
//! single source (the ColorGrade shape) — one boundary anywhere left the whole
//! card unfused. This module generalises that to **partition any flattened graph
//! into every maximal pointwise region**, cutting at boundaries, so an effect
//! with a blur (or warp, feedback, DNN, resolution change) in the middle still
//! fuses the pure runs on either side of it.
//!
//! ## The algorithm (§3 "region growing")
//!
//! 1. **Classify** each node as [`NodeClass::Eligible`] (a same-element-space
//!    pointwise/coincident atom that can thread a register) or
//!    [`NodeClass::Boundary`] (everything else — the seams). Classification is
//!    read off each atom's declared [`FusionKind`](super::classify::FusionKind)
//!    plus [`InputAccess`](super::classify::InputAccess); never inferred from
//!    a hard-coded atom list, so a newly-converted atom widens coverage with no
//!    change here (and an unclassified atom stays `Boundary` — conservative).
//! 2. **Grow** maximal connected components over texture wires between eligible
//!    nodes. Each component is a candidate region; boundaries are the cuts.
//! 3. **Resolve** each region's *external* inputs (texture wires entering it from
//!    a non-member — read once as `src_e`) and its *output(s)* (each member whose
//!    texture output leaves the region — one for a linear chain, several for a
//!    fan-out, each stored to its own `dst_<k>`). Conservative gates (length ≥ 2,
//!    every escaping consumer live) skip anything the codegen can't yet express or
//!    the executor wouldn't allocate — left unfused, never miscompiled.
//!
//! Everything here is a pure function over the def + registry — no GPU — so the
//! partition is unit-tested structurally (design §7's "cheap GPU-free layer
//! first") before the install path ever renders it.
//!
//! ## Conservative-by-construction
//!
//! Every gate fails *closed*: an ambiguous node is a `Boundary`, a region the
//! codegen can't express is dropped, an unrecognised wire shape aborts the
//! region. The unfused graph is always a correct fallback, so under-fusing only
//! costs speed; the partition never produces a region that renders differently.

use ahash::{AHashMap, AHashSet};
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};

use crate::node_graph::PrimitiveRegistry;
use crate::node_graph::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};
use crate::node_graph::freeze::classify::InputAccess;
use crate::node_graph::freeze::codegen::param_wgsl_type;
use crate::node_graph::ports::PortType;

/// Minimum members for a region to be worth fusing. A single-node "region" is
/// just the atom's own standalone kernel — fusing it changes nothing and only
/// adds a rewrite — so the smallest useful region threads one register between
/// two atoms (saving one full-canvas round-trip). The perf gate is the real
/// arbiter of whether a given region pays; this only avoids emitting no-ops.
const MIN_REGION_LEN: usize = 2;

/// How a graph node participates in fusion, resolved once per node.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NodeClass {
    /// A same-element-space atom that folds into a fused kernel: a pointwise /
    /// coincident atom threading its input register(s), or a Source generator
    /// producing the region's head value from position. Writes one texture output.
    Eligible,
    /// A fusion seam — source/final_output, any non-pointwise atom (blur,
    /// feedback, DNN, resample, generator, router), a gather-input atom, a
    /// resolution/scale override, a non-scalar param, or a control-wired param.
    /// Boundaries stay their own dispatch and bound the regions around them.
    Boundary,
}

/// One member of a fusion region: its def node-id and each texture input
/// resolved to where it reads from.
#[derive(Debug, Clone)]
pub struct RegionMember {
    /// The member's `EffectGraphNode::id` (doc id).
    pub doc_id: u32,
    /// Texture inputs in body-arg order (the atom's `inputs()` filtered to
    /// texture ports). Each resolves to an external slot or an earlier member.
    pub inputs: Vec<RegionInput>,
    /// How each input in [`Self::inputs`] is read (aligned by index). A `Gather`
    /// entry's input is always an [`RegionInput::External`] — the codegen binds
    /// it as a texture (+ sampler) the body samples itself.
    pub input_access: Vec<InputAccess>,
}

/// Where one of a member's texture inputs comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionInput {
    /// The region's Nth external input (a texture produced outside the region,
    /// read once into a register). Index into [`Region::externals`].
    External(usize),
    /// Another member's output register (must be earlier in topo order).
    Member(u32),
}

/// A texture produced outside a region and read by ≥1 of its members. Read once
/// as the fused node's `src_<slot>` input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRef {
    /// Producer node doc-id.
    pub from_node: u32,
    /// Producer output port.
    pub from_port: String,
}

/// A maximal fusable region: members in topo order, the external textures they
/// read, and the member(s) whose output leaves the region (one for a linear
/// chain, several for a fan-out).
#[derive(Debug, Clone)]
pub struct Region {
    /// Members in topological order (every `Member` input refers to an earlier
    /// entry). The fused kernel evaluates them in this order, threading registers.
    pub members: Vec<RegionMember>,
    /// External inputs, indexed by the slot a [`RegionInput::External`] names.
    pub externals: Vec<ExternalRef>,
    /// The member(s) whose texture output is consumed outside the region (feeds a
    /// boundary or `final_output`), in stable doc-id order. Usually one — the tail
    /// of a linear chain. A FAN-OUT region has several: an interior member whose
    /// output feeds two distinct downstream boundaries appears once here and is
    /// stored to its own `dst_<k>` slot (this vec's index). Every output's
    /// consumers are reachable from `final_output` (live), so the install pass can
    /// wire each `dst_<k>` to an allocated texture — a region with any escaping
    /// wire to a dead (non-final-reachable) consumer is dropped, not fused.
    pub outputs: Vec<u32>,
}

/// Partition a flattened def into its maximal pointwise-fusion regions. Returns
/// an empty vec when nothing fuses (the overwhelming-majority case today, and
/// always safe). Deterministic: members and regions come out in stable doc-id /
/// topo order so the generated WGSL — a pipeline-cache key — is reproducible.
pub fn partition_regions(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> Vec<Region> {
    // Groups must be flattened away before fusion ever sees the def (the loader
    // does this); a def still carrying a group node isn't a fusion target.
    if def.nodes.iter().any(|n| n.group.is_some()) {
        return Vec::new();
    }

    // ── Classify every node once. ──
    let class: AHashMap<u32, NodeClass> = def
        .nodes
        .iter()
        .map(|n| (n.id, classify_node(n, def, registry)))
        .collect();
    let eligible: AHashSet<u32> = class
        .iter()
        .filter(|(_, c)| **c == NodeClass::Eligible)
        .map(|(id, _)| *id)
        .collect();
    if eligible.is_empty() {
        return Vec::new();
    }

    // ── Grow regions over texture wires between eligible nodes — but only when
    // the merge keeps the collapsed graph ACYCLIC (convexity). ──
    //
    // A coincident texture wire eligible→eligible means the consumer can thread
    // the producer's register, so the two *want* to be one region. (A GATHER-
    // consumed wire does NOT union — a gather samples the whole texture at a coord
    // it computes, which a single register can't carry, so the gathered producer
    // stays an external the body samples; that's what makes gather-into-region
    // safe.) But two register-adjacent atoms still can't fuse if an *external*
    // path runs from one, out through a boundary, and back into the other:
    // collapsing them to one node would make that boundary both read from and
    // write to the fused node — a cycle the graph builder rejects (Watercolor's
    // uv_displace → blur → slope_displace is exactly this). So we merge greedily
    // and accept a union only if the region partition stays convex.
    //
    // "Acyclic" is measured on the FORWARD dependency graph: a state-capture wire
    // (a feedback node's captured input — last frame's value, not this frame's) is
    // a back edge the planner already excludes, so we exclude it too. Otherwise a
    // legal feedback loop would look like a cycle and we'd over-split.
    let forward: Vec<(u32, u32)> = def
        .wires
        .iter()
        .filter(|w| !is_state_capture_wire(def, registry, w))
        .map(|w| (w.from_node, w.to_node))
        .collect();
    let mut candidates: Vec<(u32, u32)> = def
        .wires
        .iter()
        .filter(|w| {
            eligible.contains(&w.from_node)
                && eligible.contains(&w.to_node)
                // Union over coincident wires of EITHER domain: a texture (pixel)
                // chain OR an Array (particle / instance) chain. Two eligible
                // atoms of the same domain are always wired by their own kind
                // (texture↔texture, Array↔Array), so this never merges domains.
                && (is_texture_wire(def, registry, w) || is_array_wire(def, registry, w))
                && wire_coincident_consumed(def, registry, w)
        })
        .map(|w| (w.from_node, w.to_node))
        .collect();
    candidates.sort_unstable(); // deterministic merge order → reproducible regions
    candidates.dedup();

    let mut uf = UnionFind::new(&eligible);
    for (a, b) in candidates {
        if uf.find(a) == uf.find(b) {
            continue;
        }
        // Region key of each node under a TENTATIVE merge of a's and b's regions:
        // an eligible node maps to its region rep (with b's rep folded onto a's);
        // a boundary maps to itself. Unifying via the rep keeps the test O(V+E).
        let finds: AHashMap<u32, u32> = eligible.iter().map(|&n| (n, uf.find(n))).collect();
        let (ra, rb) = (uf.find(a), uf.find(b));
        let key = |n: u32| -> u32 {
            match finds.get(&n) {
                Some(&r) if r == rb => ra,
                Some(&r) => r,
                None => n,
            }
        };
        if !collapsed_has_cycle(&forward, &key) {
            uf.union(a, b);
        }
    }
    let mut components: AHashMap<u32, Vec<u32>> = AHashMap::default();
    for &id in &eligible {
        components.entry(uf.find(id)).or_default().push(id);
    }

    // Nodes that reach a `final_output` (live). A region output's consumer must
    // be in here, so each fused `dst_<k>` lands on a texture the executor actually
    // allocates — see `build_region`.
    let final_reachable = final_reachable_nodes(def);

    // ── Build a region from each component; drop the ones v1 can't express. ──
    let mut regions: Vec<Region> = Vec::new();
    for (_, mut nodes) in components {
        nodes.sort_unstable();
        if let Some(region) = build_region(def, registry, &nodes, &final_reachable) {
            regions.push(region);
        }
    }
    // Stable order across runs (components iterate in hash order otherwise).
    regions.sort_by_key(|r| r.members.first().map(|m| m.doc_id).unwrap_or(0));
    regions
}

/// Is `start` on a dataflow cycle — i.e. inside a feedback loop? Forward node
/// reachability over the wire graph: a feedback node (`array_feedback` /
/// `node.feedback`) is a SINGLE node whose `in` wires and `out` wires both attach
/// to it, so the loop's back-edge through it shows up as an ordinary wire-graph
/// cycle (no special feedback handling needed). Returns true iff following wires
/// forward from `start` returns to `start`. Bounded by the node/wire counts; runs
/// once per fusion build (cached).
fn node_on_cycle(start: u32, def: &EffectGraphDef) -> bool {
    let mut stack = vec![start];
    let mut visited: AHashSet<u32> = AHashSet::default();
    while let Some(n) = stack.pop() {
        for w in &def.wires {
            if w.from_node != n {
                continue;
            }
            if w.to_node == start {
                return true; // wire chain from start loops back to start
            }
            if visited.insert(w.to_node) {
                stack.push(w.to_node);
            }
        }
    }
    false
}

/// Classify one node. `Eligible` requires *every* gate to pass; any failure —
/// including "the registry doesn't know this type" — is a `Boundary`.
fn classify_node(
    node: &EffectGraphNode,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> NodeClass {
    use crate::node_graph::freeze::classify::FusionKind;

    // Boundaries by identity: the graph endpoints are always seams.
    if node.type_id == SOURCE_TYPE_ID || node.type_id == FINAL_OUTPUT_TYPE_ID {
        return NodeClass::Boundary;
    }
    let Some(n) = registry.construct(&node.type_id) else {
        return NodeClass::Boundary; // unknown atom → never fuse
    };

    // Three kinds fold INTO a region: Pointwise / MultiInputCoincident thread
    // their input register(s); Source is a 0-input generator that produces the
    // region's head value from uv/dims (no input register — the fused codegen
    // already calls a 0-input body as `n{i}_body(uv, dims, params)`). Everything
    // else (Boundary) is a seam.
    if !matches!(
        n.fusion_kind(),
        FusionKind::Pointwise | FusionKind::MultiInputCoincident | FusionKind::Source
    ) {
        return NodeClass::Boundary;
    }
    if n.wgsl_body().is_none() {
        return NodeClass::Boundary;
    }

    // Every param must lay out as a scalar uniform field — the fused codegen
    // ([`super::codegen::generate_fused`]) can only carry scalars. Vec3/Table/
    // string params keep the atom standalone (a boundary) until the fused path
    // grows those layouts.
    for p in n.parameters() {
        if param_wgsl_type(p).is_err() {
            return NodeClass::Boundary;
        }
    }

    // BUFFER-domain atom (writes an `Array<T>` — particle / instance / curve).
    // The write-only-output model (fused output as a `@fused_output` array, not an
    // aliased read_write one, so the node's read-only inputs stay forward deps and
    // run after their producers) fixed the execution-ORDERING bug, and the
    // compute `arrayLength()` buffer-size-buffer index fix (manifold-gpu pins
    // SPIRV-Cross's `buffer_size_buffer_index` to the slot it actually binds)
    // closed the residual — `digitalplants_buffer_fusion_renders_like_unfused` is
    // now bit-exact (0/160000 differing). Buffer atoms fuse on the live path.
    if n.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))) {
        return classify_buffer_node(n.as_ref(), node, def, registry);
    }

    // Texture I/O shape: exactly one texture output (the register the region
    // threads). A Source reads NO texture input (it generates from position); the
    // threaded kinds read ≥1. An atom with two texture outputs (voronoi) needs
    // multi-output fused codegen — a follow-on; boundary for now.
    let tex_in = n.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
    let tex_out = n.outputs().iter().filter(|o| is_texture_port(&o.ty)).count();
    let arity_ok = if matches!(n.fusion_kind(), FusionKind::Source) {
        tex_in == 0
    } else {
        tex_in >= 1
    };
    if !arity_ok || tex_out != 1 {
        return NodeClass::Boundary;
    }

    // A gather input (the body samples at a coord it computes) IS eligible now:
    // the codegen binds the gathered texture + a sampler and the body samples it,
    // and the finder keeps the gathered producer external (it never unions across
    // a gather-consumed wire — see `partition_regions`). But a RESAMPLE — a gather
    // whose output resolution differs from the canvas (downsample, a
    // resolution-setting generator) — must stay a boundary: the fused node would
    // inherit the canvas dst size, not the resample's, and iterate at the wrong
    // resolution. Detect it generically via an `output_canvas_scale` override
    // (≠ 1:1), never a hard-coded atom list. (Fusing a resample correctly =
    // propagating the fused node's output scale — the element-space follow-on.)
    let default_params: crate::node_graph::effect_node::ParamValues = AHashMap::default();
    for o in n.outputs().iter().filter(|o| is_texture_port(&o.ty)) {
        if let Some(scale) = n.output_canvas_scale(o.name, &default_params)
            && scale != (1, 1)
        {
            return NodeClass::Boundary;
        }
    }

    // Conservative element-space (design §11.A): the fused kernel reads its
    // externals via `textureLoad` at its own coord — no rescale — so fusing
    // across a resolution/scale seam reads garbage. v1 fuses only the default
    // full-canvas 2D space: any explicit canvas-scale override, or a 3D port,
    // makes the atom a boundary. (The precise per-node element-space
    // propagation that would let quarter-res chains fuse internally is the next
    // increment; until then, cutting at every scale change is correct, just
    // conservative.)
    if !node.output_canvas_scales.is_empty() {
        return NodeClass::Boundary;
    }
    if n.inputs().iter().any(|i| i.ty == PortType::Texture3D)
        || n.outputs().iter().any(|o| o.ty == PortType::Texture3D)
    {
        return NodeClass::Boundary;
    }

    // Control wires (a scalar driving a param port — LFO → gain.gain, a focus
    // distance → scale_offset.offset — not a texture):
    //   - INTO this node's scalar param: fine. After fusion the producer feeds the
    //     fused node's port-shadow `n{i}_<param>` (DD-A5), so the param keeps being
    //     driven every frame. A wire into any OTHER non-texture input (an
    //     Array/buffer port the fused uniform can't carry) still cuts.
    //   - OUT of this node into someone else's param makes this a control PRODUCER.
    //     It must stay a boundary so it survives the rewrite and can wire its scalar
    //     into the fused node — folding it away would strand the scalar (the fused
    //     node only exposes its members' texture output). Pure scalar nodes are
    //     already boundaries by arity; this also catches a texture atom that
    //     additionally emits a control scalar, keeping the install rewrite local.
    let tex_ports: AHashSet<&str> = n
        .inputs()
        .iter()
        .filter(|i| is_texture_port(&i.ty))
        .map(|i| i.name)
        .collect();
    let scalar_params: AHashSet<&str> = n
        .parameters()
        .iter()
        .filter(|p| param_wgsl_type(p).is_ok())
        .map(|p| p.name)
        .collect();
    for w in &def.wires {
        if w.to_node == node.id
            && !tex_ports.contains(w.to_port.as_str())
            && !scalar_params.contains(w.to_port.as_str())
        {
            return NodeClass::Boundary;
        }
        if w.from_node == node.id && is_scalar_param_wire(def, registry, w) {
            return NodeClass::Boundary;
        }
    }

    // Feedback-loop exclusion (TEXTURE atoms only — buffer atoms were dispatched
    // above and are exempt). A texture atom inside a feedback loop must NOT fuse.
    // Fusing keeps intermediates in f32 registers, but the unfused editor stores
    // them through the atom's HARD-CODED rgba16float output and rounds to f16. The
    // two can't be reconciled: the GPU rounds a register differently than a texture
    // write (so a fused-side f16 round doesn't match), and the curated atoms can't
    // switch to fp32 (their shader format is compiled-in). For an ordinary effect
    // that f16-vs-f32 gap is invisible, but a chaotic feedback sim (FluidSim's flow
    // field, OilyFluid) amplifies it — and since the editor renders unfused and the
    // stage renders fused, the look would shift the moment the editor closes. So we
    // keep in-loop texture atoms unfused: editor and stage then run them
    // identically. Buffer atoms stay fused — their f32 register threading is
    // already bit-exact and their in-place feedback fusion is preserved.
    if node_on_cycle(node.id, def) {
        return NodeClass::Boundary;
    }

    // Final gate: the body must produce a kernel the PLAIN pipeline compiler
    // (naga) accepts. A few atoms — gaussian_blur_variable_width, the separable
    // gaussian — compile only through `create_specialized_compute_pipeline`, which
    // textually substitutes specialization tokens (QUALITY_LEVEL, WEIGHTING_MODE)
    // BEFORE naga sees the source; their bodies reference those tokens as free
    // identifiers. The fused node is a `node.wgsl_compute`, which parses +
    // dispatches through the plain (non-specialized) path, so such a body can't
    // fuse there. Detect it generically — generate this atom's standalone kernel
    // and parse it; if it can't parse on its own, it can't parse inside a fused
    // kernel either → boundary. No hard-coded atom list, so any future
    // specialization / free-identifier body is caught the same way.
    let standalone = crate::node_graph::freeze::codegen::generate_standalone(
        n.fusion_kind(),
        n.wgsl_body().unwrap_or(""),
        n.inputs(),
        n.parameters(),
        n.input_access(),
        n.outputs(),
    );
    match standalone {
        Ok(kernel) if naga::front::wgsl::parse_str(&kernel).is_ok() => NodeClass::Eligible,
        _ => NodeClass::Boundary,
    }
}

/// Classify a BUFFER-domain atom (writes an `Array<T>` — particle / instance /
/// curve element) for fusion eligibility. The buffer twin of the texture gates
/// in [`classify_node`]: the atom must match the v1 buffer codegen contract
/// ([`super::codegen::generate_fused`]'s buffer branch) — ≥1 Array input threaded
/// as an element register, exactly one Array output, no texture I/O, no
/// `BufferGather`, no frame-derived uniforms, no atomic output — and its
/// non-Array wires must be region edges or re-anchorable scalar params, and it
/// must not be a control PRODUCER. Anything else is a `Boundary`. The standalone
/// naga-parse gate the texture path uses is NOT applied here (it threads no
/// `wgsl_includes`, so a noise-based buffer body would falsely fail); the install
/// pass naga-parses the FUSED kernel as the real guard, falling back to unfused.
#[allow(dead_code)] // Re-enabled when the buffer-fusion call site in classify_node is un-gated.
fn classify_buffer_node(
    n: &dyn crate::node_graph::effect_node::EffectNode,
    node: &EffectGraphNode,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> NodeClass {
    let arr_in = n.inputs().iter().filter(|i| matches!(i.ty, PortType::Array(_))).count();
    let arr_out = n.outputs().iter().filter(|o| matches!(o.ty, PortType::Array(_))).count();
    // v1 codegen shape: ≥1 Array input, exactly one Array output (fan-out buffer
    // regions are a follow-on).
    if arr_in < 1 || arr_out != 1 {
        return NodeClass::Boundary;
    }
    // No texture I/O — the `*_at_particles` volume samplers (texture-gather buffer
    // atoms) stay boundaries so a particle pipeline cuts cleanly around them.
    if n.inputs().iter().any(|i| is_texture_port(&i.ty))
        || n.outputs().iter().any(|o| is_texture_port(&o.ty))
    {
        return NodeClass::Boundary;
    }
    // A BufferGather input (neighbor_smooth) indexes its global itself — it can't
    // thread one element register, so it stays a boundary (the gathered producer
    // is kept external; the finder never unions a gather-consumed wire).
    if n.input_access().iter().any(|a| a.is_gather()) {
        return NodeClass::Boundary;
    }
    // Frame-derived-uniform integrators (euler_step's dt_scaled, the forces'
    // frame_count) FUSE: the fused buffer codegen emits each derived uniform as an
    // `n{i}_<name>` Params field + body arg, and the install pass wires it from
    // system.generator_input (frame_delta / frame_count / time) — the same
    // wired-frame-value mechanism scatter's width/height use. The in-place-loop
    // hazard (fusing a region inside array_feedback's in==out loop) is handled at
    // the install/region level: `region_output_aliases_external` +
    // `external_is_inplace_loop` detect a feedback-loop region and the codegen
    // writes back to the aliased `src_k` buffer in place, preserving the loop. So
    // this per-node gate no longer excludes derived-uniform atoms; the install bails
    // to unfused only if it can't source a derived uniform (no generator_input, or a
    // vec3 camera basis). Proven by `fluidsim_buffer_fusion_renders_like_unfused`.
    //
    // Atomic-accumulator outputs (scatter) write via `atomicAdd`, not a coincident
    // element write — boundary.
    if !n.atomic_outputs().is_empty() {
        return NodeClass::Boundary;
    }
    // Wire gate: an Array input wire is a region edge (threads or stays external);
    // a scalar-param wire is re-anchored onto the fused port-shadow; any other
    // wire into a non-Array non-scalar-param input cuts; a control PRODUCER stays
    // a boundary so its scalar survives the rewrite to wire into the fused node.
    let arr_ports: AHashSet<&str> = n
        .inputs()
        .iter()
        .filter(|i| matches!(i.ty, PortType::Array(_)))
        .map(|i| i.name)
        .collect();
    let scalar_params: AHashSet<&str> = n
        .parameters()
        .iter()
        .filter(|p| param_wgsl_type(p).is_ok())
        .map(|p| p.name)
        .collect();
    for w in &def.wires {
        if w.to_node == node.id
            && !arr_ports.contains(w.to_port.as_str())
            && !scalar_params.contains(w.to_port.as_str())
        {
            return NodeClass::Boundary;
        }
        if w.from_node == node.id && is_scalar_param_wire(def, registry, w) {
            return NodeClass::Boundary;
        }
    }
    NodeClass::Eligible
}

/// Assemble a [`Region`] from a connected component's node set, or `None` if it
/// fails a v1 expressibility gate (too short, multi-output, or an unresolvable
/// input — all left unfused).
fn build_region(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    nodes: &[u32],
    final_reachable: &AHashSet<u32>,
) -> Option<Region> {
    if nodes.len() < MIN_REGION_LEN {
        return None;
    }
    let node_set: AHashSet<u32> = nodes.iter().copied().collect();
    // Texture vs buffer region — drives every port/wire filter below (a region is
    // homogeneous: texture and Array ports never wire to each other).
    let is_buffer = region_is_buffer(nodes, def, registry);

    // Topo-sort the members by intra-region wires so every Member input refers to
    // an earlier entry (the codegen threads registers in this order).
    let order = topo_sort(nodes, def, registry, &node_set, is_buffer)?;

    // Resolve external inputs (deduped, first-seen order) + each member's inputs.
    let mut externals: Vec<ExternalRef> = Vec::new();
    let mut ext_index: AHashMap<(u32, String), usize> = AHashMap::default();
    let mut members: Vec<RegionMember> = Vec::with_capacity(order.len());
    for &doc_id in &order {
        let node = def.nodes.iter().find(|n| n.id == doc_id)?;
        let constructed = registry.construct(&node.type_id)?;
        let tex_ports: Vec<&str> = constructed
            .inputs()
            .iter()
            .filter(|i| region_port_is_member(&i.ty, is_buffer))
            .map(|i| i.name)
            .collect();
        let access_list = constructed.input_access();
        let mut inputs: Vec<RegionInput> = Vec::with_capacity(tex_ports.len());
        let mut input_access: Vec<InputAccess> = Vec::with_capacity(tex_ports.len());
        for (idx, port) in tex_ports.iter().enumerate() {
            let access = access_list.get(idx).copied().unwrap_or(InputAccess::Coincident);
            let wire = def
                .wires
                .iter()
                .find(|w| w.to_node == doc_id && w.to_port == *port)?;
            let resolved = if node_set.contains(&wire.from_node) {
                // A gather input must read an external texture, never a region
                // register (a register is one texel). The finder never unions
                // across a gather-consumed wire, so a gathered producer should
                // never be a member — bail defensively if one slipped through.
                if access.is_gather() {
                    return None;
                }
                RegionInput::Member(wire.from_node)
            } else {
                let key = (wire.from_node, wire.from_port.clone());
                let slot = *ext_index.entry(key).or_insert_with(|| {
                    externals.push(ExternalRef {
                        from_node: wire.from_node,
                        from_port: wire.from_port.clone(),
                    });
                    externals.len() - 1
                });
                RegionInput::External(slot)
            };
            inputs.push(resolved);
            input_access.push(access);
        }
        members.push(RegionMember { doc_id, inputs, input_access });
    }

    // The region output(s): each member with ≥1 texture wire to a non-member. A
    // single-output linear chain has one; a FAN-OUT region (an interior member
    // feeds two distinct downstream boundaries) has several — each stored to its
    // own `dst_<k>`. Every escaping consumer MUST be live (reach `final_output`):
    // the executor only allocates a texture a live node reads, and the fused
    // `WgslCompute` early-returns its WHOLE dispatch if any storage output is
    // unbound — so a `dst_<k>` feeding a dead consumer would silently kill the
    // live outputs too. If any escaping wire targets a dead consumer, drop the
    // whole region (it renders unfused, always correct).
    let mut outputs: Vec<u32> = Vec::new();
    for &id in nodes {
        let mut escapes = false;
        for w in &def.wires {
            if w.from_node == id
                && !node_set.contains(&w.to_node)
                && is_region_wire(def, registry, w, is_buffer)
            {
                if !final_reachable.contains(&w.to_node) {
                    return None; // escaping wire to a dead consumer — don't fuse
                }
                escapes = true;
            }
        }
        if escapes {
            outputs.push(id);
        }
    }
    outputs.sort_unstable();
    if outputs.is_empty() {
        return None; // dead region — nothing leaves it, nothing to fuse
    }

    // v1 buffer regions are single-output (the fused node writes one fresh `dst`
    // array). Fan-out buffer regions are a follow-on. Texture regions allow
    // multi-output (fan-out) as before.
    if is_buffer && outputs.len() != 1 {
        return None;
    }

    Some(Region { members, externals, outputs })
}

/// Kahn topo-sort of a region's members by intra-region texture wires. `None` on
/// a cycle (feedback never appears in a pure region, but fail closed).
fn topo_sort(
    nodes: &[u32],
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    node_set: &AHashSet<u32>,
    is_buffer: bool,
) -> Option<Vec<u32>> {
    let mut indeg: AHashMap<u32, u32> = nodes.iter().map(|&id| (id, 0)).collect();
    let mut adj: AHashMap<u32, Vec<u32>> = AHashMap::default();
    for w in &def.wires {
        if node_set.contains(&w.from_node)
            && node_set.contains(&w.to_node)
            && w.from_node != w.to_node
            && is_region_wire(def, registry, w, is_buffer)
        {
            adj.entry(w.from_node).or_default().push(w.to_node);
            *indeg.get_mut(&w.to_node)? += 1;
        }
    }
    let mut queue: Vec<u32> = nodes.iter().copied().filter(|id| indeg[id] == 0).collect();
    queue.sort_unstable();
    let mut order: Vec<u32> = Vec::with_capacity(nodes.len());
    while let Some(id) = queue.pop() {
        order.push(id);
        if let Some(succs) = adj.get(&id) {
            let mut ready: Vec<u32> = Vec::new();
            for &s in succs {
                let d = indeg.get_mut(&s)?;
                *d -= 1;
                if *d == 0 {
                    ready.push(s);
                }
            }
            ready.sort_unstable();
            for s in ready.into_iter().rev() {
                queue.push(s);
            }
        }
    }
    (order.len() == nodes.len()).then_some(order)
}

/// The read-access of `type_id`'s texture input `port` (Coincident if the atom
/// is unknown or the port isn't one of its texture inputs). `input_access()` is
/// aligned to the atom's TEXTURE inputs in `inputs()` order.
fn input_port_access(registry: &PrimitiveRegistry, type_id: &str, port: &str) -> InputAccess {
    let Some(node) = registry.construct(type_id) else {
        return InputAccess::Coincident;
    };
    // `input_access` aligns to the SAME-domain inputs in declaration order:
    // texture inputs for a texture atom, Array inputs for a buffer atom (the
    // buffer codegen's `is_gather(i)` indexes the filtered Array inputs). Resolve
    // the port's index among inputs of its own kind so a `BufferGather` Array
    // input is detected (not silently treated as coincident).
    let port_ty = node.inputs().iter().find(|i| i.name == port).map(|i| i.ty);
    let idx = match port_ty {
        Some(ty) if is_texture_port(&ty) => {
            node.inputs().iter().filter(|i| is_texture_port(&i.ty)).position(|i| i.name == port)
        }
        Some(PortType::Array(_)) => node
            .inputs()
            .iter()
            .filter(|i| matches!(i.ty, PortType::Array(_)))
            .position(|i| i.name == port),
        _ => None,
    };
    match idx {
        Some(idx) => node.input_access().get(idx).copied().unwrap_or_default(),
        None => InputAccess::Coincident,
    }
}

/// Whether wire `w` is consumed COINCIDENTALLY by its target (a register-threaded
/// read) rather than GATHERED (the target samples it at a coord it computes).
/// Only coincident-consumed wires union two atoms into one region — see
/// `partition_regions`.
fn wire_coincident_consumed(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    w: &EffectGraphWire,
) -> bool {
    let Some(to) = def.nodes.iter().find(|n| n.id == w.to_node) else {
        return false;
    };
    !input_port_access(registry, &to.type_id, &w.to_port).is_gather()
}

/// Nodes with a directed path to a `final_output` node, over ALL wires (texture
/// and control alike). A region output's downstream consumer must be in this set:
/// the executor only allocates an output texture some live node reads, and the
/// fused [`super::codegen::generate_fused`] kernel — a `node.wgsl_compute` —
/// early-returns its WHOLE dispatch if any of its storage outputs is unbound. So a
/// `dst_<k>` wired to a dead consumer would silently kill the region's live
/// outputs too; `build_region` refuses to fuse a region with such a wire.
///
/// `final_output`-reachability is a safe SUBSET of the executor's full liveness
/// (which also roots at `aliased_array_io` sims): a node we mark live here is
/// always allocated, and an exotic live-but-not-final node only makes us skip the
/// region (unfused), never miscompile.
fn final_reachable_nodes(def: &EffectGraphDef) -> AHashSet<u32> {
    // Reverse adjacency (consumer → producers), so a backward BFS from every
    // final_output node visits exactly the nodes that can reach it.
    let mut rev: AHashMap<u32, Vec<u32>> = AHashMap::default();
    for w in &def.wires {
        rev.entry(w.to_node).or_default().push(w.from_node);
    }
    let mut live: AHashSet<u32> = AHashSet::default();
    let mut queue: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
        .map(|n| n.id)
        .collect();
    for &id in &queue {
        live.insert(id);
    }
    while let Some(id) = queue.pop() {
        if let Some(producers) = rev.get(&id) {
            for &p in producers {
                if live.insert(p) {
                    queue.push(p);
                }
            }
        }
    }
    live
}

/// Whether wire `w` is a CONTROL wire — it drives a scalar param port of its
/// target (LFO → gain.gain), as opposed to feeding a texture input. The target's
/// scalar params shadow as same-named input ports, and texture inputs have
/// distinct names, so a `to_port` that names a scalar param is unambiguously a
/// control wire. Used by `classify_node` to keep control PRODUCERS as boundaries
/// (so they survive and can wire into the fused node's port-shadow) and by
/// `install` to re-anchor those wires onto `n{i}_<param>`.
fn is_scalar_param_wire(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    w: &EffectGraphWire,
) -> bool {
    let Some(to) = def.nodes.iter().find(|n| n.id == w.to_node) else {
        return false;
    };
    let Some(node) = registry.construct(&to.type_id) else {
        return false;
    };
    node.parameters()
        .iter()
        .any(|p| p.name == w.to_port && param_wgsl_type(p).is_ok())
}

/// Whether wire `w` feeds a STATE-CAPTURE input port of its target (a feedback
/// node's captured input — last frame's value). The planner treats these as back
/// edges that don't form cycles ([`Graph::is_state_capture_wire`] /
/// `WireWalkMode::ForwardOnly`), so the convexity test excludes them too —
/// otherwise a legal feedback loop reads as a cycle and we over-split.
fn is_state_capture_wire(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    w: &EffectGraphWire,
) -> bool {
    let Some(to) = def.nodes.iter().find(|n| n.id == w.to_node) else {
        return false;
    };
    let Some(node) = registry.construct(&to.type_id) else {
        return false;
    };
    node.state_capture_input_ports().contains(&w.to_port.as_str())
}

/// Whether the collapsed forward graph has a directed cycle. `key` maps each def
/// node to its collapsed identity — an eligible node to its region rep, a boundary
/// to itself — so a self-edge (both endpoints in one region) is dropped and an
/// inter-identity edge is kept. Iterative three-colour DFS; the graphs are tiny
/// (one effect's nodes), so rebuilding per tentative merge is free.
fn collapsed_has_cycle(forward: &[(u32, u32)], key: &impl Fn(u32) -> u32) -> bool {
    let mut adj: AHashMap<u32, Vec<u32>> = AHashMap::default();
    let mut nodes: AHashSet<u32> = AHashSet::default();
    for &(u, v) in forward {
        let (ku, kv) = (key(u), key(v));
        nodes.insert(ku);
        nodes.insert(kv);
        if ku != kv {
            adj.entry(ku).or_default().push(kv);
        }
    }
    // 0 = white (unvisited), 1 = grey (on stack), 2 = black (done).
    let mut color: AHashMap<u32, u8> = nodes.iter().map(|&n| (n, 0u8)).collect();
    for &start in &nodes {
        if color[&start] != 0 {
            continue;
        }
        let mut stack: Vec<(u32, usize)> = vec![(start, 0)];
        color.insert(start, 1);
        while let Some(&(n, idx)) = stack.last() {
            if let Some(succs) = adj.get(&n)
                && idx < succs.len()
            {
                stack.last_mut().unwrap().1 += 1;
                let m = succs[idx];
                match color[&m] {
                    1 => return true,                          // grey → back edge → cycle
                    0 => {
                        color.insert(m, 1);
                        stack.push((m, 0));
                    }
                    _ => {}
                }
                continue;
            }
            color.insert(n, 2);
            stack.pop();
        }
    }
    false
}

/// Whether a wire carries a texture (vs a scalar/control value), determined by
/// the producer's output port type.
fn is_texture_wire(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    w: &EffectGraphWire,
) -> bool {
    let Some(from) = def.nodes.iter().find(|n| n.id == w.from_node) else {
        return false;
    };
    // Boundary endpoints (source) always emit a texture on `out`.
    if from.type_id == SOURCE_TYPE_ID {
        return true;
    }
    let Some(node) = registry.construct(&from.type_id) else {
        return false;
    };
    node.outputs()
        .iter()
        .find(|o| o.name == w.from_port)
        .map(|o| is_texture_port(&o.ty))
        .unwrap_or(false)
}

fn is_texture_port(ty: &PortType) -> bool {
    matches!(
        ty,
        PortType::Texture2D | PortType::Texture2DTyped(_) | PortType::Texture3D
    )
}

/// Whether a wire carries an `Array<T>` (buffer / particle / instance / curve)
/// value, by the producer's output port type. The buffer-domain analogue of
/// [`is_texture_wire`]; the region grower unions over coincident wires of EITHER
/// kind so a particle pipeline (Array wires) fuses just like a pixel chain.
fn is_array_wire(def: &EffectGraphDef, registry: &PrimitiveRegistry, w: &EffectGraphWire) -> bool {
    let Some(from) = def.nodes.iter().find(|n| n.id == w.from_node) else {
        return false;
    };
    let Some(node) = registry.construct(&from.type_id) else {
        return false;
    };
    node.outputs()
        .iter()
        .find(|o| o.name == w.from_port)
        .map(|o| matches!(o.ty, PortType::Array(_)))
        .unwrap_or(false)
}

/// A region's data domain. Texture regions thread `vec4` texel registers and
/// dispatch over a texture grid; buffer regions thread element-struct registers
/// and dispatch 1D over an Array length. A region is homogeneous (texture and
/// Array ports never wire to each other), so one flag drives every port/wire
/// filter in [`build_region`] / [`topo_sort`].
fn region_port_is_member(ty: &PortType, is_buffer: bool) -> bool {
    if is_buffer {
        matches!(ty, PortType::Array(_))
    } else {
        is_texture_port(ty)
    }
}

fn is_region_wire(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    w: &EffectGraphWire,
    is_buffer: bool,
) -> bool {
    if is_buffer {
        is_array_wire(def, registry, w)
    } else {
        is_texture_wire(def, registry, w)
    }
}

/// Whether a region's members are buffer-domain (their fused output is an
/// `Array<T>`). Determined from any member's constructed output ports.
fn region_is_buffer(nodes: &[u32], def: &EffectGraphDef, registry: &PrimitiveRegistry) -> bool {
    nodes.iter().any(|&id| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| registry.construct(&n.type_id))
            .map(|c| c.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))))
            .unwrap_or(false)
    })
}

/// Minimal union-find over a fixed node set (region growing). Path-halving +
/// union-by-size; ids are def doc-ids.
struct UnionFind {
    parent: AHashMap<u32, u32>,
    size: AHashMap<u32, u32>,
}

impl UnionFind {
    fn new(ids: &AHashSet<u32>) -> Self {
        UnionFind {
            parent: ids.iter().map(|&id| (id, id)).collect(),
            size: ids.iter().map(|&id| (id, 1)).collect(),
        }
    }

    fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[&x] != x {
            let grand = self.parent[&self.parent[&x]];
            *self.parent.get_mut(&x).unwrap() = grand; // path halving
            x = grand;
        }
        x
    }

    fn union(&mut self, a: u32, b: u32) {
        let (mut ra, mut rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        if self.size[&ra] < self.size[&rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        *self.parent.get_mut(&rb).unwrap() = ra;
        *self.size.get_mut(&ra).unwrap() += self.size[&rb];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    fn colorgrade_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/effect-presets/ColorGrade.json"
        ))
        .expect("read ColorGrade.json");
        serde_json::from_str(&json).expect("parse ColorGrade.json")
    }

    /// The whole ColorGrade card is one region: all 7 atoms, one external (the
    /// source, read once even though both gain and mix.a read it), output = the
    /// clamp that feeds final_output. This is the existing whole-card case now
    /// expressed as the general partition's single-component result.
    #[test]
    fn colorgrade_is_one_region() {
        let regions = partition_regions(&colorgrade_def(), &registry());
        assert_eq!(regions.len(), 1, "ColorGrade is a single region");
        let r = &regions[0];
        assert_eq!(r.members.len(), 7, "all 7 color atoms");
        assert_eq!(r.externals.len(), 1, "source read once (gain + mix.a share it)");
        // The clamp (the atom feeding final_output) is the region output.
        let out_node = colorgrade_def()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.clamp_texture")
            .map(|n| n.id)
            .unwrap();
        assert_eq!(r.outputs, vec![out_node], "clamp is the region output");
        // mix reads the external fork AND colorize's register: an External + a
        // Member input, proving the fork resolves.
        let mix_id = colorgrade_def()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.mix")
            .map(|n| n.id)
            .unwrap();
        let mix = r.members.iter().find(|m| m.doc_id == mix_id).unwrap();
        assert!(
            mix.inputs.iter().any(|i| matches!(i, RegionInput::External(0)))
                && mix.inputs.iter().any(|i| matches!(i, RegionInput::Member(_))),
            "mix threads the source fork (External) + colorize (Member)"
        );
    }

    /// A true boundary in the middle splits the graph into TWO regions — the
    /// headline generalisation. source → gain → contrast → threshold(boundary) →
    /// saturation → clamp → final yields {gain, contrast} feeding the threshold,
    /// then {saturation, clamp} reading the threshold's output. (`node.threshold`
    /// is unconverted, so it has no `wgsl_body` and stays a boundary — unlike a
    /// gather such as `gaussian_blur`, which tier 3 folds IN; see the gather
    /// tests below.)
    #[test]
    fn boundary_splits_into_two_regions() {
        let json = r#"{
            "version": 1, "name": "split", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.gain", "nodeId": "gain" },
                { "id": 2, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 3, "typeId": "node.threshold", "nodeId": "thresh" },
                { "id": 4, "typeId": "node.saturation", "nodeId": "sat" },
                { "id": 5, "typeId": "node.clamp_texture", "nodeId": "clamp" },
                { "id": 6, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in" },
                { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let mut regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 2, "the threshold boundary splits the graph in two");
        regions.sort_by_key(|r| r.members[0].doc_id);

        // Region 1: gain(1) → contrast(2), reads the source, output = contrast.
        let r1 = &regions[0];
        assert_eq!(r1.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(r1.externals.len(), 1, "region 1 reads the source");
        assert_eq!(r1.externals[0].from_node, 0);
        assert_eq!(r1.outputs, vec![2], "contrast feeds the threshold");

        // Region 2: saturation(4) → clamp(5), reads the threshold, output = clamp.
        let r2 = &regions[1];
        assert_eq!(r2.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![4, 5]);
        assert_eq!(r2.externals.len(), 1, "region 2 reads the threshold output");
        assert_eq!(r2.externals[0].from_node, 3, "the threshold is region 2's external");
        assert_eq!(r2.outputs, vec![5], "clamp feeds final_output");
    }

    /// Tier 3 — a gather atom folds INTO a region (it does NOT split it). source →
    /// gain → sharpen(Gather) → invert → final fuses into ONE region: gain threads
    /// to sharpen, sharpen gathers gain's register? No — a gather can't read a
    /// register, so the finder does NOT union gain→sharpen (a gather-consumed
    /// wire), leaving gain a lone 1-node region (dropped), and {sharpen, invert}
    /// the real region where sharpen gathers gain's output as an EXTERNAL and
    /// threads its result to invert.
    #[test]
    fn gather_atom_folds_into_a_region() {
        let json = r#"{
            "version": 1, "name": "warp", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.sharpen", "nodeId": "sharp" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "sharpen (gather) + invert are one region");
        let r = &regions[0];
        assert_eq!(r.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(r.externals.len(), 1, "sharpen gathers the source as an external");
        assert_eq!(r.externals[0].from_node, 0);
        // sharpen's one input is Gather, resolved to the external; invert reads
        // sharpen's register coincidentally.
        let sharp = r.members.iter().find(|m| m.doc_id == 1).unwrap();
        assert_eq!(sharp.input_access, vec![InputAccess::Gather]);
        assert_eq!(sharp.inputs, vec![RegionInput::External(0)]);
        let invert = r.members.iter().find(|m| m.doc_id == 2).unwrap();
        assert_eq!(invert.inputs, vec![RegionInput::Member(1)]);
    }

    /// A gather never unions across its gathered wire: gain → sharpen, where
    /// sharpen GATHERS gain, must NOT pull gain into sharpen's region (a register
    /// can't carry a whole texture). gain is left a lone atom (dropped), so the
    /// only region is {sharpen, invert} from the test above — here we assert the
    /// negative directly: a chain gain → sharpen with nothing downstream produces
    /// no region (both are 1-node after the gather cut).
    #[test]
    fn gather_wire_does_not_union() {
        let json = r#"{
            "version": 1, "name": "cut", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.gain", "nodeId": "gain" },
                { "id": 2, "typeId": "node.sharpen", "nodeId": "sharp" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert!(
            partition_regions(&def, &registry()).is_empty(),
            "the gather cut leaves gain and sharpen as lone atoms — neither fuses"
        );
    }

    /// A lone fusable atom is not a region (fusing one node changes nothing). The
    /// MIN_REGION_LEN gate drops it; the card renders unfused.
    #[test]
    fn single_atom_is_not_a_region() {
        let json = r#"{
            "version": 1, "name": "solo", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.gain", "nodeId": "gain" },
                { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert!(
            partition_regions(&def, &registry()).is_empty(),
            "a single atom is below MIN_REGION_LEN — not worth fusing"
        );
    }

    /// A graph with no fusable atoms at all yields no regions (the common case
    /// today, always safe).
    #[test]
    fn all_boundary_graph_has_no_regions() {
        let json = r#"{
            "version": 1, "name": "edge", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.edge_detect", "nodeId": "edge" },
                { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert!(partition_regions(&def, &registry()).is_empty());
    }

    /// Tier 2 — a Source generator heads a region. checkerboard (0 texture
    /// inputs, fusion_kind Source) → invert (Pointwise) form one region whose
    /// head reads NO external (it produces from position); invert threads the
    /// generator's register. The fused codegen already calls a 0-input body as
    /// `body(uv, dims, params)`, so this is purely a finder unlock.
    #[test]
    fn source_generator_heads_a_region() {
        let json = r#"{
            "version": 1, "name": "gen", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "the generator + invert form one region");
        let r = &regions[0];
        assert_eq!(r.members.len(), 2, "checkerboard + invert");
        assert!(r.externals.is_empty(), "a pure-generator region reads no external texture");
        assert_eq!(r.outputs, vec![2], "invert feeds final_output");
        let checker = r.members.iter().find(|m| m.doc_id == 1).unwrap();
        assert!(checker.inputs.is_empty(), "the Source head reads nothing");
        let invert = r.members.iter().find(|m| m.doc_id == 2).unwrap();
        assert_eq!(invert.inputs, vec![RegionInput::Member(1)], "invert threads the generator");
    }

    /// A Source generator blended with the incoming source texture: the system
    /// source feeds `mix` as an EXTERNAL while the generator feeds it as a
    /// region member. Exercises a region that has both a Source head and an
    /// external input (the common "overlay a pattern" shape).
    #[test]
    fn source_plus_external_in_one_region() {
        let json = r#"{
            "version": 1, "name": "overlay", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
                { "id": 2, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "a" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "b" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "checkerboard + mix are one region");
        let r = &regions[0];
        assert_eq!(r.members.len(), 2, "checkerboard + mix");
        assert_eq!(r.externals.len(), 1, "the system source is the one external");
        assert_eq!(r.externals[0].from_node, 0);
        assert_eq!(r.outputs, vec![2], "mix feeds final_output");
    }

    /// Fan-out — an interior member feeds two distinct downstream boundaries, so
    /// the region has TWO outputs (each stored to its own `dst_<k>`). gain forks
    /// into invert and contrast; each feeds its own `threshold` boundary, which
    /// re-merge at a `mix` before final. {gain, invert, contrast} is one region
    /// whose outputs are invert + contrast (gain is purely interior). Both
    /// thresholds reach final, so both outputs are live (allocatable).
    #[test]
    fn fanout_region_has_two_outputs() {
        let json = r#"{
            "version": 1, "name": "fanout", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.gain", "nodeId": "gain" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 4, "typeId": "node.threshold", "nodeId": "thr_a" },
                { "id": 5, "typeId": "node.threshold", "nodeId": "thr_b" },
                { "id": 6, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 7, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "source" },
                { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "source" },
                { "fromNode": 4, "fromPort": "out", "toNode": 6, "toPort": "a" },
                { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "b" },
                { "fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        // mix reads two boundaries, so it's a lone 1-node component (dropped);
        // the thresholds are boundaries. Exactly one real region: the fork.
        assert_eq!(regions.len(), 1, "the gain fork is the one region (mix is dropped)");
        let r = &regions[0];
        assert_eq!(
            r.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "gain (head) + the two forked atoms"
        );
        assert_eq!(r.externals.len(), 1, "only the source enters the region");
        assert_eq!(r.externals[0].from_node, 0);
        assert_eq!(
            r.outputs,
            vec![2, 3],
            "invert and contrast each escape to their own threshold — two outputs"
        );
    }

    /// A fan-out output whose consumer is DEAD (doesn't reach final_output) makes
    /// the whole region unfusable: the executor wouldn't allocate that output's
    /// texture, and the fused kernel early-returns its whole dispatch on the
    /// unbound store — killing the live output too. So the finder drops it
    /// (renders unfused, always correct) rather than emit an unallocated `dst_<k>`.
    #[test]
    fn fanout_to_dead_consumer_is_not_fused() {
        // gain → invert → final (live) ; gain → contrast → threshold (DEAD: the
        // threshold goes nowhere, never reaching final_output).
        let json = r#"{
            "version": 1, "name": "deadfork", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.gain", "nodeId": "gain" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 4, "typeId": "node.threshold", "nodeId": "dead" },
                { "id": 5, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 5, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "source" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert!(
            partition_regions(&def, &registry()).is_empty(),
            "a region with an escaping wire to a dead consumer must not fuse"
        );
    }

    /// Convexity — two register-adjacent atoms that ALSO have an external path
    /// between them (out through a boundary and back) must NOT land in one region:
    /// collapsing them would make the boundary both read from and write to the
    /// fused node, a cycle the graph builder rejects (Watercolor's real failure).
    /// gain forks into invert and a downstream mix; invert runs through a
    /// `threshold` boundary into contrast, and contrast also feeds the mix. A naive
    /// connected-component grouping unions {gain, invert, contrast, mix}, but
    /// invert→threshold→contrast makes that non-convex. The convex partition splits
    /// it: {gain, invert} before the threshold, {contrast, mix} after.
    #[test]
    fn convexity_splits_a_region_that_would_cycle() {
        let json = r#"{
            "version": 1, "name": "convex", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.gain", "nodeId": "gain" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.threshold", "nodeId": "thr" },
                { "id": 4, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 5, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 6, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "source" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 5, "toPort": "a" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "b" },
                { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let mut regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 2, "the non-convex group splits at the threshold");
        regions.sort_by_key(|r| r.members[0].doc_id);
        assert_eq!(
            regions[0].members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![1, 2],
            "gain + invert before the threshold"
        );
        assert_eq!(
            regions[1].members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![4, 5],
            "contrast + mix after the threshold"
        );
    }

    /// A specialization-constant atom is a BOUNDARY. `gaussian_blur_variable_width`
    /// compiles only through `create_specialized_compute_pipeline` (its body
    /// references `QUALITY_LEVEL` / `WEIGHTING_MODE` as free tokens substituted
    /// before naga sees the source); a fused `node.wgsl_compute` parses plain, so
    /// the atom can't fuse there. The classify gate catches it generically (its
    /// standalone kernel doesn't parse), so a chain through it forms no region.
    #[test]
    fn specialization_atom_is_a_boundary() {
        let json = r#"{
            "version": 1, "name": "spec", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.invert", "nodeId": "inv_a" },
                { "id": 2, "typeId": "node.gaussian_blur_variable_width", "nodeId": "blur" },
                { "id": 3, "typeId": "node.invert", "nodeId": "inv_b" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert!(
            partition_regions(&def, &registry()).is_empty(),
            "the specialization blur is a boundary, leaving both inverts lone atoms"
        );
    }

    /// A control wire into a scalar PARAM no longer cuts the consumer — it fuses,
    /// and the producer stays a boundary. `texture_dimensions` (a scalar reducer,
    /// boundary) drives `gain.gain`; gain + invert still form one region. The
    /// producer being a control producer keeps it surviving so install can route
    /// its scalar onto the fused node's port-shadow.
    #[test]
    fn control_wired_param_atom_still_fuses() {
        let json = r#"{
            "version": 1, "name": "ctrl", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.texture_dimensions", "nodeId": "dims" },
                { "id": 2, "typeId": "node.gain", "nodeId": "gain" },
                { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "aspect", "toNode": 2, "toPort": "gain" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "gain (control-wired) + invert are one region");
        assert_eq!(
            regions[0].members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![2, 3],
            "gain + invert fuse; texture_dimensions stays a boundary producer"
        );
    }

}

/// Whole-library fusion audit. Not a pass/fail gate — a `--nocapture` report that
/// runs the REAL region finder over every bundled effect + generator preset and
/// prints, per preset: grouped?, region count + sizes on the FLATTENED def, plus
/// flags for 3D-port and Array/buffer-domain atoms (the two domains the v1 finder
/// can't fuse yet). Run:
///   cargo test -p manifold-renderer --lib freeze::region::audit -- --nocapture
#[cfg(test)]
mod audit {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;
    use crate::node_graph::ports::PortType;
    use manifold_core::effect_graph_def::EffectGraphDef;

    fn domain_flags(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> (usize, usize, usize) {
        // Count worker atoms (non-boundary-by-identity), and how many touch a 3D
        // texture port or an Array port — the work the finder skips today.
        let mut workers = 0;
        let mut tex3d = 0;
        let mut arr = 0;
        for n in &def.nodes {
            if n.type_id == SOURCE_TYPE_ID || n.type_id == FINAL_OUTPUT_TYPE_ID {
                continue;
            }
            let Some(node) = registry.construct(&n.type_id) else {
                continue;
            };
            workers += 1;
            let ports = node.inputs().iter().map(|p| &p.ty).chain(node.outputs().iter().map(|p| &p.ty));
            let mut has3d = false;
            let mut hasarr = false;
            for ty in ports {
                if *ty == PortType::Texture3D {
                    has3d = true;
                }
                if matches!(ty, PortType::Array(_)) {
                    hasarr = true;
                }
            }
            if has3d {
                tex3d += 1;
            }
            if hasarr {
                arr += 1;
            }
        }
        (workers, tex3d, arr)
    }

    fn audit_one(name: &str, json: &str, registry: &PrimitiveRegistry) {
        let Ok(def) = serde_json::from_str::<EffectGraphDef>(json) else {
            eprintln!("[audit] {name}: PARSE FAILED");
            return;
        };
        let grouped = def.nodes.iter().any(|n| n.group.is_some());
        let raw = partition_regions(&def, registry).len();
        let flat = match manifold_core::flatten::flatten_groups(&def) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[audit] {name}: FLATTEN ERR {e:?}");
                return;
            }
        };
        let (workers, tex3d, arr) = domain_flags(&flat, registry);
        let regions = partition_regions(&flat, registry);
        let sizes: Vec<usize> = regions.iter().map(|r| r.members.len()).collect();
        let fused_atoms: usize = sizes.iter().sum();
        eprintln!(
            "[audit] {name:<26} grouped={grouped:<5} workers={workers:<3} 3d={tex3d:<3} arr={arr:<3} \
             raw_regions={raw:<2} flat_regions={:<2} fused_atoms={fused_atoms:<3} sizes={sizes:?}",
            regions.len(),
        );
    }

    #[test]
    #[ignore = "on-demand whole-library fusion report, not a pass/fail gate (~40s)"]
    fn audit_all_presets() {
        let registry = PrimitiveRegistry::with_builtin();
        eprintln!("=== EFFECT PRESETS ===");
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect) {
            if let Some(view) = crate::node_graph::loaded_preset_view_by_id(&type_id) {
                let json = serde_json::to_string(view.canonical_def).unwrap();
                audit_one(type_id.as_str(), &json, &registry);
            }
        }
        eprintln!("=== GENERATOR PRESETS ===");
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Generator) {
            if let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id) {
                audit_one(type_id.as_str(), &json, &registry);
            }
        }
    }
}
