//! Bridge from `manifold-core::EffectInstance` to a runnable
//! [`Graph`] built from primitive nodes.
//!
//! Every legacy effect (Bloom, Halation, WireframeDepth, …) has a 1:1
//! primitive equivalent shipped in `node_graph::primitives` and parity-
//! tested against the legacy compute path. Until users start customising
//! graphs, the on-disk `EffectInstance` shape (`effect_type` + positional
//! `param_values`) stays as-is — every instance corresponds to the same
//! canonical 3-node graph:
//!
//! ```text
//!   system.source ──▶ primitive.<effect> ──▶ system.final_output
//! ```
//!
//! [`build_effect_graph`] is the entry point the runtime calls when
//! swapping `EffectChain::apply_chain` for graph execution. It looks up
//! the primitive for the instance's `effect_type`, materialises the
//! canonical graph via the same [`GraphDocument`] loader that bundled
//! presets use, then walks `EffectMetadata.params` to translate
//! positional `param_values` slots onto named primitive parameters
//! (with `f32 → Enum/Int/Bool/Float` coercion driven by the
//! primitive's declared [`ParamType`]).
//!
//! When users start saving customised graphs (frontend UI work), this
//! same code path is the fallback for instances that don't carry an
//! inline graph payload yet.

use std::collections::BTreeMap;

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;

use crate::node_graph::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::legacy_adapter::metadata_by_id;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::persistence::{
    EffectGraphDefExt, GRAPH_DOCUMENT_VERSION, GraphDocument, LoadError, NodeDocument,
    PrimitiveRegistry, SerializedParamValue, WireDocument,
};
use crate::node_graph::primitive::PrimitiveSpec;
use crate::node_graph::primitives;

/// Errors raised by [`build_effect_graph`].
#[derive(Debug, Clone, PartialEq)]
pub enum EffectGraphError {
    /// No primitive is registered for this legacy effect type.
    /// `effect_type` is the legacy string (e.g., `"Bloom"`).
    UnsupportedEffectType { effect_type: String },
    /// The legacy effect has no [`EffectMetadata`] registered. The
    /// renderer's startup inventory should make this unreachable in
    /// production — it indicates a missing `inventory::submit!` for a
    /// shipping effect.
    MissingMetadata { effect_type: String },
    /// The canonical `GraphDocument` for this effect failed to load.
    /// Wraps the underlying [`LoadError`] so callers can surface
    /// specific failures (typed param mismatch, etc.) when an effect's
    /// param schema drifts away from its primitive.
    LoadFailure {
        effect_type: String,
        inner: LoadError,
    },
}

impl std::fmt::Display for EffectGraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedEffectType { effect_type } => write!(
                f,
                "no primitive registered for legacy effect type '{effect_type}'"
            ),
            Self::MissingMetadata { effect_type } => write!(
                f,
                "no EffectMetadata registered for effect type '{effect_type}' \
                 (renderer startup should have inserted one)"
            ),
            Self::LoadFailure { effect_type, inner } => {
                write!(
                    f,
                    "effect '{effect_type}': canonical graph load failed: {inner}"
                )
            }
        }
    }
}

impl std::error::Error for EffectGraphError {}

/// Look up the primitive `type_id` that an [`EffectTypeId`] maps onto,
/// or `None` if the effect has no primitive equivalent yet.
///
/// Every legacy effect this binary ships ([§6.1–6.5 of
/// `docs/PRIMITIVE_LIBRARY_DESIGN.md`]) appears here. Adding a new
/// legacy-effect → primitive mapping is a one-line addition to the
/// match arm. Removing one is a breaking change for saved projects.
///
/// Note: the macro-authored primitives don't export top-level
/// `*_TYPE_ID` constants (the macro stores them on `PrimitiveSpec::TYPE_ID`
/// only), so for those we go through the trait. Composite + monolithic
/// primitives ship explicit constants which we reference directly.
pub fn primitive_id_for_effect(effect_type: &EffectTypeId) -> Option<&'static str> {
    let s = effect_type.as_str();
    let id = match s {
        // §6.1 — atomic primitives derived from single-pass effects.
        "InvertColors" => <primitives::Invert as PrimitiveSpec>::TYPE_ID,
        "Transform" => <primitives::AffineTransform as PrimitiveSpec>::TYPE_ID,
        "ChromaticAberration" => <primitives::ChromaticOffset as PrimitiveSpec>::TYPE_ID,
        "EdgeStretch" => <primitives::ClampStretch as PrimitiveSpec>::TYPE_ID,
        "ColorGrade" => <primitives::ColorGrade as PrimitiveSpec>::TYPE_ID,
        "Dither" => <primitives::DitherPattern as PrimitiveSpec>::TYPE_ID,
        "EdgeGlow" => <primitives::EdgeDetect as PrimitiveSpec>::TYPE_ID,
        "Glitch" => <primitives::Glitch as PrimitiveSpec>::TYPE_ID,
        "HdrBoost" => <primitives::HighlightBoost as PrimitiveSpec>::TYPE_ID,
        "Kaleidoscope" => <primitives::KaleidoFold as PrimitiveSpec>::TYPE_ID,

        // §6.6 #5 — Infrared ships as a monolithic wrapper
        // primitive (same shape as AutoGain / BlobTracking /
        // WireframeDepth) rather than a `BakedPalette → ColorLut`
        // decomposition. The legacy effect's 512×1 baked LUTs need
        // per-slot texture-resolution support in the graph runtime
        // to decompose without breaking parity — out of scope here.
        "Infrared" => primitives::INFRARED_TYPE_ID,
        "Strobe" => <primitives::Strobe as PrimitiveSpec>::TYPE_ID,
        "VoronoiPrism" => <primitives::VoronoiPrism as PrimitiveSpec>::TYPE_ID,

        // §6.3 — fused composites (Bloom / Halation / Watercolor).
        "Bloom" => primitives::BLOOM_TYPE_ID,
        "Halation" => primitives::HALATION_TYPE_ID,
        "Watercolor" => primitives::WATERCOLOR_TYPE_ID,

        // §6.4 — DepthOfField (single fused primitive across 3 modes).
        "DepthOfField" => primitives::DEPTH_OF_FIELD_TYPE_ID,

        // §6.5 — monolithic wrappers around stateful / DNN-backed effects.
        "AutoGain" => primitives::AUTO_GAIN_TYPE_ID,
        "BlobTracking" => primitives::BLOB_TRACKING_TYPE_ID,
        "WireframeDepth" => primitives::WIREFRAME_DEPTH_TYPE_ID,

        _ => return None,
    };
    Some(id)
}

