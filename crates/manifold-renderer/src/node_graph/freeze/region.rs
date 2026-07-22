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
use crate::node_graph::freeze::space::{ElementSpace, resolve_output_spaces, space_of};
use crate::node_graph::ports::PortType;

/// Resolved per-output element spaces, or `None` when the def didn't build
/// standalone (synthetic fixtures) — every lookup then defaults to
/// [`ElementSpace::Canvas`], reproducing pre-tier-6 behaviour.
type SpaceMap = Option<AHashMap<(u32, String), ElementSpace>>;

/// Minimum members for a region to be worth fusing. A single-node "region" is
/// just the atom's own standalone kernel — fusing it changes nothing and only
/// adds a rewrite — so the smallest useful region threads one register between
/// two atoms (saving one full-canvas round-trip). The perf gate is the real
/// arbiter of whether a given region pays; this only avoids emitting no-ops.
const MIN_REGION_LEN: usize = 2;

/// How a graph node participates in fusion, resolved once per node.
///
/// `pub(crate)` (with [`classify_node`]) since GRAPH_TOOLING_DESIGN P3's
/// `graph_tool fusion` verb calls it directly for its per-node report — the
/// exact same classification `partition_regions` grows regions from, never
/// a second implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NodeClass {
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
    /// texture ports). Each resolves to an external slot, an earlier member,
    /// or `Unwired` (an optional port with no wire).
    pub inputs: Vec<RegionInput>,
    /// How each input in [`Self::inputs`] is read (aligned by index). A `Gather`
    /// entry's input is always an [`RegionInput::External`] — the codegen binds
    /// it as a texture (+ sampler) the body samples itself.
    pub input_access: Vec<InputAccess>,
    /// f16-faithful rounding (stencil tier A): this member sits inside a
    /// feedback loop and its unfused output texture is f16, so the unfused
    /// graph rounds its value to half precision every frame. The fused kernel
    /// must reproduce that rounding in-register (`q16` pack/unpack round-trip)
    /// or the f32 registers drift from the editor's unfused render and the
    /// loop amplifies the gap. False for fp32-marked members (their store is
    /// exact) and everything outside a loop (shipped behaviour, unchanged).
    pub quantize_f16: bool,
}

/// Where one of a member's texture inputs comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionInput {
    /// The region's Nth external input (a texture produced outside the region,
    /// read once into a register). Index into [`Region::externals`].
    External(usize),
    /// Another member's output register (must be earlier in topo order).
    Member(u32),
    /// Another member's output register, but that member is MULTI-OUTPUT (a
    /// struct-return body with ≥2 texture outputs — voronoi_2d's `out`/
    /// `cell_id`, D4/P6): the register alone isn't the value, so this names
    /// which `BodyOutputs` field the wire threads. Single-output members keep
    /// `Member(u32)` (the register IS the value already) — byte-identical for
    /// every prior region. Carries the producer's own output PORT NAME (from
    /// the wire), same shape as `ExternalRef::from_port`.
    MemberPort(u32, String),
    /// An OPTIONAL texture input with no wire (pack_channels' unwired b/a). The
    /// fused body receives a zero vector gated off by its injected use flag —
    /// folded to a literal `0u` at codegen since wiring is static in the def.
    Unwired,
    /// STENCIL tier: a Gather input backed by a VIRTUAL SOURCE — the producer
    /// chain is recomputed inside the consumer's `fetch_<port>` instead of
    /// rendered to a texture. Index into [`Region::virtual_chains`]. Only ever
    /// paired with a `Gather` access on a stencil-fetch member.
    Virtual(usize),
}

/// A pointwise/Source chain absorbed INTO a stencil member's gather read (the
/// stencil tier's "fuse through the blur"). The chain's members are deleted
/// from the installed def like ordinary fused members; the consumer's fetch
/// re-evaluates them at each tap's bilinear corner texels (externals read via
/// exact `textureLoad`, tail value q16-rounded to reproduce the f16 store the
/// unfused chain made), so the only fused-vs-unfused gap is the manual f32
/// lerp vs the hardware filter unit — measured by the stencil parity proof.
#[derive(Debug, Clone)]
pub struct VirtualChain {
    /// The consuming stencil member's doc id.
    pub consumer: u32,
    /// Which of the consumer's texture-input slots (index into its
    /// [`RegionMember::inputs`]) this chain backs.
    pub input_index: usize,
    /// Chain members in topo order. Inputs resolve to [`RegionInput::External`]
    /// (region externals — read at corner texels) or [`RegionInput::Member`] of
    /// an EARLIER chain member (never a main-region member; convexity ensures
    /// the region never feeds the chain).
    pub members: Vec<RegionMember>,
    /// Doc id of the chain's OUTPUT member — the node whose texture the unfused
    /// graph stored for the consumer to sample. Its (q16-rounded) value is what
    /// the fetch returns per corner; not necessarily topo-last (the output may
    /// also feed other chain members).
    pub output: u32,
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
    /// Each entry carries the escaping PORT NAME too (D4/P6): a single-output
    /// member has exactly one, so this is unchanged in shape for every prior
    /// region; a MULTI-output member (voronoi_2d) can appear TWICE here — once
    /// per distinct port that has a live external consumer — each its own
    /// `dst_<k>`.
    pub outputs: Vec<(u32, String)>,
    /// The element space every member ran at in the UNFUSED plan (tier 6).
    /// `Some` for texture regions — the install pass stamps a `Scaled` space
    /// onto the fused node's `output_canvas_scales` so the executor sizes the
    /// fused output exactly like the member output it replaced, and the
    /// build-check verifies the fused def resolves back to this space.
    /// `None` for buffer (Array) regions, which have no texture grid.
    pub space: Option<ElementSpace>,
    /// CROSS-RESOLUTION externals (workstream 4): external slots whose producer
    /// resolved to a DIFFERENT element space than the region's own grid and are
    /// read `Coincident` (resolution-robust). The fused kernel can't `textureLoad`
    /// these at its own canvas coord — a half-res producer would misread — so the
    /// codegen samples them through the shared `samp` at the fragment UV
    /// (`textureSampleLevel`), exactly the read the unfused atom makes. Same-space
    /// coincident externals stay textureLoad (byte-identical to the v1 codegen);
    /// `CoincidentTexel` externals are never admitted off-space (they'd corrupt a
    /// texel-exact pattern under rescale) — the region drops instead. Empty for
    /// every same-space region (and buffer regions). Sorted, deduped.
    pub sampled_externals: Vec<usize>,
    /// STENCIL tier: producer chains absorbed into a stencil member's gather
    /// read (recomputed per tap corner instead of round-tripped through a
    /// canvas texture). Empty for every non-stencil region.
    pub virtual_chains: Vec<VirtualChain>,
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

    // ── Resolve every output's element space from the unfused plan (tier 6).
    // `None` (def doesn't build standalone — synthetic fixtures) degrades every
    // lookup to Canvas, i.e. the pre-tier-6 single-space behaviour. ──
    let spaces: SpaceMap = resolve_output_spaces(def, registry);

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
                // chain OR an Array (particle / instance) chain. A texture wire
                // into a BUFFER atom (a force-sampler's flow field, anti_clump's
                // modulator) is a sampler-GATHER — the body samples the whole
                // texture at element-computed coords, which no register can
                // carry — so it never unions; the texture producer stays an
                // external the fused kernel binds. Without this guard an
                // eligible texture atom would merge into a buffer region across
                // that wire, producing an unexpressible mixed-domain region.
                && (is_array_wire(def, registry, w)
                    || (is_texture_wire(def, registry, w)
                        && !node_is_buffer_atom(def, registry, w.to_node)))
                && wire_coincident_consumed(def, registry, w)
                // Tier 6: a texture union additionally requires producer and
                // consumer to share one element space — the fused kernel
                // iterates a single grid. Mixed-space chains (a quarter-res
                // value feeding a node whose OWN output resolved to canvas via
                // the mixed-input fallback) split at the seam instead of
                // fusing onto the wrong grid. Array wires carry no texture
                // grid, so the check doesn't apply.
                && (is_array_wire(def, registry, w)
                    || space_of(spaces.as_ref(), w.from_node, &w.from_port)
                        == node_output_space(spaces.as_ref(), def, registry, w.to_node))
        })
        .map(|w| (w.from_node, w.to_node))
        .collect();
    candidates.sort_unstable(); // deterministic merge order → reproducible regions
    candidates.dedup();

    // D4/P6 (multi-output bridging): a gather-consumed wire's producer must
    // NEVER end up a member of the SAME region as its consumer (`build_region`
    // bails on exactly this — "gather input wired from a member" — because a
    // register can't carry a whole texture the body samples at a computed
    // coord). Before this phase every multi-output candidate was Boundary, so
    // it could never bridge two components that only interact through such a
    // wire. Now a multi-output node's TWO ports can each union independently
    // (one feeds branch A coincidentally, the other feeds branch B
    // coincidentally) and merge A and B into one component even though A and
    // B ALSO share an unrelated gather wire between two of their OTHER
    // members (Glitch: block_displace_field's `offset`→combine_offset→remap
    // bridges into `hash`→exposure→...→masked_mix, and remap's output feeds
    // rgb_split's GATHER `in` input — remap and rgb_split were never unioned
    // by that wire directly, but the multi-output bridge puts them in one
    // component anyway). Same shape as the cycle check just below: track
    // every gather-consumed eligible→eligible pair and refuse any union that
    // would collapse both endpoints into one region — the two components
    // stay separate and connect via the SAME cross-region gather the
    // multi-region model already relies on (`generate_fused`'s doc: "two
    // distinct regions can only be directly texture-wired through a GATHER").
    let gather_pairs: Vec<(u32, u32)> = def
        .wires
        .iter()
        .filter(|w| {
            eligible.contains(&w.from_node)
                && eligible.contains(&w.to_node)
                && is_texture_wire(def, registry, w)
                && !wire_coincident_consumed(def, registry, w)
        })
        .map(|w| (w.from_node, w.to_node))
        .collect();

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
        let would_bridge_a_gather_wire = gather_pairs.iter().any(|&(gp, gc)| key(gp) == key(gc));
        if !collapsed_has_cycle(&forward, &key) && !would_bridge_a_gather_wire {
            uf.union(a, b);
        }
    }
    let mut components: AHashMap<u32, Vec<u32>> = AHashMap::default();
    for &id in &eligible {
        components.entry(uf.find(id)).or_default().push(id);
    }
    // Deterministic component list (rep, sorted members) — reused by the
    // stencil absorption pass below.
    let mut comp_list: Vec<(u32, Vec<u32>)> = components
        .into_iter()
        .map(|(rep, mut nodes)| {
            nodes.sort_unstable();
            (rep, nodes)
        })
        .collect();
    comp_list.sort_unstable_by_key(|(rep, _)| *rep);

    // Nodes that reach a `final_output` (live). A region output's consumer must
    // be in here, so each fused `dst_<k>` lands on a texture the executor actually
    // allocates — see `build_region`.
    let final_reachable = final_reachable_nodes(def);

    // ── Build a region from each component; drop the ones v1 can't express. ──
    let mut regions: Vec<Region> = Vec::new();
    for (_, nodes) in &comp_list {
        if let Ok(region) = build_region(def, registry, nodes, &final_reachable, spaces.as_ref()) {
            regions.push(region);
        }
    }
    // Stable order across runs (components iterate in hash order otherwise).
    regions.sort_by_key(|r| r.members.first().map(|m| m.doc_id).unwrap_or(0));

    // ── Stencil tier: absorb producer chains into stencil members' gather
    // reads (recomputed per tap corner — no canvas round-trip). ──
    absorb_virtual_chains(def, registry, &mut regions, &comp_list, spaces.as_ref(), &forward);

    // A single-member region only pays once a chain folded into it (fusing one
    // node alone changes nothing — the MIN_REGION_LEN rule, applied after
    // absorption so a lone blur + its absorbed producer run still fuses).
    regions.retain(|r| r.members.len() >= MIN_REGION_LEN || !r.virtual_chains.is_empty());
    regions
}

/// Longest producer chain a stencil member's fetch will recompute. Per-tap
/// recomputation multiplies the chain's ALU by 4 bilinear corners × the tap
/// count, so v1 absorbs only STRANDED SINGLES — a lone pointwise/Source atom
/// that would otherwise be dropped below MIN_REGION_LEN and pay a full canvas
/// round-trip for one cheap dispatch. Multi-atom chains already fuse as their
/// own pointwise region; absorbing those trades a known win for taps×4
/// recomputes, and the perf gate (per CARD, fused vs unfused) can't compare
/// the two fused configurations — so don't raise this without per-region
/// gating.
const MAX_VIRTUAL_CHAIN: usize = 1;

/// Upper bound on the estimated WGSL the inliner will materialize for one
/// absorbed chain: consumer fetch sites × 4 bilinear corners × chain body
/// bytes. Past this, absorption is refused and the producer runs as its own
/// dispatch — one extra cheap dispatch beats a multi-second kernel compile on
/// the content thread. 256 KB clears Watercolor's warp-into-blur (~75 KB, the
/// largest shipped absorption) with ~3× headroom and rejects FilmGrain's
/// noise-into-blur (~860 KB) by ~3×. See the BUG-175 gate in
/// [`chain_is_absorbable`].
const MAX_VIRTUAL_INLINE_BYTES: usize = 256 * 1024;

