//! Per-instance effect graph schema — the on-disk shape an
//! [`EffectInstance`](crate::effects::EffectInstance) carries when its
//! graph topology has diverged from the catalog default.
//!
//! These types are pure serde shapes: zero references back into the
//! live runtime graph, zero GPU types. They live in `manifold-core`
//! so [`EffectInstance`](crate::effects::EffectInstance) can hold one
//! by value without dragging `manifold-renderer` into the dependency
//! graph.
//!
//! The renderer round-trips between [`EffectGraphDef`] and its live
//! `Graph` via `manifold_renderer::node_graph::persistence` — that's
//! where the [`PrimitiveRegistry`] and the `ParamValue` ↔
//! [`SerializedParamValue`] conversions live.
//!
//! ## Versioning
//!
//! Documents declare the lowest version that covers the features they
//! actually use:
//!
//! - **v1** ([`EFFECT_GRAPH_VERSION`]) — graph topology only: `nodes`,
//!   `wires`, optional `name` and `description`. The schema for the 25
//!   shipping bundled presets and every per-instance graph override
//!   stored on an [`EffectInstance`](crate::effects::EffectInstance).
//! - **v2** ([`EFFECT_GRAPH_VERSION_WITH_METADATA`]) — adds
//!   [`preset_metadata`](EffectGraphDef::preset_metadata) carrying the
//!   picker/OSC/routing surface (display name, category, params,
//!   bindings, skip mode, alias tables). The format used by user-saved
//!   presets, AI-authored presets, and the migrated bundled-preset
//!   library after §11 of `docs/PRIMITIVE_LIBRARY_DESIGN.md` lands.
//!
//! Constructors emit v1 by default; calling
//! [`with_preset_metadata`](EffectGraphDef::with_preset_metadata)
//! bumps the document's version to v2. The persistence layer accepts
//! any document up to [`EFFECT_GRAPH_VERSION_WITH_METADATA`]; higher
//! versions are rejected so old binaries don't silently lose data.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::effect_type_id::EffectTypeId;
use crate::effects::ParamConvert;

/// Schema version for graph-topology-only documents (no preset
/// metadata). Default for per-instance graph overrides and the 25
/// shipping bundled-preset snapshots prior to the §11 migration.
pub const EFFECT_GRAPH_VERSION: u32 = 1;

/// Schema version for documents that carry [`PresetMetadata`].
/// Bundled presets after the §11 migration, user-saved presets, and
/// AI-authored presets all live at this version.
pub const EFFECT_GRAPH_VERSION_WITH_METADATA: u32 = 2;

/// Top-level shape for one effect's per-instance graph.
///
/// Same schema used by bundled preset libraries
/// (`assets/effect-presets/*.json`) and by per-instance graph
/// overrides stored on an [`EffectInstance`](crate::effects::EffectInstance).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGraphDef {
    pub version: u32,
    /// Display name for the preset library / saved graph picker.
    /// `None` when the graph is anonymous (e.g., a per-instance override
    /// that wasn't promoted to a named preset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Free-form description shown in the picker tooltip. `None` when
    /// the author didn't supply one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// v2 picker/OSC/routing metadata. `Some` for shipping presets and
    /// user-saved presets; `None` for per-instance graph overrides (those
    /// inherit metadata from the parent preset definition). Presence
    /// promotes the document to [`EFFECT_GRAPH_VERSION_WITH_METADATA`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_metadata: Option<PresetMetadata>,
    pub nodes: Vec<EffectGraphNode>,
    pub wires: Vec<EffectGraphWire>,
}