/// Build the canonical 3-node [`GraphDocument`] for an effect:
///
/// ```text
///   id=0 system.source ─▶ id=1 primitive.<effect> ─▶ id=2 system.final_output
/// ```
///
/// Param values from `instance.param_values` are placed on the primitive
/// node as `SerializedParamValue::Float` entries keyed by
/// `EffectMetadata.params[i].id`. The downstream `into_graph` loader
/// would type-check these against the primitive's declared `ParamType`
/// and reject Enum/Int/Bool params; for runtime loading callers use
/// [`build_effect_graph`] which strips the params before loading and
/// re-applies them with the right `ParamValue` variant.
pub fn canonical_document_for(instance: &EffectInstance) -> Option<GraphDocument> {
    let prim_id = primitive_id_for_effect(instance.effect_type())?;
    let metadata = metadata_by_id(instance.effect_type())?;
    Some(build_canonical_document(instance, metadata, prim_id))
}

fn build_canonical_document(
    instance: &EffectInstance,
    metadata: &'static EffectMetadata,
    prim_id: &'static str,
) -> GraphDocument {
    // Float-encoded param map keyed by the *primitive*'s param name.
    // For each legacy param id, look up the matching primitive name
    // — most pass straight through, a handful drift across the
    // legacy/primitive boundary (audited in `param_name_for_legacy`).
    let effect_id = instance.effect_type().as_str();
    let mut prim_params: BTreeMap<String, SerializedParamValue> = BTreeMap::new();
    for (i, spec) in metadata.params.iter().enumerate() {
        if spec.id.is_empty() {
            continue;
        }
        let Some(primitive_name) = param_name_for_legacy(effect_id, spec.id) else {
            // Legacy param has no primitive counterpart — skip. The
            // primitive keeps its declared default for any param the
            // legacy effect doesn't drive (e.g., `Strobe.beat` is a
            // primitive-only param fed by the modulation system).
            continue;
        };
        let raw = instance
            .param_values
            .get(i)
            .map(|slot| slot.value)
            .unwrap_or(spec.default_value);
        let value = transform_legacy_value(effect_id, spec.id, raw);
        prim_params.insert(
            primitive_name.to_string(),
            SerializedParamValue::Float { value },
        );
    }

    GraphDocument {
        version: GRAPH_DOCUMENT_VERSION,
        name: Some(metadata.display_name.to_string()),
        description: None,
        nodes: vec![
            NodeDocument {
                id: 0,
                type_id: SOURCE_TYPE_ID.to_string(),
                handle: None,
                params: BTreeMap::new(),
                editor_pos: None,
            },
            NodeDocument {
                id: 1,
                type_id: prim_id.to_string(),
                handle: None,
                params: prim_params,
                editor_pos: None,
            },
            NodeDocument {
                id: 2,
                type_id: FINAL_OUTPUT_TYPE_ID.to_string(),
                handle: None,
                params: BTreeMap::new(),
                editor_pos: None,
            },
        ],
        wires: vec![
            WireDocument {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: LEGACY_PRIMITIVE_INPUT_PORT.to_string(),
            },
            WireDocument {
                from_node: 1,
                from_port: "out".to_string(),
                to_node: 2,
                to_port: "in".to_string(),
            },
        ],
    }
}

/// Build a fresh runnable [`Graph`] for `instance`.
///
/// The legacy effect is mapped onto its canonical primitive shape
/// (Source → primitive → FinalOutput), parameter values are lifted
/// from `instance.param_values` with f32→ParamType coercion, and the
/// result is a graph ready for `Executor::execute_frame`.
///
/// Returns `EffectGraphError::UnsupportedEffectType` for effects with
/// no primitive equivalent yet (the runtime should skip them, same
/// behaviour as the legacy `EffectChain` did for unregistered types).
pub fn build_effect_graph(
    instance: &EffectInstance,
    registry: &PrimitiveRegistry,
) -> Result<Graph, EffectGraphError> {
    let prim_id = primitive_id_for_effect(instance.effect_type()).ok_or_else(|| {
        EffectGraphError::UnsupportedEffectType {
            effect_type: instance.effect_type().as_str().to_string(),
        }
    })?;
    let metadata = metadata_by_id(instance.effect_type()).ok_or_else(|| {
        EffectGraphError::MissingMetadata {
            effect_type: instance.effect_type().as_str().to_string(),
        }
    })?;

    let doc = build_canonical_document(instance, metadata, prim_id);

    // Stash the param payload to re-apply after load — the loader
    // would otherwise type-check Float-tagged values against
    // Enum-typed primitive params (Bloom.blend, WireframeDepth.flow,
    // etc.) and raise ParamTypeMismatch.
    let prim_params: BTreeMap<String, SerializedParamValue> = doc
        .nodes
        .iter()
        .find(|n| n.id == 1)
        .map(|n| n.params.clone())
        .unwrap_or_default();
    let stripped = strip_node_params(doc, 1);

    let mut graph = stripped
        .into_graph(registry)
        .map_err(|e| EffectGraphError::LoadFailure {
            effect_type: instance.effect_type().as_str().to_string(),
            inner: e,
        })?;

    materialize_param_overrides(&mut graph, &prim_params);

    Ok(graph)
}

