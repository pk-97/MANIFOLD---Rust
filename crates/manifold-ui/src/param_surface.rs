//! The queryable param-surface model — `docs/WIDGET_TREE_DESIGN.md` D1–D3.
//!
//! One `ParamSurface` describes one manifest-backed card (effect, generator,
//! editor card, scene section): identity + descriptor + state per row, built
//! app-side by the ONE projection (`ui_bridge::state_sync::param_surface`)
//! and consumed by the card renderer, the gesture router, the tests, and the
//! dump. There is no second source: every row fact enters through the
//! projection's single manifest walk.
//!
//! # Adding a row affordance (the section 5b recipe — five steps, never five files)
//!
//! 1. Add the `RowRole` variant (P2; this module).
//! 2. Add the fact as ONE field on [`ParamRow`] (or its sub-structs) and its
//!    one line in the projection.
//! 3. Add the render arm in the row builder (`param_card.rs`).
//! 4. Add the `row_action` arm (P2).
//! 5. Add the dispatch test (`ui_bridge/inspector.rs` Harness pattern).
//!
//! Anything that can't be expressed this way is an escalation, by definition
//! of the layer. Building row/slider/drawer machinery anywhere else is
//! forbidden (WIDGET_TREE_DESIGN section 5b, Peter's standing rule, INV-8).

use crate::panels::param_card::{ParamCardKind, ParamCardStringInfo, RelightCardConfig, RowMod};
use crate::panels::param_slider_shared::{AbletonMappingDisplay, AudioRowState, AudioSendChoice};
use crate::panels::GraphParamTarget;
use manifold_foundation::{EffectId, LayerId, ParamId};

