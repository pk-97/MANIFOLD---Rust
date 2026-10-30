//! The queryable param-surface model.
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
//! forbidden (Peter's standing rule).

use crate::panels::GraphParamTarget;
use crate::panels::param_card::{ParamCardKind, ParamCardStringInfo, RowMod};
use crate::panels::param_slider_shared::{AbletonMappingDisplay, AudioRowState, AudioSendChoice};
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
    Subsurface,
}

/// Optional material features in their stable inspector order. Keep all
/// feature surfaces on this shared vocabulary so adding a feature cannot
/// leave one of the add/order paths behind.
pub const MATERIAL_FEATURES: &[MaterialFeature] = &[
    MaterialFeature::Coat,
    MaterialFeature::Iridescence,
    MaterialFeature::Emission,
    MaterialFeature::Glass,
    MaterialFeature::Sheen,
    MaterialFeature::Anisotropy,
    MaterialFeature::Translucency,
    MaterialFeature::Subsurface,
];

/// A Material's starting looks: a one-click set of features and factors,
/// not a stored setting. The Material block shows the look its values match,
/// or Custom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialLook {
    Default,
    Matte,
    Coated,
    BrushedMetal,
    Glass,
}

impl MaterialLook {
    pub const ALL: [MaterialLook; 5] = [
        MaterialLook::Default,
        MaterialLook::Matte,
        MaterialLook::Coated,
        MaterialLook::BrushedMetal,
        MaterialLook::Glass,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MaterialLook::Default => "Default",
            MaterialLook::Matte => "Matte",
            MaterialLook::Coated => "Coated",
            MaterialLook::BrushedMetal => "Brushed Metal",
            MaterialLook::Glass => "Glass",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialGroup {
    Surface,
    Opacity,
    Covers,
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
    VolumeScattering,
    Subsurface,
    Translucency,
}

pub use manifold_foundation::scene_surface::RgbChannel;

/// Which colour a swatch edits: a material colour, or a light's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColourGroup {
    Material(MaterialColour),
    Light,
}

/// The three scalar params of one colour, and the colour they held at
/// projection time (the swatch's first paint; live values arrive through the
/// value sync). On the R channel's own row it makes that row the colour row
/// ([`RowKind::Colour`]); on any other row it is a swatch beside that row.
#[derive(Debug, Clone, PartialEq)]
pub struct RgbMembers {
    pub group: ColourGroup,
    pub ids: [ParamId; 3],
    pub values: [f32; 3],
    /// One RGB write owns all three: each binds straight to one node param
    /// with no wire and no driver, envelope, audio mod, mapping or
    /// automation. When false the swatch and hex only show the colour, and
    /// the colour row opens on its channel sliders.
    pub editable: bool,
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

/// Descriptor half of a row — sourced verbatim from the manifest's
/// `ParamSpecDef` fields by the projection. Never from registry re-reads,
/// never from hand tables (INV-1).
#[derive(Debug, Clone, PartialEq)]
pub struct RowSpec {
    pub tooltip: Option<String>,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
    /// Display-only: storage stays radians; the value cell shows degrees.
    pub is_angle: bool,
    /// A boolean; draws as the switch ([`ParamRow::kind`]).
    pub is_toggle: bool,
    /// Momentary "fire once" button row.
    pub is_trigger: bool,
    /// Trigger-gate toggle row (reaches the audio "A" drawer). Always paired
    /// with `is_toggle: true, is_trigger: false`.
    pub is_trigger_gate: bool,
    /// Named value labels for discrete params; shown instead of the number.
    pub value_labels: Option<Vec<String>>,
    /// Shown after the number in the value readout (" m", " EV").
    pub unit: Option<manifold_foundation::scene_surface::Unit>,
    /// Card-bundling section name; contiguous `Some(name)` runs share one
    /// collapsible header. Usually from the manifest; the scene card titles
    /// sections by band and item without changing ids.
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
    /// The setup check found this row's shape does not fit its liquid's
    /// domain; the line says what the solver changed. Drawn in warning
    /// colour under the row. The app projection is the only place that
    /// fills it, from `SceneVm::problems`.
    pub setup_warning: Option<String>,
    /// The control's scene descriptor (role and control group) when the
    /// lookup has one. The Scene tab groups and filters its rows by it.
    pub scene: Option<manifold_foundation::scene_surface::SceneParamDesc>,
    /// What a drag may reach past the slider's end. The projection resolves `Slider`
    /// to the declared range, so `min`/`max` (the slider range) only set the
    /// drag's speed; a hand-built row left at `Slider` clamps to its span.
    pub limit: manifold_foundation::param_range::HardLimit,
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

/// Stable parameter owner and current clip source label for the shared drawer.
#[derive(Debug, Clone)]
pub struct ClipTriggerRow {
    pub target: crate::view::UiGraphTarget,
    pub source_label: String,
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
    /// Clip timing selector for compatible layer-owned parameters.
    pub clip_trigger: Option<ClipTriggerRow>,
    /// Driver/envelope/automation facts.
    pub modulation: RowMod,
    pub mapping: RowMapping,
    /// A row with a colour swatch: the three scalar params it writes as one
    /// gesture. The channels keep their own rows and bindings.
    pub rgb_members: Option<RgbMembers>,
    /// A driver, envelope, audio mod, Ableton mapping or automation lane is
    /// stored on this param, enabled or not. Unlike [`Self::is_attached`]
    /// (what moves the param now), it survives a disarm. The draw kind and
    /// material feature visibility and seed writes read it. Filled by the
    /// app projection.
    pub modulator_stored: bool,
    /// What the row is when it is not a manifest param: a scene item's
    /// structural field or a card editing helper. Such a row has no manifest
    /// slot, so no value sync and no modulation drawer.
    pub affordance: Option<RowAffordance>,
}

/// A row that carries an action instead of a manifest value
/// (SCENE_DOCUMENT_DESIGN.md section 8): the capability-driven action rows.
#[derive(Debug, Clone, PartialEq)]
pub enum RowAffordance {
    /// A structural field of a scene item. A pick sends
    /// `ProjectAction::SceneFieldSet`.
    Field {
        item: manifold_foundation::ItemId,
        field: String,
        control: crate::panels::scene_setup_panel::FieldControlVm,
    },
    /// The Scale link switch of an item's Transform section (section 7.17):
    /// a card editing helper, never a param.
    ScaleLink { item: manifold_foundation::ItemId },
}

/// How a row draws. Decided once, by [`ParamRow::kind`]; every row builder,
/// value sync and click route reads that answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A slider.
    Number,
    /// The one on/off switch (`chrome::components::toggle_style`).
    Switch,
    /// A pick list: shows the current label; a click opens the shared dropdown.
    Choice,
    /// A momentary fire button.
    Trigger,
    /// A colour: swatch, hex field and a fold holding its three channel
    /// sliders. Only the R channel's row is the colour row; the G and B rows
    /// draw inside its fold.
    Colour,
}

impl ParamRow {
    /// The one kind decision, from what the manifest spec declares: a trigger
    /// fires, a toggle switches, value labels over whole numbers are a
    /// choice, anything else is a number. Two labels over a float (Cast
    /// Shadows: Off, On) are a switch kept a float so a modulator can move it;
    /// the node reads it past 0.5.
    ///
    /// `scene` is the surface's fact, the card's `CardHost` set by the panel
    /// that hosts it. The scene card, the inspector's scene modifier cards and
    /// the Scene tab draw labelled params as switches and pick lists; other
    /// cards keep them sliders, whose driver lane is a performance tool
    /// there. A labelled row with a modulator or mapping stored on it, armed
    /// or disarmed, stays a slider anywhere: its D, E and A lane is the only
    /// way back to a disarmed modulator.
    ///
    /// On a scene surface the R channel of a grouped colour is the colour
    /// row, modulated or not: its fold keeps each channel's slider and lane.
    pub fn kind(&self, scene: bool) -> RowKind {
        let spec = &self.spec;
        match self.affordance {
            Some(RowAffordance::Field {
                control: crate::panels::scene_setup_panel::FieldControlVm::Switch { .. },
                ..
            }) => {
                return RowKind::Switch;
            }
            Some(RowAffordance::Field { .. }) => return RowKind::Choice,
            Some(RowAffordance::ScaleLink { .. }) => return RowKind::Switch,
            None => {}
        }
        if spec.is_trigger {
            return RowKind::Trigger;
        }
        if spec.is_toggle {
            return RowKind::Switch;
        }
        if scene
            && self
                .rgb_members
                .as_ref()
                .is_some_and(|members| members.ids[0] == self.id)
        {
            return RowKind::Colour;
        }
        let labels = spec.value_labels.as_deref().map_or(0, <[String]>::len);
        if !scene || labels < 2 || self.modulator_stored || self.is_attached() {
            return RowKind::Number;
        }
        if spec.whole_numbers {
            RowKind::Choice
        } else if labels == 2 {
            RowKind::Switch
        } else {
            RowKind::Number
        }
    }

