//! Step 4 — install fused regions into the live render path (design §12.2/§12.3).
//!
//! Steps 1–3 built the codegen: each atom carries a `wgsl_body`, and
//! [`super::codegen::generate_fused`] chains a region of those bodies into one
//! kernel proven bit-identical to the hand-fused reference. This module does
//! the production wiring so that kernel actually renders on screen.
//!
//! ## What it does
//!
//! Given an effect's canonical [`EffectGraphDef`] (e.g. ColorGrade's
//! source → gain → … → mix → clamp → final_output), it:
//!
//! 1. **Region-grows** (whole-card, v1): every non-boundary node must be a
//!    fusable atom ([`FusionKind::Pointwise`]/[`MultiInputCoincident`]) with a
//!    `wgsl_body`, no control wires, all texture inputs tracing to the single
//!    upstream source. If anything else is present (a blur, a feedback, a DNN,
//!    a resolution change → all `Boundary`), the whole card is left unfused —
//!    safe by construction (`None`).
//! 2. **Rewrites the def** (DD-A1 — a *definition* rewrite, not a `Graph`
//!    clone): keeps the `system.source` / `system.final_output` boundaries,
//!    deletes the region's worker nodes + wires, inserts ONE
//!    `node.wgsl_compute` node carrying the generated fused WGSL + the per-atom
//!    param values, and re-anchors `source → fused.src_0 → fused.dst →
//!    final_output`.
//! 3. **Retargets the bindings** (DD-A5): each outer-card slider that drove an
//!    inner node param (`gain.gain`, `colorize.focus`, …) is repointed at the
//!    fused node's namespaced uniform field (`n0_gain`, `n4_focus`, …). The
//!    fused [`WgslCompute`] derives those as port-shadowed params from the
//!    uniform struct, so drivers / Ableton / LFOs keep writing them every
//!    frame (DD-A4: `var<uniform>`, never std430).
//!
//! The fused [`LoadedPresetView`] is cached `&'static` (built once per effect
//! type, exactly like [`crate::node_graph::loaded_preset_view_by_id`]), so the
//! per-frame chain rebuilds on resize don't leak.
//!
//! ## What it deliberately does NOT touch (DD-A6)
//!
//! - The **unfused** canonical view ([`crate::node_graph::loaded_preset_view_by_id`])
//!   stays the authoring + fallback surface. The graph editor reads it, so
//!   drilling into a fused ColorGrade still shows the 7 atoms. Only the chain
//!   *render* path swaps in the fused view, and only for the un-edited
//!   canonical preset — an effect with a per-instance graph override
//!   (`EffectInstance.graph = Some`) is rendered from the user's wiring,
//!   unfused, so editing stays live.
//! - This is "freeze = render-only binary, graph = source" (the §12 framing).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    SerializedParamValue,
};

use crate::node_graph::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::classify::FusionKind;
use crate::node_graph::freeze::codegen::{self, FusionRegion, InputSource, RegionNode};
use crate::node_graph::parameters::{ParamDef, ParamValue};
use crate::node_graph::ports::{NodeInput, PortType};
use crate::node_graph::{LoadedPresetView, ParamBinding, ParamTarget, PrimitiveRegistry};

/// Handle + stable node-id of the single fused node a v1 whole-card fusion
/// installs. There is exactly one fused node per card, so a fixed name is
/// safe — splice handle scope is per-effect, so two cards of the same type
/// each get an independent runtime instance keyed off their own node map.
const FUSED_NODE_HANDLE: &str = "fused_region";

/// The fused kernel's single external input port and output port, named by
/// [`codegen::generate_fused`] (`src_0`, `dst`). The def-rewrite wires the
/// upstream source into `src_0` and `dst` into `final_output`.
const FUSED_INPUT_PORT: &str = "src_0";
const FUSED_OUTPUT_PORT: &str = "dst";

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