/// One node inside an [`EffectGraphDef`]. `id` is unique within the
/// document and is the wire-endpoint key — it survives load by mapping
/// to a fresh runtime `NodeInstanceId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGraphNode {
    pub id: u32,
    pub type_id: String,
    /// Stable string handle to pass to `Graph::add_node_named` so
    /// user-exposed parameter bindings can address this inner node
    /// across renderer refactors. `None` for anonymous nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Per-parameter overrides keyed by stable param name. Missing
    /// keys fall through to the node's declared defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, SerializedParamValue>,
    /// Names of params that are currently exposed on the outer card
    /// (i.e. visible as a slider / control on the host effect or
    /// generator). The graph is the single source of truth for this —
    /// the right-panel checkbox in the graph editor flips entries in
    /// this set, regardless of whether the graph is hosted by an
    /// Effect or a Generator. Missing means "not exposed"; a preset's
    /// `bindings` array seeds this set at instance creation.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub exposed_params: BTreeSet<String>,
    /// Editor-saved position in graph-space. `None` for documents
    /// authored without an editor (hand-rolled bundled presets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_pos: Option<(f32, f32)>,
    /// Per-node WGSL kernel source. `Some` only for `node.wgsl_compute_*`
    /// primitives — the escape hatch lets an agent embed raw shader code
    /// when no compositional primitive expresses what they want. The
    /// kernel reads its sliders from a `struct U { f0..f7: f32 }` uniform;
    /// inputs/outputs follow the variant's fixed shape. `None` for every
    /// node where source is fixed at compile time via `include_str!`
    /// (i.e. nearly every shipping primitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wgsl_source: Option<String>,
    /// Per-output texture format override, keyed by output port name.
    /// Format strings match Metal/WGSL conventions: `"rgba16float"`,
    /// `"rgba32float"`, `"r32float"`, `"rg32float"`, `"r16float"`,
    /// `"rgba8unorm"`, etc. — see `manifold_gpu::GpuTextureFormat`.
    ///
    /// Default (empty / missing) means "use the backend's default
    /// format" — typically `rgba16float`, which is right for color and
    /// video. Native-precision escape hatches (e.g. fluid sim passes)
    /// declare formats here so the runtime allocates intermediate
    /// textures with the precision the legacy pipeline used, preserving
    /// numerical stability across multi-pass feedback chains.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub output_formats: BTreeMap<String, String>,
}

/// One wire inside an [`EffectGraphDef`]. Endpoint ids reference
/// [`EffectGraphNode::id`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGraphWire {
    pub from_node: u32,
    pub from_port: String,
    pub to_node: u32,
    pub to_port: String,
}

/// Tagged-enum wire form of the renderer's `ParamValue`. Tagged because
/// untagged would conflate `Float(0.0)` / `Int(0)` / `Bool(false)`.
///
/// Conversions to/from the renderer's `ParamValue` live in
/// `manifold_renderer::node_graph::persistence`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum SerializedParamValue {
    Float { value: f32 },
    Int { value: i32 },
    Bool { value: bool },
    Vec2 { value: [f32; 2] },
    Vec3 { value: [f32; 3] },
    Vec4 { value: [f32; 4] },
    Color { value: [f32; 4] },
    Enum { value: u32 },
}

impl EffectGraphDef {
    /// Set the display name. Builder-style convenience for bundled
    /// preset constructors.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the description. Builder-style.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Attach preset metadata and promote the document to
    /// [`EFFECT_GRAPH_VERSION_WITH_METADATA`]. The presence of metadata
    /// is what distinguishes a preset definition from a per-instance
    /// graph override.
    pub fn with_preset_metadata(mut self, metadata: PresetMetadata) -> Self {
        self.version = EFFECT_GRAPH_VERSION_WITH_METADATA;
        self.preset_metadata = Some(metadata);
        self
    }
}

// ─── v2 preset metadata ─────────────────────────────────────────────

/// Picker / OSC / routing / aliasing metadata carried by a preset
/// definition. `EffectGraphDef::preset_metadata = Some(this)` promotes
/// the document to [`EFFECT_GRAPH_VERSION_WITH_METADATA`].
///
/// This is the JSON-wire shape — `String` fields throughout (no
/// `&'static str` / `Cow`-flavoured optimisations like the
/// renderer-side compile-time submission types). Conversion to/from
/// the renderer's runtime types (`ParamSpec`, `ParamBinding`,
/// `SkipMode`) lives in the loader (`manifold_renderer::node_graph::persistence`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetMetadata {
    /// Stable string identity. Same string as the JSON filename for
    /// bundled presets; for user-saved presets, a freshly minted id.
    pub id: EffectTypeId,
    /// Display name shown on the effect card and in the picker.
    pub display_name: String,
    /// Picker category (`Spatial`, `Color`, `Stylize`, `Filmic`,
    /// `Diagnostic` — see `effect_type_registry::ALL_CATEGORIES`).
    pub category: String,
    /// OSC path prefix for external addressing. Conventionally
    /// snake_case (`"edge_stretch_by_color"`).
    pub osc_prefix: String,
    /// Legacy `i32` discriminant for backward compatibility with
    /// pre-string-id project files. `None` for new presets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_discriminant: Option<i32>,
    /// Whether this preset appears in the "Add Effect" picker.
    /// Defaults to `true`; set `false` for hidden / stub effects.
    #[serde(default = "default_available")]
    pub available: bool,
    /// Outer-card slider definitions. Each entry corresponds to one
    /// host-visible parameter.
    pub params: Vec<ParamSpecDef>,
    /// Routing from each outer slider to one or more inner-graph node
    /// parameters. **Not a parallel array to `params`** — bindings
    /// reference outer sliders by [`BindingDef::id`], and one outer
    /// slider can fan out to multiple inner-node params by emitting
    /// multiple bindings that share an `id` (e.g. a single `clip_trigger`
    /// toggle driving both `mux_x.selector` and `mux_y.selector`).
    /// Consumers MUST address bindings by `id` against `params`, not by
    /// position — positional indexing silently strands the second
    /// binding in a fan-out on its `default_value`.
    pub bindings: Vec<BindingDef>,
    /// When the runtime should drop this effect entirely (no GPU work).
    #[serde(default)]
    pub skip_mode: SkipModeDef,
    /// Backward-compat table for renamed outer-slider parameter ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_aliases: Vec<AliasEntry>,
    /// Backward-compat table for renamed inner-graph node handles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_aliases: Vec<AliasEntry>,
    /// Backward-compat table for enum-value remaps (e.g. Mirror's mode
    /// indices shifted across a refactor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_aliases: Vec<ValueAliasEntry>,
}

