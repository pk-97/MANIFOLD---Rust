//! Step 4 — install fused regions into the live render path (design §12.2/§12.3).
//!
//! [`super::region::partition_regions`] is the finder: it splits a flattened
//! [`EffectGraphDef`] into its maximal pointwise/coincident regions, cutting at
//! every boundary. This module turns that partition into a *rendered* def — one
//! fused [`node.wgsl_compute`] kernel per region, wired back into the surviving
//! boundary nodes — and retargets the outer-card bindings onto the fused nodes.
//!
//! ## What it does
//!
//! Given an effect's canonical [`EffectGraphDef`], it:
//!
//! 1. **Partitions** it into regions ([`super::region`]). A region is a maximal
//!    run of register-threadable atoms; everything else (blur, warp/gather,
//!    feedback, DNN, resolution change, generators, control-wired params) is a
//!    boundary that bounds the regions around it. ColorGrade is the degenerate
//!    case — the whole card is one region — but an effect with a blur in the
//!    middle now fuses the pure runs on *both* sides of it.
//! 2. **Rewrites the def** (DD-A1 — a *definition* rewrite, not a `Graph`
//!    clone): every region's worker nodes + internal wires are deleted and
//!    replaced by ONE `node.wgsl_compute` node carrying the generated fused
//!    WGSL. Surviving boundary nodes carry over unchanged; each region's
//!    external producers are re-anchored onto the fused node's `src_<n>` inputs
//!    (read once) and its output onto the consumers the region used to feed.
//!    Because distinct regions are never directly texture-wired (such a wire
//!    would have merged them), every external/consumer is a surviving node, so
//!    the rewrite is local and the graph stays valid.
//! 3. **Retargets the bindings** (DD-A5): each outer-card slider that drove an
//!    inner node param (`gain.gain`, `colorize.focus`, …) is repointed at *its*
//!    region's fused node + namespaced uniform field (`n0_gain`, `n4_focus`, …);
//!    a slider driving a surviving boundary (a blur radius) is left untouched.
//!    The fused [`WgslCompute`] derives those as port-shadowed params from the
//!    uniform struct, so drivers / Ableton / LFOs keep writing them every frame
//!    (DD-A4: `var<uniform>`, never std430).
//!
//! The fused [`LoadedPresetView`] is cached `&'static` (built once per effect
//! type, exactly like [`crate::node_graph::loaded_preset_view_by_id`]), so the
//! per-frame chain rebuilds on resize don't leak.
//!
//! ## What it deliberately does NOT touch (DD-A6)
//!
//! - The **unfused** canonical view ([`crate::node_graph::loaded_preset_view_by_id`])
//!   stays the authoring + fallback surface. The graph editor reads it, so
//!   drilling into a fused effect still shows the original atoms. Only the chain
//!   *render* path swaps in the fused view, and only for the un-edited canonical
//!   preset — an effect with a per-instance graph override
//!   (`EffectInstance.graph = Some`) is rendered from the user's wiring,
//!   unfused, so editing stays live.
//! - This is "freeze = render-only binary, graph = source" (the §12 framing).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use ahash::{AHashMap, AHashSet};
use manifold_core::EffectTypeId;
use manifold_core::GeneratorTypeId;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef, EffectGraphNode,
    EffectGraphWire, SerializedParamValue,
};

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::codegen::{self, FusionRegion, InputSource, RegionNode};
use crate::node_graph::freeze::region::{RegionInput, partition_regions};
use crate::node_graph::parameters::{ParamDef, ParamValue};
use crate::node_graph::{
    EffectGraphDefExt, LoadedPresetView, ParamBinding, ParamTarget, PrimitiveRegistry, compile,
};

/// Whether fusion is enabled this process. Default ON — the freeze compiler is
/// the main render path (Peter's request). The `MANIFOLD_FREEZE` env var is the
/// v1 kill-switch: set it to `0` / `false` / `off` and relaunch to render every
/// effect unfused (the §12.3 step 7 "never fuse tonight" switch, restart-scoped
/// for now; a live hot-toggle is the step-7 follow-up). Read once and cached so
/// it's a process constant — no per-frame env lookup, no topology-hash churn.
pub fn freeze_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("MANIFOLD_FREEZE") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    })
}

/// Which kind of card the fusion gate is being asked about. Carries the type
/// id so [`should_render_fused`] can consult the matching per-device perf
/// verdict — `should_fuse` for effects, `should_fuse_generator` for generators.
pub enum FuseTarget<'a> {
    Effect(&'a EffectTypeId),
    Generator(&'a GeneratorTypeId),
}

/// The single home for the "render this card through its fused kernel?"
/// decision, shared by the effect chain build ([`crate::effect_chain_graph`])
/// and the generator registry ([`crate::generators::registry`]). Both paths
/// previously hand-maintained the same boolean shape; folding it here keeps the
/// watched-target override from drifting into a third copy.
///
/// The decision is: fuse only when fusion is enabled this process, the instance
/// isn't carrying a live editing override (its per-card graph is the canonical
/// one), it isn't the *watched* target (open in the graph editor — kept unfused
/// so per-node output preview can sample inner-node textures and edits render
/// live), and the device's perf gate kept the fused kernel for this type.
///
/// What stays per-path and is deliberately *not* unified: how the fused variant
/// is loaded (an effect `LoadedPresetView` spliced into the chain vs a generator
/// `EffectGraphDef` through `from_def`). This function is upstream of the
/// verdict logic — it only reads the existing verdicts, never recomputes them.
pub fn should_render_fused(target: FuseTarget<'_>, has_override: bool, is_watched: bool) -> bool {
    if !freeze_enabled() || has_override || is_watched {
        return false;
    }
    match target {
        FuseTarget::Effect(t) => crate::node_graph::freeze::perf_gate::should_fuse(t),
        FuseTarget::Generator(t) => {
            crate::node_graph::freeze::perf_gate::should_fuse_generator(t)
        }
    }
}

/// Look up the fused [`LoadedPresetView`] for an effect type, building the whole
/// map on first call and caching `&'static` for the process lifetime. Returns
/// `None` for any effect whose canonical graph has no fusable region (anything
/// that's all boundaries). Mirrors
/// [`crate::node_graph::loaded_preset_view_by_id`]'s lazy-cache shape.
pub fn fused_view_by_id(id: &EffectTypeId) -> Option<&'static LoadedPresetView> {
    static MAP: OnceLock<AHashMap<EffectTypeId, &'static LoadedPresetView>> = OnceLock::new();
    let map = MAP.get_or_init(build_fused_view_map);
    map.get(id).copied()
}