/// Refresh the primitive's parameters on an existing canonical graph
/// from a (possibly modulated) `EffectInstance`. Used by long-lived
/// graph-runtime executors that build the graph once and want to
/// avoid rebuilding it on every frame — primitive state (mip
/// pyramids, feedback buffers, etc.) lives inside the node and is
/// lost on rebuild.
///
/// Walks the same `EffectMetadata` ordering as [`build_effect_graph`]:
/// each legacy `params[i].id` is translated to the primitive's param
/// name via the drift table and coerced from `f32` to the primitive's
/// declared `ParamType`.
///
/// The graph is expected to be a canonical 3-node shape with the
/// primitive at runtime id `NodeInstanceId(1)` (the layout
/// `build_effect_graph` always produces).
pub fn refresh_effect_params(
    graph: &mut Graph,
    instance: &EffectInstance,
) -> Result<(), EffectGraphError> {
    let metadata = metadata_by_id(instance.effect_type()).ok_or_else(|| {
        EffectGraphError::MissingMetadata {
            effect_type: instance.effect_type().as_str().to_string(),
        }
    })?;

    let effect_id = instance.effect_type().as_str();
    let mut prim_params: BTreeMap<String, SerializedParamValue> = BTreeMap::new();
    for (i, spec) in metadata.params.iter().enumerate() {
        if spec.id.is_empty() {
            continue;
        }
        let Some(primitive_name) = param_name_for_legacy(effect_id, spec.id) else {
            continue;
        };
        let raw = instance
            .param_values
            .get(i)
            .map(|slot| slot.value)
            .unwrap_or(spec.default_value);
        let value = transform_legacy_value(effect_id, spec.id, raw);
        prim_params.insert(
            primitive_name.to_string(),
            SerializedParamValue::Float { value },
        );
    }
    materialize_param_overrides(graph, &prim_params);
    Ok(())
}

/// Like [`refresh_effect_params`] but targets a specific
/// [`NodeInstanceId`] inside a larger graph. Used by the
/// chain-as-one-graph dispatch where the primitive lives at a
/// position that depends on chain layout.
pub fn refresh_effect_params_at(
    graph: &mut Graph,
    node_id: NodeInstanceId,
    instance: &EffectInstance,
) -> Result<(), EffectGraphError> {
    let metadata = metadata_by_id(instance.effect_type()).ok_or_else(|| {
        EffectGraphError::MissingMetadata {
            effect_type: instance.effect_type().as_str().to_string(),
        }
    })?;

    let effect_id = instance.effect_type().as_str();
    let mut prim_params: BTreeMap<String, SerializedParamValue> = BTreeMap::new();
    for (i, spec) in metadata.params.iter().enumerate() {
        if spec.id.is_empty() {
            continue;
        }
        let Some(primitive_name) = param_name_for_legacy(effect_id, spec.id) else {
            continue;
        };
        let raw = instance
            .param_values
            .get(i)
            .map(|slot| slot.value)
            .unwrap_or(spec.default_value);
        let value = transform_legacy_value(effect_id, spec.id, raw);
        prim_params.insert(
            primitive_name.to_string(),
            SerializedParamValue::Float { value },
        );
    }
    materialize_param_overrides_at(graph, node_id, &prim_params);
    Ok(())
}

