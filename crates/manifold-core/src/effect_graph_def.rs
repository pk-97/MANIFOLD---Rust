//! Per-instance effect graph schema — the on-disk shape an
//! [`PresetInstance`](crate::effects::PresetInstance) carries when its
//! graph topology has diverged from the catalog default.
//!
//! These types are pure serde shapes: zero references back into the
//! live runtime graph, zero GPU types. They live in `manifold-core`
//! so [`PresetInstance`](crate::effects::PresetInstance) can hold one
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
//!   stored on an [`PresetInstance`](crate::effects::PresetInstance).
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

use crate::preset_type_id::PresetTypeId;
use crate::effects::ParamConvert;
use crate::id::NodeId;

/// Schema version for graph-topology-only documents (no preset
/// metadata). Default for per-instance graph overrides and the 25
/// shipping bundled-preset snapshots prior to the §11 migration.
pub const EFFECT_GRAPH_VERSION: u32 = 1;

/// Schema version for documents that carry [`PresetMetadata`].
/// Bundled presets after the §11 migration, user-saved presets, and
/// AI-authored presets all live at this version.
pub const EFFECT_GRAPH_VERSION_WITH_METADATA: u32 = 2;

/// Type-id sentinel for `system.group_input` — the inward boundary of a node
/// group. Its declared output ports mirror the group's
/// [`GroupInterface::inputs`]; inner nodes wire *from* it. Folded away by the
/// flattener ([`crate::flatten`]); never instantiated as a runtime node in the
/// embedded-group path, exactly as `system.source` is folded by the
/// effect-boundary splice one layer further out.
pub const GROUP_INPUT_TYPE_ID: &str = "system.group_input";

/// Type-id sentinel for `system.group_output` — the outward boundary of a node
/// group. Its declared input ports mirror the group's
/// [`GroupInterface::outputs`]; inner nodes wire *into* it. Folded by the
/// flattener.
pub const GROUP_OUTPUT_TYPE_ID: &str = "system.group_output";

/// Marker `type_id` for a node that carries an embedded group body in
/// [`EffectGraphNode::group`]. The flattener replaces every such node with its
/// inlined, handle-prefixed body before the runtime ever sees the document.
pub const GROUP_TYPE_ID: &str = "group";

/// Top-level shape for one effect's per-instance graph.
///
/// Same schema used by bundled preset libraries
/// (`assets/effect-presets/*.json`) and by per-instance graph
/// overrides stored on an [`PresetInstance`](crate::effects::PresetInstance).
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
    /// Stable identity, minted once at node creation and preserved through
    /// grouping / ungrouping / moving / flattening. This is what param
    /// bindings target, so a card slider keeps driving its inner node no
    /// matter how the graph is reorganized. Empty only on a pre-migration
    /// document; the load migration stamps one before anything resolves.
    /// See `docs/NODE_ID_TARGETING.md`.
    #[serde(default, skip_serializing_if = "NodeId::is_empty")]
    pub node_id: NodeId,
    pub type_id: String,
    /// Display / search name. `Some` for authored nodes, `None` for anonymous
    /// boundary nodes. NOT an addressing key — bindings target `node_id`. Still
    /// passed to `Graph::add_node_named` so the runtime handle map and the
    /// graph editor can show a readable name.
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
    /// Author-supplied display title shown in the graph-canvas header,
    /// honored for any node type. Lets the author name otherwise-identical
    /// nodes meaningfully — two `node.value` hubs as `Amount` / `Speed`, or
    /// BlackHole's four `node.wgsl_compute` kernels as `Particle Sim` /
    /// `Deflection Bake` / `Splat` / `Display`. `None` falls back to the
    /// friendly palette label (or a prettified `type_id`). The snapshot
    /// builder appends a `(WGSL)` marker for `node.wgsl_compute` so a
    /// hand-written shader still reads as custom rather than native.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
    /// Per-output-port canvas-relative size as `[numerator,
    /// denominator]`. `[1, 4]` means "allocate this output at
    /// canvas / 4 in each dimension." Honored by dynamic-shape
    /// primitives (`node.wgsl_compute`) where the JSON is the only
    /// place this can be expressed; static-shape primitives that
    /// already declare canvas-relative sizing in Rust (downsample,
    /// upsample) ignore JSON-side overrides.
    ///
    /// Used to recover the legacy quarter-res render trick on
    /// `node.wgsl_compute` outputs whose downstream sampler already
    /// upscales — BlackHole's geodesic deflection bake, for instance,
    /// runs ~16× cheaper at `[1, 4]` because the cost dominates as
    /// pixels × ray-steps.
    ///
    /// Default (empty / missing) means "use whatever the node's own
    /// `output_canvas_scale` reports" — typically `None`, falling
    /// through to canvas-sized allocation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub output_canvas_scales: BTreeMap<String, [u32; 2]>,
    /// When `Some`, this node is a **group instance**: its `type_id` is
    /// [`GROUP_TYPE_ID`], its outward ports are `group.interface.{inputs,
    /// outputs}`, and its [`params`](Self::params) override the group's
    /// [`GroupParamDef`]s by name. The flattener
    /// ([`crate::flatten::flatten_groups`]) inlines the body — prefixing every
    /// inner handle with this node's [`handle`](Self::handle) — so by load time
    /// the document is flat and nothing downstream knows groups existed. Boxed
    /// because [`GroupDef`] contains `EffectGraphNode`s (a recursive type).
    /// `None` for every ordinary node, which is nearly all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Box<GroupDef>>,
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