/// Absorb eligible producer components into stencil members' gather inputs as
/// [`VirtualChain`]s. A component qualifies when its ONLY escape is the single
/// gather-consumed wire into one stencil member of `regions[ri]`, every member
/// is a same-space all-coincident texture atom off any feedback cycle, every
/// texture wire INTO it comes from the region's element space, and collapsing
/// it into the region keeps the graph acyclic. A region that was built from an
/// absorbed component is removed (its work now lives inside the consumer's
/// fetch); a dropped single-node component absorbs the same way.
fn absorb_virtual_chains(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    regions: &mut Vec<Region>,
    comps: &[(u32, Vec<u32>)],
    spaces: Option<&AHashMap<(u32, String), ElementSpace>>,
    forward: &[(u32, u32)],
) {
    let comp_of: AHashMap<u32, u32> = comps
        .iter()
        .flat_map(|(rep, ns)| ns.iter().map(move |&n| (n, *rep)))
        .collect();
    let comp_nodes: AHashMap<u32, &[u32]> =
        comps.iter().map(|(rep, ns)| (*rep, ns.as_slice())).collect();

    // Running chain-rep → region-rep merges, so each candidate's convexity test
    // sees every previously accepted absorption (same incremental invariant the
    // union loop maintains).
    let mut merged_into: AHashMap<u32, u32> = AHashMap::default();
    let mut absorbed_reps: AHashSet<u32> = AHashSet::default();
    // (region index, consumer doc id, input index, chain rep, chain output doc id)
    let mut planned: Vec<(usize, u32, usize, u32, u32)> = Vec::new();

    for (ri, region) in regions.iter().enumerate() {
        let Some(region_space) = region.space else {
            continue; // buffer regions have no texture grid to recompute on
        };
        let region_rep = region
            .members
            .first()
            .and_then(|m| comp_of.get(&m.doc_id))
            .copied();
        let Some(region_rep) = region_rep else { continue };
        for member in &region.members {
            let Some(doc_node) = def.nodes.iter().find(|n| n.id == member.doc_id) else {
                continue;
            };
            let Some(node) = configured_construct(registry, doc_node) else { continue };
            if !node.stencil_fetch() {
                continue;
            }
            for (idx, (input, access)) in
                member.inputs.iter().zip(&member.input_access).enumerate()
            {
                if *access != InputAccess::Gather {
                    continue;
                }
                let RegionInput::External(e) = input else { continue };
                let prod = region.externals[*e].from_node;
                let Some(&rep) = comp_of.get(&prod) else {
                    continue; // boundary producer — stays a real external
                };
                let chain_output = prod;
                if rep == region_rep || absorbed_reps.contains(&rep) {
                    continue;
                }
                let nodes = comp_nodes[&rep];
                if nodes.len() > MAX_VIRTUAL_CHAIN {
                    continue;
                }
                if !chain_is_absorbable(def, registry, nodes, member.doc_id, region_space, spaces)
                {
                    continue;
                }
                // Convexity under the tentative merge: every eligible node maps
                // to its component rep with accepted merges (plus this one)
                // folded onto their region reps; boundaries map to themselves.
                let resolve = |r: u32| merged_into.get(&r).copied().unwrap_or(r);
                let key = |n: u32| -> u32 {
                    match comp_of.get(&n) {
                        Some(&r) if r == rep => resolve(region_rep),
                        Some(&r) => resolve(r),
                        None => n,
                    }
                };
                if collapsed_has_cycle(forward, &key) {
                    continue;
                }
                merged_into.insert(rep, resolve(region_rep));
                absorbed_reps.insert(rep);
                planned.push((ri, member.doc_id, idx, rep, chain_output));
            }
        }
    }
    if planned.is_empty() {
        return;
    }

    // ── Apply each plan: resolve the chain's members against the region's
    // externals, repoint the consumer's input, record the chain. ──
    for (ri, consumer, idx, rep, chain_output) in planned {
        let nodes = comp_nodes[&rep];
        let region = &mut regions[ri];
        let applied = (|| -> Option<(Vec<RegionMember>, Vec<ExternalRef>)> {
            let node_set: AHashSet<u32> = nodes.iter().copied().collect();
            let order = topo_sort(nodes, def, registry, &node_set, false)?;
            let mut new_externals = region.externals.clone();
            let mut ext_index: AHashMap<(u32, String), usize> = new_externals
                .iter()
                .enumerate()
                .map(|(i, e)| ((e.from_node, e.from_port.clone()), i))
                .collect();
            let mut members: Vec<RegionMember> = Vec::with_capacity(order.len());
            for &doc_id in &order {
                let constructed =
                    configured_construct(registry, def.nodes.iter().find(|n| n.id == doc_id)?)?;
                let tex_ports: Vec<&crate::node_graph::ports::NodeInput> = constructed
                    .inputs()
                    .iter()
                    .filter(|i| is_texture_port(&i.ty))
                    .collect();
                let access_list = constructed.input_access();
                let mut inputs: Vec<RegionInput> = Vec::with_capacity(tex_ports.len());
                let mut input_access: Vec<InputAccess> = Vec::with_capacity(tex_ports.len());
                for (pidx, port) in tex_ports.iter().enumerate() {
                    let access =
                        access_list.get(pidx).copied().unwrap_or(InputAccess::Coincident);
                    let Some(wire) = def
                        .wires
                        .iter()
                        .find(|w| w.to_node == doc_id && w.to_port == port.name)
                    else {
                        // A gather needs a real texture to sample — unwired
                        // (even optional) can't re-anchor; required-unwired
                        // wouldn't render anyway.
                        if port.required || access.is_gather() {
                            return None;
                        }
                        inputs.push(RegionInput::Unwired);
                        input_access.push(access);
                        continue;
                    };
                    let resolved = if node_set.contains(&wire.from_node) {
                        // A gathered producer can't thread as a per-corner
                        // register (the body samples a whole texture).
                        if access.is_gather() {
                            return None;
                        }
                        // D4/P6: a multi-output producer feeding a STENCIL
                        // chain member isn't a shape any atom needs yet (every
                        // struct-return atom today is a 0-texture-input
                        // Source, never itself gather-consumed) — bail
                        // defensively rather than guess which BodyOutputs
                        // field the chain's per-corner recompute would want.
                        if producer_tex_output_count(registry, def, wire.from_node) > 1 {
                            return None;
                        }
                        RegionInput::Member(wire.from_node)
                    } else {
                        let key = (wire.from_node, wire.from_port.clone());
                        let slot = *ext_index.entry(key).or_insert_with(|| {
                            new_externals.push(ExternalRef {
                                from_node: wire.from_node,
                                from_port: wire.from_port.clone(),
                            });
                            new_externals.len() - 1
                        });
                        RegionInput::External(slot)
                    };
                    inputs.push(resolved);
                    input_access.push(access);
                }
                // Chains never sit on a feedback cycle (gate above), so the
                // tier-A in-loop rounding never applies; the codegen q16s the
                // chain TAIL unconditionally to reproduce the f16 store the
                // unfused chain made for the blur to sample.
                members.push(RegionMember { doc_id, inputs, input_access, quantize_f16: false });
            }
            Some((members, new_externals))
        })();
        let Some((members, new_externals)) = applied else {
            continue; // defensive skip — region keeps its real external (unfused-equivalent)
        };
        region.externals = new_externals;
        let chain_idx = region.virtual_chains.len();
        if let Some(m) = region.members.iter_mut().find(|m| m.doc_id == consumer) {
            m.inputs[idx] = RegionInput::Virtual(chain_idx);
        }
        region.virtual_chains.push(VirtualChain {
            consumer,
            input_index: idx,
            members,
            output: chain_output,
        });
    }

    // ── Compact each affected region's externals (drop slots no input reads —
    // the absorbed chains' output textures) and remap indices. ──
    for region in regions.iter_mut() {
        if region.virtual_chains.is_empty() {
            continue;
        }
        let mut used: Vec<usize> = Vec::new();
        let mut mark = |inputs: &Vec<RegionInput>| {
            for input in inputs {
                if let RegionInput::External(e) = input
                    && !used.contains(e)
                {
                    used.push(*e);
                }
            }
        };
        for m in &region.members {
            mark(&m.inputs);
        }
        for c in &region.virtual_chains {
            for m in &c.members {
                mark(&m.inputs);
            }
        }
        used.sort_unstable();
        let remap: AHashMap<usize, usize> =
            used.iter().enumerate().map(|(new, &old)| (old, new)).collect();
        let rewrite = |inputs: &mut Vec<RegionInput>| {
            for input in inputs {
                if let RegionInput::External(e) = input {
                    *e = remap[e];
                }
            }
        };
        for m in &mut region.members {
            rewrite(&mut m.inputs);
        }
        for c in &mut region.virtual_chains {
            for m in &mut c.members {
                rewrite(&mut m.inputs);
            }
        }
        region.externals = used.iter().map(|&e| region.externals[e].clone()).collect();
    }

    // ── Remove regions whose whole component was absorbed into a fetch. ──
    regions.retain(|r| {
        r.members
            .first()
            .and_then(|m| comp_of.get(&m.doc_id))
            .is_none_or(|rep| !absorbed_reps.contains(rep))
    });
}

/// Per-member gates for absorbing `nodes` into a stencil fetch: single escape
/// to `consumer`, texture-domain pointwise/coincident/gather/Source members
/// off any cycle whose OUTPUT lives at the region's element space, every
/// member agreeing with the consumer's sampler address mode. Chain INPUT wires
/// carry no space constraint: the fetch reads chain externals through the
/// shared sampler at the corner uv (coincident) or a body-computed coord
/// (gather) — the same resolution-robust read the unfused standalone atom
/// makes, so a half-res flow field feeding an absorbed warp stays exact.
fn chain_is_absorbable(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    nodes: &[u32],
    consumer: u32,
    region_space: ElementSpace,
    spaces: Option<&AHashMap<(u32, String), ElementSpace>>,
) -> bool {
    let node_set: AHashSet<u32> = nodes.iter().copied().collect();
    let mut escapes = 0usize;
    for w in &def.wires {
        if node_set.contains(&w.from_node) && !node_set.contains(&w.to_node) {
            escapes += 1;
            if w.to_node != consumer {
                return false;
            }
        }
    }
    if escapes != 1 {
        return false;
    }
    // The consumer's sampler mode is the region's shared `samp`; every chain
    // member's reads go through it, so each member must create the same mode
    // standalone (default clamp for nearly every atom) or the look would shift
    // at texture edges.
    let consumer_mode = node_sampler_mode(def, registry, consumer);
    // In a PURE-TEXTURE feedback loop, absorption is sound only when the
    // consumer's taps are texel-exact (Linear blur): corner values are q16'd
    // to the unfused store and integer taps read corners exactly, so the loop
    // stays bit-identical by induction (the tier-A argument). Fractional taps
    // leave ~1 ulp of lerp noise per frame, which a loop amplifies — and a
    // PARTICLE loop amplifies anything (the parked f16 class) — both stay out.
    let consumer_taps_exact = node_taps_texel_exact(def, registry, consumer);
    // Compile-cost gate (BUG-175): absorption pastes the chain's bodies into
    // every corner evaluation of every fetch site in the consumer's body, and
    // spirv-opt's InlineExhaustive materializes all of it — fetch_sites × 4
    // corners × chain body bytes of WGSL. MAX_VIRTUAL_CHAIN prices the runtime
    // ALU of that multiplication; this prices the CODE SIZE, which otherwise
    // explodes kernel compile time. FilmGrain was the proof: noise absorbed
    // into gaussian_blur = 35 fetch sites × 4 × ~6 KB ≈ 860 KB of inlined
    // WGSL, ~50 s of synchronous spirv-opt + Metal compile per build on the
    // content thread — twice, once more for the specialized variant.
    // Watercolor's warp-into-blur (~75 KB) is the
    // largest absorption that must keep fusing.
    let consumer_fetch_sites = def
        .nodes
        .iter()
        .find(|n| n.id == consumer)
        .and_then(|doc| configured_construct(registry, doc))
        .and_then(|n| n.wgsl_body().map(|b| b.matches("fetch_").count()))
        .unwrap_or(usize::MAX)
        .max(1);
    let chain_body_bytes: usize = nodes
        .iter()
        .map(|id| {
            def.nodes
                .iter()
                .find(|n| n.id == *id)
                .and_then(|doc| configured_construct(registry, doc))
                .and_then(|n| n.wgsl_body().map(str::len))
                .unwrap_or(usize::MAX)
        })
        .fold(0usize, usize::saturating_add);
    if consumer_fetch_sites
        .saturating_mul(4)
        .saturating_mul(chain_body_bytes)
        > MAX_VIRTUAL_INLINE_BYTES
    {
        return false;
    }
    for &id in nodes {
        let Some(doc) = def.nodes.iter().find(|n| n.id == id) else {
            return false;
        };
        let Some(n) = configured_construct(registry, doc) else {
            return false;
        };
        if n.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))) {
            return false; // buffer atom — no texture grid
        }
        // Coincident reads re-anchor as sampled corner reads; a sampler-Gather
        // re-anchors as (texture, sampler) body args. Exact-texel kinds
        // (CoincidentTexel / GatherTexel) are resolution- and grid-pinned in a
        // way a re-anchored sampled read can't reproduce — boundary.
        if n.input_access()
            .iter()
            .any(|a| !matches!(a, InputAccess::Coincident | InputAccess::Gather))
        {
            return false;
        }
        if n.stencil_fetch() {
            return false; // nested stencils are a follow-on
        }
        if node_on_cycle(id, def)
            && (!consumer_taps_exact || cycle_contains_array(id, def, registry))
        {
            return false; // in-loop recompute is only bit-faithful under exact taps
        }
        if node_output_space(spaces, def, registry, id) != region_space {
            return false;
        }
        if node_sampler_mode(def, registry, id) != consumer_mode {
            return false;
        }
    }
    true
}

/// Whether node `id`'s stencil taps are texel-exact under its def params —
/// see [`EffectNode::stencil_taps_texel_exact`]. Same effective-param
/// resolution as [`node_sampler_mode`].
fn node_taps_texel_exact(def: &EffectGraphDef, registry: &PrimitiveRegistry, id: u32) -> bool {
    use crate::node_graph::parameters::ParamValue;
    let Some(doc) = def.nodes.iter().find(|n| n.id == id) else {
        return false;
    };
    let Some(n) = configured_construct(registry, doc) else {
        return false;
    };
    let mut params: crate::node_graph::effect_node::ParamValues = AHashMap::default();
    for p in n.parameters() {
        let v = match doc.params.get(p.name.as_ref()) {
            Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) => {
                Some(*value)
            }
            Some(manifold_core::effect_graph_def::SerializedParamValue::Int { value }) => {
                Some(*value as f32)
            }
            Some(manifold_core::effect_graph_def::SerializedParamValue::Enum { value }) => {
                Some(*value as f32)
            }
            Some(manifold_core::effect_graph_def::SerializedParamValue::Bool { value }) => {
                Some(if *value { 1.0 } else { 0.0 })
            }
            _ => match &p.default {
                ParamValue::Float(f) => Some(*f),
                ParamValue::Enum(u) => Some(*u as f32),
                ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                _ => None,
            },
        };
        if let Some(v) = v {
            params.insert(p.name.clone(), ParamValue::Float(v));
        }
    }
    n.stencil_taps_texel_exact(&params)
}

/// The sampler address mode node `id` would create standalone, resolved from
/// its def params (the same read the install pass's gather agreement does).
fn node_sampler_mode(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    id: u32,
) -> manifold_gpu::GpuAddressMode {
    use crate::node_graph::parameters::ParamValue;
    let Some(doc) = def.nodes.iter().find(|n| n.id == id) else {
        return manifold_gpu::GpuAddressMode::ClampToEdge;
    };
    let Some(n) = configured_construct(registry, doc) else {
        return manifold_gpu::GpuAddressMode::ClampToEdge;
    };
    let mut params: crate::node_graph::effect_node::ParamValues = AHashMap::default();
    for p in n.parameters() {
        let v = match doc.params.get(p.name.as_ref()) {
            Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) => {
                Some(*value)
            }
            Some(manifold_core::effect_graph_def::SerializedParamValue::Int { value }) => {
                Some(*value as f32)
            }
            Some(manifold_core::effect_graph_def::SerializedParamValue::Enum { value }) => {
                Some(*value as f32)
            }
            Some(manifold_core::effect_graph_def::SerializedParamValue::Bool { value }) => {
                Some(if *value { 1.0 } else { 0.0 })
            }
            _ => match &p.default {
                ParamValue::Float(f) => Some(*f),
                ParamValue::Enum(u) => Some(*u as f32),
                ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                _ => None,
            },
        };
        if let Some(v) = v {
            params.insert(p.name.clone(), ParamValue::Float(v));
        }
    }
    n.fused_gather_sampler_mode(&params)
}

/// Stencil tier A kill switch: `MANIFOLD_FREEZE_Q16=0` (or `false`/`off`)
/// keeps in-loop f16 atoms as boundaries, restoring pre-tier-A partitions.
/// Read per call (cheap env lookup at fuse-build time, never per frame) so
/// tests can flip it without process restarts.
fn q16_tier_enabled() -> bool {
    !matches!(
        std::env::var("MANIFOLD_FREEZE_Q16").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
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

/// Does `start`'s feedback cycle pass through a PARTICLE/array stage? True
/// when some node with an `Array`-typed output is mutually reachable with
/// `start` (same strongly-connected component). Distinguishes the two loop
/// families for the in-loop f16 fusion gate:
///   - pure texture loops (OilyFluid's advection): locally smooth — a 1-ulp
///     register/store gap stays ~1 ulp, and tier A's q16 rounding holds the
///     fused render bit-exact (its proof passes);
///   - particle loops (FluidSim: density → flow field → forces → particle
///     buffer → scatter → density): a 1-ulp force difference moves a particle
///     across a texel boundary and the scatter amplifies it to a visibly
///     different field (measured max_abs 0.6+ over ~30% of pixels).
fn cycle_contains_array(start: u32, def: &EffectGraphDef, registry: &PrimitiveRegistry) -> bool {
    // Forward reachability from `start`, remembering everything reachable.
    let mut forward: AHashSet<u32> = AHashSet::default();
    let mut stack = vec![start];
    while let Some(n) = stack.pop() {
        for w in &def.wires {
            if w.from_node == n && forward.insert(w.to_node) {
                stack.push(w.to_node);
            }
        }
    }
    if !forward.contains(&start) {
        return false; // not on a cycle at all
    }
    // A node is in `start`'s SCC iff start →* node AND node →* start. Check
    // each forward-reachable array-producing node for a path back to start.
    for node in &def.nodes {
        if !forward.contains(&node.id) {
            continue;
        }
        // CONFIGURED construct: a full-kernel `node.wgsl_compute` (e.g.
        // StrangeAttractor's "simulate" node, which ships a
        // `var<storage, read_write> array<Particle>` output) introspects its real
        // port list only after its `wgsl_source` is applied. A bare construct sees
        // the DEFAULT kernel (no Array output), so the particle stage would be
        // invisible to the SCC scan and a texture atom on the loop would wrongly
        // fuse tier-A f16 in-loop, where the bit-exact induction fails across a
        // scatter (BUG-007).
        let Some(n) = configured_construct(registry, node) else {
            continue;
        };
        if !n.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))) {
            continue;
        }
        let mut back: AHashSet<u32> = AHashSet::default();
        let mut stack = vec![node.id];
        while let Some(m) = stack.pop() {
            if m == start && node.id != start {
                return true;
            }
            for w in &def.wires {
                if w.from_node == m && back.insert(w.to_node) {
                    if w.to_node == start {
                        return true;
                    }
                    stack.push(w.to_node);
                }
            }
        }
    }
    false
}

/// Construct a primitive for a def node and apply the node's CONFIGURED state —
/// its `wgsl_source` (so a fragment-form `node.wgsl_compute` reparses its declared
/// ports/params and reports its `fusion_kind()` / `wgsl_body()`) and its param
/// values (so dynamic-port primitives like `node.switch_texture` reconfigure to the
/// right port count). A bare `registry.construct` returns the DEFAULT shape; the
/// freeze classifier, finder, and codegen must see the SAME shape the live loader
/// ([`instantiate_def`](crate::node_graph::graph_loader::instantiate_def)) builds —
/// mirroring its `set_wgsl_source` then param-override + `reconfigure` order.
pub(crate) fn configured_construct(
    registry: &PrimitiveRegistry,
    node: &EffectGraphNode,
) -> Option<Box<dyn crate::node_graph::effect_node::EffectNode>> {
    let mut boxed = registry.construct(&node.type_id)?;
    // (1) WGSL source first — a dynamic-shape primitive reparses its port list
    // before params are read. No-op for fixed-shape atoms.
    if let Some(src) = node.wgsl_source.as_deref() {
        boxed.set_wgsl_source(src);
    }
    // (2) Params — seed every declared default, override with the def's values,
    // then reconfigure (variadic nodes rebuild param-derived ports). Matches
    // `NodeInstance::new`. Unknown / mistyped params are skipped (the loader would
    // have rejected the def upstream; the freeze pass only needs a faithful shape).
    let mut params: crate::node_graph::effect_node::ParamValues = AHashMap::default();
    for p in boxed.parameters() {
        params.insert(p.name.clone(), p.default.clone());
    }
    for (key, value) in &node.params {
        if let Some(p) = boxed.parameters().iter().find(|p| p.name == key.as_str()) {
            params.insert(p.name.clone(), value.clone().into());
        }
    }
    boxed.reconfigure(&params);
    Some(boxed)
}