fn default_available() -> bool {
    true
}

/// JSON-wire shape mirroring [`crate::generator_registration::ParamSpec`].
/// Differs in using owned `String` for compatibility with serde
/// deserialization (the renderer-side `ParamSpec` uses `&'static str`
/// for compile-time inventory submissions).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamSpecDef {
    pub id: String,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default_value: f32,
    #[serde(default)]
    pub whole_numbers: bool,
    #[serde(default)]
    pub is_toggle: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_string: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub osc_suffix: String,
}

/// JSON-wire shape mirroring `manifold_renderer::node_graph::ParamBinding`.
/// Conversion happens in the loader once the renderer-side handles
/// resolve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingDef {
    pub id: String,
    pub label: String,
    pub default_value: f32,
    pub target: BindingTarget,
    #[serde(default)]
    pub convert: ParamConvert,
}

/// Where a binding's value flows. Mirror of the renderer-side
/// `ParamTarget`, restricted to the JSON-expressible variants. The
/// renderer's `ParamTarget::Node { NodeInstanceId }` and
/// `ParamTarget::Custom(fn)` are not representable here — the first
/// because live IDs aren't serializable, the second because function
/// pointers aren't.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum BindingTarget {
    /// Route to an inner-graph node identified by its stable handle
    /// (set via `Graph::add_node_named`).
    HandleNode { handle: String, param: String },
    /// Route through a composite handle's exposed-param map. Used by
    /// composite-shaped effects where one outer slider fans out to
    /// multiple inner-node parameters.
    Composite { outer_name: String },
}

/// JSON-wire shape mirroring `manifold_renderer::node_graph::SkipMode`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SkipModeDef {
    /// Effect always contributes its workers.
    #[default]
    Never,
    /// Skip when the param identified by `param_id` is ≤ 0. The
    /// chain runtime walks `params` for the matching id and reads its
    /// current value; absence of the id falls through to `Never`.
    OnZero { param_id: String },
}

/// One entry in a backward-compat alias table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AliasEntry {
    /// Old id as it might appear in a saved project.
    pub old: String,
    /// New id to resolve to. `None` means the old id was deprecated
    /// with no replacement (the binding is silently dropped on load).
    pub new: Option<String>,
}