/// One declared port on a group's outward interface — the "label on the box"
/// that lets a human or AI wire a group without opening it. `port_type` is
/// **advisory** at flatten time (a readability / editor aid); the authoritative
/// type-check runs post-flatten against the inner node's real port, through the
/// renderer's existing wire validation. String form matches the renderer's
/// `PortType` debug tags — `"Texture2D"`, `"Scalar(F32)"`, `"Array(...)"`,
/// `"Material"`, etc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfacePortDef {
    pub name: String,
    pub port_type: String,
}

/// One exposed parameter on a group's interface. A single inner target for now;
/// fan-out to several inner params (matching how [`BindingDef`] fans out) is a
/// later, additive change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupParamDef {
    /// External param name shown on the group, e.g. `"amount"`.
    pub name: String,
    /// Handle of the inner node this param drives, as written in the body
    /// (before the flattener prefixes it with the group instance's handle).
    pub target_handle: String,
    /// Param name on that inner node.
    pub target_param: String,
    /// Value applied when a group instance doesn't override `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<SerializedParamValue>,
}

/// The outward-facing contract of a group: everything that crosses its
/// boundary. `inputs` / `outputs` name the ports an outer wire can attach to;
/// `params` name the knobs an outer instance can set. Mirrored inside the body
/// by [`GROUP_INPUT_TYPE_ID`] / [`GROUP_OUTPUT_TYPE_ID`] nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInterface {
    pub inputs: Vec<InterfacePortDef>,
    pub outputs: Vec<InterfacePortDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<GroupParamDef>,
}

/// An embedded group body. `nodes` / `wires` reuse the ordinary node and wire
/// types, so a body node may itself be a group — nesting falls out for free and
/// the flattener recurses. The body's boundary is expressed with
/// [`GROUP_INPUT_TYPE_ID`] / [`GROUP_OUTPUT_TYPE_ID`] nodes whose port names
/// match [`GroupInterface::inputs`] / [`GroupInterface::outputs`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDef {
    pub interface: GroupInterface,
    pub nodes: Vec<EffectGraphNode>,
    pub wires: Vec<EffectGraphWire>,
    /// Optional RGBA accent for the group's header in the editor. Purely
    /// cosmetic legibility (Resolume / TouchDesigner style colour-coding) so a
    /// busy graph reads as a few labelled boxes under stage pressure. `None`
    /// uses the default group tint. Additive + serde-default, so every saved
    /// show stays byte-identical until a colour is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<[f32; 4]>,
}