/// UI-side copy of the persisted material descriptor vocabulary. The UI crate
/// deliberately does not depend on core; the app projection performs the
/// exhaustive conversion at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialFeature {
    Coat,
    Iridescence,
    Emission,
    Glass,
    Sheen,
    Anisotropy,
    Translucency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialGroup {
    Surface,
    Opacity,
    Feature(MaterialFeature),
    Advanced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialColour {
    Base,
    Specular,
    Emission,
    Sheen,
    Attenuation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RgbChannel {
    R,
    G,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialMapFamily {
    Base,
    Normal,
    MetallicRoughness,
    Occlusion,
    Emission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UvComponent {
    M00,
    M01,
    M10,
    M11,
    Tx,
    Ty,
}

/// Friendly controls projected from the six authoritative UV matrix rows.
/// The raw matrix remains available under Advanced for shear and other
/// placements that cannot be represented by these five controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UvControl {
    OffsetU,
    OffsetV,
    Rotation,
    ScaleU,
    ScaleV,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplerComponent {
    WrapU,
    WrapV,
    MagFilter,
    MinFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialParamRole {
    Scalar(MaterialGroup),
    Colour(MaterialGroup, MaterialColour, RgbChannel),
    FeatureMode(MaterialFeature),
    Placement(MaterialMapFamily, UvComponent),
    Sampler(MaterialMapFamily, SamplerComponent),
}

/// Re-export the action vocabulary beside the surface descriptors for callers
/// that consume the UI projection as one material-inspector API.
pub use crate::panels::actions::MaterialLook;

/// Descriptor half of a row — sourced verbatim from the manifest's
/// `ParamSpecDef` fields by the projection. Never from registry re-reads,
/// never from hand tables (INV-1).
#[derive(Debug, Clone, PartialEq)]
pub struct RowSpec {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
    /// Display-only: storage stays radians; the value cell shows degrees.
    pub is_angle: bool,
    /// Renders as a boolean ON/OFF button row instead of a slider.
    pub is_toggle: bool,
    /// Momentary "fire once" button row.
    pub is_trigger: bool,
    /// Trigger-gate toggle row (reaches the audio "A" drawer). Always paired
    /// with `is_toggle: true, is_trigger: false`.
    pub is_trigger_gate: bool,
    /// Named value labels for discrete params; shown instead of the number.
    pub value_labels: Option<Vec<String>>,
    /// Card-bundling section name; contiguous `Some(name)` runs share one
    /// collapsible header. Straight off the manifest spec.
    pub section: Option<String>,
    /// `None` = the row is interactive. `Some(reason)` = the row renders
    /// greyed with the reason appended to its label (the modifier-picker
    /// convention) and every gesture is a dead click. Set by the projection
    /// (e.g. Math View's Connect to Mesh on an unsupported chain); the row
    /// builders + dispatch honour it, never the renderer.
    pub disabled: Option<String>,
    /// Semantic material descriptor, when this row belongs to a PBR material.
    /// The app projection is the only authority that fills this field.
    pub material_role: Option<MaterialParamRole>,
    /// Explanation for a material control that is authored but currently has
    /// no contribution (for example, an optional feature set to Off).
    /// This is presentation state; `disabled` remains reserved for controls
    /// that cannot be edited at all.
    pub inactive_reason: Option<String>,
}

/// Value state at projection time. Per-frame effective values keep riding
/// the `sync_values` slot stream, JOINED onto rows by param id (BUG-313 —
/// never by position; INV-6 is now the id-join coverage check, not a length
/// assert); these fields are the structural-sync snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowValue {
    /// User-intended base (pre-modulation).
    pub base: f32,
    /// Post-modulation — what the slider shows.
    pub effective: f32,
    /// Exposed as a slider on the card (hidden slots keep slot-index
    /// semantics for drivers/mappings).
    pub exposed: bool,
    /// Wire-fed (read-only "driven" presentation). Filled where the caller
    /// knows (editor snapshot); `false` elsewhere.
    pub driven: bool,
}

/// Mapping facts for one row.
#[derive(Debug, Clone, PartialEq)]
pub struct RowMapping {
    /// OSC address; label click copies it.
    pub osc_address: Option<String>,
    /// Ableton mapping sub-section, when mapped.
    pub ableton_display: Option<AbletonMappingDisplay>,
    /// Ableton trim range `(min, max)` — trim handles on the track.
    pub ableton_range: Option<(f32, f32)>,
    /// Row carries a per-instance editable reshape (mapping-drawer chevron
    /// in `CardContext::Author`).
    pub mappable: bool,
}

/// One row's scene-graph write address (SCENE_MODIFIER_FRAMEWORK D4) — the
/// `(scope_path, node_doc_id, param)` tuple `ProjectAction::SceneSetupParamChanged`
/// carries. Populated ONLY by the modifier-card projection
/// (`modifier_surfaces`, ui_bridge); every other card's rows leave it `None`
/// and ride the plain manifest-param wires. UI-side copy of the renderer's
/// `scene_vm::ParamAddr` shape (`ui` cannot depend on `renderer`).
#[derive(Debug, Clone, PartialEq)]
pub struct SceneRowAddr {
    pub scope_path: Vec<u32>,
    pub node_doc_id: u32,
    pub param_id: String,
}

/// Card-level facts for one APPLIED scene modifier (SCENE_MODIFIER_FRAMEWORK
/// section 3.7) — the chrome + write addressing an ordinary effect-shell card
/// needs to behave as a modifier card. `None` on every non-modifier surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifierObjectRef {
    /// Stable object scope path in the owning scene graph.
    pub scope: Vec<manifold_foundation::NodeId>,
    /// Stable node identity within that scope.
    pub node: manifold_foundation::NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifierObjectOption {
    pub object: ModifierObjectRef,
    pub label: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModifierCardInfo {
    /// Stable identity of this applied modifier instance.
    pub instance_id: manifold_foundation::NodeId,
    /// The owning layer — every row gesture addresses `GeneratorOf(this)`.
    pub layer_id: LayerId,
    /// Authored enabled-parameter label shown by the common card toggle.
    pub enabled_label: String,
    /// Position in the owning layer's ordered modifier stack.
    pub stack_index: usize,
    /// Number of entries in the owning layer's modifier stack.
    pub stack_len: usize,
    /// Whether this modifier currently applies to every object in its scope.
    pub targets_all: bool,
    /// Available target objects and their explicit-selection state.
    pub objects: Vec<ModifierObjectOption>,
}

/// Stable address for one object-owned modifier card in Scene Setup.
///
/// The object and group owner are structural scene identities; `node_doc_id`
/// identifies the modifier within that object's ordered chain.  The card uses
/// all three facts for UI identity while the existing scene modifier action
/// uses the group owner and modifier document id for removal/reorder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectModifierCardInfo {
    pub layer_id: LayerId,
    pub object_id: u32,
    pub group_node_id: Option<u32>,
    pub node_doc_id: u32,
}

/// One catalog recipe in the modifier picker. Applicability and singleton
/// restrictions are projected from the scene by the app; the UI reads no graph.
#[derive(Debug, Clone, PartialEq)]
pub struct ModifierPickerEntry {
    pub preset_id: String,
    pub label: String,
    /// `None` = clickable (applies the kind); `Some(reason)` = disabled with
    /// the reason appended ("applied", "not applicable").
    pub disabled: Option<String>,
}

/// One card row: identity + descriptor + state. THE unit of the layer —
/// `id` is the WidgetId salt (P2), the wire identity
/// (`PanelAction`s carry it), and the test address.
#[derive(Debug, Clone)]
pub struct ParamRow {
    pub id: ParamId,
    pub spec: RowSpec,
    pub value: RowValue,
    /// Audio modulation facts owned by this row.
    pub audio: AudioRowState,
    /// Driver/envelope/automation facts.
    pub modulation: RowMod,
    pub mapping: RowMapping,
    /// The scene-graph write address for a MODIFIER card row (D4) — `Some`
    /// only on rows the modifier projection built; the card's toggle-row
    /// gestures turn it into `SceneSetupParamChanged` instead of a plain
    /// manifest-param toggle.
    pub scene_addr: Option<SceneRowAddr>,
    /// The canonical RGB row keeps the three existing scalar identities so a
    /// swatch can make one gesture while advanced rows retain their bindings.
    pub rgb_members: Option<[ParamId; 3]>,
    /// An authored external attachment (wire, automation, or host mapping)
    /// that remains meaningful even when the current frame flags are dormant.
    /// Material feature visibility and seed writes use this projection fact.
    pub material_attached: bool,
}

/// The complete queryable description of one manifest-backed param surface.
/// Replaces the former parallel-vecs card config (P1b): rows are id-keyed
/// structs, aggregates are derived, positional index maps do not exist.
#[derive(Debug, Clone)]
pub struct ParamSurface {
    pub kind: ParamCardKind,
    /// Display name — the effect name or the generator type name.
    pub title: String,
    pub collapsed: bool,
    pub enabled: bool,

    // ── Effect-only identity + flags (defaults for generators) ──
    pub effect_index: usize,
    pub effect_id: EffectId,
    pub supports_envelopes: bool,
    /// Per-card graph override exists (pink "MOD" badge + header tint).
    pub has_graph_mod: bool,

    // ── Generator-only identity ──
    pub layer_id: Option<LayerId>,

    /// SCENE_MODIFIER_FRAMEWORK section 3.7: `Some` on a scene modifier card
    /// (an effect-shell card whose rows are the kind's stamped exposures).
    /// Drives the chrome differences (no drag handle / relight chip, the
    /// common enable toggle, ordered-stack controls, and remove ×) and the
    /// `GeneratorOf(layer)` write addressing. `None` everywhere else.
    pub modifier: Option<ModifierCardInfo>,

    /// The rows, manifest order == render order.
    pub rows: Vec<ParamRow>,
    /// Generator string params (clickable text-field rows). Empty for effects.
    pub string_params: Vec<ParamCardStringInfo>,
    /// Card-level available audio sends.
    pub audio_sends: Vec<AudioSendChoice>,
    /// "3D Shading" toggle + knobs.
    pub relight: RelightCardConfig,
}

/// Every interactive element class a row can build — the vocabulary the
/// gesture router speaks (WIDGET_TREE_DESIGN D5). One enum, both card kinds.
/// Extend HERE (recipe step 1) — never as a new id-hoard field or a new
/// id-match chain in a `handle_*` body.
///
/// Coarse by design: a multi-node widget bundle (slider, driver-config
/// drawer, trim pair…) maps ALL its interactive nodes to one role; the
/// bundle's own `resolve(NodeId)` names the sub-element (the widget-contract
/// split — behavior lives in the widget, identity in the row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRole {
    /// The slider bundle (track / fill / thumb / value cell / label lane).
    Slider,
    /// The row's full-width click catcher (focus / drawer toggle surface).
    RowCatcher,
    /// Param label (OSC copy-to-clipboard when an address exists).
    Label,
    /// "D" driver arm/disarm button.
    DriverBtn,
    /// "E" envelope arm/disarm button.
    EnvelopeBtn,
    /// "A" audio-mod arm/disarm button.
    AudioBtn,
    /// Explicit automation-entry button (shown only for supported cards).
    AutomationBtn,
    /// Toggle / trigger button row (`is_toggle` / `is_trigger` decide the action).
    ToggleBtn,
    /// Driver config drawer (all its sub-buttons; retained drawer actions).
    DriverConfig,
    /// Envelope config drawer (target handle, decay… `EnvelopeConfigIds`).
    EnvelopeConfig,
    /// Audio config drawer (send/chip/matrix… the audio bundle resolves).
    AudioConfig,
    /// Ableton mapping sub-section (invert, trim handles).
    AbletonConfig,
    /// Modulation-config tab strip entry (tab index resolved by the bundle).
    ModTab,
    /// Sideways mapping-drawer chevron (`CardContext::Author`).
    MappingChevron,
    /// D5 section fold header (row = first row of the section).
    SectionHeader,
    /// Relight: header toggle ("3D Shading") — row is unused (card-level).
    RelightToggle,
    /// Relight: one of the Height From option buttons (0..3; index resolved
    /// positionally by the relight bundle).
    RelightHeightBtn,
    /// Relight: one of the six knob slider bundles.
    RelightSlider,
    /// Shared RGB swatch over the three existing scalar parameter slots.
    ColourSwatch(MaterialColour),
    /// Optional material feature header/toggle.
    MaterialFeatureToggle(MaterialFeature),
    /// Friendly material placement control backed by the six matrix rows.
    MaterialPlacement(UvControl),
}

/// Reverse map from durable widget identity to `(row index, role)` —
/// populated during `build()` from the same rows being rendered, so routing
/// agrees with rendering by construction (D5). Rebuilt with the tree; never
/// serialized; never consulted across a rebuild (WidgetId handles that).
#[derive(Debug, Default)]
pub struct RowIndex {
    map: ahash::AHashMap<crate::node::WidgetId, (usize, RowRole)>,
}

impl RowIndex {
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Register one interactive node's widget under `(row, role)`. Called at
    /// mint time inside the row builders.
    pub fn insert(&mut self, widget: crate::node::WidgetId, row: usize, role: RowRole) {
        self.map.insert(widget, (row, role));
    }

    /// Resolve a hit node's widget to its row + role. The ONLY sanctioned
    /// way a `handle_*` body identifies a row element.
    pub fn get(&self, widget: crate::node::WidgetId) -> Option<(usize, RowRole)> {
        self.map.get(&widget).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Pinned, process-stable key for identity-bearing widget salts (D4): FNV-1a
/// over the id's bytes, finalized splitmix64-style. NEVER replace with
/// `DefaultHasher`/seeded ahash — dumps expose raw `Widget(u64)` values that
/// flow scripts hold across runs; a run-varying hash silently breaks them.
pub fn stable_key(id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    // splitmix64 finalizer — spreads short ids apart.
    let mut z = h.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl ParamSurface {
    /// The card's wire identity — derived, never stored twice.
    pub fn target(&self) -> GraphParamTarget {
        match self.kind {
            ParamCardKind::Effect => GraphParamTarget::Effect(self.effect_index),
            ParamCardKind::Generator => GraphParamTarget::Generator,
        }
    }

    /// DRV badge: any row has an active driver. Derived (the stored
    /// aggregate mirrors died with the old parallel-vecs card config).
    pub fn has_drv(&self) -> bool {
        self.rows.iter().any(|r| r.modulation.driver_active)
    }

    /// ENV badge: any row has an active envelope.
    pub fn has_env(&self) -> bool {
        self.rows.iter().any(|r| r.modulation.envelope_active)
    }

    /// ABL badge: any row has an Ableton mapping.
    pub fn has_abl(&self) -> bool {
        self.rows.iter().any(|r| r.mapping.ableton_display.is_some())
    }
}

/// One enumerated affordance in the D9 widget catalog: a single sanctioned
/// interactive node on a manifest-backed card, paired with the row it belongs
/// to. The catalog is the enumeration of these over a *built* tree — it reuses
/// the two durable facts the tree dump already serializes per node (the
/// [`WidgetId`](crate::node::WidgetId) salt and the `name_of` name); the
/// [`RowRole`] comes from the card's [`RowIndex`] (the SAME index routing
/// resolves through, so the catalog can never disagree with what a click
/// reaches). No new addressing protocol — only a regrouping of those durable
/// facts, plus the role.
///
/// `name == None` is the BUG-239 shape made visible: a sanctioned affordance
/// with no queryable name. Every ROW carries at least one named affordance
/// (its slider or its toggle button, `param_row.<id>…`), so a row is always
/// nameable; a nameless *sub-element* (an arm button, a drawer cell) is reached
/// through its bundle's own `resolve(NodeId)` — the widget-contract split — not
/// by name. The catalog surfaces which is which rather than hiding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogAffordance {
    /// The owning row's durable param id (`ParamRow::id`).
    pub row_id: String,
    /// The affordance's role in the row.
    pub role: RowRole,
    /// The durable `WidgetId` salt — the SAME value the dump emits as `widget`.
    pub widget: u64,
    /// The queryable name (`UITree::name_of`), or `None` for a nameless
    /// sanctioned affordance (a BUG-239-shaped gap, made visible not fixed).
    pub name: Option<String>,
}

/// One manifest-backed card's catalog: its identity plus every sanctioned
/// affordance its rows minted this build, in tree order. Produced by
/// `ParamCardPanel::catalog`; the `--catalog` dump mode serializes a `Vec` of
/// these under the owning panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSurface {
    pub kind: ParamCardKind,
    /// The card's display title (effect name or generator type name).
    pub title: String,
    pub affordances: Vec<CatalogAffordance>,
}

#[cfg(test)]
mod stable_key_tests {
    /// INV-4's cross-process pin: known bytes → known salt, forever. If this
    /// test ever needs updating, held dump `Widget(u64)` values in flow
    /// scripts break — that is a breaking change to the automation surface,
    /// not a refactor.
    #[test]
    fn stable_key_is_pinned() {
        assert_eq!(super::stable_key("intensity"), 9_466_175_151_710_844_563_u64);
        assert_ne!(super::stable_key("intensity"), super::stable_key("speed"));
    }
}