fn build_fused_view_map() -> AHashMap<EffectTypeId, &'static LoadedPresetView> {
    // One registry for the whole build — `construct(type_id)` reads each atom's
    // fusion kind / body / ports / param defaults off a fresh instance.
    let registry = PrimitiveRegistry::with_builtin();
    let mut m: AHashMap<EffectTypeId, &'static LoadedPresetView> = AHashMap::default();
    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids() {
        let Some(base) = crate::node_graph::loaded_preset_view_by_id(&type_id) else {
            continue;
        };
        if let Some(fused) = fuse_view(base, &registry) {
            m.insert(type_id, Box::leak(Box::new(fused)));
        }
    }
    m
}

/// Build a fused [`LoadedPresetView`] from a canonical one, or `None` if the
/// canonical graph has no fusable region. The fused view keeps the same
/// outer-card params + skip mode (so the chain builder's `outer_param_index` /
/// `n_static_slots` / skip logic are byte-identical) and swaps in the fused def
/// + retargeted bindings.
fn fuse_view(base: &LoadedPresetView, registry: &PrimitiveRegistry) -> Option<LoadedPresetView> {
    let fused = fuse_canonical_def(base.canonical_def, registry)?;
    // Node ids that survive the rewrite (boundaries + the fused nodes): a binding
    // targeting one of these is left as-is; one targeting a fused-away member is
    // retargeted; anything else strands a slider, so refuse to fuse.
    let surviving: AHashSet<String> = fused
        .def
        .nodes
        .iter()
        .map(|n| resolve_node_id(n).as_str().to_string())
        .collect();
    let bindings = retarget_bindings(base.bindings, &fused.retarget, &surviving)?;
    // The fused def must actually build (not just parse) — fall back to unfused otherwise.
    if !fused_def_builds(&fused.def, registry) {
        return None;
    }
    let def_static: &'static EffectGraphDef = Box::leak(Box::new(fused.def));
    Some(LoadedPresetView {
        type_id: base.type_id.clone(),
        canonical_def: def_static,
        bindings: Box::leak(bindings.into_boxed_slice()),
        skip_mode: base.skip_mode,
    })
}

// ===========================================================================
// Generator fusion. A generator preset is the SAME `EffectGraphDef` as an
// effect, but its live render path ([`JsonGraphGenerator::from_def`]) reads its
// modulation bindings straight from the def's `preset_metadata.bindings`
// (`BindingDef`s) rather than from a separate `LoadedPresetView.bindings` list.
// So fusing a generator means rewriting the def with fused kernels (the shared
// `fuse_canonical_def`) AND retargeting those `BindingDef`s onto the fused node —
// the generator analog of `retarget_bindings`. The fused generator def then loads
// through the unchanged `from_def` path, so a wired generator param keeps
// modulating after its atom folds into a kernel.
// ===========================================================================

/// Look up the fused generator def for a generator type, building + caching the
/// whole map on first call. `None` for any generator whose canonical graph has no
/// fusable region, or whose modulation bindings can't be retargeted (stranded) —
/// either way it renders unfused, always correct. Mirrors [`fused_view_by_id`].
pub fn fused_generator_def_by_id(id: &GeneratorTypeId) -> Option<&'static EffectGraphDef> {
    static MAP: OnceLock<AHashMap<GeneratorTypeId, &'static EffectGraphDef>> = OnceLock::new();
    let map = MAP.get_or_init(build_fused_generator_map);
    map.get(id).copied()
}

fn build_fused_generator_map() -> AHashMap<GeneratorTypeId, &'static EffectGraphDef> {
    use crate::generators::bundled_generator_presets::{
        bundled_generator_preset_json, bundled_generator_preset_type_ids,
    };
    let registry = PrimitiveRegistry::with_builtin();
    let mut m: AHashMap<GeneratorTypeId, &'static EffectGraphDef> = AHashMap::default();
    for type_id in bundled_generator_preset_type_ids() {
        let Some(json) = bundled_generator_preset_json(&type_id) else {
            continue;
        };
        let Ok(def) = serde_json::from_str::<EffectGraphDef>(json) else {
            continue;
        };
        if let Some(fused) = fuse_generator_def(&def, &registry) {
            m.insert(type_id, &*Box::leak(Box::new(fused)));
        }
    }
    m
}