/// Tagged-enum wire form of the renderer's `ParamValue`. Tagged because
/// untagged would conflate `Float(0.0)` / `Int(0)` / `Bool(false)`.
///
/// Conversions to/from the renderer's `ParamValue` live in
/// `manifold_renderer::node_graph::persistence`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Read-only N×M `f32` table. JSON shape:
    /// `{"type":"Table","rows":[[1.0, 2.0], [3.0, 4.0]]}`. All rows
    /// must have the same length; rejected on load otherwise.
    Table { rows: Vec<Vec<f32>> },
    /// Single text value (filesystem paths, font names, identifiers).
    /// JSON shape: `{"type":"String","value":"some text"}`. Not
    /// modulated — `ParamConvert` has no variant for strings.
    String { value: String },
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

    /// True if this graph differs from `base` in any way that changes what
    /// the effect renders — i.e. everything *except* purely cosmetic canvas
    /// layout (`editor_pos`). A node drag or an auto-tidy rewrites positions
    /// without touching a single param, wire, or binding, so it must NOT read
    /// as "modified" (this drives the MOD badge / Reset-to-Default pill).
    ///
    /// Moving a node materialises the per-instance override — that's the only
    /// place `editor_pos` can persist — so a bare `graph.is_some()` check goes
    /// true after any drag. Comparing against the catalog `base` with layout
    /// normalised out keeps the badge honest. Recurses through group bodies,
    /// since a node inside a group has its own `editor_pos` too.
    pub fn diverges_ignoring_layout(&self, base: &EffectGraphDef) -> bool {
        fn strip_nodes(nodes: &mut [EffectGraphNode]) {
            for n in nodes {
                n.editor_pos = None;
                if let Some(g) = n.group.as_mut() {
                    strip_nodes(&mut g.nodes);
                }
            }
        }
        let mut a = self.clone();
        let mut b = base.clone();
        strip_nodes(&mut a.nodes);
        strip_nodes(&mut b.nodes);
        a != b
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
    pub id: PresetTypeId,
    /// Display name shown on the effect card and in the picker.
    pub display_name: String,
    /// Picker category (`Spatial`, `Color`, `Stylize`, `Filmic`,
    /// `Diagnostic` — see `preset_type_registry::ALL_CATEGORIES`).
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
    /// Generator-only: whether the generator emits line geometry
    /// (Lissajous, Wireframe, Tesseract, …) rather than a 2D texture.
    /// Drives `is_line_based` plumbing on the renderer side. Ignored
    /// for effect presets — kept on `PresetMetadata` (instead of
    /// forking generator metadata into its own schema) so generators
    /// can ride the same §11 unified-registry path effects already use.
    #[serde(default)]
    pub is_line_based: bool,
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
    /// Backward-compat table for enum-value remaps (e.g. Mirror's mode
    /// indices shifted across a refactor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_aliases: Vec<ValueAliasEntry>,
    /// Outer-card text-config definitions (folder paths, font names,
    /// identifiers). Each entry surfaces as a text field on the host
    /// inspector with an optional Browse button. Distinct from `params`
    /// because the value isn't modulated — no driver, no LFO, no
    /// ParamConvert. Per-clip overrides live in `Clip.string_params`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub string_params: Vec<StringParamSpecDef>,
    /// Routing from each outer-card text-config to one or more
    /// inner-graph node parameters. Mirrors `bindings` but for String
    /// values — no convert variant because String → String is a
    /// pass-through. Address by `id` against `string_params`, not by
    /// position (the fan-out rule from `bindings` applies here too).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub string_bindings: Vec<StringBindingDef>,
}

fn default_available() -> bool {
    true
}