/// Look up the fused [`LoadedPresetView`] for an effect type, building the
/// whole map on first call and caching `&'static` for the process lifetime.
/// Returns `None` for any effect whose canonical graph isn't a single
/// whole-card fusable region (the overwhelming majority — anything with a
/// blur / feedback / DNN / resolution change). Mirrors
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
/// canonical graph isn't a single whole-card fusable region. The fused view
/// keeps the same outer-card params + skip mode (so the chain builder's
/// `outer_param_index` / `n_static_slots` / skip logic are byte-identical) and
/// swaps in the fused def + retargeted bindings.
fn fuse_view(base: &LoadedPresetView, registry: &PrimitiveRegistry) -> Option<LoadedPresetView> {
    let fused = fuse_canonical_def(base.canonical_def, registry)?;
    let def_static: &'static EffectGraphDef = Box::leak(Box::new(fused.def));
    let bindings = retarget_bindings(base.bindings, &fused.fused_node_id, &fused.retarget)?;
    Some(LoadedPresetView {
        type_id: base.type_id.clone(),
        canonical_def: def_static,
        bindings: Box::leak(bindings.into_boxed_slice()),
        skip_mode: base.skip_mode,
    })
}

/// Rewrite each outer-card binding that targeted an inner region node so it
/// points at the fused node's namespaced uniform field. Bindings that target
/// nothing in the region (Composite / Custom, or a Node outside the retarget
/// map) make the whole fusion unsafe — return `None` so the card stays unfused
/// rather than silently stranding a slider.
fn retarget_bindings(
    base: &[ParamBinding],
    fused_node_id: &NodeId,
    retarget: &AHashMap<(String, String), String>,
) -> Option<Vec<ParamBinding>> {
    let mut out = Vec::with_capacity(base.len());
    for b in base {
        let mut nb = b.clone();
        match &b.target {
            ParamTarget::Node { node_id, param } => {
                let key = (node_id.as_str().to_string(), (*param).to_string());
                let field = retarget.get(&key)?;
                let field_static: &'static str = Box::leak(field.clone().into_boxed_str());
                nb.target = ParamTarget::Node {
                    node_id: fused_node_id.clone(),
                    param: field_static,
                };
            }
            // A composite/custom target can't be expressed against a single
            // fused uniform field — refuse to fuse this card.
            _ => return None,
        }
        out.push(nb);
    }
    Some(out)
}

/// A canonical def rewritten into one fused node, plus the routing the binding
/// retarget needs. `pub(crate)` so the end-to-end oracle test can drive both
/// the unfused and fused graphs from one fixture (set inner params by stable
/// node id on the unfused side, by `retarget`ed field name on the fused side).
pub(crate) struct FusedDef {
    pub def: EffectGraphDef,
    pub fused_node_id: NodeId,
    /// `(original stable node_id, original param) → fused uniform field name`
    /// (`"n{i}_{param}"`, `i` = region topo index — the codegen convention).
    pub retarget: AHashMap<(String, String), String>,
}

/// One non-boundary node, pre-screened as a whole-card-fusable atom: its stable
/// id, body, params, texture-input ports (in body-arg order), and fusion kind.
struct Worker {
    doc_id: u32,
    node_id: NodeId,
    body: &'static str,
    params: &'static [ParamDef],
    tex_inputs: Vec<&'static str>,
    kind: FusionKind,
}

impl HasDocId for Worker {
    fn doc_id(&self) -> u32 {
        self.doc_id
    }
}