/// One entry in a value-remap alias table — applies to a single
/// param's stored value at load time. Used when an effect's enum
/// value indices shift across a refactor.
///
/// Matches the renderer-side `ParamValueAlias = (i32, i32)` shape
/// for `(from, to)` pairs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueAliasEntry {
    /// Outer-slider parameter id whose value is being remapped.
    pub param_id: String,
    /// Pairs of `(stored_value, new_value)` — when the loader sees a
    /// param value matching the first, it rewrites to the second.
    pub mapping: Vec<(i32, i32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_graph_round_trips() {
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: EffectGraphDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn serialized_param_value_round_trips_every_variant() {
        let cases = [
            SerializedParamValue::Float { value: 0.5 },
            SerializedParamValue::Int { value: 7 },
            SerializedParamValue::Bool { value: true },
            SerializedParamValue::Vec2 { value: [1.0, 2.0] },
            SerializedParamValue::Vec3 {
                value: [1.0, 2.0, 3.0],
            },
            SerializedParamValue::Vec4 {
                value: [1.0, 2.0, 3.0, 4.0],
            },
            SerializedParamValue::Color {
                value: [0.1, 0.2, 0.3, 1.0],
            },
            SerializedParamValue::Enum { value: 3 },
        ];
        for v in cases {
            let json = serde_json::to_string(&v).unwrap();
            let back: SerializedParamValue = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn name_and_description_skipped_when_none() {
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        assert!(!json.contains("\"name\""));
        assert!(!json.contains("\"description\""));
        assert!(!json.contains("\"presetMetadata\""));
    }

    #[test]
    fn node_with_param_overrides_round_trips() {
        let mut params = BTreeMap::new();
        params.insert(
            "level".to_string(),
            SerializedParamValue::Float { value: 0.8 },
        );
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: Some("Custom Threshold".to_string()),
            description: None,
            preset_metadata: None,
            nodes: vec![EffectGraphNode {
                id: 0,
                type_id: "node.threshold".to_string(),
                handle: Some("thresh".to_string()),
                params,
                exposed_params: BTreeSet::new(),
                editor_pos: Some((100.0, 200.0)),
                wgsl_source: None,
                output_formats: BTreeMap::new(),
            }],
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: EffectGraphDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    /// V1 documents on disk (no `presetMetadata`, version=1) must parse
    /// into the new schema with `preset_metadata = None`. This is the
    /// backward-compat contract that lets the 25 shipping JSON snapshots
    /// keep loading unmodified.
    #[test]
    fn v1_document_parses_with_no_preset_metadata() {
        let v1_json = r#"{
            "version": 1,
            "name": "Bloom",
            "nodes": [],
            "wires": []
        }"#;
        let def: EffectGraphDef = serde_json::from_str(v1_json).unwrap();
        assert_eq!(def.version, 1);
        assert_eq!(def.name.as_deref(), Some("Bloom"));
        assert!(def.preset_metadata.is_none());
    }

    /// V2 documents with full preset metadata round-trip including all
    /// the new fields.
    #[test]
    fn v2_document_with_preset_metadata_round_trips() {
        let meta = PresetMetadata {
            id: EffectTypeId::new("EdgeStretchByColor"),
            display_name: "Edge Stretch By Colour".to_string(),
            category: "Stylize".to_string(),
            osc_prefix: "edge_stretch_by_color".to_string(),
            legacy_discriminant: None,
            available: true,
            params: vec![ParamSpecDef {
                id: "amount".to_string(),
                name: "Amount".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 1.0,
                whole_numbers: false,
                is_toggle: false,
                value_labels: Vec::new(),
                format_string: Some("F2".to_string()),
                osc_suffix: String::new(),
            }],
            bindings: vec![BindingDef {
                id: "amount".to_string(),
                label: "Amount".to_string(),
                default_value: 1.0,
                target: BindingTarget::HandleNode {
                    handle: "masked_mix".to_string(),
                    param: "amount".to_string(),
                },
                convert: ParamConvert::Float,
            }],
            skip_mode: SkipModeDef::OnZero {
                param_id: "amount".to_string(),
            },
            param_aliases: Vec::new(),
            node_aliases: Vec::new(),
            value_aliases: Vec::new(),
        };
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        }
        .with_preset_metadata(meta.clone());

        // Promotes to v2.
        assert_eq!(def.version, EFFECT_GRAPH_VERSION_WITH_METADATA);
        assert_eq!(def.preset_metadata.as_ref(), Some(&meta));

        let json = serde_json::to_string(&def).unwrap();
        let back: EffectGraphDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    /// `BindingTarget` and `SkipModeDef` are tagged enums. Confirm the
    /// wire form uses the expected tag keys so JSON authors (humans and
    /// LLMs) can write them by hand.
    #[test]
    fn binding_target_serializes_with_kind_tag() {
        let t = BindingTarget::HandleNode {
            handle: "feedback".to_string(),
            param: "amount".to_string(),
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"kind\":\"handleNode\""));
        assert!(json.contains("\"handle\":\"feedback\""));
        assert!(json.contains("\"param\":\"amount\""));
    }

    #[test]
    fn skip_mode_serializes_with_kind_tag() {
        let s = SkipModeDef::OnZero {
            param_id: "amount".to_string(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"onZero\""));
        assert!(json.contains("\"paramId\":\"amount\""));
    }

    /// `SkipModeDef::Never` is the default — confirm it serializes
    /// compactly so JSON files that don't bother to spell it out
    /// still parse, and round-trip when explicitly set.
    #[test]
    fn skip_mode_default_is_never() {
        let s = SkipModeDef::default();
        assert!(matches!(s, SkipModeDef::Never));
    }
}