/// How many texture outputs `doc_id` declares (0 if it's not in `def`, or the
/// registry doesn't know its type — a defensive default that never falsely
/// reports "multi-output"). D4/P6: a producer with ≥2 texture outputs is a
/// struct-return body (voronoi_2d's `out`/`cell_id`) — wiring FROM one of its
/// ports into a region member must disambiguate which `BodyOutputs` field
/// threads, via `RegionInput::MemberPort` instead of the plain `Member(u32)`
/// single-output producers keep.
fn producer_tex_output_count(
    registry: &PrimitiveRegistry,
    def: &EffectGraphDef,
    doc_id: u32,
) -> usize {
    def.nodes
        .iter()
        .find(|n| n.id == doc_id)
        .and_then(|n| configured_construct(registry, n))
        .map(|c| c.outputs().iter().filter(|o| is_texture_port(&o.ty)).count())
        .unwrap_or(0)
}

/// Classify one node. `Eligible` requires *every* gate to pass; any failure —
/// including "the registry doesn't know this type" — is a `Boundary`.
///
/// `pub(crate)`: `graph_tool fusion`'s report module calls this directly per
/// node (GRAPH_TOOLING_DESIGN P3) to explain *why* a node sits outside every
/// region — reusing this exact function rather than re-deriving the verdict.
pub(crate) fn classify_node(
    node: &EffectGraphNode,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> NodeClass {
    use crate::node_graph::freeze::classify::FusionKind;

    // Boundaries by identity: the graph endpoints are always seams.
    if node.type_id == SOURCE_TYPE_ID || node.type_id == FINAL_OUTPUT_TYPE_ID {
        return NodeClass::Boundary;
    }
    // Configured so a fragment-form `node.wgsl_compute` reports its real
    // fusion_kind/body/ports (a bare construct is the DEFAULT opaque kernel).
    let Some(n) = configured_construct(registry, node) else {
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

    // Register-heavy body (a bespoke inlined simplex): fusing it raises the
    // whole kernel's register pressure past the occupancy cliff, so the fused
    // region runs slower than the standalone dispatches (FluidSim2D's
    // euler+wrap+burst: 3.05 ms fused vs 2.43 unfused). Keep it a boundary —
    // its register-light neighbours still fuse around it.
    if n.fusion_register_heavy() {
        return NodeClass::Boundary;
    }

    // Every param must lay out in the fused per-node namespaced uniform —
    // scalar (`param_wgsl_type`), OR Vec3 (three consecutive `_x`/`_y`/`_z`
    // f32 fields) / Vec4/Color (four `_x`/`_y`/`_z`/`_w` f32 fields, no
    // padding needed) via `param_is_fusable`. Table/String stay boundary:
    // Table is storage-shaped data (a fixed-size array-of-vec4 the per-node
    // namespacing doesn't extend to) and String has no GPU representation —
    // neither is debt, both are a deliberate boundary by nature (D4 #5).
    for p in n.parameters() {
        if !crate::node_graph::freeze::codegen::param_is_fusable(p) {
            return NodeClass::Boundary;
        }
        // Binding-targeted ENUM params are no longer boundaries (the 59b3cf25
        // gate): the retarget rewrites the binding's `EnumRound` convert to
        // `IntRound` when it repoints onto the fused uniform field, which the
        // field's u32 cast consumes identically (round + clamp-at-0 happens at
        // the uniform-write boundary either way). FluidSim3D's `container` →
        // container_repel_force_3d fuses through this. Specialization-token
        // enum params (blurVW's `quality`) are a different story — see the
        // wgsl_specialization gate below, which stays.
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

    // Texture I/O shape: ≥1 texture output (the register(s) the region
    // threads). A Source reads NO texture input (it generates from position); the
    // threaded kinds read ≥1. D4/P6 (narrowed cut rule 6): a MULTI-output atom
    // (≥2 texture outputs — voronoi_2d's `out`/`cell_id`, block_displace_field's
    // `offset`/`raw_hash`) is Eligible too, on the same struct-return-body
    // mechanism the codegen's buffer path already ships (`codegen.rs`'s
    // BufferOutputs wrapper) extended to texture kernels: every atom that
    // declares ≥2 texture outputs in this codebase already returns a
    // `BodyOutputs` struct (there is no "multi-output but NOT struct-return"
    // atom on the codegen path — an atom with 0/1 outputs never declares the
    // struct; ≥2 always does, by the `primitive!` authoring contract
    // `ADDING_PRIMITIVES.md` documents). Only tex_out == 0 (no register to
    // thread at all) stays Boundary.
    let tex_in = n.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
    let tex_out = n.outputs().iter().filter(|o| is_texture_port(&o.ty)).count();
    let arity_ok = if matches!(n.fusion_kind(), FusionKind::Source) {
        tex_in == 0
    } else {
        tex_in >= 1
    };
    if !arity_ok || tex_out == 0 {
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
        if let Some(scale) = n.output_canvas_scale(o.name.as_ref(), &default_params)
            && scale != (1, 1)
        {
            return NodeClass::Boundary;
        }
    }

    // Element space (tier 6): per-node spaces are resolved from the unfused
    // plan in `partition_regions` — unions are gated on space equality and
    // `build_region` enforces member/external uniformity, so a def-level
    // canvas-scale override no longer makes the atom a blanket boundary (its
    // space simply has to match its neighbours'). Atom-level resamples
    // (`output_canvas_scale` ≠ 1:1, the gate above) remain boundaries: folding
    // a resampler INTO a region needs cross-scale reads (stencil-tier work).
    // 3D ports stay out of the texture finder.
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
        .map(|i| i.name.as_ref())
        .collect();
    let scalar_params: AHashSet<&str> = n
        .parameters()
        .iter()
        .filter(|p| param_wgsl_type(p).is_ok())
        .map(|p| p.name.as_ref())
        .collect();
    // D7/P0 exemption (`docs/CINEMATIC_POST_DESIGN.md`): a wire into a
    // CPU-struct (Camera) port no longer cuts, PROVIDED the atom consumes that
    // struct entirely via `derived_uniforms()` — never as a GPU binding (already
    // true by construction: `is_texture_port` excludes `PortType::Camera`, so a
    // Camera port was never a texture/buffer binding). The predicate is
    // therefore: the wire's target port is Camera-typed AND the atom declares a
    // non-empty `derived_uniforms()` list. A Camera port on an atom with NO
    // derived_uniforms (a complex 3D renderer reading the whole matrix inline,
    // not expressible as a handful of recomputed scalar fields) is not exempted
    // — it still cuts, same as any other non-texture non-param wire. Install
    // routes the exempted wire's producer onto the fused node's synthesized
    // `camera_ext_N` port (`freeze/install.rs`); the fused kernel recomputes the
    // member's derived fields every frame via
    // `derived_uniform_registry::recompute` (`primitives/wgsl_compute.rs`).
    //
    // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P1: `Object` joins the `Camera`
    // line above — same CPU-struct-wire reasoning (`is_texture_port`
    // excludes `PortType::Object` too, so an Object port was never a
    // texture/buffer binding). No fusable atom declares `derived_uniforms`
    // for an Object input today (the single-hop invariant means the only
    // legal `Object` consumer, `render_scene`, is a draw-call rasterizer —
    // always `NodeClass::Boundary` by cut rule 4 already, never reaching
    // this predicate); this keeps the exemption set structurally complete
    // for the day a second Object consumer exists, which is itself an
    // escalation trigger (design doc §8).
    let camera_ports: AHashSet<&str> = if n.derived_uniforms().is_empty() {
        AHashSet::default()
    } else {
        n.inputs()
            .iter()
            .filter(|i| matches!(i.ty, PortType::Camera | PortType::Object))
            .map(|i| i.name.as_ref())
            .collect()
    };
    // D3 exemption (`docs/FUSION_SOTA_DESIGN.md`, closes BUG-114): a wire into
    // an Array-typed input the atom tags `BufferIndex` no longer cuts — the
    // narrowing cut rule 9, same shape as the Camera exemption above. The
    // array producer still never becomes a region MEMBER (`build_region`
    // below appends it as an external, exactly like a gather-consumed wire);
    // this only stops the WIRE from forcing the whole node to Boundary.
    // `INPUT_ACCESS` packs [texture accesses] ++ [array accesses] for a
    // texture-domain atom with array inputs (see `input_port_access`'s D3
    // comment) — offset by the atom's own texture-input count.
    let buffer_index_ports: AHashSet<&str> = {
        let tex_count = n.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
        n.inputs()
            .iter()
            .filter(|i| matches!(i.ty, PortType::Array(_)))
            .enumerate()
            .filter(|(idx, _)| n.input_access().get(tex_count + idx) == Some(&InputAccess::BufferIndex))
            .map(|(_, i)| i.name.as_ref())
            .collect()
    };
    for w in &def.wires {
        if w.to_node == node.id
            && !tex_ports.contains(w.to_port.as_str())
            && !scalar_params.contains(w.to_port.as_str())
            && !camera_ports.contains(w.to_port.as_str())
            && !buffer_index_ports.contains(w.to_port.as_str())
        {
            return NodeClass::Boundary;
        }
        if w.from_node == node.id && is_scalar_param_wire(def, registry, w) {
            return NodeClass::Boundary;
        }
    }

    // Feedback-loop precision (TEXTURE atoms only — buffer atoms were dispatched
    // above and their f32 register threading is already bit-exact). A texture
    // atom inside a feedback loop must NOT change its rounding when fused: the
    // unfused editor stores each intermediate through its output texture (f16
    // chain default, or fp32 via `outputFormats: rgba32float`), and a chaotic
    // feedback sim amplifies any register-vs-store rounding gap until the look
    // shifts when the editor closes. Two reconciliations, both fuse-eligible:
    //   - fp32-marked: the unfused store is exact, matching the fused f32
    //     register — fuses as-is.
    //   - f16 (stencil tier A): the fused kernel reproduces the unfused f16
    //     rounding in-register — `build_region` flags the member
    //     `quantize_f16` and the codegen wraps its body call in a `q16`
    //     pack2x16float/unpack2x16float round-trip (exact IEEE-half RTNE,
    //     identical to an rgba16float store+load). Costs a few ALU per member;
    //     no preset edit, no editor memory increase, no look change.
    // Kill switch: `MANIFOLD_FREEZE_Q16=0` restores the pre-tier-A behaviour
    // (in-loop f16 atoms stay boundaries) without touching fp32 admission.
    if !q16_tier_enabled()
        && node_on_cycle(node.id, def)
        && !node.output_formats.values().any(|s| s.contains("32float"))
    {
        return NodeClass::Boundary;
    }

    // In-loop f16 texture atoms on a PARTICLE loop: boundary. The q16 round-
    // trip reproduces store rounding but not cross-kernel body ULP noise (the
    // out-of-loop probe measured this 1-ulp drift), and a loop that passes
    // through a particle buffer + scatter amplifies one ulp of force into a
    // visibly different field (FluidSim flow field, 2026-06-10: max_abs 0.73
    // / 31% of pixels fused vs unfused; still 0.62 with only the pointwise
    // pair fused). Pure-texture loops (OilyFluid's advection) stay smooth
    // under the same ulp and keep fusing via tier A — its bit-exact proof
    // stands. fp32-marked atoms keep fusing as-is (exact stores), but fp32 is
    // an explicit data-texture opt-in now, never a compiler default.
    if !node.output_formats.values().any(|s| s.contains("32float"))
        && cycle_contains_array(node.id, def, registry)
    {
        return NodeClass::Boundary;
    }

    // Specialization tokens (QUALITY_LEVEL, WEIGHTING_MODE): the freeze paths
    // bake the def's STATIC param value into the body text. A baked value must
    // not be able to diverge from the live one, so a specialization param that
    // an outer binding targets or a control wire drives keeps the atom a
    // boundary (its run() keeps specializing per-dispatch as before).
    for (_, sp_param) in n.wgsl_specialization() {
        if param_is_binding_target(node, sp_param, def)
            || def
                .wires
                .iter()
                .any(|w| w.to_node == node.id && w.to_port == *sp_param)
        {
            return NodeClass::Boundary;
        }
    }

    // Final gate: the body must produce a kernel the PLAIN pipeline compiler
    // (naga) accepts — after substituting any declared specialization tokens
    // exactly as the fused codegen will. An atom whose body still carries a
    // free identifier (an undeclared token, a typo) fails the parse and stays
    // a boundary; the substituted-and-parsed text is precisely what fusion
    // compiles, so the gate remains sound. No hard-coded atom list.
    let Some(body) = substituted_body(n.as_ref(), node) else {
        return NodeClass::Boundary;
    };
    let standalone = crate::node_graph::freeze::codegen::generate_standalone(
        &crate::node_graph::freeze::codegen::StandaloneKernelSpec {
            fusion_kind: n.fusion_kind(),
            body: &body,
            inputs: n.inputs(),
            params: n.parameters(),
            input_access: n.input_access(),
            derived_uniforms: n.derived_uniforms(),
            outputs: n.outputs(),
            stencil_fetch: n.stencil_fetch(),
            includes: n.wgsl_includes(),
        },
    );
    match standalone {
        Ok(kernel) if naga::front::wgsl::parse_str(&kernel).is_ok() => NodeClass::Eligible,
        _ => NodeClass::Boundary,
    }
}

/// The atom's `wgsl_body` with its declared specialization tokens substituted
/// by the def node's STATIC param values — the exact text every freeze path
/// (classify parse gate, install, fused codegen) works from. `None` when the
/// atom has no body, or a token's param value isn't a scalar it can bake.
/// Enum/Bool/Int params bake as `u32` literals (the token comparison form the
/// specialized pipelines use); Float bakes as a decimal literal.
pub(crate) fn substituted_body(
    n: &dyn crate::node_graph::effect_node::EffectNode,
    node: &EffectGraphNode,
) -> Option<std::borrow::Cow<'static, str>> {
    use crate::node_graph::parameters::ParamValue;
    use manifold_core::effect_graph_def::SerializedParamValue;

    let body = n.wgsl_body()?;
    let spec = n.wgsl_specialization();
    if spec.is_empty() {
        return Some(std::borrow::Cow::Borrowed(body));
    }
    let mut text = body.to_string();
    for (token, param) in spec {
        let def_param = n.parameters().iter().find(|p| p.name == *param)?;
        let literal = match node.params.get(*param) {
            Some(SerializedParamValue::Enum { value }) => format!("{value}u"),
            Some(SerializedParamValue::Bool { value }) => format!("{}u", u32::from(*value)),
            Some(SerializedParamValue::Int { value }) => format!("{value}u"),
            Some(SerializedParamValue::Float { value }) => format!("{value:?}"),
            Some(_) => return None,
            None => match &def_param.default {
                ParamValue::Enum(v) => format!("{v}u"),
                ParamValue::Bool(b) => format!("{}u", u32::from(*b)),
                ParamValue::Float(f) => format!("{f:?}"),
                _ => return None,
            },
        };
        text = super::codegen::rename_ident(&text, token, &literal);
    }
    Some(std::borrow::Cow::Owned(text))
}

/// Classify a BUFFER-domain atom (writes an `Array<T>` — particle / instance /
/// curve element) for fusion eligibility. The buffer twin of the texture gates
/// in [`classify_node`]: the atom must match the buffer codegen contract
/// ([`super::codegen::generate_fused`]'s buffer branch) — ≥1 Array input threaded
/// as an element register, exactly one Array output (no texture output), no
/// `BufferGather`, no atomic output — and its non-Array wires must be region
/// edges, gathered texture externals (wired plain `Texture2D` only — the body
/// samples them at element-computed coords), or re-anchorable scalar params,
/// and it must not be a control PRODUCER. Anything else is a `Boundary`. The
/// standalone naga-parse gate the texture path uses is NOT applied here (it
/// threads no `wgsl_includes`, so a noise-based buffer body would falsely fail);
/// the install pass naga-parses the FUSED kernel as the real guard, falling back
/// to unfused.
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
    // A texture OUTPUT from a buffer atom has no fused expression (the kernel
    // writes element arrays) — boundary. Texture INPUTS fuse: the body samples
    // the bound texture at a coord it computes from the element (the
    // `*_at_particles` force-sampler family, anti_clump's modulator) — the
    // buffer-domain analogue of the texture path's sampler-Gather. The fused
    // codegen binds each as a `src_<slot>` texture + the shared `samp`, exactly
    // like the standalone buffer kernel, so the sample is bit-identical. Gates:
    //   - sampled `Texture2D` / `Texture3D` only: both bind through the fused
    //     node (`node.wgsl_compute` introspects sampled 2D and 3D). 3D is what
    //     lets the FluidSim3D integrator fuse whole — its force sampler
    //     (sample_texture_3d_at_particles) reads the vector volume inline,
    //     like the original fused `fluid_simulate_3d` kernel did;
    //   - WIRED only: the fused node's texture port is required, and an unwired
    //     port would silently kill its whole dispatch. The standalone atom binds
    //     a dummy texture for an unwired optional; the fused path has no node to
    //     do that, so unwired (even optional) stays a boundary.
    if n.outputs().iter().any(|o| is_texture_port(&o.ty)) {
        return NodeClass::Boundary;
    }
    for i in n.inputs().iter().filter(|i| is_texture_port(&i.ty)) {
        if !matches!(i.ty, PortType::Texture2D | PortType::Texture3D) {
            return NodeClass::Boundary;
        }
        if !def
            .wires
            .iter()
            .any(|w| w.to_node == node.id && w.to_port == i.name)
        {
            return NodeClass::Boundary;
        }
    }
    // A BufferGather input (neighbor_smooth) indexes its global itself — it can't
    // thread one element register, so it stays a boundary (the gathered producer
    // is kept external; the finder never unions a gather-consumed wire).
    if n.input_access().iter().any(|a| a.is_gather()) {
        return NodeClass::Boundary;
    }
    // Frame-derived-uniform integrators (euler_step's dt_scaled, the forces'
    // frame_count, flatten_to_camera_plane's cam_fwd_x/_y/_z) FUSE: the fused
    // buffer codegen emits each derived uniform as an `n{i}_<name>` Params field
    // + body arg, and `node.wgsl_compute` recomputes its VALUE every frame via
    // `derived_uniform_registry::recompute` (D7/P0 — the install-time
    // `system.generator_input` control-wire whitelist this comment used to
    // describe is deleted; see `docs/CINEMATIC_POST_DESIGN.md` D7). The
    // in-place-loop hazard (fusing a region inside array_feedback's in==out
    // loop) is handled at the install/region level: `region_output_aliases_external`
    // + `external_is_inplace_loop` detect a feedback-loop region and the codegen
    // writes back to the aliased `src_k` buffer in place, preserving the loop. So
    // this per-node gate no longer excludes derived-uniform atoms; install bails
    // to unfused only if a member's `type_id` has no registered recompute
    // (`derived_uniform_registry::has_recompute`). Proven by
    // `fluidsim_buffer_fusion_renders_like_unfused`.
    //
    // Atomic-accumulator outputs (scatter) write via `atomicAdd`, not a coincident
    // element write — boundary.
    if !n.atomic_outputs().is_empty() {
        return NodeClass::Boundary;
    }
    // Wire gate: an Array input wire is a region edge (threads or stays external);
    // a texture input wire is a gathered external (bound + sampled by the body);
    // a scalar-param wire is re-anchored onto the fused port-shadow; any other
    // wire into a non-Array non-texture non-scalar-param input cuts; a control
    // PRODUCER stays a boundary so its scalar survives the rewrite to wire into
    // the fused node.
    let arr_ports: AHashSet<&str> = n
        .inputs()
        .iter()
        .filter(|i| matches!(i.ty, PortType::Array(_)) || is_texture_port(&i.ty))
        .map(|i| i.name.as_ref())
        .collect();
    let scalar_params: AHashSet<&str> = n
        .parameters()
        .iter()
        .filter(|p| param_wgsl_type(p).is_ok())
        .map(|p| p.name.as_ref())
        .collect();
    // D7/P0 exemption — same predicate as `classify_node`'s texture-domain wire
    // gate above: a wire into a Camera-typed port no longer cuts, provided the
    // atom declares a non-empty `derived_uniforms()` (consumes the struct
    // entirely via recomputed scalar fields, never a GPU binding). This is what
    // lets `node.flatten_to_camera_plane` — a real, shipped buffer atom already
    // declaring `derived_uniforms: ["cam_fwd_x", "cam_fwd_y", "cam_fwd_z"]` and
    // `fusion_kind: Pointwise` in anticipation of exactly this fix — actually
    // fuse with a pointwise neighbour instead of being permanently stuck at
    // Boundary by this gate.
    let camera_ports: AHashSet<&str> = if n.derived_uniforms().is_empty() {
        AHashSet::default()
    } else {
        n.inputs()
            .iter()
            .filter(|i| matches!(i.ty, PortType::Camera | PortType::Object))
            .map(|i| i.name.as_ref())
            .collect()
    };
    for w in &def.wires {
        if w.to_node == node.id
            && !arr_ports.contains(w.to_port.as_str())
            && !scalar_params.contains(w.to_port.as_str())
            && !camera_ports.contains(w.to_port.as_str())
        {
            return NodeClass::Boundary;
        }
        if w.from_node == node.id && is_scalar_param_wire(def, registry, w) {
            return NodeClass::Boundary;
        }
    }
    NodeClass::Eligible
}

/// Assemble a [`Region`] from a connected component's node set, or `Err` naming
/// the first v1 expressibility gate it failed (too short, multi-output, or an
/// unresolvable input — all left unfused). The reason string feeds the
/// `explain_presets` report: a component can union cleanly and STILL not fuse,
/// and without the reason that reads as a convexity bug (Watercolor's tail did,
/// 2026-06-11 — see the element-space gate below).
fn build_region(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    nodes: &[u32],
    final_reachable: &AHashSet<u32>,
    spaces: Option<&AHashMap<(u32, String), ElementSpace>>,
) -> Result<Region, &'static str> {
    // Singles ARE built here: a lone stencil atom (a blur) becomes worth fusing
    // the moment the absorption pass folds a producer chain into its fetch.
    // `partition_regions` drops chainless sub-MIN_REGION_LEN regions at the end.
    if nodes.is_empty() {
        return Err("empty component");
    }
    let node_set: AHashSet<u32> = nodes.iter().copied().collect();
    // Texture vs buffer region — drives every port/wire filter below (a region is
    // homogeneous: texture and Array ports never wire to each other).
    let is_buffer = region_is_buffer(nodes, def, registry);

    // Topo-sort the members by intra-region wires so every Member input refers to
    // an earlier entry (the codegen threads registers in this order).
    let order = topo_sort(nodes, def, registry, &node_set, is_buffer)
        .ok_or("intra-region wires form a cycle")?;

    // Resolve external inputs (deduped, first-seen order) + each member's inputs.
    let mut externals: Vec<ExternalRef> = Vec::new();
    let mut ext_index: AHashMap<(u32, String), usize> = AHashMap::default();
    let mut members: Vec<RegionMember> = Vec::with_capacity(order.len());
    for &doc_id in &order {
        let node = def
            .nodes
            .iter()
            .find(|n| n.id == doc_id)
            .ok_or("member id missing from def")?;
        let constructed = configured_construct(registry, node).ok_or("unknown member type")?;
        let tex_ports: Vec<&str> = constructed
            .inputs()
            .iter()
            .filter(|i| region_port_is_member(&i.ty, is_buffer))
            .map(|i| i.name.as_ref())
            .collect();
        let access_list = constructed.input_access();
        let mut inputs: Vec<RegionInput> = Vec::with_capacity(tex_ports.len());
        let mut input_access: Vec<InputAccess> = Vec::with_capacity(tex_ports.len());
        for (idx, port) in tex_ports.iter().enumerate() {
            let access = access_list.get(idx).copied().unwrap_or(InputAccess::Coincident);
            let Some(wire) = def
                .wires
                .iter()
                .find(|w| w.to_node == doc_id && w.to_port == *port)
            else {
                // No wire into this port. An OPTIONAL coincident input fuses as
                // `Unwired` (the body's injected use flag gates the read off, the
                // same contract run() fulfils with a dummy bind) — this is what
                // lets pack_channels fuse with only r/g wired. Required-unwired
                // (the node wouldn't render anyway) and gather-unwired (the body
                // needs a real texture to sample) drop the region — unfused,
                // always correct.
                let spec = constructed
                    .inputs()
                    .iter()
                    .find(|i| i.name == *port)
                    .ok_or("port missing from member spec")?;
                if spec.required || access.is_gather() || is_buffer {
                    return Err("required/gather/buffer input unwired");
                }
                inputs.push(RegionInput::Unwired);
                input_access.push(access);
                continue;
            };
            let resolved = if node_set.contains(&wire.from_node) {
                // A gather input must read an external texture, never a region
                // register (a register is one texel). The finder never unions
                // across a gather-consumed wire, so a gathered producer should
                // never be a member — bail defensively if one slipped through.
                if access.is_gather() {
                    return Err("gather input wired from a member");
                }
                // D4/P6: a multi-output producer (struct-return body, ≥2
                // texture outputs) isn't a single register — the wire names
                // WHICH BodyOutputs field threads. Zero for every buffer-array
                // producer and every single-texture-output atom, so this stays
                // `Member(u32)` — byte-identical — for every region that
                // existed before this phase.
                if producer_tex_output_count(registry, def, wire.from_node) > 1 {
                    RegionInput::MemberPort(wire.from_node, wire.from_port.clone())
                } else {
                    RegionInput::Member(wire.from_node)
                }
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
        // BUFFER members: append each texture input as a gathered EXTERNAL after
        // the array entries — the buffer analogue of the texture path's sampler-
        // Gather (the body samples the bound texture at an element-computed
        // coord). Array entries stay first so array-port indexing (the in-place
        // alias trace, the codegen's element registers) is untouched. Classify
        // admitted only WIRED sampled 2D/3D texture inputs; a member can never
        // produce a texture (buffer atoms with texture outputs are boundaries),
        // so the producer is always external — bail defensively if violated.
        if is_buffer {
            for port in constructed.inputs().iter().filter(|i| is_texture_port(&i.ty)) {
                if !matches!(port.ty, PortType::Texture2D | PortType::Texture3D) {
                    return Err("buffer member samples a non-2D/3D texture");
                }
                let wire = def
                    .wires
                    .iter()
                    .find(|w| w.to_node == doc_id && w.to_port == port.name)
                    .ok_or("buffer member's texture input unwired")?;
                if node_set.contains(&wire.from_node) {
                    return Err("buffer member's texture produced inside the region");
                }
                let key = (wire.from_node, wire.from_port.clone());
                let slot = *ext_index.entry(key).or_insert_with(|| {
                    externals.push(ExternalRef {
                        from_node: wire.from_node,
                        from_port: wire.from_port.clone(),
                    });
                    externals.len() - 1
                });
                inputs.push(RegionInput::External(slot));
                input_access.push(InputAccess::Gather);
            }
        }
        // TEXTURE members with a `BufferIndex`-tagged Array input (D3, closes
        // BUG-114): append each such input as a gathered EXTERNAL after the
        // texture entries — the mirror image of the buffer-member append
        // above (there array-first-texture-after; here texture-first-array-
        // after, since texture is this member's PRIMARY domain). The array
        // producer must never be a region member — classify_node's wire gate
        // only stops the wire from forcing the whole node to Boundary, the
        // "gather never unions" contract (`is_gather()`) still applies, so a
        // producer that slipped in as a member here is a defensive bail, same
        // as the ordinary gather check above.
        if !is_buffer {
            let tex_count = constructed.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
            for (arr_idx, port) in constructed
                .inputs()
                .iter()
                .filter(|i| matches!(i.ty, PortType::Array(_)))
                .enumerate()
            {
                if access_list.get(tex_count + arr_idx) != Some(&InputAccess::BufferIndex) {
                    continue; // not a BufferIndex-tagged array input on this atom
                }
                let wire = def
                    .wires
                    .iter()
                    .find(|w| w.to_node == doc_id && w.to_port == port.name)
                    .ok_or("BufferIndex input unwired")?;
                if node_set.contains(&wire.from_node) {
                    return Err("BufferIndex array produced inside the region");
                }
                let key = (wire.from_node, wire.from_port.clone());
                let slot = *ext_index.entry(key).or_insert_with(|| {
                    externals.push(ExternalRef {
                        from_node: wire.from_node,
                        from_port: wire.from_port.clone(),
                    });
                    externals.len() - 1
                });
                inputs.push(RegionInput::External(slot));
                input_access.push(InputAccess::BufferIndex);
            }
        }
        // f16-faithful rounding (stencil tier A): an in-loop member whose
        // unfused output texture is f16 gets its fused register quantized to
        // half precision after every body call — see the classify comment.
        // Deliberately NOT extended to out-of-loop members: a 2026-06-10 probe
        // (simplex→scale fused vs unfused) measured q16-everywhere WORSE than
        // plain f32 registers (all-pixel 1-ulp drift vs half-pixel) — the
        // residual out-of-loop gap is body-level FMA/inlining ULP noise across
        // kernel contexts, which quantization can't reconcile, only amplify.
        // Out-of-loop regions live with the documented ≈ulp tolerance; loops
        // get q16 because there the inputs are identical by induction and the
        // store rounding is the only gap.
        let quantize_f16 = !is_buffer
            && node_on_cycle(doc_id, def)
            && !node.output_formats.values().any(|s| s.contains("32float"));
        members.push(RegionMember { doc_id, inputs, input_access, quantize_f16 });
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
    // D4/P6: dedup by (id, PORT) — not just id — so a multi-output member with
    // TWO distinct ports each feeding a live external consumer gets two
    // entries (its own `dst_<k>` each), while a single-output member (or a
    // multi-output member escaping through only one of its ports) still gets
    // exactly one, same as before.
    let mut outputs: Vec<(u32, String)> = Vec::new();
    for &id in nodes {
        let mut escaping_ports: Vec<String> = Vec::new();
        for w in &def.wires {
            if w.from_node == id
                && !node_set.contains(&w.to_node)
                && is_region_wire(def, registry, w, is_buffer)
            {
                if !final_reachable.contains(&w.to_node) {
                    return Err("escaping wire to a dead consumer");
                }
                if !escaping_ports.contains(&w.from_port) {
                    escaping_ports.push(w.from_port.clone());
                }
            }
        }
        for port in escaping_ports {
            outputs.push((id, port));
        }
    }
    outputs.sort_unstable();
    if outputs.is_empty() {
        return Err("dead region — nothing leaves it");
    }

    // v1 buffer regions are single-output (the fused node writes one fresh `dst`
    // array). Fan-out buffer regions are a follow-on. Texture regions allow
    // multi-output (fan-out) as before.
    if is_buffer && outputs.len() != 1 {
        return Err("fan-out buffer region (v1 is single-output)");
    }

    // ── Tier 6: element-space uniformity. The fused kernel iterates one grid,
    // so every member's unfused output must have resolved to the SAME space,
    // and every coincident external (read via `textureLoad` at the kernel's
    // own coordinate) must live at that space too. Gathered externals are
    // exempt — the body samples them at a normalized UV through `samp`, which
    // is resolution-independent by construction. Any mismatch drops the whole
    // region (renders unfused, always correct). Buffer regions have no
    // texture grid; their space is `None`. ──
    let mut sampled_externals: Vec<usize> = Vec::new();
    let space = if is_buffer {
        None
    } else {
        let region_space = node_output_space(spaces, def, registry, order[0]);
        for &id in &order {
            if node_output_space(spaces, def, registry, id) != region_space {
                return Err("member off the region's element space");
            }
        }
        // CROSS-RESOLUTION externals (workstream 4 — the Watercolor/Bloom unlock).
        // This gate — not convexity — is what split Watercolor's tail
        // (luma_blur_v → dilute → guard → wet_dry): the component unions cleanly
        // across the feedback loop (the guard → feedback state-capture wire IS
        // correctly excluded as a back edge), but `dilute.mask` reads `mask_map`
        // — the half-res flow field, rescaled — as a COINCIDENT external. A
        // coincident read used to be a `textureLoad` at the fused kernel's own
        // canvas coordinate, which on a half-res texture reads the wrong texel
        // (or out of bounds), so the region dropped here.
        //
        // The fix mirrors the unfused graph exactly: a plain `Coincident` input
        // is resolution-ROBUST by contract — the standalone atom reads it through
        // a sampler at the fragment UV, and a sampler rescales across the seam
        // (the stencil session proved sampler-based chain reads bit-exact). So a
        // space-MISMATCHED `Coincident` external is now ADMITTED and its slot
        // marked SAMPLED: the codegen reads it via `textureSampleLevel(src_e,
        // samp, uv, 0.0)` — the identical standalone read — instead of the
        // same-res textureLoad pre-read. Same-space externals stay textureLoad
        // (byte-identical to v1). Scope guards: a `CoincidentTexel` external is
        // texel-EXACT (dither's ordered-threshold pattern) and a rescaled sample
        // would blend neighbours into garbage, so it stays same-res-gated — drop
        // the region. Gathers are already exempt (they sample at a body-computed
        // UV regardless of space). Resampling ATOMS (`output_canvas_scale` ≠ 1:1)
        // never reach here — classify keeps them boundaries; we unlock cross-res
        // READS into a region, not folding a resampler in.
        for member in &members {
            for (input, access) in member.inputs.iter().zip(&member.input_access) {
                if access.is_gather() {
                    continue;
                }
                if let RegionInput::External(slot) = input {
                    let ext = &externals[*slot];
                    if space_of(spaces, ext.from_node, &ext.from_port) != region_space {
                        if *access == InputAccess::CoincidentTexel {
                            return Err("texel-exact external off the region's element space");
                        }
                        if !sampled_externals.contains(slot) {
                            sampled_externals.push(*slot);
                        }
                    }
                }
            }
        }
        sampled_externals.sort_unstable();
        Some(region_space)
    };

    Ok(Region { members, externals, outputs, space, sampled_externals, virtual_chains: Vec::new() })
}

/// The element space of `id`'s (single) texture output in the unfused plan —
/// [`ElementSpace::Canvas`] when the node is unknown, has no texture output,
/// or the spaces map is unavailable.
fn node_output_space(
    spaces: Option<&AHashMap<(u32, String), ElementSpace>>,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    id: u32,
) -> ElementSpace {
    let Some(node) = def.nodes.iter().find(|n| n.id == id) else {
        return ElementSpace::Canvas;
    };
    let Some(constructed) = configured_construct(registry, node) else {
        return ElementSpace::Canvas;
    };
    let Some(port) = constructed
        .outputs()
        .iter()
        .find(|o| is_texture_port(&o.ty))
        .map(|o| o.name.clone())
    else {
        return ElementSpace::Canvas;
    };
    space_of(spaces, id, port.as_ref())
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

/// The read-access of `node`'s texture input `port` (Coincident if the atom
/// is unknown or the port isn't one of its texture inputs). `input_access()` is
/// aligned to the atom's TEXTURE inputs in `inputs()` order.
///
/// CONFIGURED construct (BUG-007 sibling): a fragment-form `node.wgsl_compute`
/// declares its input ports + their access modes only after `wgsl_source` is
/// parsed — a bare construct sees the default kernel, so a gather input would read
/// as coincident and wrongly union into a region.
fn input_port_access(registry: &PrimitiveRegistry, node: &EffectGraphNode, port: &str) -> InputAccess {
    let Some(node) = configured_construct(registry, node) else {
        return InputAccess::Coincident;
    };
    // `input_access` aligns to the SAME-domain inputs in declaration order:
    // texture inputs for a texture atom, Array inputs for a buffer atom (the
    // buffer codegen's `is_gather(i)` indexes the filtered Array inputs). Resolve
    // the port's index among inputs of its own kind so a `BufferGather` Array
    // input is detected (not silently treated as coincident).
    //
    // D3 (BUG-114) extends this for a TEXTURE-domain atom (no Array OUTPUT —
    // `is_buffer_atom` below) that also carries an Array INPUT (the `draw_*`
    // family's detections array): the flat `INPUT_ACCESS` const packs
    // [texture-input accesses] ++ [array-input accesses] for such an atom, so
    // an array port's slot is offset past the texture-input count. Every
    // existing atom has array inputs ONLY when it's buffer-domain (an Array
    // OUTPUT), so `is_buffer_atom` is true and this offset is never applied to
    // shipped atoms — additive, zero behavior change for anything but the new
    // mixed shape.
    let is_buffer_atom = node.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_)));
    let port_ty = node.inputs().iter().find(|i| i.name == port).map(|i| i.ty);
    let idx = match port_ty {
        Some(ty) if is_texture_port(&ty) => {
            node.inputs().iter().filter(|i| is_texture_port(&i.ty)).position(|i| i.name == port)
        }
        Some(PortType::Array(_)) if is_buffer_atom => node
            .inputs()
            .iter()
            .filter(|i| matches!(i.ty, PortType::Array(_)))
            .position(|i| i.name == port),
        Some(PortType::Array(_)) => {
            let tex_count = node.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
            node.inputs()
                .iter()
                .filter(|i| matches!(i.ty, PortType::Array(_)))
                .position(|i| i.name == port)
                .map(|p| p + tex_count)
        }
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
    !input_port_access(registry, to, &w.to_port).is_gather()
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
    let Some(node) = configured_construct(registry, to) else {
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
    let Some(node) = configured_construct(registry, to) else {
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
    // Configured so a fragment-form `node.wgsl_compute` producer reports its real
    // output port (`dst`) rather than the default kernel's.
    let Some(node) = configured_construct(registry, from) else {
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
    let Some(node) = configured_construct(registry, from) else {
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

/// Whether an outer-card binding in the def's preset metadata targets
/// (`node`, `param`). Addressed by stable `node_id`, falling back to the
/// handle for defs minted before node-id targeting (same resolution rule as
/// the install pass's `resolve_node_id`).
fn param_is_binding_target(node: &EffectGraphNode, param: &str, def: &EffectGraphDef) -> bool {
    let Some(meta) = &def.preset_metadata else {
        return false;
    };
    let stable = if node.node_id.is_empty() {
        node.handle.clone().unwrap_or_default()
    } else {
        node.node_id.as_str().to_string()
    };
    meta.bindings.iter().any(|b| {
        matches!(
            &b.target,
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, param: p }
                if node_id.as_str() == stable && p == param
        )
    })
}

/// Whether node `id` is a BUFFER-domain atom (writes an `Array<T>` output).
/// Drives the union guard: a texture wire into such an atom is a gathered
/// external, never a register-threading union edge.
fn node_is_buffer_atom(def: &EffectGraphDef, registry: &PrimitiveRegistry, id: u32) -> bool {
    // CONFIGURED construct (BUG-007 sibling): a full-kernel `node.wgsl_compute`
    // with a `var<storage, read_write> array<T>` output only reports that Array
    // port after its `wgsl_source` is applied — a bare construct sees the default
    // kernel and would miss the buffer domain, mis-driving the gather-vs-union
    // guard.
    def.nodes
        .iter()
        .find(|n| n.id == id)
        .and_then(|n| configured_construct(registry, n))
        .map(|c| c.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))))
        .unwrap_or(false)
}

/// Whether a region's members are buffer-domain (their fused output is an
/// `Array<T>`). Determined from any member's constructed output ports.
fn region_is_buffer(nodes: &[u32], def: &EffectGraphDef, registry: &PrimitiveRegistry) -> bool {
    nodes.iter().any(|&id| {
        // CONFIGURED construct (BUG-007 sibling) — see `node_is_buffer_atom`: a
        // full-kernel `node.wgsl_compute` reports its Array output only after
        // `wgsl_source` is applied, so a bare construct would pick the wrong
        // (texture) codegen path for a buffer region.
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| configured_construct(registry, n))
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

    fn strange_attractor_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/generator-presets/StrangeAttractor.json"
        ))
        .expect("read StrangeAttractor.json");
        serde_json::from_str(&json).expect("parse StrangeAttractor.json")
    }

    /// BUG-007: `cycle_contains_array` must construct nodes CONFIGURED. A
    /// full-kernel `node.wgsl_compute` particle node (StrangeAttractor's sim
    /// stage) declares its `var<storage, read_write> array<Particle>` output only
    /// after its `wgsl_source` is parsed. A bare construct sees the default kernel
    /// with no Array output, so the particle stage is invisible to the SCC scan and
    /// a texture atom on the same feedback loop wrongly passes cut rule 12 and
    /// fuses tier-A f16 in-loop (where the bit-exact induction fails across a
    /// scatter — FluidSim divergence class).
    #[test]
    fn cycle_through_configured_particle_wgsl_compute_is_particle_loop() {
        let reg = registry();
        let def =
            manifold_core::flatten::flatten_groups(&strange_attractor_def()).expect("flatten");
        let sim = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .expect("StrangeAttractor ships a full-kernel particle wgsl_compute node")
            .clone();

        // Root-cause pin: the Array output exists only on the CONFIGURED construct.
        let bare = reg.construct(&sim.type_id).expect("construct node.wgsl_compute");
        assert!(
            !bare.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))),
            "bare construct sees the default kernel — no Array output (the blind spot)",
        );
        let configured = configured_construct(&reg, &sim).expect("configured construct");
        assert!(
            configured
                .outputs()
                .iter()
                .any(|o| matches!(o.ty, PortType::Array(_))),
            "configured construct reports the particle Array output",
        );

        // Minimal feedback cycle: texture atom (id 101) ↔ particle node (id 100).
        // Only wires + node type_ids matter to `cycle_contains_array`.
        let mut sim = sim;
        sim.id = 100;
        let mut tex = sim.clone();
        tex.id = 101;
        tex.type_id = "node.channel_mixer".to_string();
        tex.wgsl_source = None;
        let def2 = EffectGraphDef {
            version: def.version,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![tex, sim],
            wires: vec![
                EffectGraphWire {
                    from_node: 101,
                    from_port: "out".into(),
                    to_node: 100,
                    to_port: "in".into(),
                },
                EffectGraphWire {
                    from_node: 100,
                    from_port: "out".into(),
                    to_node: 101,
                    to_port: "in".into(),
                },
            ],
        };
        assert!(
            cycle_contains_array(101, &def2, &reg),
            "a loop through a configured particle wgsl_compute node is a particle \
             loop — cut rule 12 must fire (BUG-007)",
        );
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
            .find(|n| n.type_id == "node.clamp")
            .map(|n| n.id)
            .unwrap();
        assert_eq!(r.outputs, vec![(out_node, "out".to_string())], "clamp is the region output");
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
    /// headline generalisation. source → gain → contrast → multi_blend(boundary)
    /// → saturation → clamp → final yields {gain, contrast} feeding the blend,
    /// then {saturation, clamp} reading the blend's output. (`node.multi_blend`
    /// is a self-synthesizing router — a PERMANENT boundary by design, so this
    /// fixture can't silently start fusing when vocabulary atoms gain bodies —
    /// unlike a gather such as `gaussian_blur`, which tier 3 folds IN; see the
    /// gather tests below.)
    #[test]
    fn boundary_splits_into_two_regions() {
        let json = r#"{
            "version": 1, "name": "split", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 3, "typeId": "node.multi_blend", "nodeId": "thresh" },
                { "id": 4, "typeId": "node.saturation", "nodeId": "sat" },
                { "id": 5, "typeId": "node.clamp", "nodeId": "clamp" },
                { "id": 6, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in_0" },
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
        assert_eq!(r1.outputs, vec![(2, "out".to_string())], "contrast feeds the threshold");

        // Region 2: saturation(4) → clamp(5), reads the threshold, output = clamp.
        let r2 = &regions[1];
        assert_eq!(r2.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![4, 5]);
        assert_eq!(r2.externals.len(), 1, "region 2 reads the threshold output");
        assert_eq!(r2.externals[0].from_node, 3, "the threshold is region 2's external");
        assert_eq!(r2.outputs, vec![(5, "out".to_string())], "clamp feeds final_output");
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
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
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

    /// D3 (BUG-114): a `BufferIndex`-tagged wire never unions and the array
    /// producer never becomes a region member — the array analogue of
    /// `gather_atom_folds_into_a_region` / `gather_wire_does_not_union` above.
    ///
    /// `draw_dots` carries a `Color` param, which independently keeps
    /// `classify_node` from ever admitting ANY atom into `eligible` (cut rule
    /// 4 — no non-scalar param may join a multi-node region; P5's scope, not
    /// this design's). This is the SAME reason six of wave2's seven
    /// shading-family atoms stay lone Boundaries despite being individually
    /// fusable (`wave2_color_param_atoms_stay_boundary_in_shipped_presets`),
    /// and it applies to every `draw_*` atom (all six carry a Color param) —
    /// so `partition_regions` itself can never exercise draw_dots as a region
    /// MEMBER until P5 lifts that gate. This test proves the D3 mechanism
    /// directly at the two layers that ARE reachable today: the wire-level
    /// gather contract (`input_port_access`/`wire_coincident_consumed`, which
    /// `partition_regions`' union filter reads regardless of the atom's
    /// overall eligibility) and `build_region` itself (called directly here,
    /// as `partition_regions` would once P5 makes draw_dots `eligible`).
    #[test]
    fn buffer_index_external_stays_external() {
        let json = r#"{
            "version": 1, "name": "dots-hud", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.blob_tracker", "nodeId": "blobs" },
                { "id": 3, "typeId": "node.draw_dots", "nodeId": "dots" },
                { "id": 4, "typeId": "node.saturation", "nodeId": "sat" },
                { "id": 5, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "blobs", "toNode": 3, "toPort": "detections" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();

        // draw_dots is on the codegen path (Pointwise, a real body, no
        // BoundaryReason) and, since P5 lifted Vec3/Vec4/Color params (D4
        // scope expansion, closing the P4a escalation), `classify_node` now
        // returns Eligible — the Color param no longer cuts it, and the
        // BufferIndex wire never did either. Pin both facts so a future
        // regression in either gate is caught here.
        let dots_node = def.nodes.iter().find(|n| n.id == 3).unwrap();
        let dots_prim = configured_construct(&registry(), dots_node).unwrap();
        assert_eq!(
            dots_prim.fusion_kind(),
            crate::node_graph::freeze::classify::FusionKind::Pointwise
        );
        assert!(dots_prim.boundary_reason().is_none());
        assert_eq!(
            dots_prim.input_access(),
            &[InputAccess::Coincident, InputAccess::BufferIndex]
        );
        assert_eq!(
            classify_node(dots_node, &def, &registry()),
            NodeClass::Eligible,
            "P5 lifted the Color param — draw_dots is no longer cut by cut rule 4"
        );
        // explain_presets/census verdict: draw_dots is Eligible now, so
        // `classify_refusal` has nothing to bucket for it (None) — the
        // BufferIndex codegen gap AND the Color-param gap are both closed.
        assert_eq!(audit::classify_refusal(dots_node, &def, &registry()), None);

        // Wire-level contract: the array wire into `detections` is gather-
        // shaped (BufferIndex.is_gather()), so partition_regions' union
        // filter refuses it regardless of either endpoint's eligibility.
        let det_wire = def.wires.iter().find(|w| w.to_port == "detections").unwrap();
        assert!(
            !wire_coincident_consumed(&def, &registry(), det_wire),
            "a BufferIndex-consumed wire must never be a union candidate"
        );

        // build_region itself (the D3 mechanism): fed draw_dots directly,
        // bypassing the orthogonal Color-param eligibility filter — the real
        // region-assembly code path this design adds. Its array input must
        // resolve to an EXTERNAL (never a Member), naming blob_tracker.
        let final_reachable = final_reachable_nodes(&def);
        let region = build_region(&def, &registry(), &[3], &final_reachable, None)
            .expect("draw_dots assembles as a region shape on its own");
        assert_eq!(region.members.len(), 1);
        let dots = &region.members[0];
        assert_eq!(
            dots.input_access,
            vec![InputAccess::Coincident, InputAccess::BufferIndex]
        );
        assert_eq!(dots.inputs.len(), 2);
        let RegionInput::External(slot) = dots.inputs[1] else {
            panic!("detections must resolve to an external, not a member: {:?}", dots.inputs[1]);
        };
        assert_eq!(region.externals[slot].from_node, 2, "blob_tracker is the external producer");
        assert_eq!(region.externals[slot].from_port, "blobs");

        // P5's concrete proof (P4a's escalation, resolved): on the full graph
        // (not the isolated single-node `build_region` call above), draw_dots
        // now actually UNIONS with a texture neighbour (sat) into one real
        // region, closing the gap `wave2_color_param_atoms_stay_boundary_in_
        // shipped_presets` (P4a) pinned as still-open.
        let regions = partition_regions(&def, &registry());
        let dots_region = regions
            .iter()
            .find(|r| r.members.iter().any(|m| m.doc_id == 3))
            .expect("draw_dots forms a real region with a neighbor now that its Color param lifts");
        assert!(
            dots_region.members.iter().any(|m| m.doc_id == 4),
            "sat (the consumer) joins draw_dots' region"
        );
        // gain (node 1) stays OUTSIDE this region — not because of its own
        // Color/Vec3/Vec4 param (it has none), but because it fans out to
        // TWO consumers, one of them (blob_tracker) a boundary: a shared
        // producer with a non-fusable branch keeps its own texture rather
        // than being absorbed as a member (`shared_producer_is_not_absorbed`
        // is the same principle). Orthogonal to this test's point — the real
        // proof is that draw_dots + sat now form one 2-member region at all,
        // where before P5 draw_dots was Boundary and no region ever formed.
        assert!(
            !dots_region.members.iter().any(|m| m.doc_id == 1),
            "gain fans out to the boundary blob_tracker too, so it stays external"
        );
    }

    /// A lone fusable atom is not a region (fusing one node changes nothing). The
    /// MIN_REGION_LEN gate drops it; the card renders unfused.
    #[test]
    fn single_atom_is_not_a_region() {
        let json = r#"{
            "version": 1, "name": "solo", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
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
        assert_eq!(r.outputs, vec![(2, "out".to_string())], "invert feeds final_output");
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
        assert_eq!(r.outputs, vec![(2, "out".to_string())], "mix feeds final_output");
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
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 4, "typeId": "node.multi_blend", "nodeId": "thr_a" },
                { "id": 5, "typeId": "node.multi_blend", "nodeId": "thr_b" },
                { "id": 6, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 7, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "in_0" },
                { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "in_0" },
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
            vec![(2, "out".to_string()), (3, "out".to_string())],
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
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 4, "typeId": "node.multi_blend", "nodeId": "dead" },
                { "id": 5, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 5, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in_0" }
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
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.multi_blend", "nodeId": "thr" },
                { "id": 4, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 5, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 6, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in_0" },
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

    /// D4/P6 regression guard (found + fixed by the Glitch real-preset proof):
    /// a MULTI-output node's two ports can each union independently into
    /// otherwise-unrelated branches — one branch ends in a node whose output
    /// GATHER-feeds the other branch's node. Neither branch unions with the
    /// other directly (the gather wire is correctly excluded from union
    /// candidates), but the multi-output producer bridges them into ONE
    /// component via two separate coincident wires. `build_region` would
    /// then find the gather wire's endpoints BOTH inside that one merged
    /// component and bail the WHOLE thing to unfused — costing every member,
    /// not just the gather pair. The gather-bridge guard in `partition_regions`
    /// must keep the two components separate instead, so each still fuses
    /// on its own and the two connect via the SAME cross-region gather the
    /// multi-region model already relies on.
    ///
    /// Topology: `cells` (Source, 2 texture outputs) → `out` feeds `invert`
    /// feeds `remap.uv_field` (branch A); `cells` → `cell_id` feeds
    /// `hash.field` (branch B). `remap`'s output GATHER-feeds `rgb_split.in`;
    /// `hash`'s output feeds `rgb_split.velocity` (Coincident) — so without
    /// the guard, `cells` bridges A and B into one component that contains
    /// both `remap` and `rgb_split`, the gather pair.
    #[test]
    fn multi_output_producer_never_bridges_a_gather_pair_into_one_region() {
        let json = r#"{
            "version": 1, "name": "gather_bridge", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.voronoi_2d", "nodeId": "cells" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.remap", "nodeId": "remap" },
                { "id": 4, "typeId": "node.hash_field_by_seed", "nodeId": "hash" },
                { "id": 5, "typeId": "node.rgb_split", "nodeId": "split" },
                { "id": 6, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "uv_field" },
                { "fromNode": 0, "fromPort": "out", "toNode": 3, "toPort": "source" },
                { "fromNode": 1, "fromPort": "cell_id", "toNode": 4, "toPort": "field" },
                { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "in" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "velocity" },
                { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let mut regions = partition_regions(&def, &registry());
        // Two separate regions (branch A: cells+invert+remap; branch B: hash
        // alone, or folded with whichever side the finder groups it) — NEVER
        // one region containing both `remap` (3) and `rgb_split` (5), which
        // would mean the gather pair got bridged into one component.
        for r in &regions {
            let ids: Vec<u32> = r.members.iter().map(|m| m.doc_id).collect();
            assert!(
                !(ids.contains(&3) && ids.contains(&5)),
                "remap and rgb_split (a gather pair) must never share a region: {ids:?}"
            );
        }
        regions.sort_by_key(|r| r.members[0].doc_id);
        assert!(
            regions.iter().any(|r| r.members.iter().any(|m| m.doc_id == 1)
                && r.members.iter().any(|m| m.doc_id == 3)),
            "cells must still fuse with its OWN branch (invert + remap)"
        );
    }

    /// A specialization-constant atom now FUSES: classify substitutes the
    /// declared tokens (`QUALITY_LEVEL` / `WEIGHTING_MODE`) with the def's
    /// static param values before the naga parse gate, so
    /// `gaussian_blur_variable_width` stops being a permanent boundary. Here
    /// the upstream invert is a stranded single absorbed into the blur's `in`
    /// fetch; the `width` input gathers the source as a real external; the
    /// downstream invert threads the blur's register. One region.
    #[test]
    fn specialization_atom_fuses_with_substituted_tokens() {
        let json = r#"{
            "version": 1, "name": "spec", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.invert", "nodeId": "inv_a" },
                { "id": 2, "typeId": "node.variable_blur", "nodeId": "blur" },
                { "id": 3, "typeId": "node.invert", "nodeId": "inv_b" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "width" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "the variable-width blur fuses");
        let r = &regions[0];
        assert_eq!(
            r.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![2, 3],
            "blur + downstream invert"
        );
        assert_eq!(r.virtual_chains.len(), 1, "the upstream invert is absorbed");
        assert_eq!(r.virtual_chains[0].members[0].doc_id, 1);
        assert_eq!(r.externals.len(), 1, "the source backs both the chain and width");
        assert_eq!(r.outputs, vec![(3, "out".to_string())]);
    }

    /// A specialization param that an outer binding targets keeps the atom a
    /// BOUNDARY — the baked token value could diverge from the live binding.
    #[test]
    fn binding_targeted_specialization_param_stays_boundary() {
        let json = r#"{
            "version": 1, "name": "specbind", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 2, "typeId": "node.variable_blur", "nodeId": "blur" },
                { "id": 3, "typeId": "node.invert", "nodeId": "inv_b" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "width" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ],
            "presetMetadata": {
                "id": "specbind", "displayName": "Spec Bind", "category": "Stylize",
                "oscPrefix": "specbind",
                "params": [],
                "bindings": [
                    { "id": "outer_quality", "label": "Quality", "defaultValue": 1.0,
                      "target": { "kind": "node", "nodeId": "blur", "param": "quality" },
                      "convert": { "type": "Float" } }
                ]
            }
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert!(
            partition_regions(&def, &registry()).is_empty(),
            "a binding-targeted specialization param keeps the blur a boundary"
        );
    }

    /// Stencil tier — a STRANDED SINGLE producer (a pointwise atom whose only
    /// consumer is a stencil blur's gather input) is absorbed into the blur's
    /// fetch as a virtual chain: one region, member = the blur, the gain
    /// recomputed per tap corner, the source as the chain's external. Without
    /// absorption both nodes are lone components and nothing fuses.
    #[test]
    fn stranded_single_absorbs_into_blur_fetch() {
        let json = r#"{
            "version": 1, "name": "stencil", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.gaussian_blur", "nodeId": "blur" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "blur + absorbed gain form one region");
        let r = &regions[0];
        assert_eq!(r.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(r.virtual_chains.len(), 1, "gain absorbed as a virtual chain");
        let chain = &r.virtual_chains[0];
        assert_eq!(chain.consumer, 2);
        assert_eq!(chain.input_index, 0);
        assert_eq!(chain.output, 1);
        assert_eq!(chain.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(r.externals.len(), 1, "the source backs the chain");
        assert_eq!(r.externals[0].from_node, 0);
        assert_eq!(chain.members[0].inputs, vec![RegionInput::External(0)]);
        let blur = &r.members[0];
        assert_eq!(blur.inputs, vec![RegionInput::Virtual(0)], "the blur reads the chain");
        assert_eq!(r.outputs, vec![(2, "out".to_string())]);
    }

    /// A producer with a SECOND consumer (the blur and a mix both read the
    /// gain) is NOT absorbed — its texture must still exist for the other
    /// consumer, so recomputing it inside the fetch would only add work.
    #[test]
    fn shared_producer_is_not_absorbed() {
        let json = r#"{
            "version": 1, "name": "shared", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.gaussian_blur", "nodeId": "blur" },
                { "id": 3, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "a" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "b" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert!(
            regions.iter().all(|r| r.virtual_chains.is_empty()),
            "a producer with two consumers keeps its texture"
        );
    }

    /// A two-atom run upstream of a blur keeps fusing as its OWN pointwise
    /// region (the v1 absorption cap is stranded singles — see
    /// MAX_VIRTUAL_CHAIN); the blur stays standalone.
    #[test]
    fn two_atom_chain_keeps_its_own_region() {
        let json = r#"{
            "version": 1, "name": "pair", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 3, "typeId": "node.gaussian_blur", "nodeId": "blur" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "the gain+contrast pair fuses; the blur stays standalone");
        assert_eq!(
            regions[0].members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(regions[0].virtual_chains.is_empty());
    }

    /// P3 wave 2 (2026-07-14): `node.shininess`/`node.rim_light`/
    /// `node.matcap_two_tone` (OilyFluid), `node.brightness` (MetallicGlass)
    /// and `node.channel_mixer` (StarField) all converted onto the freeze
    /// codegen path that wave — `fusion_kind() == Pointwise` with a real
    /// `wgsl_body` — but every one of them carries a Color/Vec3/Vec4 param.
    /// Before P5 `classify_node`'s
    /// scalar-only cut rule rejected all five, so none was ever a region
    /// member. P5 lifts Vec3/Vec4/Color (three/four namespaced uniform
    /// fields, same mechanism as the standalone codegen's "P3 wave 2"
    /// reassembly) — this test now pins the OPPOSITE, equally real finding:
    /// every one of these five atoms, in the three bundled presets that ship
    /// them, now DOES join a region. `graph_tool fusion` confirms the same
    /// per-node verdict interactively.
    #[test]
    fn wave2_color_param_atoms_now_fuse_in_shipped_presets() {
        let registry = registry();
        let cases: &[(&str, &[&str])] = &[
            ("OilyFluid", &["node.shininess", "node.rim_light", "node.matcap_two_tone"]),
            ("MetallicGlass", &["node.brightness"]),
            ("StarField", &["node.channel_mixer"]),
        ];
        for (preset_name, type_ids) in cases {
            let type_id = manifold_core::PresetTypeId::new(preset_name);
            let json = crate::node_graph::bundled_presets::bundled_preset_json(&type_id)
                .unwrap_or_else(|| panic!("{preset_name}: no bundled JSON"));
            let def: EffectGraphDef = serde_json::from_str(&json).expect("preset parses");
            let def = manifold_core::flatten::flatten_groups(&def).expect("flattens");

            let fused_doc_ids: std::collections::HashSet<u32> = partition_regions(&def, &registry)
                .iter()
                .flat_map(|r| r.members.iter().map(|m| m.doc_id))
                .collect();

            for &type_id_str in *type_ids {
                let hits: Vec<u32> = def
                    .nodes
                    .iter()
                    .filter(|n| n.type_id == type_id_str)
                    .map(|n| n.id)
                    .collect();
                assert!(!hits.is_empty(), "{preset_name}: expected {type_id_str} to appear");
                for doc_id in hits {
                    assert!(
                        fused_doc_ids.contains(&doc_id),
                        "{preset_name}: {type_id_str} (doc_id={doc_id}) should now join a region \
                         — P5 lifted its Color/Vec3/Vec4 param"
                    );
                }
            }
        }
    }

    /// `node.tone_map` and `node.gradient_map` ship no bundled preset today
    /// (unlike the other five wave-2 atoms), so the real-preset proof above
    /// can't cover them — this minimal synthetic graph (`node.contrast`, the
    /// doc's own worked scalar-param example, feeding the target atom) covers
    /// each instead, per the task's documented fallback.
    ///
    /// - `node.tone_map` has ZERO non-scalar params (exposure/paper_white/
    ///   max_nits are Float, curve/mode are Enum->u32) — it's exactly as
    ///   scalar-clean as `node.contrast`, so it joins the region.
    /// - `node.gradient_map` carries two Color params (`color_a`/`color_b`).
    ///   Before P5 this was the same scalar-only cut as the five real-preset
    ///   atoms above (a standalone dispatch); P5 (D4's Vec3/Vec4/Color lift)
    ///   now lifts it too, so both atoms join the region — no longer an
    ///   "opposite behaviour" pair, both genuinely fuse.
    #[test]
    fn tone_map_and_gradient_map_both_fuse_next_to_a_fusable_neighbor() {
        let registry = registry();
        for (type_id, in_port, should_fuse) in
            [("node.tone_map", "in", true), ("node.gradient_map", "source", true)]
        {
            let json = format!(
                r#"{{
                    "version": 1, "name": "pair", "nodes": [
                        {{ "id": 0, "typeId": "system.source", "nodeId": "source" }},
                        {{ "id": 1, "typeId": "node.contrast", "nodeId": "contrast" }},
                        {{ "id": 2, "typeId": "{type_id}", "nodeId": "target" }},
                        {{ "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }}
                    ], "wires": [
                        {{ "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" }},
                        {{ "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "{in_port}" }},
                        {{ "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }}
                    ]
                }}"#
            );
            let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
            let regions = partition_regions(&def, &registry);
            let fused_doc_ids: std::collections::HashSet<u32> =
                regions.iter().flat_map(|r| r.members.iter().map(|m| m.doc_id)).collect();
            assert_eq!(
                fused_doc_ids.contains(&2),
                should_fuse,
                "{type_id}: region-membership next to a fusable neighbour (contrast) \
                 didn't match expectation (fuse={should_fuse})"
            );
        }
    }

    /// P3 wave 3 (2026-07-14, final conversion wave): `node.rotate_coordinates`
    /// and `node.sine_wave` also ship in zero bundled presets today (palette/
    /// vocabulary atoms, not yet wired into a shipped preset), so — same
    /// documented fallback as `tone_map_fuses_gradient_map_stays_boundary_
    /// next_to_a_fusable_neighbor` above — this synthetic pair covers each
    /// instead of a real-preset proof. Both are genuinely scalar-only
    /// (`angle`; `a, b, c, freq, freq_scale, time, time_scale` — all Float,
    /// no Color/Vec3/Vec4), so both actually DO join the region next to
    /// `node.contrast`, unlike the wave-2 Color-param atoms above. `graph_tool
    /// fusion` confirms the same per-node verdict interactively. The third
    /// remaining ledger atom, `node.watercolor`, is NOT covered here — it's a
    /// 7-pass composite with persistent cross-frame feedback state, not a
    /// single-dispatch atom, so it was left in `CONVERSION_DEBT_LEDGER`
    /// rather than converted this wave (see the ledger's comment).
    #[test]
    fn wave3_scalar_only_atoms_fuse_next_to_a_fusable_neighbor() {
        let registry = registry();
        for (type_id, in_port, should_fuse) in
            [("node.rotate_coordinates", "in", true), ("node.sine_wave", "field", true)]
        {
            let json = format!(
                r#"{{
                    "version": 1, "name": "pair", "nodes": [
                        {{ "id": 0, "typeId": "system.source", "nodeId": "source" }},
                        {{ "id": 1, "typeId": "node.contrast", "nodeId": "contrast" }},
                        {{ "id": 2, "typeId": "{type_id}", "nodeId": "target" }},
                        {{ "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }}
                    ], "wires": [
                        {{ "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" }},
                        {{ "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "{in_port}" }},
                        {{ "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }}
                    ]
                }}"#
            );
            let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
            let regions = partition_regions(&def, &registry);
            let fused_doc_ids: std::collections::HashSet<u32> =
                regions.iter().flat_map(|r| r.members.iter().map(|m| m.doc_id)).collect();
            assert_eq!(
                fused_doc_ids.contains(&2),
                should_fuse,
                "{type_id}: region-membership next to a fusable neighbour (contrast) \
                 didn't match expectation (fuse={should_fuse})"
            );
        }
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
                { "id": 1, "typeId": "node.texture_size", "nodeId": "dims" },
                { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
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

    /// Checkpoint (wgsl_compute fusion contract): a FRAGMENT-form `node.wgsl_compute`
    /// is a first-class fusable atom — an atom → fragment → atom chain partitions
    /// into ONE region holding all three. The fragment reports `Pointwise` + a
    /// `wgsl_body` only because `configured_construct` applies its `wgslSource`
    /// before the classifier reads it; a bare construct sees the opaque default
    /// kernel (Boundary) and the chain would split into three singletons. Note the
    /// fragment's output port is `dst` — the name the standalone codegen gives the
    /// single storage-texture output it synthesizes.
    #[test]
    fn wgsl_compute_fragment_fuses_with_atoms() {
        use crate::node_graph::freeze::markers::Marker;
        // Same placeholder-substitution convention as the proof.rs sibling of this
        // fixture — the `@fusion: pointwise` marker is routed through `Marker::emit`
        // rather than hand-typed, so this test stays off the single-sourced grammar's
        // negative gate.
        let json = r#"{
            "version": 1, "name": "frag", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain",
                  "params": { "gain": { "type": "Float", "value": 1.2 } } },
                { "id": 2, "typeId": "node.wgsl_compute", "nodeId": "frag",
                  "wgslSource": "FUSION_MARKER\n// @in: src\n// @param: scale = 0.75 [0, 2]\nfn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, scale: f32) -> vec4<f32> {\n    return vec4<f32>(c.rgb * scale, c.a);\n}\n",
                  "params": { "scale": { "type": "Float", "value": 0.75 } } },
                { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "src" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#
        .replacen("FUSION_MARKER", &Marker::Fusion { kind: "pointwise".to_string() }.emit(), 1);
        let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "gain + fragment + invert form one region");
        assert_eq!(
            regions[0].members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "the fragment-form wgsl_compute fuses between the two atoms"
        );
        assert!(regions[0].virtual_chains.is_empty());
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
        let chains: usize = regions.iter().map(|r| r.virtual_chains.len()).sum();
        eprintln!(
            "[audit] {name:<26} grouped={grouped:<5} workers={workers:<3} 3d={tex3d:<3} arr={arr:<3} \
             raw_regions={raw:<2} flat_regions={:<2} fused_atoms={fused_atoms:<3} chains={chains:<2} sizes={sizes:?}",
            regions.len(),
        );
    }

    /// Per-preset WHY report: classification of every node, region membership,
    /// and — for each eligible↔eligible wire that did NOT union — which gate cut
    /// it. Diagnoses "this pointwise atom stayed standalone" without guessing.
    fn explain_preset(name: &str, json: &str, registry: &PrimitiveRegistry) {
        use crate::node_graph::freeze::classify::FusionKind;
        let def: EffectGraphDef = serde_json::from_str(json).expect("preset parses");
        let def = manifold_core::flatten::flatten_groups(&def).expect("flattens");
        let spaces = resolve_output_spaces(&def, registry);
        let regions = partition_regions(&def, registry);
        let member_of: AHashMap<u32, usize> = regions
            .iter()
            .enumerate()
            .flat_map(|(i, r)| r.members.iter().map(move |m| (m.doc_id, i)))
            .collect();
        eprintln!("=== {name}: {} regions ===", regions.len());
        for n in &def.nodes {
            let class = classify_node(n, &def, registry);
            let kind = registry
                .construct(&n.type_id)
                .map(|p| format!("{:?}", p.fusion_kind()))
                .unwrap_or_else(|| "?".into());
            let region = member_of
                .get(&n.id)
                .map(|i| format!("region {i}"))
                .unwrap_or_else(|| "-".into());
            eprintln!(
                "  [{:>3}] {:<28} {:<34} {:?} kind={kind:<22} {region}",
                n.id,
                n.handle.as_deref().unwrap_or("-"),
                n.type_id,
                class
            );
        }
        // Re-run the union-candidate filters per eligible↔eligible wire and name
        // the first gate that cut it.
        for w in &def.wires {
            let from_ok = classify_node(
                def.nodes.iter().find(|n| n.id == w.from_node).unwrap(),
                &def,
                registry,
            ) == NodeClass::Eligible;
            let to_ok = classify_node(
                def.nodes.iter().find(|n| n.id == w.to_node).unwrap(),
                &def,
                registry,
            ) == NodeClass::Eligible;
            if !(from_ok && to_ok) {
                continue;
            }
            let same_region = member_of.get(&w.from_node).is_some()
                && member_of.get(&w.from_node) == member_of.get(&w.to_node);
            if same_region {
                continue;
            }
            let verdict = if !(is_texture_wire(&def, registry, w) || is_array_wire(&def, registry, w)) {
                "not a texture/Array wire (control)"
            } else if !wire_coincident_consumed(&def, registry, w) {
                "gather-consumed (producer stays external)"
            } else if !is_array_wire(&def, registry, w)
                && space_of(spaces.as_ref(), w.from_node, &w.from_port)
                    != node_output_space(spaces.as_ref(), &def, registry, w.to_node)
            {
                "element-space mismatch"
            } else {
                // The MERGE REJECTED / COMPONENT lines below name the cutter:
                // either convexity refused the union, or the component formed
                // and build_region dropped it (reason printed).
                "see MERGE/COMPONENT lines"
            };
            eprintln!(
                "  WIRE {} -> {} ({}.{} -> {}.{}): {verdict}",
                w.from_node, w.to_node, w.from_node, w.from_port, w.to_node, w.to_port
            );
        }
        let _ = FusionKind::Pointwise; // keep the import used even if kinds print as "?"

        // Replicate the union loop to separate "merge rejected (convexity)" from
        // "component merged fine, then build_region dropped it".
        let class: AHashMap<u32, NodeClass> = def
            .nodes
            .iter()
            .map(|n| (n.id, classify_node(n, &def, registry)))
            .collect();
        let eligible: AHashSet<u32> = class
            .iter()
            .filter(|(_, c)| **c == NodeClass::Eligible)
            .map(|(id, _)| *id)
            .collect();
        let forward: Vec<(u32, u32)> = def
            .wires
            .iter()
            .filter(|w| !is_state_capture_wire(&def, registry, w))
            .map(|w| (w.from_node, w.to_node))
            .collect();
        let mut candidates: Vec<(u32, u32)> = def
            .wires
            .iter()
            .filter(|w| {
                eligible.contains(&w.from_node)
                    && eligible.contains(&w.to_node)
                    && (is_texture_wire(&def, registry, w) || is_array_wire(&def, registry, w))
                    && wire_coincident_consumed(&def, registry, w)
                    && (is_array_wire(&def, registry, w)
                        || space_of(spaces.as_ref(), w.from_node, &w.from_port)
                            == node_output_space(spaces.as_ref(), &def, registry, w.to_node))
            })
            .map(|w| (w.from_node, w.to_node))
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        // D4/P6: mirrors `partition_regions`' gather-bridge guard exactly (see
        // its comment) — must stay in lockstep or this explainer would print
        // "component merged fine" for a union the real algorithm now refuses.
        let gather_pairs: Vec<(u32, u32)> = def
            .wires
            .iter()
            .filter(|w| {
                eligible.contains(&w.from_node)
                    && eligible.contains(&w.to_node)
                    && is_texture_wire(&def, registry, w)
                    && !wire_coincident_consumed(&def, registry, w)
            })
            .map(|w| (w.from_node, w.to_node))
            .collect();
        let mut uf = UnionFind::new(&eligible);
        for (a, b) in candidates {
            if uf.find(a) == uf.find(b) {
                continue;
            }
            let finds: AHashMap<u32, u32> = eligible.iter().map(|&n| (n, uf.find(n))).collect();
            let (ra, rb) = (uf.find(a), uf.find(b));
            let key = |n: u32| -> u32 {
                match finds.get(&n) {
                    Some(&r) if r == rb => ra,
                    Some(&r) => r,
                    None => n,
                }
            };
            let would_bridge_a_gather_wire =
                gather_pairs.iter().any(|&(gp, gc)| key(gp) == key(gc));
            if collapsed_has_cycle(&forward, &key) || would_bridge_a_gather_wire {
                eprintln!("  MERGE REJECTED (convexity or gather-bridge): {a} + {b}");
            } else {
                uf.union(a, b);
            }
        }
        let mut components: AHashMap<u32, Vec<u32>> = AHashMap::default();
        for &id in &eligible {
            components.entry(uf.find(id)).or_default().push(id);
        }
        let final_reachable = final_reachable_nodes(&def);
        for (_, mut nodes) in components {
            nodes.sort_unstable();
            if nodes.len() < 2 {
                continue;
            }
            match build_region(&def, registry, &nodes, &final_reachable, spaces.as_ref()) {
                Ok(_) => eprintln!("  COMPONENT {nodes:?}: build_region=ok"),
                Err(reason) => {
                    eprintln!("  COMPONENT {nodes:?}: build_region DROPPED — {reason}")
                }
            }
        }
    }

    // ═══════════════════════════ FUSION_SOTA P3 — refusal census (D4) ═══════════════════════════
    //
    // Buckets every refusal `explain_presets` can already print, by the family
    // D4 names, and estimates dispatches-saved-if-lifted per family. Read-only
    // instrumentation: every function below DUPLICATES `classify_node` /
    // `classify_buffer_node` / `build_region`'s existing gates for REPORTING —
    // it never disagrees with them about Eligible/Boundary, and it changes
    // nothing about what actually fuses (that would be touching the decision
    // model, forbidden by FUSION_SOTA_DESIGN.md's phasing preamble).

    /// One of the eight buckets FUSION_SOTA_DESIGN.md D4/P3 names. `Other`
    /// catches every boundary that isn't one of D4's four under-fusing
    /// families or the D3 BufferIndex-shaped family — control wires,
    /// register-heavy bodies, non-GPU/IoBridge/DrawCall/CrossFrameState/
    /// BarrieredReduction/FusedBundle/ConversionDebt atoms, Texture3D ports,
    /// q16/particle-loop cuts, specialization-token cuts, and any
    /// `build_region` drop that isn't the fan-out message.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub(crate) enum RefusalFamily {
        ParamType,
        Arity,
        MultiOutput,
        Resample,
        BufferFanOut,
        StencilDepth,
        BufferIndexShaped,
        Other,
    }

    impl RefusalFamily {
        fn label(self) -> &'static str {
            match self {
                RefusalFamily::ParamType => "param-type",
                RefusalFamily::Arity => "arity",
                RefusalFamily::MultiOutput => "multi-output",
                RefusalFamily::Resample => "resample",
                RefusalFamily::BufferFanOut => "buffer-fan-out",
                RefusalFamily::StencilDepth => "stencil-depth",
                RefusalFamily::BufferIndexShaped => "buffer-index-shaped",
                RefusalFamily::Other => "other",
            }
        }
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct FamilyStats {
        refusals: usize,
        dispatches_saved: usize,
    }

    /// WHY a node classifies `Boundary` — same gate ORDER as [`classify_node`],
    /// stopping at the first cut and naming its D4 family. `None` for an
    /// `Eligible` node (nothing to bucket) — the function must never return
    /// `Some` where `classify_node` returns `Eligible`, or vice versa; that
    /// invariant is exactly what `refusal_census_matches_classify_node` checks.
    pub(crate) fn classify_refusal(
        node: &EffectGraphNode,
        def: &EffectGraphDef,
        registry: &PrimitiveRegistry,
    ) -> Option<RefusalFamily> {
        use crate::node_graph::freeze::classify::{BoundaryReason, FusionKind};

        if node.type_id == SOURCE_TYPE_ID || node.type_id == FINAL_OUTPUT_TYPE_ID {
            return None; // graph endpoints: seams by identity, not a refusal
        }
        let Some(n) = configured_construct(registry, node) else {
            return Some(RefusalFamily::Other); // unknown atom
        };
        if !matches!(
            n.fusion_kind(),
            FusionKind::Pointwise | FusionKind::MultiInputCoincident | FusionKind::Source
        ) {
            return Some(match n.boundary_reason() {
                Some(BoundaryReason::Blocked) => RefusalFamily::BufferIndexShaped,
                _ => RefusalFamily::Other,
            });
        }
        if n.wgsl_body().is_none() {
            return Some(RefusalFamily::Other);
        }
        if n.fusion_register_heavy() {
            return Some(RefusalFamily::Other);
        }
        for p in n.parameters() {
            // Mirrors `classify_node`'s narrowed param gate exactly (P5/D4):
            // Vec3/Vec4/Color now pass via `param_is_fusable`; only
            // Table/String still bucket as `ParamType`. Must stay in
            // lockstep with `classify_node` — `refusal_census_matches_
            // classify_node` enforces it.
            if !crate::node_graph::freeze::codegen::param_is_fusable(p) {
                return Some(RefusalFamily::ParamType);
            }
        }
        if n.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))) {
            // Buffer-domain gates (classify_buffer_node): BufferFanOut is a
            // REGION-level refusal (`build_region`'s "fan-out buffer region"
            // error, counted in `region_level_refusals` below), never a
            // per-node one — a node-level buffer refusal here is always Other.
            return match classify_buffer_node(n.as_ref(), node, def, registry) {
                NodeClass::Boundary => Some(RefusalFamily::Other),
                NodeClass::Eligible => None,
            };
        }
        let tex_in = n.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
        let tex_out = n.outputs().iter().filter(|o| is_texture_port(&o.ty)).count();
        let arity_ok = if matches!(n.fusion_kind(), FusionKind::Source) {
            tex_in == 0
        } else {
            tex_in >= 1
        };
        // Mirrors `classify_node`'s narrowed cut rule 6 exactly (D4/P6): a
        // MULTI-output atom (tex_out >= 2) is no longer a refusal by itself —
        // every atom that declares ≥2 texture outputs on the codegen path is
        // already struct-return, so `RefusalFamily::MultiOutput` now only
        // fires for the true arity mismatch (wrong texture-INPUT count for
        // the atom's FusionKind, or zero texture outputs at all — no register
        // to thread). Kept as its own bucket (not folded into `Arity`) so a
        // future non-struct-return multi-output atom would still show up
        // distinctly if the authoring contract is ever violated — but no atom
        // shipped today reaches it (`refusal_census_matches_classify_node`
        // enforces the lockstep either way).
        if !arity_ok || tex_out == 0 {
            return Some(RefusalFamily::Arity);
        }
        let default_params: crate::node_graph::effect_node::ParamValues = AHashMap::default();
        for o in n.outputs().iter().filter(|o| is_texture_port(&o.ty)) {
            if let Some(scale) = n.output_canvas_scale(o.name.as_ref(), &default_params)
                && scale != (1, 1)
            {
                return Some(RefusalFamily::Resample);
            }
        }
        if n.inputs().iter().any(|i| i.ty == PortType::Texture3D)
            || n.outputs().iter().any(|o| o.ty == PortType::Texture3D)
        {
            return Some(RefusalFamily::Other);
        }
        let tex_ports: AHashSet<&str> = n
            .inputs()
            .iter()
            .filter(|i| is_texture_port(&i.ty))
            .map(|i| i.name.as_ref())
            .collect();
        let scalar_params: AHashSet<&str> = n
            .parameters()
            .iter()
            .filter(|p| param_wgsl_type(p).is_ok())
            .map(|p| p.name.as_ref())
            .collect();
        let camera_ports: AHashSet<&str> = if n.derived_uniforms().is_empty() {
            AHashSet::default()
        } else {
            n.inputs()
                .iter()
                .filter(|i| matches!(i.ty, PortType::Camera | PortType::Object))
                .map(|i| i.name.as_ref())
                .collect()
        };
        // D3 exemption mirror (BUG-114) — must match `classify_node`'s own
        // `buffer_index_ports` set exactly, or a `BufferIndex`-tagged Array
        // wire (draw_dots' `detections`) wrongly buckets `Other` here once
        // the param gate above (P5) no longer stops execution before this
        // wire loop. Found by `refusal_census_matches_classify_node` going
        // red the moment P5's param-gate narrowing made this code reachable
        // for draw_dots for the first time.
        let buffer_index_ports: AHashSet<&str> = {
            let tex_count = n.inputs().iter().filter(|i| is_texture_port(&i.ty)).count();
            n.inputs()
                .iter()
                .filter(|i| matches!(i.ty, PortType::Array(_)))
                .enumerate()
                .filter(|(idx, _)| {
                    n.input_access().get(tex_count + idx) == Some(&InputAccess::BufferIndex)
                })
                .map(|(_, i)| i.name.as_ref())
                .collect()
        };
        for w in &def.wires {
            if w.to_node == node.id
                && !tex_ports.contains(w.to_port.as_str())
                && !scalar_params.contains(w.to_port.as_str())
                && !camera_ports.contains(w.to_port.as_str())
                && !buffer_index_ports.contains(w.to_port.as_str())
            {
                return Some(RefusalFamily::Other);
            }
            if w.from_node == node.id && is_scalar_param_wire(def, registry, w) {
                return Some(RefusalFamily::Other);
            }
        }
        if !q16_tier_enabled()
            && node_on_cycle(node.id, def)
            && !node.output_formats.values().any(|s| s.contains("32float"))
        {
            return Some(RefusalFamily::Other);
        }
        if !node.output_formats.values().any(|s| s.contains("32float"))
            && cycle_contains_array(node.id, def, registry)
        {
            return Some(RefusalFamily::Other);
        }
        for (_, sp_param) in n.wgsl_specialization() {
            if param_is_binding_target(node, sp_param, def)
                || def.wires.iter().any(|w| w.to_node == node.id && w.to_port == *sp_param)
            {
                return Some(RefusalFamily::Other);
            }
        }
        let Some(body) = substituted_body(n.as_ref(), node) else {
            return Some(RefusalFamily::Other);
        };
        let standalone = crate::node_graph::freeze::codegen::generate_standalone(
            &crate::node_graph::freeze::codegen::StandaloneKernelSpec {
                fusion_kind: n.fusion_kind(),
                body: &body,
                inputs: n.inputs(),
                params: n.parameters(),
                input_access: n.input_access(),
                derived_uniforms: n.derived_uniforms(),
                outputs: n.outputs(),
                stencil_fetch: n.stencil_fetch(),
                includes: n.wgsl_includes(),
            },
        );
        match standalone {
            Ok(kernel) if naga::front::wgsl::parse_str(&kernel).is_ok() => None,
            _ => Some(RefusalFamily::Other),
        }
    }

    /// Replicates `explain_preset`'s union-candidate/convexity replay to find
    /// every eligible↔eligible connected component of size ≥2 and the
    /// `build_region` verdict for each — the same computation `explain_preset`
    /// prints per-COMPONENT, factored out so the census can bucket the ERROR
    /// strings by family instead of just eprintln-ing them.
    fn component_build_results(
        def: &EffectGraphDef,
        registry: &PrimitiveRegistry,
    ) -> Vec<(Vec<u32>, Result<(), &'static str>)> {
        let spaces = resolve_output_spaces(def, registry);
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
                    && (is_texture_wire(def, registry, w) || is_array_wire(def, registry, w))
                    && wire_coincident_consumed(def, registry, w)
                    && (is_array_wire(def, registry, w)
                        || space_of(spaces.as_ref(), w.from_node, &w.from_port)
                            == node_output_space(spaces.as_ref(), def, registry, w.to_node))
            })
            .map(|w| (w.from_node, w.to_node))
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        // D4/P6: mirrors `partition_regions`' gather-bridge guard (see its
        // comment) — must stay in lockstep or this test helper would report a
        // component the real algorithm no longer forms.
        let gather_pairs: Vec<(u32, u32)> = def
            .wires
            .iter()
            .filter(|w| {
                eligible.contains(&w.from_node)
                    && eligible.contains(&w.to_node)
                    && is_texture_wire(def, registry, w)
                    && !wire_coincident_consumed(def, registry, w)
            })
            .map(|w| (w.from_node, w.to_node))
            .collect();
        let mut uf = UnionFind::new(&eligible);
        for (a, b) in candidates {
            if uf.find(a) == uf.find(b) {
                continue;
            }
            let finds: AHashMap<u32, u32> = eligible.iter().map(|&n| (n, uf.find(n))).collect();
            let (ra, rb) = (uf.find(a), uf.find(b));
            let key = |n: u32| -> u32 {
                match finds.get(&n) {
                    Some(&r) if r == rb => ra,
                    Some(&r) => r,
                    None => n,
                }
            };
            let would_bridge_a_gather_wire =
                gather_pairs.iter().any(|&(gp, gc)| key(gp) == key(gc));
            if !collapsed_has_cycle(&forward, &key) && !would_bridge_a_gather_wire {
                uf.union(a, b);
            }
        }
        let mut components: AHashMap<u32, Vec<u32>> = AHashMap::default();
        for &id in &eligible {
            components.entry(uf.find(id)).or_default().push(id);
        }
        let final_reachable = final_reachable_nodes(def);
        let mut out = Vec::new();
        for (_, mut nodes) in components {
            nodes.sort_unstable();
            if nodes.len() < 2 {
                continue;
            }
            let result = build_region(def, registry, &nodes, &final_reachable, spaces.as_ref()).map(|_| ());
            out.push((nodes, result));
        }
        out
    }

    /// Bucket every refusal in one (already-flattened) def into `stats`. Three
    /// passes: (1) per-node `classify_refusal`, with a conservative
    /// lower-bound dispatches-saved estimate (documented below); (2)
    /// region-level stencil-depth (a gather producer that is itself Eligible,
    /// sits in a fused region of ≥2 members, and was NOT absorbed — the exact
    /// `MAX_VIRTUAL_CHAIN=1` cut); (3) region-level `build_region` drops
    /// (fan-out named explicitly, everything else Other).
    fn census_def(def: &EffectGraphDef, registry: &PrimitiveRegistry, stats: &mut AHashMap<RefusalFamily, FamilyStats>) {
        let regions = partition_regions(def, registry);
        let member_of: AHashMap<u32, usize> = regions
            .iter()
            .enumerate()
            .flat_map(|(i, r)| r.members.iter().map(move |m| (m.doc_id, i)))
            .collect();

        // Pass 1 — per-node refusals. Dispatches-saved estimator: a
        // CONSERVATIVE LOWER BOUND, documented in docs/fusion_census.md — 1 if
        // the refused node has at least one texture/Array-wired neighbour
        // (either direction) that classifies `Eligible`, else 0. That single
        // neighbour is the case lifting this node would join it to (2
        // dispatches collapse to 1); it does not credit bridging two existing
        // regions together or a chain of >1 newly-eligible neighbours, so the
        // real saving from lifting a whole family is >= what's reported here.
        for n in &def.nodes {
            let Some(family) = classify_refusal(n, def, registry) else { continue };
            let has_eligible_neighbor = def.wires.iter().any(|w| {
                let (other, this_is_from) = if w.from_node == n.id {
                    (w.to_node, true)
                } else if w.to_node == n.id {
                    (w.from_node, false)
                } else {
                    return false;
                };
                let _ = this_is_from;
                (is_texture_wire(def, registry, w) || is_array_wire(def, registry, w))
                    && def
                        .nodes
                        .iter()
                        .find(|o| o.id == other)
                        .is_some_and(|on| classify_node(on, def, registry) == NodeClass::Eligible)
            });
            let e = stats.entry(family).or_default();
            e.refusals += 1;
            e.dispatches_saved += usize::from(has_eligible_neighbor);
        }

        // Pass 2 — stencil-depth: a Gather-consumed external producer that is
        // ITSELF Eligible and sits in a region of >=2 members (i.e. its chain
        // is strictly longer than MAX_VIRTUAL_CHAIN=1) and wasn't absorbed.
        // Saved estimate: 1 (the consumer's own dispatch the fetch would fold
        // away, on top of whatever the chain's own region already fuses).
        for r in &regions {
            for m in &r.members {
                for (idx, (input, access)) in m.inputs.iter().zip(&m.input_access).enumerate() {
                    if *access != InputAccess::Gather {
                        continue;
                    }
                    let RegionInput::External(e) = input else { continue };
                    let prod = r.externals[*e].from_node;
                    let Some(&prod_region) = member_of.get(&prod) else { continue };
                    if regions[prod_region].members.len() < 2 {
                        continue; // single-node chain — MAX_VIRTUAL_CHAIN=1 admits it; not this family
                    }
                    let absorbed = r
                        .virtual_chains
                        .iter()
                        .any(|vc| vc.consumer == m.doc_id && vc.input_index == idx);
                    if absorbed {
                        continue;
                    }
                    let e_ = stats.entry(RefusalFamily::StencilDepth).or_default();
                    e_.refusals += 1;
                    e_.dispatches_saved += 1;
                }
            }
        }

        // Pass 3 — region-level `build_region` drops (fan-out named
        // explicitly; every other drop reason is Other — space mismatch, dead
        // region, escaping-to-dead-consumer, cycle).
        for (nodes, result) in component_build_results(def, registry) {
            let Err(reason) = result else { continue };
            let family = if reason.contains("fan-out buffer region") {
                RefusalFamily::BufferFanOut
            } else {
                RefusalFamily::Other
            };
            let e = stats.entry(family).or_default();
            e.refusals += 1;
            e.dispatches_saved += nodes.len().saturating_sub(1);
        }
    }

    /// Run the census over every bundled effect/generator preset plus the
    /// Liveschool fixture's `embedded_presets` (per-instance forked/edited
    /// graphs — the one place a real show's graphs diverge from the catalog
    /// canonical defs), and render the FUSION_SOTA_DESIGN §D4 report. Returns
    /// the exact text committed to `docs/fusion_census.md`.
    fn build_census_report() -> String {
        let registry = PrimitiveRegistry::with_builtin();
        let mut stats: AHashMap<RefusalFamily, FamilyStats> = AHashMap::default();
        let mut preset_count = 0usize;
        let mut liveschool_count = 0usize;
        let mut liveschool_note = String::from("NOT FOUND (gitignored fixture; skipped)");

        let mut census_json = |json: &str| {
            if let Ok(def) = serde_json::from_str::<manifold_core::effect_graph_def::EffectGraphDef>(json)
                && let Ok(flat) = manifold_core::flatten::flatten_groups(&def)
            {
                census_def(&flat, &registry, &mut stats);
            }
        };

        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Effect,
        ) {
            if let Some(view) = crate::node_graph::loaded_preset_view_by_id(&type_id) {
                let json = serde_json::to_string(&view.canonical_def).unwrap();
                census_json(&json);
                preset_count += 1;
            }
        }
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Generator,
        ) {
            if let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id) {
                census_json(&json);
                preset_count += 1;
            }
        }

        // Liveschool fixture — resolved via the git-common-dir trick
        // (`manifold-io/tests/load_project.rs`'s `fixture_path`) so this runs
        // from a worktree checkout, which doesn't contain the gitignored
        // `.manifold` fixtures itself.
        match liveschool_fixture_path() {
            None => {
                eprintln!("[census] Liveschool fixture not found on disk — skipping");
            }
            Some(path) => match manifold_io::loader::load_project(&path) {
                Err(e) => {
                    eprintln!("[census] Liveschool fixture found but failed to load: {e:?}");
                    liveschool_note = format!("load FAILED: {e:?}");
                }
                Ok(project) => {
                    for embedded in &project.embedded_presets {
                        let Ok(flat) = manifold_core::flatten::flatten_groups(&embedded.def) else {
                            continue;
                        };
                        census_def(&flat, &registry, &mut stats);
                        liveschool_count += 1;
                    }
                    liveschool_note = format!(
                        "{liveschool_count} embedded_presets censused (of {} total in the project)",
                        project.embedded_presets.len()
                    );
                }
            },
        }

        let mut out = String::new();
        out.push_str("# Fusion refusal census (FUSION_SOTA P3 / D4)\n\n");
        out.push_str(
            "Generated by `crates/manifold-renderer/src/node_graph/freeze/region.rs`'s \
             `audit::build_census_report` (invoked by the `#[ignore]`d test \
             `audit::refusal_census`). Regenerate with:\n\n```\ncargo test -p manifold-renderer \
             --lib node_graph::freeze::region::audit::refusal_census -- --ignored --nocapture\n```\n\n",
        );
        out.push_str(&format!(
            "Corpus: {preset_count} bundled effect/generator presets + Liveschool fixture \
             ({liveschool_note}).\n\n",
        ));
        if liveschool_count == 0 {
            out.push_str(
                "**Liveschool note:** the fixture loaded successfully but has ZERO \
                 `embedded_presets` — this real show never forked/saved a preset to the project \
                 (PROJECT_LIBRARY_DESIGN's \"fork\" mechanism), so every card it plays IS one of \
                 the canonical bundled defs above, differing only by param VALUES, which don't \
                 change fusion topology. The bundled-preset census already covers Liveschool's \
                 graph shapes; the fixture run adds no additional structural coverage this time, \
                 but stays wired so a future show that DOES fork a preset gets censused too.\n\n",
            );
        }
        out.push_str(
            "**Dispatches-saved estimator (documented, not hand-waved):** a CONSERVATIVE LOWER \
             BOUND. Per-node families (param-type/arity/multi-output/resample/buffer-index-shaped/\
             other): 1 dispatch saved if the refused node has >=1 texture/Array-wired neighbour \
             that already classifies `Eligible` (the node it would join, collapsing 2 dispatches \
             into 1), else 0 — a lone refused node with no eligible neighbour would form no region \
             even if lifted, so it correctly counts 0. This undercounts bridging (a lifted node \
             connecting two existing regions into one saves MORE than 1) and chains of newly-eligible \
             neighbours. Stencil-depth: 1 per (region, gather-member) pair whose producer is Eligible, \
             sits in a region of >=2 members (the exact MAX_VIRTUAL_CHAIN=1 cut), and wasn't absorbed \
             — the consumer's own dispatch the fetch would fold away, on top of what the chain's own \
             region already fuses. Buffer-fan-out / other region-level drops: component size - 1 (the \
             component renders as N separate dispatches today; fused it would be 1).\n\n",
        );
        out.push_str("| Family | Refusals | Dispatches saved if lifted |\n|---|---:|---:|\n");
        let mut rows: Vec<(RefusalFamily, FamilyStats)> = stats.into_iter().collect();
        rows.sort_by_key(|(f, _)| *f);
        for (family, s) in &rows {
            out.push_str(&format!("| {} | {} | {} |\n", family.label(), s.refusals, s.dispatches_saved));
        }
        out.push('\n');

        let get = |f: RefusalFamily| rows.iter().find(|(rf, _)| *rf == f).map(|(_, s)| *s).unwrap_or_default();
        let fan_out = get(RefusalFamily::BufferFanOut);
        let resample = get(RefusalFamily::Resample);
        let vec3 = get(RefusalFamily::ParamType);
        let multi = get(RefusalFamily::MultiOutput);

        out.push_str("## Reading the numbers against D4's defaults\n\n");
        out.push_str(&format!(
            "**Vec3 params — LIFT (default stands).** `param-type` bucket also includes Table/String \
             (boundary by nature per D4, not debt) so this count is an upper bound on Vec3-specifically; \
             {} refusal(s) / {} dispatch(es) potentially saved support building the lift as scoped for P5.\n\n",
            vec3.refusals, vec3.dispatches_saved
        ));
        out.push_str(&format!(
            "**Multi-output texture atoms — LIFT, census-gated (default stands).** {} refusal(s) / {} \
             dispatch(es). {}\n\n",
            multi.refusals,
            multi.dispatches_saved,
            if multi.refusals == 0 {
                "Zero shipped-preset cuts from this family — per D4, it still lands as capability \
                 (voronoi's cell+distance vocabulary) but ordered LAST of the lifts, exactly the \
                 documented fallback."
            } else {
                "Non-zero — P6 has shipped-preset cuts to point at, strengthening (not just \
                 capability-justifying) the case to build it."
            }
        ));
        out.push_str(&format!(
            "**Buffer fan-out — DEFER (trigger: >=3 refusals across shipped content). Measured: {} \
             refusal(s) / {} dispatch(es) potentially saved.** {}\n\n",
            fan_out.refusals,
            fan_out.dispatches_saved,
            if fan_out.refusals >= 3 {
                "**TRIGGER CROSSED — see ESCALATION in the P3 phase report; this default is NOT \
                 changed here, only flagged.**"
            } else {
                "Below the trigger — DEFER stands as-is."
            }
        ));
        out.push_str(&format!(
            "**Resample-into-region — DEFER (trigger: evidence of hot resample-sandwich chains). \
             Measured: {} refusal(s) / {} dispatch(es) potentially saved.** {}\n\n",
            resample.refusals,
            resample.dispatches_saved,
            "This census counts STRUCTURAL refusal COUNT, not runtime hotness (dispatch frequency \
             isn't visible to a static def walk) — a nonzero count is not itself the trigger; the \
             trigger is evidence of a HOT sandwich, which needs a runtime profile, not this census. \
             DEFER stands as-is from this data alone.",
        ));
        out.push_str(
            "**Nested stencils (`MAX_VIRTUAL_CHAIN=1`) — CORRECT AS-IS, not a D4-revisable family; \
             the `stencil-depth` row above is diagnostic only (D4 explicitly rejects raising the cap \
             without a measured per-region cost comparison this census cannot do).**\n",
        );
        out
    }

    /// Resolves the Liveschool fixture path, worktree-safe (git-common-dir
    /// trick — same as `manifold-io/tests/load_project.rs`'s `fixture_path`,
    /// since this crate's `.manifold` fixtures are gitignored and absent from
    /// a `git worktree` checkout).
    fn liveschool_fixture_path() -> Option<std::path::PathBuf> {
        const NAME: &str = "Liveschool Live Show V6 LEDS.manifold";
        if let Ok(out) = std::process::Command::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            && out.status.success()
            && let Ok(common) =
                std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).canonicalize()
            && let Some(main_root) = common.parent()
        {
            let candidate = main_root.join("tests/fixtures").join(NAME);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("../../tests/fixtures");
        p.push(NAME);
        if p.exists() { Some(p) } else { None }
    }

    #[test]
    #[ignore = "on-demand refusal census (FUSION_SOTA P3/D4) — writes docs/fusion_census.md"]
    fn refusal_census() {
        let report = build_census_report();
        eprintln!("{report}");
        let doc_path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/fusion_census.md"));
        std::fs::write(&doc_path, &report).expect("write docs/fusion_census.md");
        eprintln!("[census] wrote {}", doc_path.display());
    }

    /// The invariant `classify_refusal` must never violate: it agrees with
    /// `classify_node` on every node of every bundled preset — `Some(_)` iff
    /// `Boundary`, `None` iff `Eligible`. Runs the (cheap, GPU-free) full
    /// library sweep, so a future classify_node gate added without updating
    /// this file's replica fails LOUD, not as a silently wrong census number.
    #[test]
    fn refusal_census_matches_classify_node() {
        let registry = PrimitiveRegistry::with_builtin();
        let mut checked = 0usize;
        let mut defs: Vec<manifold_core::effect_graph_def::EffectGraphDef> = Vec::new();
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Effect,
        ) {
            if let Some(view) = crate::node_graph::loaded_preset_view_by_id(&type_id) {
                defs.push((*view.canonical_def).clone());
            }
        }
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Generator,
        ) {
            if let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id)
                && let Ok(def) = serde_json::from_str(&json)
            {
                defs.push(def);
            }
        }
        for def in &defs {
            let Ok(flat) = manifold_core::flatten::flatten_groups(def) else { continue };
            for n in &flat.nodes {
                // Graph endpoints are a documented exception: `classify_node`
                // reports them Boundary (they're seams by identity), but
                // `classify_refusal` reports `None` on purpose — a
                // source/final_output isn't a "refusal" in the census sense
                // (there is no lift that would ever fuse a graph's own
                // endpoints away), so it must never inflate the `Other` bucket.
                if n.type_id == SOURCE_TYPE_ID || n.type_id == FINAL_OUTPUT_TYPE_ID {
                    continue;
                }
                let class = classify_node(n, &flat, &registry);
                let refusal = classify_refusal(n, &flat, &registry);
                match (&class, &refusal) {
                    (NodeClass::Eligible, None) | (NodeClass::Boundary, Some(_)) => {}
                    _ => panic!(
                        "classify_refusal disagrees with classify_node on node {} ({}): class={class:?} refusal={refusal:?}",
                        n.id, n.type_id
                    ),
                }
                checked += 1;
            }
        }
        assert!(checked > 100, "sweep too small to trust ({checked} nodes) — bundled preset enumeration broke");
    }

    #[test]
    #[ignore = "on-demand per-preset fusion WHY report"]
    fn explain_presets() {
        let registry = PrimitiveRegistry::with_builtin();
        for name in ["MetallicGlass", "OilyFluid", "FluidSim2D"] {
            let type_id = manifold_core::PresetTypeId::new(name);
            if let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id) {
                explain_preset(name, &json, &registry);
            } else {
                eprintln!("=== {name}: NO BUNDLED JSON ===");
            }
        }
        // Effect presets live in the loaded-view registry, not bundled_preset_json.
        for name in ["DepthOfField", "Watercolor", "Bloom"] {
            let type_id = manifold_core::PresetTypeId::new(name);
            if let Some(view) = crate::node_graph::loaded_preset_view_by_id(&type_id) {
                let json = serde_json::to_string(&view.canonical_def).unwrap();
                explain_preset(name, &json, &registry);
            } else {
                eprintln!("=== {name}: NO LOADED VIEW ===");
            }
        }
    }

    #[test]
    #[ignore = "on-demand whole-library fusion report, not a pass/fail gate (~40s)"]
    fn audit_all_presets() {
        let registry = PrimitiveRegistry::with_builtin();
        eprintln!("=== EFFECT PRESETS ===");
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect) {
            if let Some(view) = crate::node_graph::loaded_preset_view_by_id(&type_id) {
                let json = serde_json::to_string(&view.canonical_def).unwrap();
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