/// Try to collapse `def`'s entire worker body into one fused node. Returns
/// `None` (leave unfused) unless EVERY non-boundary node is a fusable atom, the
/// graph has the single-source / single-output linear-ish shape v1 supports,
/// and no node carries a control wire. Conservative: ambiguity → don't fuse.
pub(crate) fn fuse_canonical_def(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<FusedDef> {
    // Group nodes must already be flattened away by the loader before fusion
    // would ever see them; a def carrying a `group` node isn't a v1 target.
    if def.nodes.iter().any(|n| n.group.is_some()) {
        return None;
    }

    // ── Boundaries (exactly one source, one final_output). ──
    let mut source_id: Option<u32> = None;
    let mut final_id: Option<u32> = None;
    for n in &def.nodes {
        if n.type_id == SOURCE_TYPE_ID {
            if source_id.is_some() {
                return None;
            }
            source_id = Some(n.id);
        } else if n.type_id == FINAL_OUTPUT_TYPE_ID {
            if final_id.is_some() {
                return None;
            }
            final_id = Some(n.id);
        }
    }
    let source_id = source_id?;
    let final_id = final_id?;

    // ── Construct every worker once: read its fusion kind, body, ports,
    // param defaults. Bail the moment one isn't a whole-card-fusable atom. ──
    let mut workers: Vec<Worker> = Vec::new();
    for n in &def.nodes {
        if n.id == source_id || n.id == final_id {
            continue;
        }
        let node = registry.construct(&n.type_id)?;
        let kind = node.fusion_kind();
        if !kind.is_fusable() {
            return None; // a Boundary atom anywhere → leave the card unfused
        }
        let body = node.wgsl_body()?;
        // Every param must lay out as a scalar uniform field, or the codegen
        // can't fuse it (vec/color/table/string params).
        for p in node.parameters() {
            codegen::param_wgsl_type(p).ok()?;
        }
        // Texture I/O shape: exactly one texture output; ≥1 texture inputs.
        let tex_inputs: Vec<&'static str> = node
            .inputs()
            .iter()
            .filter(|i| is_texture_input(i))
            .map(|i| i.name)
            .collect();
        let tex_outputs: Vec<&'static str> = node
            .outputs()
            .iter()
            .filter(|o| matches!(o.ty, PortType::Texture2D | PortType::Texture2DTyped(_)))
            .map(|o| o.name)
            .collect();
        if tex_inputs.is_empty() || tex_outputs.len() != 1 {
            return None;
        }
        // No control wires: every incoming wire to this node must land on a
        // texture input port. A scalar/control wire (LFO → gain.gain) would
        // dangle when the node folds away — v1 cuts rather than re-anchors it.
        let tex_input_set: ahash::AHashSet<&str> = tex_inputs.iter().copied().collect();
        for w in &def.wires {
            if w.to_node == n.id && !tex_input_set.contains(w.to_port.as_str()) {
                return None;
            }
        }
        // `body` is `&'static str` (the macro emits a const), independent of the
        // boxed node's lifetime — store it directly. `parameters()` is borrowed
        // through the box, so copy that slice out (`leak_params`).
        workers.push(Worker {
            doc_id: n.id,
            node_id: resolve_node_id(n),
            body,
            params: leak_params(node.parameters()),
            tex_inputs,
            kind,
        });
    }
    if workers.is_empty() {
        return None;
    }

    // ── Topo-sort workers by their texture wires (worker → worker). A cycle
    // means feedback, which a fusable region never contains; bail if found. ──
    let order = topo_sort_workers(&workers, &def.wires, source_id)?;

    // ── The region output is the single worker feeding final_output.in. ──
    let mut output_doc: Option<u32> = None;
    for w in &def.wires {
        if w.to_node == final_id {
            if output_doc.is_some() {
                return None; // more than one wire into final_output
            }
            output_doc = Some(w.from_node);
        }
    }
    let output_doc = output_doc?;
    if !workers.iter().any(|w| w.doc_id == output_doc) {
        return None;
    }

    // ── Build the FusionRegion in topo order, resolving each worker's texture
    // inputs to External(source) / Node(earlier worker). v1 supports exactly
    // one external producer: the upstream source. ──
    let worker_by_doc: AHashMap<u32, &Worker> = workers.iter().map(|w| (w.doc_id, w)).collect();
    let mut region_nodes: Vec<RegionNode<'_>> = Vec::with_capacity(order.len());
    for &doc_id in &order {
        let w = worker_by_doc[&doc_id];
        let mut inputs: Vec<InputSource> = Vec::with_capacity(w.tex_inputs.len());
        for port in &w.tex_inputs {
            let wire = def
                .wires
                .iter()
                .find(|wi| wi.to_node == doc_id && wi.to_port == *port)?;
            if wire.from_node == source_id {
                inputs.push(InputSource::External(0)); // the single chain input
            } else if worker_by_doc.contains_key(&wire.from_node) {
                inputs.push(InputSource::Node(NodeInstanceId(wire.from_node)));
            } else {
                // Texture input from a non-source, non-worker node — not a v1
                // single-source region.
                return None;
            }
        }
        region_nodes.push(RegionNode {
            node_id: NodeInstanceId(doc_id),
            fusion_kind: w.kind,
            body: w.body,
            params: w.params,
            inputs,
        });
    }
    let region = FusionRegion {
        nodes: region_nodes,
        num_external_inputs: 1,
        output: NodeInstanceId(output_doc),
    };
    let generated = codegen::generate_fused(&region).ok()?;

    // ── Build the retarget map + seed the fused node's params with each
    // worker's effective value (def override else atom default). The field name
    // `n{i}_{param}` matches the codegen's region-index convention. ──
    let mut retarget: AHashMap<(String, String), String> = AHashMap::default();
    let mut fused_params: BTreeMap<String, SerializedParamValue> = BTreeMap::new();
    for (i, &doc_id) in order.iter().enumerate() {
        let w = worker_by_doc[&doc_id];
        let doc_node = def.nodes.iter().find(|n| n.id == doc_id)?;
        for p in w.params {
            let field = format!("n{i}_{}", p.name);
            retarget.insert(
                (w.node_id.as_str().to_string(), p.name.to_string()),
                field.clone(),
            );
            let value = effective_param_f32(doc_node.params.get(p.name), &p.default)?;
            fused_params.insert(field, SerializedParamValue::Float { value });
        }
    }

    // ── Assemble the rewritten def: boundaries + one fused node. ──
    let fused_doc_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
    let fused_node_id = NodeId::new(FUSED_NODE_HANDLE);
    let source_node = def.nodes.iter().find(|n| n.id == source_id)?.clone();
    let final_node = def.nodes.iter().find(|n| n.id == final_id)?.clone();
    let fused_node = EffectGraphNode {
        id: fused_doc_id,
        node_id: fused_node_id.clone(),
        // The dynamic-WGSL escape-hatch primitive — same stable type id the
        // preset JSON uses; it derives its ports/params from the source string.
        type_id: "node.wgsl_compute".to_string(),
        handle: Some(FUSED_NODE_HANDLE.to_string()),
        params: fused_params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: Some(generated.wgsl),
        title: Some("Fused Region".to_string()),
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    };
    let wires = vec![
        EffectGraphWire {
            from_node: source_id,
            from_port: "out".to_string(),
            to_node: fused_doc_id,
            to_port: FUSED_INPUT_PORT.to_string(),
        },
        EffectGraphWire {
            from_node: fused_doc_id,
            from_port: FUSED_OUTPUT_PORT.to_string(),
            to_node: final_id,
            to_port: "in".to_string(),
        },
    ];
    let fused_def = EffectGraphDef {
        version: EFFECT_GRAPH_VERSION_WITH_METADATA,
        name: def.name.clone(),
        description: def.description.clone(),
        // Keep the outer-card surface (params / skip / aliases) byte-identical
        // so the chain builder's outer_param_index + skip logic are unchanged.
        preset_metadata: def.preset_metadata.clone(),
        nodes: vec![source_node, fused_node, final_node],
        wires,
    };

    Some(FusedDef {
        def: fused_def,
        fused_node_id,
        retarget,
    })
}

fn is_texture_input(i: &NodeInput) -> bool {
    matches!(i.ty, PortType::Texture2D | PortType::Texture2DTyped(_))
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
/// value (which `param_wgsl_type` already rejected upstream — defensive).
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

/// Kahn topo-sort of workers by worker→worker texture wires. Source-origin
/// wires contribute no edge (the source isn't a worker). Returns the doc-id
/// order, or `None` on a cycle (feedback — never present in a fusable region).
fn topo_sort_workers(
    workers: &[impl HasDocId],
    wires: &[EffectGraphWire],
    source_id: u32,
) -> Option<Vec<u32>> {
    let ids: ahash::AHashSet<u32> = workers.iter().map(|w| w.doc_id()).collect();
    let mut indeg: AHashMap<u32, u32> = workers.iter().map(|w| (w.doc_id(), 0)).collect();
    let mut adj: AHashMap<u32, Vec<u32>> = AHashMap::default();
    for w in wires {
        if w.from_node == source_id {
            continue;
        }
        if ids.contains(&w.from_node) && ids.contains(&w.to_node) && w.from_node != w.to_node {
            adj.entry(w.from_node).or_default().push(w.to_node);
            *indeg.get_mut(&w.to_node).unwrap() += 1;
        }
    }
    // Seed the queue in stable doc-id order so the output is deterministic.
    let mut queue: Vec<u32> = workers
        .iter()
        .map(|w| w.doc_id())
        .filter(|id| indeg[id] == 0)
        .collect();
    queue.sort_unstable();
    let mut order: Vec<u32> = Vec::with_capacity(workers.len());
    while let Some(id) = queue.pop() {
        order.push(id);
        if let Some(succs) = adj.get(&id) {
            let mut newly_ready: Vec<u32> = Vec::new();
            for &s in succs {
                let d = indeg.get_mut(&s).unwrap();
                *d -= 1;
                if *d == 0 {
                    newly_ready.push(s);
                }
            }
            newly_ready.sort_unstable();
            // Push in reverse so the smallest is popped first (stable order).
            for s in newly_ready.into_iter().rev() {
                queue.push(s);
            }
        }
    }
    if order.len() == workers.len() {
        Some(order)
    } else {
        None // cycle
    }
}

/// Tiny helper so `topo_sort_workers` can be generic over the `Worker` struct
/// without leaking its fields into the public surface.
trait HasDocId {
    fn doc_id(&self) -> u32;
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

    /// The whole ColorGrade card (7 atoms) collapses to ONE `node.wgsl_compute`
    /// node between the retained boundaries, wired source → fused → final_output.
    /// The retarget maps each inner (node_id, param) to its `n{i}_{param}` field
    /// in topo order — the load-bearing routing for the binding rewrite.
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
            fused.def.wires.iter().any(|w| w.to_port == FUSED_INPUT_PORT),
            "an input wire targets the fused src port"
        );
        assert!(
            fused.def.wires.iter().any(|w| w.from_port == FUSED_OUTPUT_PORT),
            "the fused output wire leaves the dst port"
        );

        // Region topo order: gain(0) sat(1) hue(2) contrast(3) colorize(4)
        // mix(5) clamp(6). Spot-check the routing the binding rewrite depends on.
        let get = |nid: &str, p: &str| fused.retarget.get(&(nid.into(), p.into())).cloned();
        assert_eq!(get("gain", "gain").as_deref(), Some("n0_gain"));
        assert_eq!(get("saturation", "saturation").as_deref(), Some("n1_saturation"));
        assert_eq!(get("hue", "hue").as_deref(), Some("n2_hue"));
        assert_eq!(get("contrast", "contrast").as_deref(), Some("n3_contrast"));
        assert_eq!(get("colorize", "focus").as_deref(), Some("n4_focus"));
        assert_eq!(get("grade_mix", "amount").as_deref(), Some("n5_amount"));
        assert_eq!(get("clamp", "max").as_deref(), Some("n6_max"));
        // 14 inner params across the 7 atoms (1+1+3+1+4+2+2).
        assert_eq!(fused.retarget.len(), 14);
    }

    /// Every seeded field name + every retarget target exists as a real param on
    /// the `WgslCompute` node once it reparses the generated source. This is the
    /// drift guard: if the codegen's `n{i}_{param}` field-naming convention ever
    /// diverges from the install-side reconstruction, the seeded params would
    /// land on non-existent fields and silently no-op — this catches it without
    /// a GPU (naga introspection only).
    #[test]
    fn seeded_fields_match_wgsl_compute_params() {
        use crate::node_graph::primitives::WgslCompute;
        let def = colorgrade_def();
        let fused = fuse_canonical_def(&def, &registry()).expect("ColorGrade fuses");
        let node = fused
            .def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .unwrap();

        use crate::node_graph::effect_node::EffectNode;
        let mut wc = WgslCompute::new();
        wc.set_wgsl_source(node.wgsl_source.as_deref().unwrap());
        let param_names: ahash::AHashSet<&str> =
            wc.parameters().iter().map(|p| p.name).collect();

        for field in node.params.keys() {
            assert!(
                param_names.contains(field.as_str()),
                "seeded field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
        for field in fused.retarget.values() {
            assert!(
                param_names.contains(field.as_str()),
                "retarget field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
    }

    /// The cached fused view retargets every outer-card binding onto the fused
    /// node, preserving the card surface: 9 bindings, all pointing at the fused
    /// node id, at the matching `n{i}_{param}` field.
    #[test]
    fn fused_view_retargets_every_binding() {
        let view = fused_view_by_id(&EffectTypeId::new("ColorGrade"))
            .expect("ColorGrade has a fused view");
        assert_eq!(view.bindings.len(), 9, "all outer-card sliders survive");
        for b in view.bindings {
            match &b.target {
                ParamTarget::Node { node_id, param } => {
                    assert_eq!(node_id.as_str(), FUSED_NODE_HANDLE);
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

    /// An effect with any non-fusable (Boundary) node is left entirely unfused —
    /// safe by construction. `node.threshold` defaults to `Boundary`, so a
    /// source → threshold → final_output card returns `None`.
    #[test]
    fn boundary_node_blocks_whole_card_fusion() {
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
            "a Boundary atom must block whole-card fusion"
        );
    }
}
