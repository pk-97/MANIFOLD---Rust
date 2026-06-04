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
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    SerializedParamValue,
};

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::codegen::{self, FusionRegion, InputSource, RegionNode};
use crate::node_graph::freeze::region::{RegionInput, partition_regions};
use crate::node_graph::parameters::{ParamDef, ParamValue};
use crate::node_graph::{LoadedPresetView, ParamBinding, ParamTarget, PrimitiveRegistry};

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
    let def_static: &'static EffectGraphDef = Box::leak(Box::new(fused.def));
    Some(LoadedPresetView {
        type_id: base.type_id.clone(),
        canonical_def: def_static,
        bindings: Box::leak(bindings.into_boxed_slice()),
        skip_mode: base.skip_mode,
    })
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
            });
        }
        let fusion_region = FusionRegion {
            nodes: region_nodes,
            num_external_inputs: region.externals.len(),
            output: NodeInstanceId(region.output),
        };
        let generated = codegen::generate_fused(&fusion_region).ok()?;

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
                fused_params.insert(field, SerializedParamValue::Float { value });
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

    // ── Rewire. Distinct regions are never directly texture-wired (a wire
    // between two eligible nodes would have merged them into one region), so
    // every region's external producer and output consumer is a SURVIVING node
    // — each rewrite is local. ──
    let mut new_wires: Vec<EffectGraphWire> = Vec::new();
    // (a) surviving → surviving wires pass through.
    for w in &def.wires {
        if !member_region.contains_key(&w.from_node) && !member_region.contains_key(&w.to_node) {
            new_wires.push(w.clone());
        }
    }
    for (i, region) in regions.iter().enumerate() {
        let fused_doc = fused_docs[i];
        // (b) each external producer → the fused node's `src_<slot>` (read once,
        // even if several members read the same external — the finder deduped).
        for (e, ext) in region.externals.iter().enumerate() {
            new_wires.push(EffectGraphWire {
                from_node: ext.from_node,
                from_port: ext.from_port.clone(),
                to_node: fused_doc,
                to_port: format!("src_{e}"),
            });
        }
        // (c) the fused node's `dst` → every consumer the region output fed.
        for w in &def.wires {
            if w.from_node == region.output && !member_region.contains_key(&w.to_node) {
                new_wires.push(EffectGraphWire {
                    from_node: fused_doc,
                    from_port: "dst".to_string(),
                    to_node: w.to_node,
                    to_port: w.to_port.clone(),
                });
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ParamTarget;
    use manifold_core::effect_graph_def::EffectGraphDef;

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
}
