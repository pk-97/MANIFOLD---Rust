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
//!    a non-member — read once as `src_e`) and its *output* (the member whose
//!    texture output leaves the region). Conservative gates (single output,
//!    length ≥ 2) skip anything the v1 codegen can't yet express — left unfused,
//!    never miscompiled.
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
/// read, and the single member whose output leaves the region.
#[derive(Debug, Clone)]
pub struct Region {
    /// Members in topological order (every `Member` input refers to an earlier
    /// entry). The fused kernel evaluates them in this order, threading registers.
    pub members: Vec<RegionMember>,
    /// External inputs, indexed by the slot a [`RegionInput::External`] names.
    pub externals: Vec<ExternalRef>,
    /// The member whose texture output is consumed outside the region (feeds a
    /// boundary or `final_output`). v1 is single-output; a region with several
    /// escaping members is dropped (multi-output codegen is a follow-on).
    pub output: u32,
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

    // ── Grow connected components over texture wires between eligible nodes. ──
    // A texture wire eligible→eligible means the consumer can thread the
    // producer's register, so the two belong to one region. Union-find over
    // those edges; each set is a candidate region.
    // Only a COINCIDENT-consumed wire unions two atoms into one region: the
    // consumer threads the producer's register. A GATHER-consumed wire does NOT
    // union — a gather samples the whole texture at a coord it computes, which a
    // single register can't carry, so the gathered producer must stay external
    // (it becomes a bound `src_e` the body samples). This is what makes
    // gather-into-region safe: the gather atom fuses with its coincident inputs +
    // its downstream, but never swallows the texture it gathers.
    let mut uf = UnionFind::new(&eligible);
    for w in &def.wires {
        if eligible.contains(&w.from_node)
            && eligible.contains(&w.to_node)
            && is_texture_wire(def, registry, w)
            && wire_coincident_consumed(def, registry, w)
        {
            uf.union(w.from_node, w.to_node);
        }
    }
    let mut components: AHashMap<u32, Vec<u32>> = AHashMap::default();
    for &id in &eligible {
        components.entry(uf.find(id)).or_default().push(id);
    }

    // ── Build a region from each component; drop the ones v1 can't express. ──
    let mut regions: Vec<Region> = Vec::new();
    for (_, mut nodes) in components {
        nodes.sort_unstable();
        if let Some(region) = build_region(def, registry, &nodes) {
            regions.push(region);
        }
    }
    // Stable order across runs (components iterate in hash order otherwise).
    regions.sort_by_key(|r| r.members.first().map(|m| m.doc_id).unwrap_or(0));
    regions
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

    // No control wire into a non-texture port: a scalar/control wire (LFO →
    // gain.gain) would dangle when the node folds into the kernel — v1 cuts
    // rather than re-anchors it (re-anchoring a boundary-driven param as a
    // per-dispatch uniform is a documented follow-on, §11.B). A live PARAM
    // binding (slider/MIDI) is fine — it's retargeted onto the fused uniform.
    let tex_ports: AHashSet<&str> = n
        .inputs()
        .iter()
        .filter(|i| is_texture_port(&i.ty))
        .map(|i| i.name)
        .collect();
    for w in &def.wires {
        if w.to_node == node.id && !tex_ports.contains(w.to_port.as_str()) {
            return NodeClass::Boundary;
        }
    }

    NodeClass::Eligible
}

/// Assemble a [`Region`] from a connected component's node set, or `None` if it
/// fails a v1 expressibility gate (too short, multi-output, or an unresolvable
/// input — all left unfused).
fn build_region(def: &EffectGraphDef, registry: &PrimitiveRegistry, nodes: &[u32]) -> Option<Region> {
    if nodes.len() < MIN_REGION_LEN {
        return None;
    }
    let node_set: AHashSet<u32> = nodes.iter().copied().collect();

    // Topo-sort the members by intra-region texture wires so every Member input
    // refers to an earlier entry (the codegen threads registers in this order).
    let order = topo_sort(nodes, def, registry, &node_set)?;

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
            .filter(|i| is_texture_port(&i.ty))
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

    // The region output(s): members with ≥1 texture wire to a non-member. v1
    // supports exactly one; a region whose value escapes at several members
    // needs multi-output codegen — drop it (left unfused) for now.
    let mut outputs: Vec<u32> = nodes
        .iter()
        .copied()
        .filter(|&id| {
            def.wires
                .iter()
                .any(|w| w.from_node == id && !node_set.contains(&w.to_node))
        })
        .collect();
    outputs.sort_unstable();
    let output = match outputs.as_slice() {
        [single] => *single,
        _ => return None, // 0 = dead region; >1 = multi-output (follow-on)
    };

    Some(Region { members, externals, output })
}

/// Kahn topo-sort of a region's members by intra-region texture wires. `None` on
/// a cycle (feedback never appears in a pure region, but fail closed).
fn topo_sort(
    nodes: &[u32],
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    node_set: &AHashSet<u32>,
) -> Option<Vec<u32>> {
    let mut indeg: AHashMap<u32, u32> = nodes.iter().map(|&id| (id, 0)).collect();
    let mut adj: AHashMap<u32, Vec<u32>> = AHashMap::default();
    for w in &def.wires {
        if node_set.contains(&w.from_node)
            && node_set.contains(&w.to_node)
            && w.from_node != w.to_node
            && is_texture_wire(def, registry, w)
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
    let tex_idx = node
        .inputs()
        .iter()
        .filter(|i| is_texture_port(&i.ty))
        .position(|i| i.name == port);
    match tex_idx {
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
        assert_eq!(r.output, out_node, "clamp is the region output");
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
        assert_eq!(r1.output, 2, "contrast feeds the threshold");

        // Region 2: saturation(4) → clamp(5), reads the threshold, output = clamp.
        let r2 = &regions[1];
        assert_eq!(r2.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![4, 5]);
        assert_eq!(r2.externals.len(), 1, "region 2 reads the threshold output");
        assert_eq!(r2.externals[0].from_node, 3, "the threshold is region 2's external");
        assert_eq!(r2.output, 5, "clamp feeds final_output");
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
        assert_eq!(r.output, 2, "invert feeds final_output");
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
        assert_eq!(r.output, 2, "mix feeds final_output");
    }
}