    /// Whether the row can open a modulation drawer under itself: a slider
    /// and a fire button can, and so can a trigger-gate switch (its audio
    /// drawer) and a colour row (its R slider's drawer, in the fold); a plain
    /// switch and a pick list cannot. A pick list's T, D and A buttons arm a
    /// modulator, and the armed row draws as a slider, which can.
    pub fn grows_drawer(&self, scene: bool) -> bool {
        match self.kind(scene) {
            RowKind::Number | RowKind::Trigger | RowKind::Colour => true,
            RowKind::Switch => self.spec.is_trigger_gate,
            RowKind::Choice => false,
        }
    }

    /// A driver, envelope, automation lane, audio mod or Ableton mapping on
    /// the row. Hiding such a row hides a live control from the performer, so
    /// the Scene tab shows it whatever its role; the card keeps it through
    /// the pin the attach wrote (SCENE_SURFACES D6 (attached rows)).
    pub fn is_attached(&self) -> bool {
        self.modulation.driver_active
            || self.modulation.envelope_active
            || self.modulation.automation_active
            || self.audio.active
            || self.mapping.ableton_display.is_some()
    }
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

    /// The rows, manifest order == render order.
    pub rows: Vec<ParamRow>,
    /// Generator string params (clickable text-field rows). Empty for effects.
    pub string_params: Vec<ParamCardStringInfo>,
    /// Card-level available audio sends.
    pub audio_sends: Vec<AudioSendChoice>,
}

/// Every interactive element class a row can build — the vocabulary the
/// gesture router speaks. One enum, both card kinds.
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
    /// The button of a button row: a switch, a pick list or a fire button.
    /// The row's [`RowKind`], recorded on its button ids, decides the action.
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
    /// A scene card section's row-end "Open in Scene" button (row = first row
    /// of the section): opens the Scene tab on the item that owns that row.
    SectionTitle,
    /// Colour swatch over the row's `rgb_members`: opens the colour menu.
    ColourSwatch(ColourGroup),
    /// A colour row's hex field: opens the hex type-in.
    ColourHex,
    /// A colour row's name and triangle: folds its channel sliders.
    ColourFold,
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
            ParamCardKind::Effect => GraphParamTarget::Effect(self.effect_id.clone()),
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
        self.rows
            .iter()
            .any(|r| r.mapping.ableton_display.is_some())
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
        assert_eq!(
            super::stable_key("intensity"),
            9_466_175_151_710_844_563_u64
        );
        assert_ne!(super::stable_key("intensity"), super::stable_key("speed"));
    }
}