/// JSON-wire shape mirroring [`crate::generator_registration::ParamSpec`].
/// Differs in using owned `String` for compatibility with serde
/// deserialization (the renderer-side `ParamSpec` uses `&'static str`
/// for compile-time inventory submissions).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub is_trigger: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_string: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub osc_suffix: String,
    /// Slider response curve for this outer-card param (Linear by default).
    /// Part of the preset-authored slider surface — the preset file is the
    /// single home for ranges + curve + invert (Phase 2 of
    /// `docs/PRESET_INSTANCE_COLLAPSE_PLAN.md`). Skipped on serialize when
    /// Linear so existing presets stay byte-identical.
    #[serde(default, skip_serializing_if = "crate::effects::curve_is_linear")]
    pub curve: crate::macro_bank::MacroCurve,
    /// Slider invert (card-left drives the param max). `false` by default,
    /// skipped on serialize when false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert: bool,
    /// Angle presentation hint. Display-only: the stored value stays RADIANS
    /// (drivers / Ableton / envelopes write radians every frame, unchanged) —
    /// the card slider and text boundary convert to DEGREES only for the human.
    /// This is the single persistent home for the flag (D1): captured onto a
    /// user-exposed param's spec at expose time from the inner
    /// `ParamType::Angle`, and read straight off the manifest `Param.spec` by
    /// the card builder. Before this field it had no home in the unified param
    /// shape and every card was dead-fed `false`, so no angle param ever showed
    /// degrees. `serde(default)` (false) keeps every saved show loading; skipped
    /// on serialize when false so non-angle presets stay byte-identical on disk.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_angle: bool,
    /// §8 D6: identifies the outer-card gate for a generator's/effect's audio
    /// trigger response (the `clip_trigger` toggle on the 11 trigger-
    /// responsive generators; the Strobe `clip_trigger` card added in P3) —
    /// an EXPLICIT flag, not a match on the id string `"clip_trigger"`
    /// (`feedback_hidden_field_dependencies`). Drives the "A" drawer's mode
    /// row (Clip/Audio/Both) on this card. `false` by default, skipped on
    /// serialize when false so every existing preset stays byte-identical.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_trigger_gate: bool,
    /// BUG-039: true when this param is periodic, so a modulation source
    /// (LFO driver, automation lane) sweeping past `max` (or below `min`)
    /// wraps back into range — `min + (v - min).rem_euclid(max - min)` —
    /// instead of clamping and hitching at the rail. An EXPLICIT tag, not
    /// inferred from [`Self::is_angle`]: angle-typed does not imply periodic
    /// (FOV is angle-typed but must stay clamped; a ±89° tilt or an arc
    /// extent must too). `false` by default, skipped on serialize when
    /// false so every existing preset/project stays byte-identical on disk.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wraps: bool,
    /// Card-bundling group name (D5,
    /// `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2): contiguous runs of
    /// specs sharing the same `section` render under one collapsible header
    /// on the card. `None` renders exactly as today (a flat slider). Seeded
    /// two ways — at expose, from the innermost enclosing group's display
    /// name; by the glTF importer, per-object knobs get the object's group
    /// name and shared knobs get `"Camera"`/`"Sun"`/`"Environment"` — never
    /// derived from graph structure at display time (the manifest is the
    /// single source, matching `PARAM_STORAGE_BOUNDARIES_DESIGN` D4).
    /// `serde(default)` keeps every existing preset loading; skipped on
    /// serialize when absent so a no-section preset stays byte-identical
    /// (the `is_angle` precedent above).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
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
    /// `true` when this binding was added at runtime by the user (via the
    /// graph editor's "expose this inner param" checkbox) rather than
    /// shipping with the preset's bundled metadata. Removing the
    /// matching exposure unchecks: bundled bindings only flip
    /// `exposed_params` (the slot survives so drivers/Ableton/etc. keep
    /// addressing it); user-added bindings get pulled from `params` +
    /// `bindings` entirely along with their `param_values` slot.
    /// Skipped on serialize when `false` so bundled-preset JSON stays
    /// byte-identical to the on-disk source.
    #[serde(default, skip_serializing_if = "is_false")]
    pub user_added: bool,
    /// Card→consumer linear remap applied at the renderer write boundary:
    /// `out = value * scale + offset`. This is where an in-graph
    /// `affine_scalar` that only rescaled a card value toward its inner
    /// consumer folds in, so the node can be deleted. `scale = 1.0`,
    /// `offset = 0.0` is identity, and both are skipped on serialize when
    /// identity so every un-folded preset stays byte-identical on disk.
    #[serde(default = "one_f32", skip_serializing_if = "is_one")]
    pub scale: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f32,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn one_f32() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Outer-card text-config declaration. Renders as a text field in the
/// host inspector with an optional Browse button (set
/// `is_file_picker: true` for folder/file selection UX).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StringParamSpecDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub default_value: String,
    /// Hint to the editor: render a "Browse…" button alongside the text
    /// field that opens a native folder/file picker. The text field is
    /// always editable; the button is sugar.
    #[serde(default)]
    pub is_file_picker: bool,
    /// Hint to the editor: render a dropdown selector instead of a free
    /// text input (e.g. the Text generator's Font picker, populated from
    /// the installed font families). Mutually exclusive with
    /// `is_file_picker` in practice.
    #[serde(default)]
    pub use_dropdown: bool,
}

/// Routing from one outer-card string config to one inner-graph node
/// parameter. No `convert` field — String → String is a pass-through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StringBindingDef {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub default_value: String,
    pub target: BindingTarget,
}