/// One entry in a precompiled per-effect refresh plan. Captures
/// everything needed to convert a single legacy `param_values[idx]`
/// f32 into the primitive's expected `ParamValue` variant, with the
/// drift-table transform pre-resolved. Built once at chain-graph
/// construction; per-frame refresh is one `ParamValues::insert` per
/// entry — zero allocations, no string interning, no linear scans
/// over `metadata.params`.
#[derive(Debug, Clone, Copy)]
pub enum RefreshEntry {
    /// Pass-through f32 → `ParamValue::Float`.
    Float { idx: usize, name: &'static str },
    /// f32 → `ParamValue::Int(round)`.
    Int { idx: usize, name: &'static str },
    /// f32 → `ParamValue::Bool(>= 0.5)`.
    Bool { idx: usize, name: &'static str },
    /// f32 → `ParamValue::Enum(round.max(0))`.
    Enum { idx: usize, name: &'static str },
    /// Transform-effect rotation: `-(raw * π / 180)` → `Float`.
    TransformRot { idx: usize, name: &'static str },
    /// Strobe rate: `STROBE_NOTE_RATES[round(raw)]` → `Float`.
    StrobeRate { idx: usize, name: &'static str },
}

impl RefreshEntry {
    #[inline]
    fn lookup(&self) -> (usize, &'static str) {
        match *self {
            Self::Float { idx, name }
            | Self::Int { idx, name }
            | Self::Bool { idx, name }
            | Self::Enum { idx, name }
            | Self::TransformRot { idx, name }
            | Self::StrobeRate { idx, name } => (idx, name),
        }
    }
}

/// Ctx-derived param injection: which `EffectNodeContext::time` /
/// `beat` / `edge_stretch_width` field maps onto which primitive
/// parameter name. Built once at chain-graph construction.
#[derive(Debug, Clone, Copy)]
pub enum CtxEntry {
    Time(&'static str),
    Beat(&'static str),
    EdgeStretchWidth(&'static str),
}

/// Build the per-effect refresh plan for a primitive (or legacy
/// adapter) node, given its declared parameters and the metadata of
/// the legacy effect it stands in for. Runs at chain-graph build time
/// — typically once per topology change.
///
/// The plan captures: (1) the index into `EffectInstance.param_values`,
/// (2) the primitive-side param name (`&'static str` — no interning
/// at runtime), (3) the primitive-side `ParamType` (drives the
/// `f32 → ParamValue` coercion), and (4) which drift-table transform
/// applies (radians-from-degrees, strobe-rate-table-lookup).
///
/// Returns an empty plan if the effect has no metadata (effectively a
/// no-op refresh, same behavior as the BTreeMap-based variant on
/// missing metadata).
pub fn build_refresh_plan(
    effect_type: &EffectTypeId,
    node_params: &[ParamDef],
) -> Vec<RefreshEntry> {
    let Some(metadata) = metadata_by_id(effect_type) else {
        return Vec::new();
    };
    let effect_id = effect_type.as_str();
    let mut plan = Vec::with_capacity(metadata.params.len());
    for (i, spec) in metadata.params.iter().enumerate() {
        if spec.id.is_empty() {
            continue;
        }
        let Some(primitive_name) = param_name_for_legacy(effect_id, spec.id) else {
            continue;
        };
        let Some(param_def) = node_params.iter().find(|p| p.name == primitive_name) else {
            continue;
        };
        // Special transforms first (they only apply when the
        // primitive's param is Float-typed; the drift-table maps to
        // a continuous primitive value).
        let entry = match (effect_id, spec.id) {
            ("Transform", "rot") => RefreshEntry::TransformRot {
                idx: i,
                name: param_def.name,
            },
            ("Strobe", "rate") => RefreshEntry::StrobeRate {
                idx: i,
                name: param_def.name,
            },
            _ => match param_def.ty {
                ParamType::Float => RefreshEntry::Float {
                    idx: i,
                    name: param_def.name,
                },
                ParamType::Int => RefreshEntry::Int {
                    idx: i,
                    name: param_def.name,
                },
                ParamType::Bool => RefreshEntry::Bool {
                    idx: i,
                    name: param_def.name,
                },
                ParamType::Enum => RefreshEntry::Enum {
                    idx: i,
                    name: param_def.name,
                },
                // Vec*/Color — legacy effects don't store these in
                // `param_values`, so we skip the entry (primitive
                // keeps its declared default).
                _ => continue,
            },
        };
        plan.push(entry);
    }
    plan
}

/// Build the ctx-param injection plan for an effect. Returns the
/// primitive-side param names that the chain runtime should fill from
/// `EffectContext` each frame.
pub fn build_ctx_param_plan(effect_type: &EffectTypeId) -> Vec<CtxEntry> {
    match effect_type.as_str() {
        "Glitch" => vec![CtxEntry::Time("time")],
        "Strobe" => vec![CtxEntry::Beat("beat")],
        "VoronoiPrism" => vec![
            CtxEntry::Beat("beat"),
            CtxEntry::EdgeStretchWidth("source_width"),
        ],
        "Watercolor" => vec![CtxEntry::Time("time")],
        _ => Vec::new(),
    }
}

/// Apply a precompiled [`RefreshEntry`] plan to a node's parameter
/// map, plus a [`CtxEntry`] plan with the supplied per-frame values.
/// Zero allocations — direct `ParamValues::insert` writes on the
/// node's existing `AHashMap`.
///
/// The names captured in each entry came from the node's declared
/// `parameters()` at plan-build time, so existence is guaranteed and
/// the legacy `graph.set_param` validation pass is skipped here.
pub fn apply_refresh_plan(
    graph: &mut Graph,
    node_id: NodeInstanceId,
    refresh: &[RefreshEntry],
    ctx_plan: &[CtxEntry],
    instance: &EffectInstance,
    time: f32,
    beat: f32,
    edge_stretch_width: f32,
) {
    let Some(inst) = graph.get_node_mut(node_id) else {
        return;
    };
    for entry in refresh {
        let (idx, name) = entry.lookup();
        let raw = instance
            .param_values
            .get(idx)
            .map(|s| s.value)
            .unwrap_or(0.0);
        let pv = match entry {
            RefreshEntry::Float { .. } => ParamValue::Float(raw),
            RefreshEntry::Int { .. } => ParamValue::Int(raw.round() as i32),
            RefreshEntry::Bool { .. } => ParamValue::Bool(raw >= 0.5),
            RefreshEntry::Enum { .. } => ParamValue::Enum(raw.max(0.0).round() as u32),
            RefreshEntry::TransformRot { .. } => {
                ParamValue::Float(-(raw * std::f32::consts::PI / 180.0))
            }
            RefreshEntry::StrobeRate { .. } => {
                let i = raw.max(0.0).round() as usize;
                let v = crate::node_graph::primitives::STROBE_NOTE_RATES
                    .get(i)
                    .copied()
                    .unwrap_or_else(|| {
                        *crate::node_graph::primitives::STROBE_NOTE_RATES
                            .last()
                            .unwrap_or(&1.0)
                    });
                ParamValue::Float(v)
            }
        };
        inst.params.insert(name, pv);
    }
    for entry in ctx_plan {
        let (name, pv) = match *entry {
            CtxEntry::Time(name) => (name, ParamValue::Float(time)),
            CtxEntry::Beat(name) => (name, ParamValue::Float(beat)),
            CtxEntry::EdgeStretchWidth(name) => (name, ParamValue::Float(edge_stretch_width)),
        };
        inst.params.insert(name, pv);
    }
}

/// Apply context-derived primitive-only parameters onto the canonical
/// graph for `effect_type`. Some primitives expose params (`time`,
/// `beat`, `source_width`) that the legacy effect read directly from
/// `EffectContext` instead of from its param list; the graph runtime
/// dispatch needs to inject those values explicitly each frame so the
/// shader sees the same clock the legacy path did.
///
/// Audited against each parity test's `set_params` closure — those
/// closures wire ctx-derived values straight onto the primitive (e.g.,
/// `graph.set_param(prim_id, "time", ParamValue::Float(ctx.time))`)
/// and are the source of truth for what each primitive expects.
pub fn apply_ctx_params(
    graph: &mut Graph,
    effect_type: &EffectTypeId,
    time: f32,
    beat: f32,
    edge_stretch_width: f32,
) {
    apply_ctx_params_at(
        graph,
        NodeInstanceId(1),
        effect_type,
        time,
        beat,
        edge_stretch_width,
    );
}

/// Like [`apply_ctx_params`] but targets a specific [`NodeInstanceId`]
/// inside a larger graph. Used by the chain-as-one-graph dispatch.
pub fn apply_ctx_params_at(
    graph: &mut Graph,
    node_id: NodeInstanceId,
    effect_type: &EffectTypeId,
    time: f32,
    beat: f32,
    edge_stretch_width: f32,
) {
    let mut set = |name: &'static str, value: ParamValue| {
        // Ignore errors — a primitive that doesn't declare `name`
        // means the bridge over-listed ctx params; the primitive
        // keeps its declared default in that case. Legacy adapter
        // nodes never declare these primitive-only names, so the
        // call is a no-op there.
        let _ = graph.set_param(node_id, name, value);
    };

    match effect_type.as_str() {
        "Glitch" => {
            set("time", ParamValue::Float(time));
        }
        "Strobe" => {
            set("beat", ParamValue::Float(beat));
        }
        "VoronoiPrism" => {
            set("beat", ParamValue::Float(beat));
            set("source_width", ParamValue::Float(edge_stretch_width));
        }
        "Watercolor" => {
            set("time", ParamValue::Float(time));
        }
        _ => {}
    }
}

/// Translate a legacy `EffectInstance.param_values[i].value` (always
/// `f32` on the wire) into the value the primitive expects. Most
/// params pass through unchanged; a handful of legacy effects did
/// unit conversions inside their pre-GPU bookkeeping that the
/// primitive's WGSL doesn't replicate. Each entry below is the
/// inverse of what the legacy `apply` body did before encoding its
/// uniform.
fn transform_legacy_value(effect_type: &str, legacy_id: &str, raw: f32) -> f32 {
    match (effect_type, legacy_id) {
        // `TransformFX` reads `rot` as degrees, applies
        // `-rot * PI / 180`, then writes the rotation uniform.
        // `node.affine_transform` takes rotation straight in
        // radians (Y-down baked in via the negation), so the bridge
        // does the same conversion at the boundary.
        ("Transform", "rot") => -(raw * std::f32::consts::PI / 180.0),
        // `StrobeFX` stores `rate` as an index into its NOTE_RATES
        // table (0..9). `node.strobe` takes the raw rate value
        // (strobes-per-beat) so the bridge indexes the table here.
        ("Strobe", "rate") => {
            let idx = raw.max(0.0).round() as usize;
            primitives::STROBE_NOTE_RATES
                .get(idx)
                .copied()
                .unwrap_or(*primitives::STROBE_NOTE_RATES.last().unwrap_or(&1.0))
        }
        _ => raw,
    }
}

/// Apply a `f32`-keyed param map onto the primitive at node id=1 with
/// `ParamType`-driven coercion. Unknown keys are skipped silently —
/// same policy as the legacy chain, which would also ignore drift
/// between effect metadata and the live processor.
fn materialize_param_overrides(graph: &mut Graph, params: &BTreeMap<String, SerializedParamValue>) {
    materialize_param_overrides_at(graph, NodeInstanceId(1), params);
}

fn materialize_param_overrides_at(
    graph: &mut Graph,
    prim_id: NodeInstanceId,
    params: &BTreeMap<String, SerializedParamValue>,
) {
    let inst = match graph.get_node(prim_id) {
        Some(inst) => inst,
        None => return,
    };

    let typed: Vec<(&'static str, ParamType)> = inst
        .node
        .parameters()
        .iter()
        .map(|p| (p.name, p.ty))
        .collect();

    for (key, sv) in params {
        let Some(&(name_static, ty)) = typed.iter().find(|(n, _)| *n == key.as_str()) else {
            continue;
        };
        let pv = match (sv, ty) {
            // Canonical doc only emits Float; coerce based on the
            // primitive's declared type.
            (SerializedParamValue::Float { value }, ParamType::Float) => ParamValue::Float(*value),
            (SerializedParamValue::Float { value }, ParamType::Int) => {
                ParamValue::Int(value.round() as i32)
            }
            (SerializedParamValue::Float { value }, ParamType::Bool) => {
                ParamValue::Bool(*value >= 0.5)
            }
            (SerializedParamValue::Float { value }, ParamType::Enum) => {
                let clamped = value.max(0.0).round();
                ParamValue::Enum(clamped as u32)
            }
            // Legacy effects never store Vec*/Color in `param_values`,
            // so we don't have a sensible coercion. Keep the primitive
            // default.
            (
                SerializedParamValue::Float { .. },
                ParamType::Vec2 | ParamType::Vec3 | ParamType::Vec4 | ParamType::Color,
            ) => continue,
            // Future migration may emit typed entries directly.
            (other, _) => (*other).into(),
        };
        graph
            .set_param(prim_id, name_static, pv)
            .expect("name resolved from inst.parameters()");
    }
}

fn strip_node_params(mut doc: GraphDocument, node_id: u32) -> GraphDocument {
    if let Some(node) = doc.nodes.iter_mut().find(|n| n.id == node_id) {
        node.params.clear();
    }
    doc
}

/// Every primitive in the legacy-effect mapping names its texture
/// input `"in"` — both the macro-authored atomics and the
/// hand-authored fused composites / monolithic wrappers agreed on
/// that convention. (The early `node.threshold` / `node.blur`
/// generation used `"source"`, but those primitives aren't in the
/// legacy mapping.)
const LEGACY_PRIMITIVE_INPUT_PORT: &str = "in";

/// Translate a legacy `EffectMetadata.params[i].id` onto the matching
/// primitive's param name. Most legacy ids pass straight through —
/// effects authored after the §6.2 ParamSpec migration tend to use
/// the same name as their primitive. The drift cases below were
/// introduced in §6.1 when the macro-authored atomic primitives used
/// more descriptive names (`block` → `block_size`, `algo` →
/// `algorithm`, `segs` → `segments`, …).
///
/// Returns `None` when the legacy effect names a param the primitive
/// doesn't expose. In that case the bridge keeps the primitive's
/// declared default and skips the slot.
fn param_name_for_legacy(effect_type: &str, legacy_id: &str) -> Option<&'static str> {
    match (effect_type, legacy_id) {
        // Invert — first §6.1 primitive, renamed `amount` to
        // `intensity` to match the legacy WGSL uniform.
        ("InvertColors", "amount") => Some("intensity"),

        // AffineTransform — re-spelled for clarity, since the
        // legacy effect's `x`/`y`/`zoom`/`rot` were one-letter for
        // historical reasons.
        ("Transform", "x") => Some("translate_x"),
        ("Transform", "y") => Some("translate_y"),
        ("Transform", "zoom") => Some("scale"),
        ("Transform", "rot") => Some("rotation"),

        // ClampStretch / EdgeStretch.
        ("EdgeStretch", "width") => Some("source_width"),
        ("EdgeStretch", "dir") => Some("mode"),

        // ColorGrade — long-form rename for human-readability.
        ("ColorGrade", "sat") => Some("saturation"),
        ("ColorGrade", "tint_hue") => Some("colorize_hue"),
        ("ColorGrade", "tint_sat") => Some("colorize_saturation"),
        ("ColorGrade", "focus") => Some("colorize_focus"),

        // DitherPattern / Dither.
        ("Dither", "algo") => Some("algorithm"),

        // EdgeDetect / EdgeGlow.
        ("EdgeGlow", "thresh") => Some("threshold"),
        // Legacy `mode` is a binary-only switch that the primitive
        // collapsed into the always-on shader path; drop silently.
        ("EdgeGlow", "mode") => None,

        // Glitch — block size + clarity rename.
        ("Glitch", "block") => Some("block_size"),

        // HighlightBoost / HdrBoost.
        ("HdrBoost", "thresh") => Some("threshold"),

        // KaleidoFold / Kaleidoscope.
        ("Kaleidoscope", "segs") => Some("segments"),

        // VoronoiPrism.
        ("VoronoiPrism", "cells") => Some("cell_count"),

        // Halation — long-form rename matching ColorGrade's
        // `sat` → `saturation` convention.
        ("Halation", "thresh") => Some("threshold"),
        ("Halation", "sat") => Some("saturation"),

        // No drift — legacy id matches primitive name verbatim.
        _ => Some(static_str_passthrough(legacy_id)),
    }
}

/// Lift a `&str` onto `&'static str` by interning through the
/// `static_str` table — we can't return `legacy_id` directly because
/// its lifetime is bound to the caller. The set of legitimate passes
/// through `param_name_for_legacy` is finite and known at compile
/// time (every legacy `ParamSpec::id` is a `&'static str` in
/// inventory), but the function takes `&str` for ergonomic match
/// arms, so we re-resolve to the static via a small table.
fn static_str_passthrough(s: &str) -> &'static str {
    // The pass-through case is dominated by a handful of common names.
    // For anything not in the small table, fall back to a leak — the
    // amount of leakage is bounded by the number of distinct legacy
    // param ids across all shipping effects (~50). Acceptable for a
    // table that runs once per effect instance load, never per frame.
    match s {
        "amount" => "amount",
        "offset" => "offset",
        "mode" => "mode",
        "angle" => "angle",
        "falloff" => "falloff",
        "thresh" => "thresh",
        "smooth" => "smooth",
        "connect" => "connect",
        "sens" => "sens",
        "blur" => "blur",
        "decay" => "decay",
        "displace" => "displace",
        "density" => "density",
        "width" => "width",
        "z_scale" => "z_scale",
        "subject" => "subject",
        "blend" => "blend",
        "wire_res" => "wire_res",
        "mesh_rate" => "mesh_rate",
        "flow" => "flow",
        "lock" => "lock",
        "edge_follow" => "edge_follow",
        "spread" => "spread",
        "hue" => "hue",
        "sat" => "sat",
        "ratio" => "ratio",
        "punch" => "punch",
        "target" => "target",
        "hdr_ret" => "hdr_ret",
        "color" => "color",
        "char" => "char",
        "focus" => "focus",
        "focus_x" => "focus_x",
        "quality" => "quality",
        "gain" => "gain",
        "knee" => "knee",
        "contrast" => "contrast",
        "rate" => "rate",
        "rgb_shift" => "rgb_shift",
        "scanline" => "scanline",
        "speed" => "speed",
        _ => Box::leak(s.to_string().into_boxed_str()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_definition_registry;
    use manifold_core::effects::EffectInstance;

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    fn make_default(type_id: EffectTypeId) -> EffectInstance {
        effect_definition_registry::create_default(&type_id)
    }

    #[test]
    fn every_shipped_effect_type_resolves_to_a_primitive() {
        let cases: &[EffectTypeId] = &[
            EffectTypeId::INVERT_COLORS,
            EffectTypeId::TRANSFORM,
            EffectTypeId::CHROMATIC_ABERRATION,
            EffectTypeId::EDGE_STRETCH,
            EffectTypeId::COLOR_GRADE,
            EffectTypeId::DITHER,
            EffectTypeId::EDGE_DETECT,
            EffectTypeId::GLITCH,
            EffectTypeId::HDR_BOOST,
            EffectTypeId::KALEIDOSCOPE,
            EffectTypeId::INFRARED,
            EffectTypeId::STROBE,
            EffectTypeId::VORONOI_PRISM,
            EffectTypeId::BLOOM,
            EffectTypeId::HALATION,
            EffectTypeId::WATERCOLOR,
            EffectTypeId::DEPTH_OF_FIELD,
            EffectTypeId::AUTO_GAIN,
            EffectTypeId::BLOB_TRACKING,
            EffectTypeId::WIREFRAME_DEPTH,
        ];
        let reg = registry();
        for ty in cases {
            let prim_id = primitive_id_for_effect(ty)
                .unwrap_or_else(|| panic!("no primitive mapping for {ty:?}"));
            assert!(
                reg.contains(prim_id),
                "primitive '{prim_id}' (for {ty:?}) not in PrimitiveRegistry"
            );
        }
    }

    #[test]
    fn build_effect_graph_for_invert_produces_three_node_shape() {
        let mut inst = make_default(EffectTypeId::INVERT_COLORS);
        inst.param_values[0].value = 0.7;
        let g = build_effect_graph(&inst, &registry()).unwrap();
        assert_eq!(g.node_count(), 3);
        assert_eq!(g.wires().len(), 2);

        let prim = g.get_node(NodeInstanceId(1)).unwrap();
        assert_eq!(
            prim.node.type_id().as_str(),
            <primitives::Invert as PrimitiveSpec>::TYPE_ID
        );
        assert_eq!(
            prim.params.get("intensity").copied().unwrap(),
            ParamValue::Float(0.7)
        );
    }

    #[test]
    fn build_effect_graph_for_wireframe_depth_coerces_enum_params() {
        // WireframeDepth has Float + Enum params side-by-side. Legacy
        // stores both as f32; the primitive wants Enum for Blend /
        // MeshRate / Flow / Lock.
        let mut inst = make_default(EffectTypeId::WIREFRAME_DEPTH);
        inst.param_values[0].value = 1.0; // amount (Float)
        inst.param_values[6].value = 3.0; // blend = "Screen" (Enum)
        inst.param_values[9].value = 0.0; // flow = "Off" (Enum)

        let g = build_effect_graph(&inst, &registry()).unwrap();
        let prim = g.get_node(NodeInstanceId(1)).unwrap();
        assert_eq!(
            prim.params.get("amount").copied().unwrap(),
            ParamValue::Float(1.0)
        );
        assert_eq!(
            prim.params.get("blend").copied().unwrap(),
            ParamValue::Enum(3)
        );
        assert_eq!(
            prim.params.get("flow").copied().unwrap(),
            ParamValue::Enum(0)
        );
    }

    #[test]
    fn build_effect_graph_with_unsupported_type_is_clean_error() {
        // QUAD_MIRROR is in EffectTypeId but has no primitive mapping
        // (removed effect). Build should report it cleanly.
        let inst = EffectInstance::new(EffectTypeId::QUAD_MIRROR);
        let err = match build_effect_graph(&inst, &registry()) {
            Ok(_) => panic!("expected UnsupportedEffectType, got Ok(Graph)"),
            Err(e) => e,
        };
        match err {
            EffectGraphError::UnsupportedEffectType { effect_type } => {
                assert_eq!(effect_type, "QuadMirror");
            }
            other => panic!("expected UnsupportedEffectType, got {other:?}"),
        }
    }

    #[test]
    fn canonical_document_uses_in_port_for_fused_composite() {
        let inst = make_default(EffectTypeId::BLOOM);
        let doc = canonical_document_for(&inst).expect("bloom resolves");
        let to_prim = doc
            .wires
            .iter()
            .find(|w| w.to_node == 1)
            .expect("wire into primitive present");
        assert_eq!(to_prim.to_port, "in");
    }

    #[test]
    fn canonical_document_uses_in_port_for_atomic_primitive() {
        let inst = make_default(EffectTypeId::INVERT_COLORS);
        let doc = canonical_document_for(&inst).expect("invert resolves");
        let to_prim = doc
            .wires
            .iter()
            .find(|w| w.to_node == 1)
            .expect("wire into primitive present");
        // Macro-authored atomic primitives all use `"in"` as their
        // texture-input port name — matches the fused composites.
        assert_eq!(to_prim.to_port, "in");
    }

    #[test]
    fn build_effect_graph_resulting_graph_is_valid_and_compilable() {
        use crate::node_graph::{compile, validate};
        let inst = make_default(EffectTypeId::HALATION);
        let g = build_effect_graph(&inst, &registry()).unwrap();
        validate(&g).expect("canonical graph validates");
        let plan = compile(&g).expect("canonical graph compiles");
        assert_eq!(plan.steps().len(), 3);
    }

    #[test]
    fn canonical_document_carries_display_name() {
        let inst = make_default(EffectTypeId::BLOOM);
        let doc = canonical_document_for(&inst).expect("bloom resolves");
        assert!(doc.name.is_some());
    }

    #[test]
    fn legacy_param_drift_renames_resolve_to_primitive_param_names() {
        // Spot-check a few of the rename arms. Each (legacy, primitive)
        // pair below was authored by audit — change carefully.
        assert_eq!(
            param_name_for_legacy("InvertColors", "amount"),
            Some("intensity")
        );
        assert_eq!(param_name_for_legacy("Transform", "zoom"), Some("scale"));
        assert_eq!(
            param_name_for_legacy("ColorGrade", "tint_hue"),
            Some("colorize_hue")
        );
        assert_eq!(
            param_name_for_legacy("EdgeGlow", "thresh"),
            Some("threshold")
        );
        assert_eq!(
            param_name_for_legacy("Kaleidoscope", "segs"),
            Some("segments")
        );
        // Pass-through: no rename → returns the input name.
        assert_eq!(param_name_for_legacy("Bloom", "amount"), Some("amount"));
        // Dropped: legacy param without a primitive counterpart.
        // `Infrared.palette` now maps to `node.infrared.palette`
        // (after the §6.6 #5 monolithic wrapper landed).
        assert_eq!(
            param_name_for_legacy("Infrared", "palette"),
            Some("palette")
        );
        assert_eq!(param_name_for_legacy("EdgeGlow", "mode"), None);
    }

    #[test]
    fn every_legacy_param_resolves_to_a_real_primitive_param() {
        // For each shipping legacy effect, walk its EffectMetadata
        // params, resolve to primitive names, and assert each name
        // exists on the primitive. Catches drift introduced by renaming
        // a primitive param without updating `param_name_for_legacy`.
        let cases: &[EffectTypeId] = &[
            EffectTypeId::INVERT_COLORS,
            EffectTypeId::TRANSFORM,
            EffectTypeId::CHROMATIC_ABERRATION,
            EffectTypeId::EDGE_STRETCH,
            EffectTypeId::COLOR_GRADE,
            EffectTypeId::DITHER,
            EffectTypeId::EDGE_DETECT,
            EffectTypeId::GLITCH,
            EffectTypeId::HDR_BOOST,
            EffectTypeId::KALEIDOSCOPE,
            EffectTypeId::INFRARED,
            EffectTypeId::STROBE,
            EffectTypeId::VORONOI_PRISM,
            EffectTypeId::BLOOM,
            EffectTypeId::HALATION,
            EffectTypeId::WATERCOLOR,
            EffectTypeId::DEPTH_OF_FIELD,
            EffectTypeId::AUTO_GAIN,
            EffectTypeId::BLOB_TRACKING,
            EffectTypeId::WIREFRAME_DEPTH,
        ];

        let reg = registry();
        for ty in cases {
            let metadata =
                metadata_by_id(ty).unwrap_or_else(|| panic!("no metadata for {}", ty.as_str()));
            let prim_id = primitive_id_for_effect(ty)
                .unwrap_or_else(|| panic!("no primitive for {}", ty.as_str()));
            let boxed = reg
                .construct(prim_id)
                .unwrap_or_else(|| panic!("no constructor for {prim_id}"));
            let names: std::collections::HashSet<&'static str> =
                boxed.parameters().iter().map(|p| p.name).collect();

            for spec in metadata.params {
                if spec.id.is_empty() {
                    continue;
                }
                let Some(prim_name) = param_name_for_legacy(ty.as_str(), spec.id) else {
                    // Drift table explicitly dropped this legacy param;
                    // nothing to verify on the primitive side.
                    continue;
                };
                assert!(
                    names.contains(prim_name),
                    "{}: legacy param '{}' maps to primitive name '{}' \
                     which doesn't exist on primitive '{}' (available: {:?})",
                    ty.as_str(),
                    spec.id,
                    prim_name,
                    prim_id,
                    names,
                );
            }
        }
    }

    #[test]
    fn drifted_legacy_param_value_lands_on_renamed_primitive_param() {
        // ColorGrade.sat (legacy) → ColorGrade.saturation (primitive).
        let mut inst = make_default(EffectTypeId::COLOR_GRADE);
        // The metadata for ColorGrade puts `sat` at position 2.
        let metadata = metadata_by_id(&EffectTypeId::COLOR_GRADE).unwrap();
        let sat_idx = metadata
            .params
            .iter()
            .position(|p| p.id == "sat")
            .expect("`sat` slot present in ColorGrade metadata");
        inst.param_values[sat_idx].value = 0.42;

        let g = build_effect_graph(&inst, &registry()).unwrap();
        let prim = g.get_node(NodeInstanceId(1)).unwrap();
        // The primitive sees `saturation`, not `sat`.
        assert_eq!(
            prim.params.get("saturation").copied().unwrap(),
            ParamValue::Float(0.42)
        );
        assert!(prim.params.get("sat").is_none());
    }
}