/// Fuse a generator's canonical def + retarget its `preset_metadata.bindings`
/// onto the fused nodes. `None` if nothing fuses or a binding strands. The result
/// loads through the same `from_def` path as the unfused preset — only the def
/// changed.
pub fn fuse_generator_def(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<EffectGraphDef> {
    let fused = fuse_canonical_def(def, registry)?;
    // Node ids that survive (boundaries + fused nodes) — a binding targeting one
    // is left as-is; one targeting a fused-away member is retargeted; anything
    // else strands, so refuse to fuse (render unfused).
    let surviving: AHashSet<String> = fused
        .def
        .nodes
        .iter()
        .map(|n| resolve_node_id(n).as_str().to_string())
        .collect();
    let mut out_def = fused.def;
    if let Some(meta) = out_def.preset_metadata.as_mut() {
        meta.bindings = retarget_binding_defs(&meta.bindings, &fused.retarget, &surviving)?;
    }
    // The fused def must actually build (not just parse) — fall back to unfused otherwise.
    if !fused_def_builds(&out_def, registry) {
        return None;
    }
    Some(out_def)
}

/// Rewrite each `preset_metadata` `BindingDef` so it lands right after fusion: a
/// binding that drove a fused-away inner node is repointed at that node's fused
/// uniform field (`n{idx}_<param>`); one driving a surviving boundary is left
/// alone; one that hits neither strands modulation, so `None` (unfused fallback).
/// The generator twin of [`retarget_bindings`] — same routing, `BindingDef`
/// instead of `ParamBinding`.
fn retarget_binding_defs(
    bindings: &[BindingDef],
    retarget: &AHashMap<(String, String), (NodeId, String)>,
    surviving: &AHashSet<String>,
) -> Option<Vec<BindingDef>> {
    let mut out = Vec::with_capacity(bindings.len());
    for b in bindings {
        let mut nb = b.clone();
        if let BindingTarget::Node { node_id, param } = &b.target {
            let key = (node_id.as_str().to_string(), param.clone());
            if let Some((fused_id, field)) = retarget.get(&key) {
                nb.target = BindingTarget::Node {
                    node_id: fused_id.clone(),
                    param: field.clone(),
                };
            } else if !surviving.contains(node_id.as_str()) {
                return None; // stranded binding — refuse to fuse this generator
            }
            // else: drives a surviving boundary node — leave it exactly as-is.
        }
        // Composite targets route by outer name, never by a fused-away id.
        out.push(nb);
    }
    Some(out)
}

/// Rewrite each outer-card binding so it lands on the right place after fusion.
/// A binding that drove a fused-away inner node is repointed at that node's
/// region's fused uniform field; a binding driving a surviving boundary node is
/// left untouched; a binding that hits neither (a stranded slider) makes the
/// whole fusion unsafe — return `None` so the card renders unfused rather than
/// silently dropping live control.
fn retarget_bindings(
    base: &[ParamBinding],
    retarget: &AHashMap<(String, String), (NodeId, String)>,
    surviving: &AHashSet<String>,
) -> Option<Vec<ParamBinding>> {
    let mut out = Vec::with_capacity(base.len());
    for b in base {
        let mut nb = b.clone();
        if let ParamTarget::Node { node_id, param } = &b.target {
            let key = (node_id.as_str().to_string(), (*param).to_string());
            if let Some((fused_id, field)) = retarget.get(&key) {
                let field_static: &'static str = Box::leak(field.clone().into_boxed_str());
                nb.target = ParamTarget::Node {
                    node_id: fused_id.clone(),
                    param: field_static,
                };
            } else if !surviving.contains(node_id.as_str()) {
                // Neither retargeted nor surviving — a stranded slider. Refuse.
                return None;
            }
            // else: drives a surviving boundary node — leave it exactly as-is.
        }
        // Composite / Custom targets route by outer-name / fn pointer, never by a
        // fused-away inner node id, so they pass through unchanged.
        out.push(nb);
    }
    Some(out)
}

/// A canonical def rewritten with one fused node per region, plus the routing the
/// binding retarget needs. `pub(crate)` so the end-to-end oracle test can drive
/// both the unfused and fused graphs from one fixture (set inner params by stable
/// node id on the unfused side, by the `retarget`ed `(fused id, field)` on the
/// fused side).
pub(crate) struct FusedDef {
    pub def: EffectGraphDef,
    /// `(original stable node_id, original param) → (fused node id, fused uniform
    /// field)`. The field is `"n{idx}_{param}"` (`idx` = the member's topo index
    /// within its region — the codegen convention); the node id is that region's
    /// `fused_region_{i}`.
    pub retarget: AHashMap<(String, String), (NodeId, String)>,
}

/// Partition `def` into its fusable regions and rewrite it with one fused
/// `node.wgsl_compute` per region. Returns `None` (leave the card entirely
/// unfused) when nothing fuses. Conservative throughout: any inability to
/// express a region's params, body, or wiring aborts the whole rewrite.
pub(crate) fn fuse_canonical_def(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<FusedDef> {
    // The finder operates on a FLATTENED graph: `partition_regions` refuses any
    // def still carrying a group node (group boundary nodes would fragment every
    // region), and the live loader (`graph_loader`) flattens before building. So
    // flatten here too — otherwise a grouped preset (Glitch, FluidSimulation)
    // silently never fuses even though its flattened form has regions. Flatten
    // PRESERVES each node's stable `node_id` (only the debug handle is prefixed),
    // so the binding retarget downstream — which keys on `node_id` via
    // `resolve_node_id` — still lands correctly. An ungrouped def is returned
    // clone-equal (ids byte-identical), making this a no-op for the common case;
    // a malformed group def errors out to "render unfused", always safe.
    let flattened = manifold_core::flatten::flatten_groups(def).ok()?;
    let def = &flattened;
    let regions = partition_regions(def, registry);
    if regions.is_empty() {
        return None;
    }

    // Which region (if any) each fused-away member belongs to.
    let member_region: AHashMap<u32, usize> = regions
        .iter()
        .enumerate()
        .flat_map(|(i, r)| r.members.iter().map(move |m| (m.doc_id, i)))
        .collect();

    let max_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0);
    let mut new_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut retarget: AHashMap<(String, String), (NodeId, String)> = AHashMap::default();
    let mut fused_docs: Vec<u32> = Vec::with_capacity(regions.len());
    // Control wires re-anchored onto a fused node's port-shadow: (fused_doc,
    // producer node, producer port, `n{idx}_<param>` field). Emitted after the
    // texture rewrite so the producer (a surviving boundary) is already in place.
    let mut control_wires: Vec<(u32, u32, String, String)> = Vec::new();

    for (i, region) in regions.iter().enumerate() {
        // ── Build the codegen region from this component's members (topo order),
        // resolving each member's inputs to an external slot or an earlier
        // member's register. ──
        let mut region_nodes: Vec<RegionNode<'_>> = Vec::with_capacity(region.members.len());
        for member in &region.members {
            let doc_node = def.nodes.iter().find(|n| n.id == member.doc_id)?;
            let node = registry.construct(&doc_node.type_id)?;
            let body = node.wgsl_body()?;
            let inputs: Vec<InputSource> = member
                .inputs
                .iter()
                .map(|ri| match ri {
                    RegionInput::External(e) => InputSource::External(*e),
                    RegionInput::Member(doc) => InputSource::Node(NodeInstanceId(*doc)),
                })
                .collect();
            region_nodes.push(RegionNode {
                node_id: NodeInstanceId(member.doc_id),
                fusion_kind: node.fusion_kind(),
                body,
                params: leak_params(node.parameters()),
                inputs,
                input_access: member.input_access.clone(),
                // Leaked so the buffer codegen path can read each Array port's
                // element ChannelSpecs after `node` drops. Texture regions ignore
                // these. One-time at fuse-build, cached &'static, so the leak is bounded.
                node_inputs: leak_ports(node.inputs()),
                node_outputs: leak_ports(node.outputs()),
                node_includes: node.wgsl_includes(),
            });
        }
        let fusion_region = FusionRegion {
            nodes: region_nodes,
            num_external_inputs: region.externals.len(),
            outputs: region.outputs.iter().map(|&d| NodeInstanceId(d)).collect(),
        };
        let generated = codegen::generate_fused(&fusion_region).ok()?;
        // Defense in depth: the fused kernel must parse through the plain pipeline
        // compiler — the same `naga` front-end the live `WgslCompute` node uses. The
        // classify gate already keeps specialization / free-identifier atoms out of
        // regions, but two bodies could still collide at module scope (e.g. two
        // same-named consts with different values, which dedup can't merge). If the
        // kernel doesn't parse, leave the whole card unfused rather than ship a
        // node whose introspection silently fails back to its default shape.
        if naga::front::wgsl::parse_str(&generated.wgsl).is_err() {
            return None;
        }

        // ── Seed the fused node's params (def override else atom default) + the
        // retarget map. The field `n{idx}_{param}` matches the codegen's
        // region-topo-index convention. ──
        let fused_doc = max_id + 1 + i as u32;
        let fused_id = NodeId::new(format!("fused_region_{i}").as_str());
        let mut fused_params: BTreeMap<String, SerializedParamValue> = BTreeMap::new();
        for (idx, member) in region.members.iter().enumerate() {
            let doc_node = def.nodes.iter().find(|n| n.id == member.doc_id)?;
            let node = registry.construct(&doc_node.type_id)?;
            let stable = resolve_node_id(doc_node);
            for p in node.parameters() {
                let field = format!("n{idx}_{}", p.name);
                retarget.insert(
                    (stable.as_str().to_string(), p.name.to_string()),
                    (fused_id.clone(), field.clone()),
                );
                let value = effective_param_f32(doc_node.params.get(p.name), &p.default)?;
                fused_params.insert(field.clone(), SerializedParamValue::Float { value });

                // A control wire driving this param (LFO → gain.gain) is re-anchored
                // onto the fused node's port-shadow `n{idx}_<param>`, so the producer
                // keeps driving it every frame (DD-A5). The seeded value above is the
                // fallback the shadow port overrides. The producer is a control
                // producer and so a boundary (survives) — guard defensively.
                if let Some(cw) = def
                    .wires
                    .iter()
                    .find(|w| w.to_node == member.doc_id && w.to_port == p.name)
                {
                    if member_region.contains_key(&cw.from_node) {
                        return None; // producer fused away — can't route its scalar
                    }
                    control_wires.push((fused_doc, cw.from_node, cw.from_port.clone(), field));
                }
            }
        }

        new_nodes.push(EffectGraphNode {
            id: fused_doc,
            node_id: fused_id,
            // The dynamic-WGSL escape-hatch primitive — same stable type id the
            // preset JSON uses; it derives its ports/params from the source.
            type_id: "node.wgsl_compute".to_string(),
            handle: Some(format!("fused_region_{i}")),
            params: fused_params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: Some(generated.wgsl),
            title: Some(format!("Fused Region {i}")),
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        });
        fused_docs.push(fused_doc);
    }

    // ── Surviving (non-member) nodes carry over unchanged. ──
    for n in &def.nodes {
        if !member_region.contains_key(&n.id) {
            new_nodes.push(n.clone());
        }
    }

    // ── Rewire. Two distinct regions can only be directly texture-wired through a
    // GATHER (a coincident eligible→eligible wire would have merged them into one
    // region) — region A's output member is region B's gathered external. So an
    // external producer may itself be a fused-away member; `resolve_producer`
    // repoints it onto its region's fused `dst_<k>`. Output consumers are always
    // surviving nodes (a member consumer is the OTHER region's external, handled
    // from that side), so the output rewrite stays local. ──
    let mut new_wires: Vec<EffectGraphWire> = Vec::new();
    // Where a texture producer lands in the rewritten def: itself if it survived,
    // else its region's fused node at the dst slot for its output index. A
    // fused-away producer is always one of its region's outputs (it escaped the
    // region to be read here), so the slot lookup resolves — `?` bails to unfused
    // if that invariant is ever violated.
    let resolve_producer = |from_node: u32, from_port: &str| -> Option<(u32, String)> {
        match member_region.get(&from_node) {
            None => Some((from_node, from_port.to_string())),
            Some(&r) => {
                let producer = &regions[r];
                let k = producer.outputs.iter().position(|&o| o == from_node)?;
                let port = if producer.outputs.len() > 1 {
                    format!("dst_{k}")
                } else {
                    "dst".to_string()
                };
                Some((fused_docs[r], port))
            }
        }
    };
    // (a) surviving → surviving wires pass through.
    for w in &def.wires {
        if !member_region.contains_key(&w.from_node) && !member_region.contains_key(&w.to_node) {
            new_wires.push(w.clone());
        }
    }
    for (i, region) in regions.iter().enumerate() {
        let fused_doc = fused_docs[i];
        // (b) each external producer → the fused node's `src_<slot>` (read once,
        // even if several members read the same external — the finder deduped). A
        // producer that was itself fused away (cross-region gather) is repointed
        // onto its region's fused dst.
        for (e, ext) in region.externals.iter().enumerate() {
            let (from_node, from_port) = resolve_producer(ext.from_node, &ext.from_port)?;
            new_wires.push(EffectGraphWire {
                from_node,
                from_port,
                to_node: fused_doc,
                to_port: format!("src_{e}"),
            });
        }
        // (c) each region output → every consumer it fed. A single-output region
        // exposes the `dst` port (byte-identical to v1); a FAN-OUT region routes
        // each escaping member through its own `dst_<k>` (k = its index in
        // `region.outputs`, matching the codegen's binding order). The finder
        // already guaranteed every such consumer is a live surviving node, so each
        // `dst_<k>` lands on a texture the executor allocates.
        // Single-output regions (every buffer region, and the common texture
        // case) expose `dst`; a texture FAN-OUT region exposes `dst_<k>`. A buffer
        // region's fresh `// @fused_output` array is also named `dst`.
        let multi = region.outputs.len() > 1;
        for (k, &out_doc) in region.outputs.iter().enumerate() {
            let from_port = if multi { format!("dst_{k}") } else { "dst".to_string() };
            for w in &def.wires {
                if w.from_node == out_doc && !member_region.contains_key(&w.to_node) {
                    new_wires.push(EffectGraphWire {
                        from_node: fused_doc,
                        from_port: from_port.clone(),
                        to_node: w.to_node,
                        to_port: w.to_port.clone(),
                    });
                }
            }
        }
    }
    // (d) control wires: the surviving producer drives the fused node's port-shadow
    // `n{idx}_<param>`, so a graph-wired param (LFO → gain.gain) keeps modulating
    // after the atom folds into the kernel. WgslCompute shadows every uniform field
    // as an optional ScalarF32 input, and reads the wire when present (else the
    // seeded fallback), so this is a plain control wire onto the fused node.
    for (fused_doc, from_node, from_port, field) in control_wires {
        new_wires.push(EffectGraphWire {
            from_node,
            from_port,
            to_node: fused_doc,
            to_port: field,
        });
    }

    let fused_def = EffectGraphDef {
        version: EFFECT_GRAPH_VERSION_WITH_METADATA,
        name: def.name.clone(),
        description: def.description.clone(),
        // Keep the outer-card surface (params / skip / aliases) byte-identical so
        // the chain builder's outer_param_index + skip logic are unchanged.
        preset_metadata: def.preset_metadata.clone(),
        nodes: new_nodes,
        wires: new_wires,
    };

    Some(FusedDef { def: fused_def, retarget })
}

/// Defense in depth: a fused def must BUILD, not just contain valid WGSL. The
/// per-region naga-parse in [`fuse_canonical_def`] catches malformed shader text,
/// but a fused node can still be a well-formed shader the GRAPH compiler rejects
/// — e.g. a buffer region whose `var<storage, read_write>` output introspects as
/// a required-but-unwired aliased input port. The real entry points
/// ([`fuse_view`] / [`fuse_generator_def`]) run this on their final def and fall
/// back to unfused on any failure, so a def that can't build never installs.
/// (Not called from [`fuse_canonical_def`] itself — the install unit tests drive
/// it with synthetic fixtures that intentionally don't fully compile.) Runs once
/// at fuse-build (cached), so the cost is negligible.
fn fused_def_builds(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> bool {
    def.clone()
        .into_graph(registry)
        .map_err(|_| ())
        .and_then(|g| compile(&g).map_err(|_| ()))
        .is_ok()
}

/// A node's stable id defaults to its handle when the document carries none —
/// the same convention `instantiate_def` / the preset stamp use.
fn resolve_node_id(n: &EffectGraphNode) -> NodeId {
    if n.node_id.is_empty() {
        n.handle.as_deref().map(NodeId::new).unwrap_or_default()
    } else {
        n.node_id.clone()
    }
}

/// Effective scalar value for a region param: the def override if present, else
/// the atom's declared default. Every fused uniform field is f32 / i32 / u32
/// (the codegen maps Bool/Enum → u32 too), so all seed as a single f32 the
/// `WgslCompute` casts at the uniform-write boundary. `None` for a non-scalar
/// value (which the finder already rejected upstream — defensive).
fn effective_param_f32(
    override_val: Option<&SerializedParamValue>,
    default: &ParamValue,
) -> Option<f32> {
    if let Some(v) = override_val {
        return serialized_to_f32(v);
    }
    param_value_to_f32(default)
}

fn serialized_to_f32(v: &SerializedParamValue) -> Option<f32> {
    match v {
        SerializedParamValue::Float { value } => Some(*value),
        SerializedParamValue::Int { value } => Some(*value as f32),
        SerializedParamValue::Enum { value } => Some(*value as f32),
        SerializedParamValue::Bool { value } => Some(if *value { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn param_value_to_f32(v: &ParamValue) -> Option<f32> {
    match v {
        ParamValue::Float(f) => Some(*f),
        ParamValue::Enum(u) => Some(*u as f32),
        ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Leak a node's param-def slice to `'static`. The slice is already `&'static`
/// for converted atoms (the `primitive!` macro emits a `const`), but it's
/// borrowed through the boxed node — copy the slice out so the `RegionNode`
/// can hold it for the codegen call. Bounded leak (one per atom per fused view).
fn leak_params(params: &[ParamDef]) -> &'static [ParamDef] {
    let owned: Vec<ParamDef> = params.to_vec();
    Box::leak(owned.into_boxed_slice())
}

/// Leak a node's port defs to `&'static` so a [`RegionNode`] can carry them past
/// the constructed node's drop (the buffer codegen reads Array element specs from
/// them). One-time at fuse-build, the result is cached `&'static`, so bounded.
fn leak_ports(ports: &[crate::node_graph::ports::NodePort]) -> &'static [crate::node_graph::ports::NodePort] {
    let owned: Vec<crate::node_graph::ports::NodePort> = ports.to_vec();
    Box::leak(owned.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ParamTarget;
    use manifold_core::effect_graph_def::EffectGraphDef;

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    #[test]
    fn shared_gate_vetoes_fusion_for_override_and_watched_targets() {
        // The single home for the fuse-or-not decision. Both vetoes (a live
        // per-card override, and the watched open-in-editor target) must force
        // unfused regardless of the perf verdict — they short-circuit before it.
        // The effect and generator arms share this exact logic so the watched
        // override can't drift between the two render paths.
        let ty = EffectTypeId::new("ColorGrade");
        // Neither veto: fuses (freeze is on in this test binary; untuned perf is
        // optimistic, so the verdict is `true`).
        assert!(should_render_fused(FuseTarget::Effect(&ty), false, false));
        // A per-card graph override is the live editing surface → never fused.
        assert!(!should_render_fused(FuseTarget::Effect(&ty), true, false));
        // Watched (open in the editor) → never fused, so per-node preview can
        // sample inner-node textures and edits render live.
        assert!(!should_render_fused(FuseTarget::Effect(&ty), false, true));

        // Same contract on the generator arm — proves it's one decision, not two.
        let gty = GeneratorTypeId::new("DigitalPlants");
        assert!(!should_render_fused(FuseTarget::Generator(&gty), false, true));
        assert!(!should_render_fused(FuseTarget::Generator(&gty), true, false));
    }

    fn colorgrade_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/effect-presets/ColorGrade.json"
        ))
        .expect("read ColorGrade.json");
        serde_json::from_str(&json).expect("parse ColorGrade.json")
    }

    /// The whole ColorGrade card (7 atoms, one region) collapses to ONE
    /// `node.wgsl_compute` node between the retained boundaries, wired
    /// source → fused.src_0 → final_output. The retarget maps each inner
    /// (node_id, param) to its region's fused node + `n{i}_{param}` field — the
    /// load-bearing routing for the binding rewrite.
    #[test]
    fn colorgrade_fuses_to_single_wgsl_node() {
        let def = colorgrade_def();
        let fused = fuse_canonical_def(&def, &registry()).expect("ColorGrade fuses");

        // 3 nodes: source, fused, final_output. 2 wires.
        assert_eq!(fused.def.nodes.len(), 3, "boundaries + one fused node");
        let wgsl_nodes: Vec<_> = fused
            .def
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.wgsl_compute")
            .collect();
        assert_eq!(wgsl_nodes.len(), 1, "exactly one fused node");
        assert!(wgsl_nodes[0].wgsl_source.is_some(), "fused node carries WGSL");
        assert_eq!(fused.def.wires.len(), 2, "source→fused, fused→final_output");
        assert!(
            fused.def.wires.iter().any(|w| w.to_port == "src_0"),
            "an input wire targets the fused src_0 port"
        );
        assert!(
            fused.def.wires.iter().any(|w| w.from_port == "dst"),
            "the fused output wire leaves the dst port"
        );

        // Region topo order: gain(0) sat(1) hue(2) contrast(3) colorize(4)
        // mix(5) clamp(6). Spot-check the routing the binding rewrite depends on.
        let field_of = |nid: &str, p: &str| {
            fused
                .retarget
                .get(&(nid.into(), p.into()))
                .map(|(_, f)| f.clone())
        };
        assert_eq!(field_of("gain", "gain").as_deref(), Some("n0_gain"));
        assert_eq!(field_of("saturation", "saturation").as_deref(), Some("n1_saturation"));
        assert_eq!(field_of("hue", "hue").as_deref(), Some("n2_hue"));
        assert_eq!(field_of("contrast", "contrast").as_deref(), Some("n3_contrast"));
        assert_eq!(field_of("colorize", "focus").as_deref(), Some("n4_focus"));
        assert_eq!(field_of("grade_mix", "amount").as_deref(), Some("n5_amount"));
        assert_eq!(field_of("clamp", "max").as_deref(), Some("n6_max"));
        // 14 inner params across the 7 atoms (1+1+3+1+4+2+2).
        assert_eq!(fused.retarget.len(), 14);
        // All routed onto the single region's fused node.
        for (fused_id, _) in fused.retarget.values() {
            assert_eq!(fused_id.as_str(), "fused_region_0");
        }
    }

    /// A true boundary in the middle splits the card into TWO fused nodes — the
    /// headline generalisation past whole-card fusion. source → gain → contrast
    /// → threshold(boundary) → saturation → clamp → final rewrites to
    /// source → fused_region_0 → threshold → fused_region_1 → final_output. (A
    /// gather like gaussian_blur would instead fold IN — see the region tests.)
    #[test]
    fn boundary_splits_into_two_fused_nodes() {
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
        let fused = fuse_canonical_def(&def, &registry()).expect("two regions fuse");

        // Nodes: source, fused_region_0, threshold, fused_region_1, final_output.
        let wgsl_nodes: Vec<_> = fused
            .def
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.wgsl_compute")
            .collect();
        assert_eq!(wgsl_nodes.len(), 2, "two fused regions");
        assert!(
            fused.def.nodes.iter().any(|n| n.type_id == "node.threshold"),
            "the threshold boundary survives between the two fused nodes"
        );

        // Routing: gain/contrast → fused_region_0; sat/clamp → fused_region_1.
        let region_of = |nid: &str, p: &str| {
            fused
                .retarget
                .get(&(nid.into(), p.into()))
                .map(|(id, _)| id.as_str().to_string())
        };
        assert_eq!(region_of("gain", "gain").as_deref(), Some("fused_region_0"));
        assert_eq!(region_of("contrast", "contrast").as_deref(), Some("fused_region_0"));
        assert_eq!(region_of("sat", "saturation").as_deref(), Some("fused_region_1"));
        assert_eq!(region_of("clamp", "max").as_deref(), Some("fused_region_1"));

        // The chain reconnects: source → r0, r0 → threshold, threshold → r1 → final.
        let id_of =
            |nid: &str| fused.def.nodes.iter().find(|n| n.node_id.as_str() == nid).map(|n| n.id);
        let thresh = id_of("thresh").unwrap();
        let r0 = id_of("fused_region_0").unwrap();
        let r1 = id_of("fused_region_1").unwrap();
        let has_wire =
            |from: u32, to: u32| fused.def.wires.iter().any(|w| w.from_node == from && w.to_node == to);
        assert!(has_wire(r0, thresh), "fused_region_0 feeds the threshold");
        assert!(has_wire(thresh, r1), "the threshold feeds fused_region_1");
    }

    /// Every seeded field name + every retarget target exists as a real param on
    /// the `WgslCompute` node once it reparses the generated source. The drift
    /// guard: if the codegen's `n{i}_{param}` field-naming convention diverges
    /// from the install-side reconstruction, the seeded params would land on
    /// non-existent fields and silently no-op — this catches it without a GPU.
    #[test]
    fn seeded_fields_match_wgsl_compute_params() {
        use crate::node_graph::effect_node::EffectNode;
        use crate::node_graph::primitives::WgslCompute;
        let def = colorgrade_def();
        let fused = fuse_canonical_def(&def, &registry()).expect("ColorGrade fuses");
        let node = fused
            .def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .unwrap();

        let mut wc = WgslCompute::new();
        wc.set_wgsl_source(node.wgsl_source.as_deref().unwrap());
        let param_names: AHashSet<&str> = wc.parameters().iter().map(|p| p.name).collect();

        for field in node.params.keys() {
            assert!(
                param_names.contains(field.as_str()),
                "seeded field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
        for (_, field) in fused.retarget.values() {
            assert!(
                param_names.contains(field.as_str()),
                "retarget field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
    }

    /// The cached fused view retargets every outer-card binding onto its region's
    /// fused node, preserving the card surface: 9 bindings, all pointing at the
    /// fused node, at the matching `n{i}_{param}` field.
    #[test]
    fn fused_view_retargets_every_binding() {
        let view = fused_view_by_id(&EffectTypeId::new("ColorGrade"))
            .expect("ColorGrade has a fused view");
        assert_eq!(view.bindings.len(), 9, "all outer-card sliders survive");
        for b in view.bindings {
            match &b.target {
                ParamTarget::Node { node_id, param } => {
                    assert_eq!(node_id.as_str(), "fused_region_0");
                    assert!(param.starts_with('n'), "retargeted to a fused field");
                }
                other => panic!("binding {:?} not retargeted to a node: {other:?}", b.id),
            }
        }
        // Spot-check two specific routings end-to-end through the cache.
        let field_for = |id: &str| {
            view.bindings
                .iter()
                .find(|b| AsRef::<str>::as_ref(&b.id) == id)
                .and_then(|b| match &b.target {
                    ParamTarget::Node { param, .. } => Some(*param),
                    _ => None,
                })
        };
        assert_eq!(field_for("amount"), Some("n5_amount"));
        assert_eq!(field_for("gain"), Some("n0_gain"));
        assert_eq!(field_for("tint_focus"), Some("n4_focus"));
    }

    /// An effect with no fusable node has no region — left entirely unfused, safe
    /// by construction. `node.threshold` is a Boundary, so a single-threshold
    /// card returns `None`.
    #[test]
    fn boundary_only_card_does_not_fuse() {
        let json = r#"{
            "version": 1,
            "name": "t",
            "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.threshold", "handle": "t", "nodeId": "t" },
                { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert!(
            fuse_canonical_def(&def, &registry()).is_none(),
            "a card with no fusable region must not fuse"
        );
    }

    /// Fan-out — a region with two escaping members fuses to ONE node exposing
    /// two output ports (`dst_0`, `dst_1`), each wired to the boundary its member
    /// fed. gain forks into invert and contrast; each runs into its own threshold,
    /// which re-merge at a mix. The rewrite keeps both thresholds + the mix as
    /// surviving nodes and routes `dst_0 → thr_a`, `dst_1 → thr_b`.
    #[test]
    fn fanout_region_wires_two_dst_ports() {
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
        let fused = fuse_canonical_def(&def, &registry()).expect("the fan-out region fuses");

        // Exactly one fused node (gain+invert+contrast), both thresholds + the mix
        // survive.
        let wgsl_nodes: Vec<_> =
            fused.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").collect();
        assert_eq!(wgsl_nodes.len(), 1, "the fork is one fused node");
        let fused_doc = wgsl_nodes[0].id;
        assert_eq!(
            fused.def.nodes.iter().filter(|n| n.type_id == "node.threshold").count(),
            2,
            "both threshold boundaries survive"
        );

        // The fused node exposes two outputs, each routed to its member's boundary.
        let id_of =
            |nid: &str| fused.def.nodes.iter().find(|n| n.node_id.as_str() == nid).map(|n| n.id);
        let thr_a = id_of("thr_a").unwrap();
        let thr_b = id_of("thr_b").unwrap();
        let port_into = |to: u32| -> Option<String> {
            fused
                .def
                .wires
                .iter()
                .find(|w| w.from_node == fused_doc && w.to_node == to)
                .map(|w| w.from_port.clone())
        };
        // invert(2) < contrast(3) by doc-id, so invert → dst_0, contrast → dst_1.
        assert_eq!(port_into(thr_a).as_deref(), Some("dst_0"), "invert's output drives thr_a via dst_0");
        assert_eq!(port_into(thr_b).as_deref(), Some("dst_1"), "contrast's output drives thr_b via dst_1");

        // Retarget still routes both members' params onto the one fused node.
        let region_of = |nid: &str, p: &str| {
            fused.retarget.get(&(nid.into(), p.into())).map(|(id, _)| id.as_str().to_string())
        };
        assert_eq!(region_of("gain", "gain").as_deref(), Some("fused_region_0"));
        assert_eq!(region_of("invert", "intensity").as_deref(), Some("fused_region_0"));
        assert_eq!(region_of("contrast", "contrast").as_deref(), Some("fused_region_0"));
    }

    /// A control wire driving a fused-away atom's param is re-anchored onto the
    /// fused node's port-shadow `n{idx}_<param>`. texture_dimensions.aspect drives
    /// gain.gain; gain is member 0 of its region, so after fusion the wire runs
    /// texture_dimensions → fused.n0_gain — keeping the modulation live.
    #[test]
    fn control_wire_reanchors_onto_fused_shadow_port() {
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
        let fused = fuse_canonical_def(&def, &registry()).expect("the control-wired region fuses");
        let fused_doc =
            fused.def.nodes.iter().find(|n| n.type_id == "node.wgsl_compute").unwrap().id;
        let dims_doc =
            fused.def.nodes.iter().find(|n| n.type_id == "node.texture_dimensions").unwrap().id;
        let cw = fused
            .def
            .wires
            .iter()
            .find(|w| w.from_node == dims_doc && w.to_node == fused_doc)
            .expect("texture_dimensions still drives the fused node");
        assert_eq!(cw.from_port, "aspect", "the producer's aspect output");
        assert_eq!(cw.to_port, "n0_gain", "re-anchored onto gain's shadow field (member 0)");
        // The fused WgslCompute must actually expose that shadow port.
        use crate::node_graph::effect_node::EffectNode;
        use crate::node_graph::primitives::WgslCompute;
        let src = fused
            .def
            .nodes
            .iter()
            .find(|n| n.id == fused_doc)
            .and_then(|n| n.wgsl_source.as_deref())
            .unwrap();
        let mut wc = WgslCompute::new();
        wc.set_wgsl_source(src);
        assert!(
            wc.inputs().iter().any(|i| i.name == "n0_gain"),
            "the fused node exposes n0_gain as a control input"
        );
    }

    /// A generator's `preset_metadata` binding is retargeted onto the fused node.
    /// checkerboard (Source) → gain → invert fuse into one region; the binding that
    /// drove `gain.gain` is repointed at the fused node's `n1_gain` field (gain is
    /// member 1), so the generator's modulation surface keeps driving the kernel.
    #[test]
    fn generator_binding_def_retargets_onto_fused() {
        use manifold_core::effect_graph_def::BindingTarget;
        let json = r#"{
            "version": 1, "name": "FuseGen",
            "presetMetadata": {
                "id": "FuseGen", "displayName": "Fuse Gen", "category": "Diagnostic",
                "oscPrefix": "fuse_gen",
                "params": [{ "id": "g", "name": "Gain", "min": 0.0, "max": 4.0, "defaultValue": 2.0 }],
                "bindings": [{ "id": "g", "label": "Gain", "defaultValue": 2.0,
                    "target": { "kind": "node", "nodeId": "gain", "param": "gain" } }]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "nodeId": "gen_in" },
                { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
                { "id": 2, "typeId": "node.gain", "nodeId": "gain" },
                { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let fused = fuse_generator_def(&def, &registry()).expect("the generator fuses");
        let meta = fused.preset_metadata.as_ref().expect("metadata preserved");
        assert_eq!(meta.bindings.len(), 1);
        match &meta.bindings[0].target {
            BindingTarget::Node { node_id, param } => {
                assert_eq!(node_id.as_str(), "fused_region_0", "binding re-anchored to the fused node");
                assert_eq!(param, "n1_gain", "gain is member 1, so its field is n1_gain");
            }
            other => panic!("binding not retargeted to a node: {other:?}"),
        }
    }
}