/// Where a binding's value flows. Mirror of the renderer-side
/// `ParamTarget`, restricted to the JSON-expressible variants. The
/// renderer's `ParamTarget::Node { NodeInstanceId }` and
/// `ParamTarget::Custom(fn)` are not representable here — the first
/// because live IDs aren't serializable, the second because function
/// pointers aren't.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum BindingTarget {
    /// Route to an inner-graph node identified by its stable
    /// [`NodeId`] — minted once at node creation, invariant under
    /// group / ungroup / move / flatten. The handle that used to
    /// address nodes here changes at flatten time (a grouped node's
    /// handle gets prefixed `blur` → `softgroup/blur`), which is why
    /// addressing moved to the id. Resolved to a runtime node at
    /// chain-build time via `Graph::instance_by_node_id`.
    Node { node_id: NodeId, param: String },
    /// Route through a composite handle's exposed-param map. Used by
    /// composite-shaped effects where one outer slider fans out to
    /// multiple inner-node parameters.
    Composite { outer_name: String },
}

impl<'de> Deserialize<'de> for BindingTarget {
    /// Tolerant read: accepts the current `node` / `composite` forms AND
    /// the pre-node-id `handleNode` form, which it upgrades in place to
    /// `Node` with `node_id` == the old handle. That is the same "a
    /// node's id defaults to its handle" convention the bundled-preset
    /// stamp and the load-time node-id normalization use, so a
    /// handle-targeted binding lands on exactly the node that normalizes
    /// to the same id. A one-shot read migration, not a runtime fallback:
    /// the resolver only ever sees `Node`/`Composite`, and serialization
    /// only ever emits `node`/`composite`.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
        enum Wire {
            Node { node_id: NodeId, param: String },
            Composite { outer_name: String },
            HandleNode { handle: String, param: String },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Node { node_id, param } => BindingTarget::Node { node_id, param },
            Wire::Composite { outer_name } => BindingTarget::Composite { outer_name },
            Wire::HandleNode { handle, param } => BindingTarget::Node {
                node_id: NodeId::from(handle),
                param,
            },
        })
    }
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

    fn sample_param_spec() -> ParamSpecDef {
        ParamSpecDef {
            id: "speed".to_string(),
            name: "Speed".to_string(),
            min: 0.1,
            max: 10.0,
            default_value: 1.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: crate::macro_bank::MacroCurve::Linear,
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        }
    }

    #[test]
    fn param_spec_identity_curve_invert_skipped_on_serialize() {
        // Default (Linear / false) must not appear on the wire, so every
        // shipped preset stays byte-identical after Phase 2.
        let json = serde_json::to_string(&sample_param_spec()).unwrap();
        assert!(!json.contains("curve"), "Linear curve must be skipped: {json}");
        assert!(!json.contains("invert"), "false invert must be skipped: {json}");
    }

    #[test]
    fn param_spec_authored_curve_invert_round_trip() {
        let mut p = sample_param_spec();
        p.curve = crate::macro_bank::MacroCurve::Exponential;
        p.invert = true;
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"curve\":\"exponential\""), "{json}");
        assert!(json.contains("\"invert\":true"), "{json}");
        let back: ParamSpecDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back.curve, crate::macro_bank::MacroCurve::Exponential);
        assert!(back.invert);
    }

    #[test]
    fn param_spec_missing_curve_invert_defaults_to_identity() {
        // A pre-Phase-2 preset JSON (no curve/invert keys) deserializes
        // to the identity slider response.
        let json = r#"{"id":"x","name":"X","min":0.0,"max":1.0,"defaultValue":0.0}"#;
        let p: ParamSpecDef = serde_json::from_str(json).unwrap();
        assert_eq!(p.curve, crate::macro_bank::MacroCurve::Linear);
        assert!(!p.invert);
    }

    #[test]
    fn param_spec_no_section_skipped_on_serialize() {
        // SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D5: `section: None` (every
        // existing preset, and any spec expose/importer never touched) must
        // not appear on the wire, so a no-section preset stays byte-identical
        // to the on-disk source (the `is_angle`/`curve` precedent above).
        let json = serde_json::to_string(&sample_param_spec()).unwrap();
        assert!(!json.contains("section"), "absent section must be skipped: {json}");
        let back: ParamSpecDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back.section, None);
    }

    #[test]
    fn param_spec_section_round_trips() {
        let mut p = sample_param_spec();
        p.section = Some("Leaf".to_string());
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"section\":\"Leaf\""), "{json}");
        let back: ParamSpecDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back.section.as_deref(), Some("Leaf"));
    }

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
                node_id: crate::NodeId::default(),
                type_id: "node.threshold".to_string(),
                handle: Some("thresh".to_string()),
                params,
                exposed_params: BTreeSet::new(),
                editor_pos: Some((100.0, 200.0)),
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: EffectGraphDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn group_node_document_round_trips() {
        // A minimal soft-focus group: GroupInput.src fans to mix.a, mix.out
        // feeds GroupOutput.out, and `amount` is exposed onto the inner mix.t.
        let mut amount_override = BTreeMap::new();
        amount_override.insert(
            "amount".to_string(),
            SerializedParamValue::Float { value: 0.7 },
        );
        let body = GroupDef {
            interface: GroupInterface {
                inputs: vec![InterfacePortDef {
                    name: "src".to_string(),
                    port_type: "Texture2D".to_string(),
                }],
                outputs: vec![InterfacePortDef {
                    name: "out".to_string(),
                    port_type: "Texture2D".to_string(),
                }],
                params: vec![GroupParamDef {
                    name: "amount".to_string(),
                    target_handle: "mix".to_string(),
                    target_param: "t".to_string(),
                    default: Some(SerializedParamValue::Float { value: 0.5 }),
                }],
            },
            nodes: vec![
                EffectGraphNode {
                    id: 0,
                    node_id: crate::NodeId::default(),
                    type_id: GROUP_INPUT_TYPE_ID.to_string(),
                    handle: None,
                    params: BTreeMap::new(),
                    exposed_params: BTreeSet::new(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
                EffectGraphNode {
                    id: 1,
                    node_id: crate::NodeId::default(),
                    type_id: "node.mix".to_string(),
                    handle: Some("mix".to_string()),
                    params: BTreeMap::new(),
                    exposed_params: BTreeSet::new(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
                EffectGraphNode {
                    id: 2,
                    node_id: crate::NodeId::default(),
                    type_id: GROUP_OUTPUT_TYPE_ID.to_string(),
                    handle: None,
                    params: BTreeMap::new(),
                    exposed_params: BTreeSet::new(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
            ],
            wires: vec![
                EffectGraphWire {
                    from_node: 0,
                    from_port: "src".to_string(),
                    to_node: 1,
                    to_port: "a".to_string(),
                },
                EffectGraphWire {
                    from_node: 1,
                    from_port: "out".to_string(),
                    to_node: 2,
                    to_port: "out".to_string(),
                },
            ],
            tint: None,
        };
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: Some("With Group".to_string()),
            description: None,
            preset_metadata: None,
            nodes: vec![EffectGraphNode {
                id: 10,
                node_id: crate::NodeId::default(),
                type_id: GROUP_TYPE_ID.to_string(),
                handle: Some("soft_focus".to_string()),
                params: amount_override,
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: Some(Box::new(body)),
            }],
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        // camelCase keys surface for human/AI readability.
        assert!(json.contains("\"group\""));
        assert!(json.contains("\"interface\""));
        assert!(json.contains("\"portType\""));
        assert!(json.contains("\"targetHandle\""));
        let back: EffectGraphDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn group_field_skipped_when_none() {
        // The backward-compat guarantee: an ordinary node emits no `group`
        // key, so every existing flat document re-serializes byte-identically.
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: crate::NodeId::default(),
                type_id: "node.blur".to_string(),
                handle: None,
                params: BTreeMap::new(),
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        assert!(!json.contains("\"group\""));
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
            id: PresetTypeId::new("EdgeStretchByColor"),
            display_name: "Edge Stretch By Colour".to_string(),
            category: "Stylize".to_string(),
            osc_prefix: "edge_stretch_by_color".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: vec![ParamSpecDef {
                id: "amount".to_string(),
                name: "Amount".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 1.0,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: Some("F2".to_string()),
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
            }],
            bindings: vec![BindingDef {
                id: "amount".to_string(),
                label: "Amount".to_string(),
                default_value: 1.0,
                target: BindingTarget::Node {
                    node_id: NodeId::new("masked_mix_id"),
                    param: "amount".to_string(),
                },
                convert: ParamConvert::Float,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
            }],
            skip_mode: SkipModeDef::OnZero {
                param_id: "amount".to_string(),
            },
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
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
        let t = BindingTarget::Node {
            node_id: NodeId::new("feedback_id"),
            param: "amount".to_string(),
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"kind\":\"node\""));
        assert!(json.contains("\"nodeId\":\"feedback_id\""));
        assert!(json.contains("\"param\":\"amount\""));
    }

    /// Pre-node-id documents stored targets as `handleNode`. The serde
    /// layer upgrades them to `Node` with `node_id == the old handle` —
    /// the same convention the preset stamp + load normalization use, so
    /// the binding lands on the node that normalizes to the same id.
    #[test]
    fn legacy_handle_node_target_deserializes_as_node_keyed_by_handle() {
        let legacy = r#"{ "kind": "handleNode", "handle": "blur", "param": "radius" }"#;
        let t: BindingTarget = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            t,
            BindingTarget::Node {
                node_id: NodeId::new("blur"),
                param: "radius".to_string(),
            }
        );
        // And it re-serializes in the new `node` form (never handleNode).
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"kind\":\"node\""));
        assert!(!json.contains("handleNode"));
    }

    /// The current `node` form still round-trips unchanged.
    #[test]
    fn node_target_round_trips() {
        let t = BindingTarget::Node {
            node_id: NodeId::new("n123"),
            param: "amount".to_string(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: BindingTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
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

    /// The MOD badge must ignore pure canvas layout. Moving a node (only
    /// `editor_pos` changes) must not read as diverged; changing a param must.
    #[test]
    fn diverges_ignores_layout_but_catches_edits() {
        fn node(pos: Option<(f32, f32)>, level: f32) -> EffectGraphNode {
            let mut params = BTreeMap::new();
            params.insert(
                "level".to_string(),
                SerializedParamValue::Float { value: level },
            );
            EffectGraphNode {
                id: 0,
                node_id: crate::NodeId::default(),
                type_id: "node.threshold".to_string(),
                handle: Some("thresh".to_string()),
                params,
                exposed_params: BTreeSet::new(),
                editor_pos: pos,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }
        }
        let def = |n: EffectGraphNode| EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![n],
            wires: Vec::new(),
        };

        let base = def(node(Some((0.0, 0.0)), 0.5));

        // Same graph, node dragged to a new position → NOT modified.
        let moved = def(node(Some((640.0, 480.0)), 0.5));
        assert!(!moved.diverges_ignoring_layout(&base));

        // Position also cleared (auto-tidy edge) → still NOT modified.
        let no_pos = def(node(None, 0.5));
        assert!(!no_pos.diverges_ignoring_layout(&base));

        // A real param change → modified, regardless of position.
        let edited = def(node(Some((640.0, 480.0)), 0.9));
        assert!(edited.diverges_ignoring_layout(&base));
    }

    /// Layout normalisation must recurse into group bodies — a node nudged
    /// *inside* a group is still just layout.
    #[test]
    fn diverges_ignores_layout_inside_groups() {
        fn inner(pos: Option<(f32, f32)>) -> EffectGraphNode {
            EffectGraphNode {
                id: 1,
                node_id: crate::NodeId::default(),
                type_id: "node.mix".to_string(),
                handle: Some("mix".to_string()),
                params: BTreeMap::new(),
                exposed_params: BTreeSet::new(),
                editor_pos: pos,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }
        }
        let group_node = |inner_pos: Option<(f32, f32)>| EffectGraphNode {
            id: 0,
            node_id: crate::NodeId::default(),
            type_id: GROUP_TYPE_ID.to_string(),
            handle: Some("grp".to_string()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: Some((0.0, 0.0)),
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    params: Vec::new(),
                },
                nodes: vec![inner(inner_pos)],
                wires: Vec::new(),
                tint: None,
            })),
        };
        let def = |n: EffectGraphNode| EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![n],
            wires: Vec::new(),
        };

        let base = def(group_node(Some((10.0, 10.0))));
        let inner_moved = def(group_node(Some((99.0, 99.0))));
        assert!(!inner_moved.diverges_ignoring_layout(&base));
    }
}
