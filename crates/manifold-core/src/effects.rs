use crate::effect_graph_def::EffectGraphDef;
use crate::preset_type_id::PresetTypeId;
use crate::id::{EffectGroupId, EffectId, NodeId};
use crate::types::{BeatDivision, DriverWaveform};
use crate::units::Beats;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Stable string identifier for a host-visible parameter.
///
/// `Cow::Borrowed("amount")` for compile-time IDs (developer-defined
/// effects). `Cow::Owned(...)` for V2 user-exposed parameters allocated
/// at runtime. External mappings (OSC, Ableton, MIDI, modulation
/// drivers, envelopes) all key on this — never on positional indices.
///
/// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 for the full design.
///
/// Defined in `manifold-foundation` (the shared primitive vocabulary) and
/// re-exported here at its historical path so the UI can share the identical
/// type without depending on the engine. See `docs/UI_LAYERING_INVERSION.md`.
pub use manifold_foundation::ParamId;

// ─── Param Definition ───

/// Registry-side param descriptor: the manifest's [`crate::effect_graph_def::ParamSpecDef`]
/// (the ONE slider-surface shape, shared with the graph metadata now that
/// `ParamDef` no longer exists as a separate near-twin) plus the one fact a
/// registry entry genuinely owns that a card manifest must not carry — the
/// range contract (PARAM_RANGE_CONTRACT_DESIGN.md D3/D4: the card manifest
/// must stay unable to carry a contract). Not serialized: `PresetDef` and its
/// `param_defs` are built in-memory at registry-construction time from
/// `inventory::submit!` sources or JSON-loaded `ParamSpecDef`s, never
/// deserialized as this shape.
///
/// Collapses the former `effects::ParamDef` / `effect_graph_def::ParamSpecDef`
/// twin (and the three hand-written converters between them) into one
/// descriptor that exists once — see `handoff_param_descriptor_unification_brief`.
#[derive(Debug, Clone, Default)]
pub struct RegistryParamDef {
    pub spec: crate::effect_graph_def::ParamSpecDef,
    /// A real physical/mathematical boundary this param's inner value must
    /// not cross — as opposed to `spec.min`/`spec.max`, which are display
    /// hints (default slider travel) a card, text entry, or modulation is
    /// free to exceed. `None` for the overwhelming majority of params
    /// (PARAM_RANGE_CONTRACT_DESIGN.md D6: remove-by-default — no
    /// kernel/shader proof, no contract). See [`RangeContract`].
    pub contract: Option<RangeContract>,
}

/// A named, real boundary on a param's inner value — the ONLY thing card
/// range validation (`node_graph::validate` lint (h)) enforces as an error.
/// Everything else (`RegistryParamDef::spec.min`/`max`) is a display hint that never
/// restricts (Peter, `docs/PARAM_RANGE_CONTRACT_DESIGN.md`: *"Inner nodes
/// that don't have a real physical range or boundary shouldn't have a
/// boundary — that's what the card mappings and ranges are for."*).
///
/// One-sided bounds are first-class (`min`/`max` are independently
/// optional) — a contract may only forbid going too low, or too high, or
/// both. `reason` is mandatory: there is no contract without a named
/// excuse, mirroring the `BoundaryReason` declared-excuse pattern
/// (`node_graph::freeze::classify::BoundaryReason`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RangeContract {
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub reason: RangeReason,
}

/// Why a `RangeContract` exists (design doc D2). A closed enum: every
/// contract names exactly one of these — the meta-test
/// `every_range_contract_names_a_real_boundary` (manifold-renderer,
/// `node_graph::freeze::classify`) pins each contracted param to its
/// reason in a curated table, so a contract can't creep back onto a
/// param whose range is merely a creative-amount hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RangeReason {
    /// Addresses a discrete resource (mux select, array slot).
    Index,
    /// Sizes an allocation (num_inputs, particle caps).
    Count,
    /// The kernel divides/degenerates at or below the bound.
    DegenerateFloor,
    /// Geometry collapses outside the bound.
    DegenerateGeometry,
    /// The shader physically clamps; beyond the bound is a dead input.
    ShaderClamp,
    /// The math is ONLY defined on the interval — a true domain, not a
    /// lerp/blend factor (those extrapolate legitimately; see the Bloom
    /// ruling in the design doc's intro).
    NormalizedDomain,
}

/// serde `skip_serializing_if` for [`crate::effect_graph_def::ParamSpecDef::curve`].
pub(crate) fn curve_is_linear(c: &crate::macro_bank::MacroCurve) -> bool {
    matches!(c, crate::macro_bank::MacroCurve::Linear)
}

/// serde `skip_serializing_if` for a defaulted `false` bool field.
pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

// ─── Traits ───

/// Shared contract for entities that own a modular effects list.
/// Port of Unity IEffectContainer.cs.
/// Implemented by TimelineClip, Layer, and ProjectSettings.
pub trait EffectContainer {
    fn effects(&self) -> &[PresetInstance];
    fn effects_mut(&mut self) -> &mut Vec<PresetInstance>;
    fn effect_groups(&self) -> &[EffectGroup];
    fn effect_groups_mut(&mut self) -> &mut Vec<EffectGroup>;
    fn has_modular_effects(&self) -> bool;
    fn find_effect(&self, effect_type: &PresetTypeId) -> Option<&PresetInstance>;
    fn find_effect_group(&self, group_id: &str) -> Option<&EffectGroup>;
}

/// Abstracts a "thing with named params, drivers, and ranges."
/// Port of Unity IParamSource.cs.
/// Both PresetInstance and generator params implement this.
pub trait ParamSource {
    fn display_name(&self) -> &str;
    fn param_count(&self) -> usize;
    fn get_param_def(&self, id: &str) -> crate::effect_graph_def::ParamSpecDef;
    fn get_param(&self, id: &str) -> f32;
    fn set_param(&mut self, id: &str, value: f32);
    fn get_base_param(&self, id: &str) -> f32;
    fn set_base_param(&mut self, id: &str, value: f32);
    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver>;
    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>>;
    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver;
    fn remove_driver(&mut self, param_id: &str);
}

// ─── User-Exposed Parameters ───

/// Conversion shape for a user-exposed parameter — the core-side
/// counterpart to `manifold-renderer`'s `ParamConvert`.
///
/// User bindings always route 1:1 from a host-visible slider straight
/// to a single inner-node param, so the renderer's `EnumRemap` and
/// `Custom` variants are intentionally absent here. Effect authors who
/// want non-trivial conversions on a user-exposable surface should keep
/// the conversion inside the effect itself.
///
/// Wire format: serialized as a tagged enum (`{"type": "Float"}` etc.)
/// so future variants don't break round-trips of existing fixtures.
///
/// Phase 4 of the bindings unification plan merged this with the
/// renderer-side `ParamConvert` (the renamed name retained — the
/// previous `ParamConvert` name implied a user-tier-only enum).
/// Both static spec bindings and per-instance user bindings now share
/// this one type at every layer. The `EnumRemap` and `FloatTransform`
/// variants that used to live on the renderer side are gone — their
/// curation moved into the primitives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum ParamConvert {
    /// Pass-through `f32`.
    #[default]
    Float,
    /// Round to nearest int (slider sees float; node sees rounded value).
    IntRound,
    /// Threshold at 0.5 (slider sees 0..1; node sees 0 or 1).
    BoolThreshold,
    /// Round to nearest enum index.
    EnumRound,
    /// Momentary button. Storage is a monotonic `u32` counter held as
    /// `f32`; the outer-card click handler increments by one per press
    /// (no toggle state). Consuming primitives detect rising edges via
    /// the standard `last_count: Option<u32>` cold-start pattern — same
    /// as `node.trigger_gate`. Behaves like an `IntRound` for modulation
    /// resolution (whole-number domain).
    Trigger,
}

// `resolve_param_in` / `ResolvedParam` / the `override_range` closure are
// DELETED (PARAM_STORAGE_DESIGN.md D3/D6/D7). Modulation no longer resolves an
// id to a positional slot: the driver / envelope / audio-mod evaluators read
// `fx.params.get_mut(id)` and take range + whole-number data straight off the
// entry — `p.spec.min` / `p.spec.max` / `p.whole_numbers()`. Calibration edits
// `Param.spec.min`/`max` in place, so a recalibrated slider's range IS the
// entry's range; the old "read the graph override, else the catalog" split
// (the driver-overshoot bug's home) is gone with the split.
/// A user-exposed parameter on an [`PresetInstance`].
///
/// V2 user-exposed-params surface (see `docs/EFFECT_RUNTIME_UNIFICATION.md`
/// §7.6). Each binding is per-instance: ticking "expose UVTransform.translate"
/// on Mirror#0 doesn't affect Mirror#1.
///
/// Stable addressing comes from [`NodeId`] — the inner node's identity,
/// minted once at node creation and invariant under group / ungroup /
/// move / flatten. (The node's handle is a display name that flatten
/// prefixes when the node is grouped, which is exactly why addressing
/// can't key off it.) The renderer resolves this id to a runtime node
/// via `Graph::instance_by_node_id` at chain-build time.
///
/// Storage: each `UserParamBinding` reserves one slot at the tail of
/// the parent `PresetInstance.param_values` (positions
/// `def.param_count..def.param_count + user_param_bindings.len()`).
/// The host writes through that slot; the renderer reads it once per
/// frame via the binding's resolved inner-node target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserParamBinding {
    /// Stable user-generated `ParamId`. Convention:
    /// `"user.<short_node_handle>.<inner_param>.<n>"` where `<n>`
    /// disambiguates collisions (linear probe from 1).
    ///
    /// Once shipped in a saved project, this id is forever — driver,
    /// envelope, Ableton, OSC mappings reference it.
    pub id: String,
    /// Display label shown on the effect-card slider. Mutable; doesn't
    /// affect addressing.
    pub label: String,
    /// Stable [`NodeId`] of the inner node this binding drives. Minted
    /// at node creation, invariant under group / ungroup / move /
    /// flatten. `#[serde(default)]` so pre-node-id projects load with an
    /// empty id; the load migration backfills it from the resolved
    /// graph (`migrate_user_param_bindings_to_node_id`).
    #[serde(default)]
    pub node_id: NodeId,
    /// Load-migration shim. Pre-node-id projects addressed the target by
    /// handle under the JSON key `nodeHandle`; that value is captured
    /// here on load so the renderer-layer migration can resolve it to a
    /// [`NodeId`] (against the instance's graph or its canonical preset),
    /// then it's cleared. `skip_serializing_if` empty so it never
    /// appears in freshly-saved files — the runtime resolver only ever
    /// reads `node_id`, never this. Not a fallback; a one-shot upgrade.
    #[serde(
        default,
        rename = "nodeHandle",
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_node_handle: Option<String>,
    /// Inner-node parameter name (matches `ParamDef::id` on the addressed node).
    pub inner_param: String,
    pub min: f32,
    pub max: f32,
    pub default_value: f32,
    /// Conversion shape applied at the renderer boundary. Defaults to
    /// `Float` (1:1 pass-through), the only convert needed for the
    /// initial three graph-backed effects (Mirror, SoftFocus,
    /// StylizedFeedback).
    #[serde(default)]
    pub convert: ParamConvert,
    /// Angle presentation hint, captured from the inner param's
    /// `ParamType::Angle` at expose time. Display-only: the stored value
    /// stays RADIANS (drivers / Ableton / envelopes write radians every
    /// frame, unchanged), and the card slider converts to DEGREES only at
    /// the text boundary. `ParamConvert::Float` cannot carry this because
    /// angle and plain-float share it, so it rides its own flag.
    /// `serde(default)` keeps pre-existing projects loading; the post-load
    /// alignment pass backfills it from the registry.
    #[serde(default)]
    pub is_angle: bool,
    /// Card-slider invert. When true the normalized slider position is
    /// flipped (card-left drives inner-max) at the renderer write boundary.
    /// Mapping-only: the stored slot stays physical-valued, so drivers /
    /// Ableton / envelopes writing the same slot are unaffected.
    /// `serde(default)` (false) keeps every saved show 1:1.
    #[serde(default)]
    pub invert: bool,
    /// Card-slider response curve, applied to the normalized position at the
    /// renderer write boundary (after invert, before scaling to [min, max]).
    /// Reuses the macro-bank curve so the whole app shares one curve type.
    /// `serde(default)` (Linear) keeps every saved show 1:1.
    #[serde(default)]
    pub curve: crate::macro_bank::MacroCurve,
    /// Card→consumer linear remap applied at the renderer write boundary AFTER
    /// the slider reshape and BEFORE wrap/convert: `out = value * scale + offset`.
    /// This is where an in-graph `affine_scalar` that only rescaled a card value
    /// toward its inner consumer folds in — the card keeps storing the friendly
    /// value (Curl 85°, Particle Count 2.0), drivers/Ableton/envelopes write the
    /// same slot unchanged, and the binding does the scale the node used to do.
    /// `serde(default = "one")` keeps `scale = 1.0`; with `offset = 0.0` that is
    /// identity, so every saved show stays byte-identical until a fold sets them.
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub offset: f32,
    /// Enum option labels captured from the inner param's `ParamDef` at
    /// expose time. Drives the card slider's stepped/labelled rendering so an
    /// exposed enum (Fold Mode, Blend Mode, …) shows its option names instead
    /// of a bare 0..N numeric slider. Empty for non-enum params; carried onto
    /// the appended `ParamSpecDef` so the card reads it through the normal
    /// reshape overlay. `serde(default)` keeps pre-existing projects loading.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_labels: Vec<String>,
    /// Card-bundling section name (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2
    /// D5), captured from the innermost enclosing group's display name at
    /// expose time and carried onto the appended [`crate::effect_graph_def
    /// ::ParamSpecDef`] so the card reads it through the normal manifest
    /// path. `None` for a top-level (unscoped) expose. `serde(default)`
    /// keeps pre-existing projects loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

/// serde default for [`UserParamBinding::scale`] — identity is `1.0`, not the
/// `f32::default()` of `0.0` (which would zero every un-migrated binding).
fn one() -> f32 {
    1.0
}

// ─── Card → consumer reshape pipeline ───

/// The card→consumer reshape pipeline — the **single definition** shared by
/// the renderer's runtime write boundary (`ResolvedBinding`'s `Reshape::apply`)
/// and the mapping-popover's live preview, so the two can never drift. A
/// preview computed by different math than the engine is a lie the moment one
/// side changes; routing both through this function makes that unrepresentable.
///
/// The reshape (display label, slider range, invert, response curve, and the
/// card→consumer affine `scale`/`offset`) lives in the preset's authoring
/// surface — each param's `ParamSpecDef` plus its `BindingDef` — the single
/// source after the per-instance `ParamMapping` note was deleted. A
/// recalibration edits that spec on the instance's per-instance graph override
/// ([`UserParamBinding`] does the same inline for user-exposed params); the
/// value slot the modulation surface writes is never touched, so the reshape
/// applies DOWNSTREAM at the renderer boundary and the live rig is unaffected.
///
/// Two stages, matching the [`UserParamBinding`] reshape semantics:
/// 1. **Slider response** — only when `invert` or a non-Linear `curve` is set:
///    normalize the value within `[min, max]`, invert, apply the curve, scale
///    back. This stage clamps to `[0, 1]` so the response is well defined
///    across the slider. A pure scale/offset fold skips this entirely, so it
///    reproduces the `affine_scalar` it replaced exactly.
/// 2. **Card→consumer affine** — `out = v * scale + offset`, UNCLAMPED, where a
///    folded `affine_scalar` lands (e.g. a deg→rad scale a driver may push past
///    the slider max, which the angle wrap then tames downstream).
///
/// Identity inputs (`invert = false`, `curve = Linear`, `scale = 1`,
/// `offset = 0`) return `value` unchanged.
pub fn apply_card_reshape(
    value: f32,
    min: f32,
    max: f32,
    invert: bool,
    curve: crate::macro_bank::MacroCurve,
    scale: f32,
    offset: f32,
) -> f32 {
    let mut v = value;
    if invert || curve != crate::macro_bank::MacroCurve::Linear {
        let range = max - min;
        if range.abs() >= f32::EPSILON {
            let mut n = ((v - min) / range).clamp(0.0, 1.0);
            if invert {
                n = 1.0 - n;
            }
            n = curve.apply(n);
            v = min + range * n;
        }
    }
    v * scale + offset
}

/// Exact inverse of [`apply_card_reshape`] where one exists
/// (`PARAM_TWO_WAY_BINDING_DESIGN.md` D2) — the write-back direction for a
/// node-face edit on a card-bound param: given the target's new value, solve
/// for the card value that would forward-reshape to it. Returns `None` only
/// for a degenerate affine (`scale ≈ 0`, unrepresentable). Out-of-range
/// targets clamp to the slider ends, matching the forward stage-1 clamp —
/// the inverse of a clamped map is defined on the range.
///
/// Body order is the forward run reversed: affine first (undo
/// `v*scale+offset`), then — only when `invert || curve != Linear` —
/// normalize, [`crate::macro_bank::MacroCurve::inverse`], un-invert,
/// denormalize.
pub fn invert_card_reshape(
    target: f32,
    min: f32,
    max: f32,
    invert: bool,
    curve: crate::macro_bank::MacroCurve,
    scale: f32,
    offset: f32,
) -> Option<f32> {
    if scale.abs() < f32::EPSILON {
        return None;
    }
    let mut v = (target - offset) / scale;
    if invert || curve != crate::macro_bank::MacroCurve::Linear {
        let range = max - min;
        if range.abs() >= f32::EPSILON {
            let mut n = ((v - min) / range).clamp(0.0, 1.0);
            n = curve.inverse(n);
            if invert {
                n = 1.0 - n;
            }
            v = min + range * n;
        }
    }
    Some(v)
}

// ─── Param Value (per-slot state) ───

// A single parameter slot's runtime state used to live here.
// `ParamSlot` is DELETED (PARAM_STORAGE_DESIGN.md D1/D3). Its four fields
// (`value`, `base`, `exposed`, `touched`) now live on `crate::params::Param`
// alongside the descriptor (`spec`), `origin`, and `calibrated` — one struct,
// one list, id as identity. Construct a param with `Param::bundled(spec)` /
// `Param::user_added(spec)`; read/write value + base + exposed + touched
// through the manifest (`params.get(id)` / `params.get_mut(id)`).

/// Everything removed when an exposed card param is pruned from an instance:
/// its manifest [`crate::params::Param`] entry (descriptor + state), its
/// `BindingDef`, and any drivers / Ableton mappings / envelopes that
/// referenced its id — plus the display position + binding position each
/// occupied. Returned by [`PresetInstance::remove_exposures_for_node`] and
/// handed back to [`PresetInstance::restore_exposures`] so an undo restores the
/// pre-delete state byte-for-byte. Opaque to callers (the command stack just
/// carries it).
#[derive(Debug, Clone)]
pub struct RemovedExposure {
    /// Display position in the [`crate::params::ParamManifest`] this entry
    /// occupied — captured purely to restore card order via `insert_at`
    /// (PARAM_STORAGE_DESIGN.md D10: a display-order snapshot, never an
    /// identity). `None` when the pruned binding had no param entry
    /// (composite / fan-out binding with no outer slider).
    param_position: Option<usize>,
    /// Index in `preset_metadata.bindings` the `BindingDef` occupied.
    binding_index: usize,
    /// The removed manifest entry (descriptor + live state), or `None` for a
    /// binding that had no matching param.
    param: Option<crate::params::Param>,
    binding: crate::effect_graph_def::BindingDef,
    drivers: Vec<ParameterDriver>,
    ableton_mappings: Vec<crate::ableton_mapping::AbletonParamMapping>,
    envelopes: Vec<ParamEnvelope>,
    audio_mods: Vec<crate::audio_mod::ParameterAudioMod>,
}

// ─── Effect Instance ───

/// One of the D3 relight-stage float knobs (`docs/DEPTH_RELIGHT_DESIGN.md`
/// D3). Lives in `manifold-core` because both the renderer (per-frame
/// uniform writes) and the editing commands need to address the same field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelightField {
    LightX,
    LightY,
    Relief,
    AoIntensity,
    ShadowSoftness,
    Gain,
}

impl RelightField {
    /// Read this field off a [`RelightParams`].
    pub fn get(self, p: &RelightParams) -> f32 {
        match self {
            Self::LightX => p.light_x,
            Self::LightY => p.light_y,
            Self::Relief => p.relief,
            Self::AoIntensity => p.ao_intensity,
            Self::ShadowSoftness => p.shadow_softness,
            Self::Gain => p.gain,
        }
    }

    /// Write this field on a [`RelightParams`].
    pub fn set(self, p: &mut RelightParams, value: f32) {
        match self {
            Self::LightX => p.light_x = value,
            Self::LightY => p.light_y = value,
            Self::Relief => p.relief = value,
            Self::AoIntensity => p.ao_intensity = value,
            Self::ShadowSoftness => p.shadow_softness = value,
            Self::Gain => p.gain = value,
        }
    }

    /// Every float field, in UI declaration order.
    pub const ALL: &[Self] = &[
        Self::LightX,
        Self::LightY,
        Self::Relief,
        Self::AoIntensity,
        Self::ShadowSoftness,
        Self::Gain,
    ];
}

/// D4's height-origin override for the "3D Shading" relight stage
/// (`docs/DEPTH_RELIGHT_DESIGN.md` D2/D4, phase P5): `Auto` runs the
/// compiler's D1 structural walk (falling back to luminance-of-output only
/// when no `SourceHeight` producer is reachable — the proven default the
/// whole probe sweep ran on); `Luminance`/`InvertedLuminance` force the
/// height tap onto the final color's luminance (inverted or not) regardless
/// of what the structural walk would find, for effects/generators whose
/// natural output reads better relit from its brightness than from its
/// nominal depth producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelightHeightFrom {
    #[default]
    Auto,
    Luminance,
    InvertedLuminance,
}

/// The D3 relight-stage knobs, exposed as ordinary card params once the "3D
/// Shading" toggle (`PresetInstance::relight`) is on. Always present on the
/// instance regardless of the toggle's state — per the no-conditionally-
/// visible-UI rule the card renders these rows greyed rather than hidden
/// when the toggle is off, so the values must survive a toggle-off/toggle-on
/// round trip. Defaults are the probe sweep's proven v6 recipe (D3) /
/// `node.heightfield_shadow`'s own defaults (D5) — see `relight.rs`'s
/// template mint for where each field lands.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelightParams {
    /// Light direction X — fans out to `rl_lambert.light_x` AND
    /// `rl_shadow.light_x` (the shadow raymarch must track the same light
    /// direction as the Lambert term, or the shadow reads mismatched against
    /// the shading whenever this is dragged).
    #[serde(default = "RelightParams::default_light_x")]
    pub light_x: f32,
    /// Light direction Y — same fan-out as `light_x`.
    #[serde(default = "RelightParams::default_light_y")]
    pub light_y: f32,
    /// Bump/occlusion/shadow strength — fans out to `rl_bumps.z_scale`
    /// (rescaled ×12, so the proven default 0.25 lands on the proven
    /// z_scale default 3.0), `rl_ao.relief`, and `rl_shadow.relief`.
    #[serde(default = "RelightParams::default_relief")]
    pub relief: f32,
    /// `rl_ao.intensity`.
    #[serde(default = "RelightParams::default_ao_intensity")]
    pub ao_intensity: f32,
    /// `rl_shadow.softness`.
    #[serde(default = "RelightParams::default_shadow_softness")]
    pub shadow_softness: f32,
    /// `rl_exposure.gain`.
    #[serde(default = "RelightParams::default_gain")]
    pub gain: f32,
    /// D4 height-origin override.
    #[serde(default)]
    pub height_from: RelightHeightFrom,
}

impl RelightParams {
    fn default_light_x() -> f32 {
        0.4
    }
    fn default_light_y() -> f32 {
        0.6
    }
    fn default_relief() -> f32 {
        0.25
    }
    fn default_ao_intensity() -> f32 {
        1.3
    }
    fn default_shadow_softness() -> f32 {
        0.5
    }
    fn default_gain() -> f32 {
        1.4
    }

    /// Whether every field is at its proven-recipe default — the
    /// serialize-skip gate so an untouched instance's `relightParams`
    /// doesn't appear on the wire (byte-identical old projects, D2).
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for RelightParams {
    fn default() -> Self {
        Self {
            light_x: Self::default_light_x(),
            light_y: Self::default_light_y(),
            relief: Self::default_relief(),
            ao_intensity: Self::default_ao_intensity(),
            shadow_softness: Self::default_shadow_softness(),
            gain: Self::default_gain(),
            height_from: RelightHeightFrom::Auto,
        }
    }
}

/// A single effect applied to a clip, layer, or master chain.
///
/// Serialization (custom impls below):
///
/// - `params` is one id-keyed V1.4 map (PARAM_STORAGE_DESIGN.md §4):
///   `{ id: { value, exposed, base? } }`, `base` present iff
///   `base_tracked`. This is the ONLY wire shape the typed (de)serialize
///   understands — the historical positional/keyed value duo (a values
///   container plus a parallel pre-modulation-base container) is gone;
///   `manifold-io`'s `migrations::param_storage_v14` converts every
///   legacy shape to `params` before typed deserialization ever runs.
/// - `build_effect_param_values` places incoming `params` entries onto
///   `[static prefix | user tail]` positional slots on load;
///   `ParamsSer` walks the same order in reverse on save.
///
/// In-memory storage stays positional (`Vec<ParamSlot>`) — the hot
/// path reads/writes by index. The Map form only exists on the wire.
/// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 12.
#[derive(Debug, Clone)]
pub struct PresetInstance {
    /// Whether this instance is an effect (transforms an input) or a
    /// generator (produces from nothing). The single discriminator that
    /// carries every effect/generator behavioral difference — kind-aware
    /// serde, the generator registry clamp on `set_param*`, and the
    /// generator-only fields below. Effects ignore `legacy_param_version`;
    /// generators ignore `id`/`enabled`/`collapsed`/`group_id` — those are
    /// chain-membership fields with no generator meaning. (`envelopes` and
    /// `graph` are now shared homes for both kinds — envelope-home + graph-home
    /// unification.)
    pub kind: crate::preset_def::PresetKind,
    /// Unique identifier for this effect instance. Synthetic for generators
    /// (a layer has one generator, addressed by `LayerId`).
    pub id: EffectId,
    effect_type: PresetTypeId,
    pub enabled: bool,
    pub collapsed: bool,
    /// Id-keyed parameter manifest (PARAM_STORAGE_DESIGN.md D1): descriptor
    /// and live state in one [`crate::params::Param`] per parameter, keyed by
    /// id, with insertion order as card display order. Replaces the former
    /// positional `Vec<ParamSlot>` plus three id→index resolvers — there is no
    /// positional identity anymore, so nothing between creation and disk
    /// resolves a param through an index or the registry. Address a param by
    /// its stable id (`params.get(id)` / `params.get_mut(id)`); the renderer's
    /// `EffectSlot.bindings` read the same manifest by `source_id`.
    pub params: crate::params::ParamManifest,
    /// Whether the pre-modulation base (`ParamSlot.base`) is *tracked* — the
    /// single presence bit that gates the `base` key on each V1.4 `params`
    /// entry. Set on load whenever ANY incoming entry carried `base`, and by
    /// any base write (`set_base_param` / `reset_param_effectives` /
    /// `init_defaults_for_type`). While `false`, `get_base_param` falls
    /// through to the effective value, so per-slot `base` is allowed to be
    /// stale; the field is also the serialize-time gate for whether `base`
    /// appears on the wire at all. Not serialized; derived on load.
    pub base_tracked: bool,
    /// Raw V1.4 wire entries, held between deserialize and the loader's
    /// reconcile pass (`PARAM_STORAGE_BOUNDARIES_DESIGN.md` D1/D3). The
    /// custom `Serialize` impl above never reads this field, so it never
    /// rides the wire. `Some` from the moment `Deserialize`/`into_instance`
    /// builds the manifest until [`Self::reconcile_manifest`] rebuilds it
    /// against a registry that has since resolved the template — cleared at
    /// that point (the common case: one load, one reconcile). Stays `Some`
    /// across a reconcile call that still can't resolve a template (the
    /// keep-don't-drop path — BUG-036), so a *later* reconcile — after the
    /// registry catches up — can retry. `None` on a freshly-constructed
    /// instance (`new`/`new_generator`), which never had a wire to stash.
    pending_wire: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
    pub drivers: Option<Vec<ParameterDriver>>,
    /// Per-instance ADSR/Random envelopes, keyed by `param_id`. Envelope-home
    /// unification: both effects and generators store their envelopes here (the
    /// instance the envelope sits on is the target). `None` when unused.
    pub envelopes: Option<Vec<ParamEnvelope>>,
    pub ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
    /// Per-instance audio modulations, keyed by `param_id`. The fourth
    /// modulation source, parallel to `drivers`/`envelopes`/`ableton_mappings`:
    /// drives a param from a named audio send. `None` when unused. See
    /// `docs/AUDIO_MODULATION_DESIGN.md`.
    pub audio_mods: Option<Vec<crate::audio_mod::ParameterAudioMod>>,
    /// Per-instance timeline automation, keyed by `param_id` — a lane is a
    /// beat-indexed base writer sampled each tick (a tier-1 "hand"), riding
    /// on top of the same modulation pipeline audio_mods/drivers/envelopes
    /// feed. `None` when unused. See `docs/AUTOMATION_LANES_DESIGN.md`.
    pub automation_lanes: Option<Vec<AutomationLane>>,
    pub group_id: Option<EffectGroupId>,

    /// Per-instance graph topology override. `None` means "use the
    /// catalog default graph for this effect type" — every shipping
    /// fixture today loads with `graph: None` and round-trips
    /// byte-identically (the field is skipped when serializing).
    /// `Some(def)` carries a full graph definition that the renderer
    /// hydrates from instead of calling the catalog `build_*` helper.
    /// Phase 1 of the per-card-divergence work — see
    /// `docs/NODE_GRAPH_SYSTEM.md`.
    pub graph: Option<EffectGraphDef>,

    /// Monotonically bumped each time `graph` is replaced. Renderer
    /// caches the last seen version per instance, rebuilds the runtime
    /// `Graph` + plan + render state when it differs. Not serialized;
    /// resets to 0 on load — the renderer's `u32::MAX` sentinel forces
    /// a first-frame hydration whenever the loaded instance has a
    /// `Some(graph)`.
    pub graph_version: u32,

    /// Monotonically bumped only when `graph`'s **structure** changes —
    /// a node added/removed, a wire connected/disconnected, the whole def
    /// reverted. NOT bumped by a value-only edit (an inner param tweak) or a
    /// pure-layout edit (dragging a node), both of which still bump
    /// `graph_version` for the UI snapshot. The renderer hashes *this* into
    /// its rebuild key, so value/position edits refresh in place (state
    /// preserved) while only a real topology change forces a chain rebuild.
    /// Not serialized; resets to 0 on load.
    pub graph_structure_version: u32,

    // Legacy flat param fields (V1.0.0 format).
    pub legacy_param0: Option<f32>,
    pub legacy_param1: Option<f32>,
    pub legacy_param2: Option<f32>,
    pub legacy_param3: Option<f32>,

    /// Generator-only legacy flat field from V1.0.0 (before genParams
    /// nesting); serialized as `genParamVersion` for generator kind.
    pub legacy_param_version: Option<i32>,

    /// The "3D Shading" compile-level toggle (`docs/DEPTH_RELIGHT_DESIGN.md`
    /// D2, phase P5). `false` (default) is the exact graph that ships today,
    /// byte-identical — every existing project loads unchanged. `true` makes
    /// the renderer splice `relight_augment`'s D3 template before
    /// `final_output` on next rebuild. Shared by both kinds (effects and
    /// generators are both `PresetInstance`s), so one flag + one command path
    /// covers both cards. `PresetInstance` has custom Serialize/Deserialize
    /// impls below (not derived) — this field's wire handling lives there,
    /// not in a field attribute.
    pub relight: bool,
    /// The D3 relight-stage knobs. Always present (see
    /// [`RelightParams`]'s doc) — the toggle gates whether they're wired
    /// into the compiled graph, not whether they exist. Same custom-impl
    /// note as `relight` above.
    pub relight_params: RelightParams,
}

// ─── Wire-format helpers for `params` (V1.4) ───
//
// PARAM_STORAGE_DESIGN.md D4/D12: the typed loader understands ONLY the V1.4
// id-keyed `params` shape, and the manifest is the single authority. The four
// historical positional/keyed value shapes are gone — `manifold-io`'s
// `migrations::param_storage_v14` converts every preset instance to the V1.4
// shape BEFORE typed deserialization runs (V1 JSON + V2 ZIP), so that module is
// the only place positional param knowledge survives.
//
// Save is trivial: iterate the manifest, emit each entry by its own id
// ([`ManifestSer`]). Load is the §4 reconcile: seed bundled + user-added
// descriptors from the template/graph, overlay the file's state + calibration
// by id, append self-describing inline-`spec` entries ([`build_param_manifest`]).
// `meta.params` is READ at load only to reconstruct pre-P2 descriptors; it is
// NOT re-derived on save (a user param's spec rides the wire's inline `spec`,
// D12 §4 step 3; a bundled param's range edit rides the `calibration` block,
// D6). This keeps `meta.params` byte-stable across a round-trip and keeps the
// manifest the sole runtime authority.

/// The per-entry calibration block: the recalibrated range (and curve/invert
/// when non-default) a chevron popover wrote onto a *bundled* param. Present on
/// the wire iff [`crate::params::Param::calibrated`]; a bundled param without
/// it tracks the template (D6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalibrationWire {
    min: f32,
    max: f32,
    #[serde(default, skip_serializing_if = "curve_is_linear")]
    curve: crate::macro_bank::MacroCurve,
    #[serde(default, skip_serializing_if = "is_false")]
    invert: bool,
}

/// One entry in `PresetInstance.params` — the id is the map key. `base` iff
/// `base_tracked` (D5), `calibration` iff calibrated (D6), `spec` inline iff
/// the param is user-added (D12). `exposed` always serializes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ParamEntryWire {
    value: f32,
    #[serde(default = "default_true")]
    exposed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    calibration: Option<CalibrationWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spec: Option<crate::effect_graph_def::ParamSpecDef>,
}

impl ParamEntryWire {
    /// Wire entry for a manifest param.
    fn from_param(p: &crate::params::Param, base_tracked: bool) -> Self {
        Self {
            value: p.value,
            exposed: p.exposed,
            base: base_tracked.then_some(p.base),
            calibration: p.calibrated.then_some(CalibrationWire {
                min: p.spec.min,
                max: p.spec.max,
                curve: p.spec.curve,
                invert: p.spec.invert,
            }),
            spec: matches!(p.origin, crate::params::ParamOrigin::UserAdded)
                .then(|| p.spec.clone()),
        }
    }

    /// Overlay this file entry onto a manifest param already seeded from the
    /// template. A self-describing inline `spec` (user-added) replaces the
    /// descriptor first; then value/base/exposed; then a `calibration` block
    /// overrides the range (setting `calibrated`). Returns whether the entry
    /// carried a `base` (folds into the instance `base_tracked` bit).
    fn apply_to(&self, p: &mut crate::params::Param) -> bool {
        if let Some(spec) = &self.spec {
            p.spec = spec.clone();
        }
        p.value = self.value;
        p.base = self.base.unwrap_or(self.value);
        p.exposed = self.exposed;
        if let Some(c) = &self.calibration {
            p.spec.min = c.min;
            p.spec.max = c.max;
            p.spec.curve = c.curve;
            p.spec.invert = c.invert;
            p.calibrated = true;
        }
        self.base.is_some()
    }
}

/// Serialize a `PresetInstance`'s `params` — the single V1.4 id-keyed map for
/// BOTH kinds (D12). Emits each manifest entry by its own id in card order; no
/// registry lookup, no positional prefix/tail split.
struct ManifestSer<'a> {
    manifest: &'a crate::params::ParamManifest,
    base_tracked: bool,
}

impl Serialize for ManifestSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.manifest.len()))?;
        for p in self.manifest.iter() {
            if p.id().is_empty() {
                continue;
            }
            map.serialize_entry(p.id(), &ParamEntryWire::from_param(p, self.base_tracked))?;
        }
        map.end()
    }
}

/// Serializes the instance's graph override with `preset_metadata.params`
/// rewritten from the live manifest (PARAM_STORAGE_BOUNDARIES_DESIGN.md D12/
/// D4: `meta.params` is derived on save from the manifest, the sole live
/// authority — not a second thing calibration keeps in sync by hand). Every
/// OTHER field on the graph (nodes, wires, `preset_metadata.bindings`,
/// `skip_mode`, ...) serializes unchanged — this wrapper touches only the
/// `params` list's per-entry CONTENT, by id, never its shape: which entries
/// exist is still governed by expose/unexpose (`append_user_binding` /
/// `remove_user_binding_by_id`), not by this derivation.
struct GraphWithDerivedParams<'a> {
    graph: &'a crate::effect_graph_def::EffectGraphDef,
    manifest: &'a crate::params::ParamManifest,
}

impl Serialize for GraphWithDerivedParams<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(meta) = self.graph.preset_metadata.as_ref() else {
            // No metadata to derive onto — serialize the graph as-is.
            return self.graph.serialize(serializer);
        };
        if meta.params.iter().all(|p| self.manifest.get(&p.id).is_none()) {
            // Nothing on this graph resolves against the manifest (e.g. a
            // fused/frozen def with no matching live instance) — nothing to
            // derive; avoid the clone.
            return self.graph.serialize(serializer);
        }
        // Save-time-only cost (not per-frame) — the manifest double-build
        // this mirrors (D1) is priced the same way.
        let mut derived = self.graph.clone();
        if let Some(meta) = derived.preset_metadata.as_mut() {
            for spec in meta.params.iter_mut() {
                if let Some(p) = self.manifest.get(&spec.id) {
                    *spec = p.spec.clone();
                }
            }
        }
        derived.serialize(serializer)
    }
}

/// A minimal `ParamSpecDef` for a `user_added` binding with no matching
/// `meta.params` entry (pre-spec files): range 0..1, linear, integral-ness
/// inferred from the binding's convert.
fn spec_from_binding(
    b: &crate::effect_graph_def::BindingDef,
) -> crate::effect_graph_def::ParamSpecDef {
    let whole_numbers = matches!(
        b.convert,
        ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger
    );
    crate::effect_graph_def::ParamSpecDef {
        id: b.id.clone(),
        name: b.label.clone(),
        min: 0.0,
        max: 1.0,
        default_value: b.default_value,
        whole_numbers,
        is_toggle: matches!(b.convert, ParamConvert::BoolThreshold),
        is_trigger: matches!(b.convert, ParamConvert::Trigger),
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        // Pre-spec fallback: a `BindingDef` records no angle-ness, so the flag
        // starts false. A later expose/edit reseeds it with the real value.
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
    }
}

/// Placeholder descriptor for a wire entry whose preset template is
/// unresolvable at load (see `build_param_manifest`'s keep-don't-drop
/// branch): identity + state preserved, range 0..1 unless the entry carries
/// a calibration block (which `apply_to` overlays afterward).
fn placeholder_spec(
    id: &str,
    entry: &ParamEntryWire,
) -> crate::effect_graph_def::ParamSpecDef {
    crate::effect_graph_def::ParamSpecDef {
        id: id.to_string(),
        name: id.to_string(),
        min: 0.0,
        max: 1.0,
        default_value: entry.value,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
    }
}

/// Template + user-added descriptors a fresh manifest is seeded from at load,
/// in card order. Bundled descriptors: a graph-backed generator's own
/// `meta.params`, else the registry `param_defs`. User-added descriptors: the
/// per-instance graph's `user_added` bindings (spec from `meta.params`, else
/// synthesized). This load-time read of the graph reconstructs the manifest;
/// the manifest is the authority afterward.
fn gather_known_params(
    is_generator: bool,
    effect_type: &PresetTypeId,
    graph: &Option<EffectGraphDef>,
) -> Vec<(crate::effect_graph_def::ParamSpecDef, crate::params::ParamOrigin)> {
    use crate::params::ParamOrigin;
    let meta = graph.as_ref().and_then(|g| g.preset_metadata.as_ref());

    // Generator with a per-instance graph: its `meta.params` is the full
    // ordered descriptor authority; origin is driven by a matching `user_added`
    // binding.
    if is_generator
        && let Some(meta) = meta
        && !meta.params.is_empty()
    {
        return meta
            .params
            .iter()
            .map(|s| {
                let user = meta.bindings.iter().any(|b| b.user_added && b.id == s.id);
                let origin = if user {
                    ParamOrigin::UserAdded
                } else {
                    ParamOrigin::Bundled
                };
                (s.clone(), origin)
            })
            .collect();
    }

    // Effect (or graph-less generator): bundled from the registry, then the
    // user-added tail from the graph's `user_added` bindings.
    let mut out = Vec::new();
    if let Some(def) = crate::preset_definition_registry::try_get(effect_type) {
        for pd in def.param_defs.iter() {
            out.push((pd.spec.clone(), ParamOrigin::Bundled));
        }
    }
    if let Some(meta) = meta {
        for b in meta.bindings.iter().filter(|b| b.user_added) {
            let spec = meta
                .params
                .iter()
                .find(|p| p.id == b.id)
                .cloned()
                .unwrap_or_else(|| spec_from_binding(b));
            out.push((spec, ParamOrigin::UserAdded));
        }
    }
    out
}

/// Whether a descriptor authority resolves for this instance right now: an
/// inline generator graph's own `meta.params`, or a registry template.
/// Shared by [`build_param_manifest`] (decides informed-drop vs
/// keep-don't-drop) and [`PresetInstance::reconcile_manifest`] (decides
/// whether a reconcile pass definitively resolved the instance, so its
/// `pending_wire` stash can be cleared, or whether it should stay parked for
/// a later retry — BUG-036's class).
fn template_known_for(
    is_generator: bool,
    effect_type: &PresetTypeId,
    graph: &Option<EffectGraphDef>,
) -> bool {
    (is_generator
        && graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .is_some_and(|m| !m.params.is_empty()))
        || crate::preset_definition_registry::try_get(effect_type).is_some()
}

/// Build a `PresetInstance`'s manifest from its V1.4 `params` wire map (§4 load
/// reconcile): seed known descriptors, overlay each file entry's state +
/// calibration by id (alias-aware), append self-describing inline-`spec`
/// entries that match nothing, and drop unknown entries with a warning
/// (today's unknown-id policy). Returns the manifest + the `base_tracked` bit.
fn build_param_manifest(
    is_generator: bool,
    effect_type: &PresetTypeId,
    graph: &Option<EffectGraphDef>,
    wire: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
) -> (crate::params::ParamManifest, bool) {
    use crate::params::{Param, ParamOrigin};
    let mut entries: Vec<Param> = gather_known_params(is_generator, effect_type, graph)
        .into_iter()
        .map(|(spec, origin)| match origin {
            ParamOrigin::Bundled => Param::bundled(spec),
            ParamOrigin::UserAdded => Param::user_added(spec),
        })
        .collect();

    // Alias map (old id → new id; `None` = deprecated, drop) from the graph's
    // per-preset aliases plus the registry's legacy renames.
    let mut alias: ahash::AHashMap<String, Option<String>> = ahash::AHashMap::new();
    if let Some(meta) = graph.as_ref().and_then(|g| g.preset_metadata.as_ref()) {
        for a in &meta.param_aliases {
            alias.insert(a.old.clone(), a.new.clone());
        }
    }
    if let Some(def) = crate::preset_definition_registry::try_get(effect_type) {
        for (old, new) in def.legacy_param_aliases.iter() {
            alias
                .entry((*old).to_string())
                .or_insert_with(|| new.map(str::to_string));
        }
    }

    // Whether a descriptor authority was actually available for this
    // instance: an inline generator graph's `meta.params`, or a registry
    // template. Only an *informed* drop is allowed — when the template is
    // resolvable and says the id is gone, that's a deliberate deprecation
    // (today's unknown-id policy). When NO template resolves (e.g. a
    // project-local import whose def isn't registered at deserialize time),
    // dropping is silent data loss (BUG-036), so the entry is kept on a
    // placeholder spec instead: state (value/base/exposed/calibration) is
    // everything the file stores for a bundled param, and the next load
    // with the template present reconciles it against the real descriptor.
    let template_known = template_known_for(is_generator, effect_type, graph);

    let mut base_tracked = false;
    if let Some(wire) = wire {
        for (raw_id, entry) in wire {
            let id = match alias.get(&raw_id) {
                Some(Some(new_id)) => new_id.clone(),
                Some(None) => continue, // deprecated, no replacement
                None => raw_id,
            };
            if let Some(p) = entries.iter_mut().find(|p| p.id() == id) {
                base_tracked |= entry.apply_to(p);
            } else if let Some(spec) = &entry.spec {
                let mut p = Param::user_added(spec.clone());
                base_tracked |= entry.apply_to(&mut p);
                entries.push(p);
            } else if !template_known {
                eprintln!(
                    "[manifold-core] keeping param {id:?} on {effect_type:?} load with a \
                     placeholder spec (preset template unresolved — project-local preset \
                     not registered yet?)"
                );
                // Bundled origin: the placeholder spec never serializes
                // (only state does), so the real descriptor wins on the
                // next resolvable load. Card position is tail-appended in
                // wire order — acceptable for this recovery path.
                let mut p = Param::bundled(placeholder_spec(&id, &entry));
                base_tracked |= entry.apply_to(&mut p);
                entries.push(p);
            } else {
                eprintln!(
                    "[manifold-core] dropping unknown param id {id:?} on {effect_type:?} load \
                     (no template descriptor, no inline spec)"
                );
            }
        }
    }
    (crate::params::ParamManifest::from_params(entries), base_tracked)
}

// ─── Custom Serialize / Deserialize for PresetInstance ───

impl Serialize for PresetInstance {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        // Generator-kind serializes the legacy `PresetInstance` shape
        // (generatorType + the generator-keyed param maps + envelopes +
        // genParamVersion) so existing generator fixtures stay byte-identical.
        if self.is_generator() {
            return self.serialize_as_generator(serializer);
        }

        // `params` always emits (D5: `base` rides inside each entry now,
        // gated by `base_tracked` — no more separate pre-modulation-base
        // field). Other optional fields use the same `skip_if_none` policy
        // as the previous derive(Serialize) impl.
        let mut field_count = 5; // id, effectType, enabled, collapsed, params
        if self.drivers.is_some() {
            field_count += 1;
        }
        if self.envelopes.is_some() {
            field_count += 1;
        }
        if self.ableton_mappings.is_some() {
            field_count += 1;
        }
        if self.audio_mods.is_some() {
            field_count += 1;
        }
        if self.automation_lanes.is_some() {
            field_count += 1;
        }
        if self.group_id.is_some() {
            field_count += 1;
        }
        if self.graph.is_some() {
            field_count += 1;
        }
        if self.legacy_param0.is_some() {
            field_count += 1;
        }
        if self.legacy_param1.is_some() {
            field_count += 1;
        }
        if self.legacy_param2.is_some() {
            field_count += 1;
        }
        if self.legacy_param3.is_some() {
            field_count += 1;
        }
        if self.relight {
            field_count += 1;
        }
        if !self.relight_params.is_default() {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("PresetInstance", field_count)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("effectType", &self.effect_type)?;
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("collapsed", &self.collapsed)?;
        // The `params` map carries each manifest entry by its own id in card
        // order (bundled + user-added unified); the user-added bindings still
        // ride out inside the `graph` field.
        s.serialize_field(
            "params",
            &ManifestSer {
                manifest: &self.params,
                base_tracked: self.base_tracked,
            },
        )?;
        if let Some(d) = &self.drivers {
            s.serialize_field("drivers", d)?;
        }
        // Envelope-home unification: effect envelopes ride on the instance.
        if let Some(e) = &self.envelopes {
            s.serialize_field("envelopes", e)?;
        }
        if let Some(m) = &self.ableton_mappings {
            s.serialize_field("abletonMappings", m)?;
        }
        if let Some(a) = &self.audio_mods {
            s.serialize_field("audioMods", a)?;
        }
        if let Some(a) = &self.automation_lanes {
            s.serialize_field("automationLanes", a)?;
        }
        if let Some(g) = &self.group_id {
            s.serialize_field("groupId", g)?;
        }
        // `graph` is skipped when None — same round-trip-invariance
        // policy. `None` means "use the catalog default for this
        // effect type"; only per-instance overrides emit. `params` on the
        // wrapper is derived from the live manifest (D12) — see
        // `GraphWithDerivedParams`.
        if let Some(graph) = &self.graph {
            s.serialize_field(
                "graph",
                &GraphWithDerivedParams { graph, manifest: &self.params },
            )?;
        }
        if let Some(v) = self.legacy_param0 {
            s.serialize_field("param0", &v)?;
        }
        if let Some(v) = self.legacy_param1 {
            s.serialize_field("param1", &v)?;
        }
        if let Some(v) = self.legacy_param2 {
            s.serialize_field("param2", &v)?;
        }
        if let Some(v) = self.legacy_param3 {
            s.serialize_field("param3", &v)?;
        }
        if self.relight {
            s.serialize_field("relight", &self.relight)?;
        }
        if !self.relight_params.is_default() {
            s.serialize_field("relightParams", &self.relight_params)?;
        }
        s.end()
    }
}

impl PresetInstance {
    /// Serialize a generator-kind instance in the legacy `PresetInstance`
    /// wire shape (so generator fixtures round-trip byte-identically). Ported
    /// from the former `impl Serialize for PresetInstance`.
    fn serialize_as_generator<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut field_count = 2; // generatorType + params
        if self.drivers.is_some() {
            field_count += 1;
        }
        if self.envelopes.is_some() {
            field_count += 1;
        }
        if self.ableton_mappings.is_some() {
            field_count += 1;
        }
        if self.audio_mods.is_some() {
            field_count += 1;
        }
        if self.automation_lanes.is_some() {
            field_count += 1;
        }
        if self.graph.is_some() {
            field_count += 1;
        }
        if self.legacy_param_version.is_some() {
            field_count += 1;
        }
        if self.relight {
            field_count += 1;
        }
        if !self.relight_params.is_default() {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("PresetInstance", field_count)?;
        s.serialize_field("generatorType", &self.effect_type)?;
        s.serialize_field(
            "params",
            &ManifestSer {
                manifest: &self.params,
                base_tracked: self.base_tracked,
            },
        )?;
        if let Some(d) = &self.drivers {
            s.serialize_field("drivers", d)?;
        }
        if let Some(e) = &self.envelopes {
            s.serialize_field("envelopes", e)?;
        }
        if let Some(m) = &self.ableton_mappings {
            s.serialize_field("abletonMappings", m)?;
        }
        if let Some(a) = &self.audio_mods {
            s.serialize_field("audioMods", a)?;
        }
        if let Some(a) = &self.automation_lanes {
            s.serialize_field("automationLanes", a)?;
        }
        if let Some(g) = &self.graph {
            // `params` on the wrapper is derived from the live manifest
            // (D12) — see `GraphWithDerivedParams`.
            s.serialize_field(
                "graph",
                &GraphWithDerivedParams { graph: g, manifest: &self.params },
            )?;
        }
        if let Some(v) = self.legacy_param_version {
            s.serialize_field("genParamVersion", &v)?;
        }
        if self.relight {
            s.serialize_field("relight", &self.relight)?;
        }
        if !self.relight_params.is_default() {
            s.serialize_field("relightParams", &self.relight_params)?;
        }
        s.end()
    }
}

/// §9 U5 load migration: a legacy `audioTrigger` field (§8 D2's now-deleted
/// `AudioTriggerMod`) converts to a `ParameterAudioMod` on the instance's
/// trigger-gate param — the same param the `clip_trigger` toggle card lives
/// on (`spec.is_trigger_gate`). Runs from BOTH `PresetInstance` Deserialize
/// paths (effect `Raw` and generator `GeneratorInstanceRaw`), which is also
/// the only choke point either V1 JSON or V2 ZIP load ever passes through
/// (`manifold-io`'s loader deserializes the whole `Project` via one
/// `serde_json::from_str`, so there is nothing V2-specific to wire).
///
/// `enabled` and `mode` carry over exactly; `sensitivity` (an input-gain-
/// style fire-threshold knob) approximates onto `AudioModShape.sensitivity`
/// (the closest surviving "how hard is this to trigger" knob) — U5 is
/// explicit that exact-feel fidelity is NOT owed here, since the field
/// existed in roughly one project for one day. No trigger-gate param on this
/// instance (a hand-edited file, or one that predates the flag) drops the
/// config with a warning rather than guessing a target.
fn migrate_legacy_audio_trigger(
    legacy: crate::audio_trigger::LegacyAudioTriggerMod,
    params: &crate::params::ParamManifest,
    audio_mods: &mut Option<Vec<crate::audio_mod::ParameterAudioMod>>,
) {
    let Some(gate_id) = params
        .iter()
        .find(|p| p.spec.is_trigger_gate)
        .map(|p| p.spec.id.clone())
    else {
        log::warn!(
            "[Migration] legacy audioTrigger config found no trigger-gate param on this \
             instance; dropping it (the instance predates the trigger-gate flag or was \
             hand-edited)"
        );
        return;
    };

    let crate::audio_mod::AudioModSource { send_id, feature } = legacy.source;
    let mut m = crate::audio_mod::ParameterAudioMod::new(gate_id.into(), send_id, feature);
    m.enabled = legacy.enabled;
    m.trigger_mode = Some(legacy.mode);
    m.shape.sensitivity = legacy.sensitivity;
    audio_mods.get_or_insert_with(Vec::new).push(m);
}

impl<'de> Deserialize<'de> for PresetInstance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default = "generate_effect_id")]
            id: EffectId,
            #[serde(deserialize_with = "crate::preset_type_id::deserialize_effect_type")]
            effect_type: PresetTypeId,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            collapsed: bool,
            #[serde(default)]
            params: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
            #[serde(default)]
            drivers: Option<Vec<ParameterDriver>>,
            #[serde(default)]
            envelopes: Option<Vec<ParamEnvelope>>,
            #[serde(default)]
            ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
            #[serde(default)]
            audio_mods: Option<Vec<crate::audio_mod::ParameterAudioMod>>,
            /// §9 U5: the deleted `AudioTriggerMod`'s wire shape, kept only so
            /// an old project's `audioTrigger` field migrates onto
            /// `audio_mods` below — see [`migrate_legacy_audio_trigger`].
            #[serde(default, rename = "audioTrigger")]
            legacy_audio_trigger: Option<crate::audio_trigger::LegacyAudioTriggerMod>,
            #[serde(default)]
            automation_lanes: Option<Vec<AutomationLane>>,
            #[serde(default)]
            group_id: Option<EffectGroupId>,
            #[serde(default)]
            graph: Option<EffectGraphDef>,
            #[serde(default, rename = "param0")]
            legacy_param0: Option<f32>,
            #[serde(default, rename = "param1")]
            legacy_param1: Option<f32>,
            #[serde(default, rename = "param2")]
            legacy_param2: Option<f32>,
            #[serde(default, rename = "param3")]
            legacy_param3: Option<f32>,
            #[serde(default)]
            relight: bool,
            #[serde(default)]
            relight_params: RelightParams,
        }

        let raw = Raw::deserialize(deserializer)?;
        // V1.4 §4 reconcile: seed the manifest from the effect's registry
        // template + the graph's `user_added` bindings, then overlay the
        // incoming `params` map (value/exposed/base/calibration by id, inline
        // spec for self-describing user params). Stash a copy of the wire
        // map first (PARAM_STORAGE_BOUNDARIES_DESIGN.md D1) — the loader's
        // `reconcile_param_manifests` re-runs this same build later, against
        // whatever registry state exists once the project's own embedded
        // presets have been installed.
        let pending_wire = raw.params.clone();
        let (params, base_tracked) =
            build_param_manifest(false, &raw.effect_type, &raw.graph, raw.params);

        let mut audio_mods = raw.audio_mods;
        if let Some(legacy) = raw.legacy_audio_trigger {
            migrate_legacy_audio_trigger(legacy, &params, &mut audio_mods);
        }

        Ok(PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: raw.id,
            effect_type: raw.effect_type,
            enabled: raw.enabled,
            collapsed: raw.collapsed,
            params,
            base_tracked,
            pending_wire,
            drivers: raw.drivers,
            envelopes: raw.envelopes,
            ableton_mappings: raw.ableton_mappings,
            audio_mods,
            automation_lanes: raw.automation_lanes,
            group_id: raw.group_id,
            graph: raw.graph,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: raw.legacy_param0,
            legacy_param1: raw.legacy_param1,
            legacy_param2: raw.legacy_param2,
            legacy_param3: raw.legacy_param3,
            legacy_param_version: None,
            relight: raw.relight,
            relight_params: raw.relight_params,
        })
    }
}

/// Wire shape for a generator-kind instance — the legacy `PresetInstance`
/// JSON. Used by [`deserialize_generator_instance`] (and its Option wrapper) so
/// `Layer.gen_params` decodes into a `PresetInstance { kind: Generator }`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeneratorInstanceRaw {
    #[serde(
        default,
        deserialize_with = "crate::preset_type_id::deserialize_generator_type"
    )]
    generator_type: PresetTypeId,
    #[serde(default)]
    params: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
    #[serde(default)]
    drivers: Option<Vec<ParameterDriver>>,
    #[serde(default)]
    envelopes: Option<Vec<ParamEnvelope>>,
    #[serde(default)]
    ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
    #[serde(default)]
    audio_mods: Option<Vec<crate::audio_mod::ParameterAudioMod>>,
    /// §9 U5: see `Raw::legacy_audio_trigger` on the effect-kind Deserialize
    /// impl above — same migration, generator wire shape.
    #[serde(default, rename = "audioTrigger")]
    legacy_audio_trigger: Option<crate::audio_trigger::LegacyAudioTriggerMod>,
    #[serde(default)]
    automation_lanes: Option<Vec<AutomationLane>>,
    /// The generator's per-instance graph override. Lives on the generator
    /// `PresetInstance` now (graph-home unification) exactly like an effect's
    /// `graph`; older projects carried it on the layer (`generatorGraph`) and
    /// the load migration relocates it here.
    #[serde(default)]
    graph: Option<EffectGraphDef>,
    #[serde(default, rename = "genParamVersion")]
    legacy_param_version: Option<i32>,
    #[serde(default)]
    relight: bool,
    #[serde(default)]
    relight_params: RelightParams,
}

impl GeneratorInstanceRaw {
    fn into_instance(self) -> PresetInstance {
        // V1.4 §4 reconcile: a graph-backed generator's own `meta.params` is
        // the descriptor authority (else the registry); overlay the incoming
        // `params` map by id. Stash a copy of the wire map first (D1) — see
        // the effect-kind `Deserialize` impl above for the same pattern.
        let pending_wire = self.params.clone();
        let (params, base_tracked) =
            build_param_manifest(true, &self.generator_type, &self.graph, self.params);
        let mut audio_mods = self.audio_mods;
        if let Some(legacy) = self.legacy_audio_trigger {
            migrate_legacy_audio_trigger(legacy, &params, &mut audio_mods);
        }
        PresetInstance {
            kind: crate::preset_def::PresetKind::Generator,
            id: generate_effect_id(),
            effect_type: self.generator_type,
            enabled: true,
            collapsed: false,
            params,
            base_tracked,
            pending_wire,
            drivers: self.drivers,
            envelopes: self.envelopes,
            ableton_mappings: self.ableton_mappings,
            audio_mods,
            automation_lanes: self.automation_lanes,
            group_id: None,
            graph: self.graph,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: self.legacy_param_version,
            relight: self.relight,
            relight_params: self.relight_params,
        }
    }
}

/// Decode a generator-kind `PresetInstance` from the legacy
/// `PresetInstance` JSON shape.
pub fn deserialize_generator_instance<'de, D>(deserializer: D) -> Result<PresetInstance, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(GeneratorInstanceRaw::deserialize(deserializer)?.into_instance())
}

/// `deserialize_with` for an `Option<PresetInstance>` field that holds a
/// generator (e.g. `Layer.gen_params`): decode the legacy generator JSON shape
/// into a `PresetInstance { kind: Generator }`.
pub fn deserialize_opt_generator_instance<'de, D>(
    deserializer: D,
) -> Result<Option<PresetInstance>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<GeneratorInstanceRaw>::deserialize(deserializer)?.map(|raw| raw.into_instance()))
}

impl PresetInstance {
    /// The per-instance graph override (`None` ⇒ use the catalog default).
    /// One home for both effects and generators after the graph-home
    /// unification (the generator graph used to live on `Layer`).
    #[inline]
    pub fn graph_def(&self) -> &Option<EffectGraphDef> {
        &self.graph
    }

    /// Mutable handle to the per-instance graph override, for the graph
    /// editing commands.
    #[inline]
    pub fn graph_def_mut(&mut self) -> &mut Option<EffectGraphDef> {
        &mut self.graph
    }

    /// Bump the graph version so the renderer re-snapshots the graph for the
    /// UI. Bumped by *every* graph edit (value, position, structure). Does NOT
    /// on its own force a chain rebuild — see [`Self::bump_graph_structure_version`].
    #[inline]
    pub fn bump_graph_version(&mut self) {
        self.graph_version = self.graph_version.wrapping_add(1);
    }

    /// Bump the *structure* version — and the snapshot version with it — for an
    /// edit that changes the graph's topology (node/wire add or remove, a full
    /// revert). The renderer keys its rebuild on the structure version, so this
    /// is the only path that forces a chain rebuild (and the state reset that
    /// comes with it). Value/position-only edits call
    /// [`Self::bump_graph_version`] instead, so they refresh in place.
    #[inline]
    pub fn bump_graph_structure_version(&mut self) {
        self.graph_structure_version = self.graph_structure_version.wrapping_add(1);
        self.graph_version = self.graph_version.wrapping_add(1);
    }

    /// Write the user-set base value (pre-modulation) for a `param_id`.
    /// Returns `true` if the id resolved. Thin id-forwarding wrapper kept for
    /// the editing commands that drive a card param through a [`GraphTarget`];
    /// [`Self::set_base_param`] is now itself id-keyed and returns the same
    /// bool.
    pub fn set_base_param_by_id(&mut self, param_id: &str, value: f32) -> bool {
        self.set_base_param(param_id, value)
    }

    /// Create a new effect-kind PresetInstance with the given type.
    pub fn new(effect_type: PresetTypeId) -> Self {
        Self {
            kind: crate::preset_def::PresetKind::Effect,
            id: generate_effect_id(),
            effect_type,
            enabled: true,
            collapsed: false,
            params: crate::params::ParamManifest::default(),
            base_tracked: false,
            pending_wire: None,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            automation_lanes: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
            relight: false,
            relight_params: RelightParams::default(),
        }
    }

    /// Create a new generator-kind PresetInstance, fully initialized to the
    /// generator type's registry defaults. The generator mirror of [`Self::new`]
    /// (ported from the former `PresetInstance::new`).
    pub fn new_generator(generator_type: PresetTypeId) -> Self {
        let mut s = Self {
            kind: crate::preset_def::PresetKind::Generator,
            id: generate_effect_id(),
            effect_type: generator_type,
            enabled: true,
            collapsed: false,
            params: crate::params::ParamManifest::default(),
            base_tracked: false,
            pending_wire: None,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            automation_lanes: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
            relight: false,
            relight_params: RelightParams::default(),
        };
        s.init_defaults();
        s
    }

    /// Rebuild this instance's `ParamManifest` from its stashed wire entries
    /// (D1) against the CURRENT registry — called by
    /// [`crate::project::Project::reconcile_param_manifests`] once the
    /// project's own embedded presets have been installed. No-op if there is
    /// no stash (a freshly-constructed instance, or one already resolved).
    ///
    /// Idempotent, and safe to call more than once: if this pass still can't
    /// resolve a template for the instance (`template_known_for` false —
    /// BUG-036's class, e.g. a project-local preset not registered yet), the
    /// stash is kept rather than cleared, so a *later* reconcile call — once
    /// the registry catches up — can retry from the same wire entries. Once
    /// a pass resolves a real template, the stash is cleared; this is the
    /// common case (one load, one reconcile).
    pub(crate) fn reconcile_manifest(&mut self) {
        let Some(wire) = self.pending_wire.clone() else {
            return;
        };
        let (params, base_tracked) =
            build_param_manifest(self.is_generator(), &self.effect_type, &self.graph, Some(wire));
        self.params = params;
        self.base_tracked = base_tracked;
        if template_known_for(self.is_generator(), &self.effect_type, &self.graph) {
            self.pending_wire = None;
        }
    }

    /// Whether this instance's preset def failed to resolve at load (BUG-036's
    /// class) — no registry template AND no inline generator `meta.params`,
    /// so its saved params are kept on a placeholder spec rather than dropped
    /// (`build_param_manifest`'s keep-don't-drop branch). Used by
    /// `Project::reconcile_param_manifests` (BUG-079) to count how many
    /// instances need the "opened with repairs" toast to name them — the
    /// same condition `PresetRuntime::try_build` hits later at chain-build
    /// time when it can't find a `LoadedPresetView` for the effect type and
    /// falls back to source passthrough (`preset_runtime.rs`).
    pub fn template_unresolved(&self) -> bool {
        !template_known_for(self.is_generator(), &self.effect_type, &self.graph)
    }

    /// `true` when this instance's manifest was built against an incomplete
    /// registry and hasn't been reconciled yet (BUG-080). `pending_wire` is
    /// the structural marker for "provisional" — set at deserialize-time
    /// build, cleared by [`Self::reconcile_manifest`] once a real template
    /// resolves. A provisional manifest reaching a runtime seam (chain build,
    /// UI row translation) means a load/ingest path skipped the reconcile
    /// call; see `docs/PARAM_MANIFEST_GATE_DESIGN.md` D1.
    pub fn manifest_provisional(&self) -> bool {
        self.pending_wire.is_some()
    }

    #[inline]
    pub fn is_generator(&self) -> bool {
        matches!(self.kind, crate::preset_def::PresetKind::Generator)
    }

    /// Read-only access to the effect type.
    #[inline]
    pub fn effect_type(&self) -> &PresetTypeId {
        &self.effect_type
    }

    /// Retarget this instance at a different preset id WITHOUT resetting params
    /// (unlike `change_type`). Used by the fork flow: a fork is a copy of the
    /// same preset under a new id, so the existing param values stay valid.
    #[inline]
    pub fn set_preset_id(&mut self, id: PresetTypeId) {
        self.effect_type = id;
    }

    /// Read-only access to the generator type (alias of [`Self::effect_type`]
    /// — the `effect_type` field holds the preset type for both kinds).
    #[inline]
    pub fn generator_type(&self) -> &PresetTypeId {
        &self.effect_type
    }

    /// Has any drivers? Unity PresetInstance.cs line 28.
    pub fn has_drivers(&self) -> bool {
        self.drivers.as_ref().is_some_and(|d| !d.is_empty())
    }

    pub fn clone_deep(&self) -> Self {
        self.clone()
    }

    /// Assign a fresh EffectId (used when deep-cloning a layer or effect chain).
    pub fn regenerate_id(&mut self) {
        self.id = EffectId::new(crate::math::short_id());
    }

    /// A fresh, independent copy of this instance for duplication / paste.
    ///
    /// Mints a new [`EffectId`] (so the copy is a distinct identity, not a
    /// reference to the original) and applies the "fresh copy" carry-rule:
    /// hardware/external bindings are dropped — `ableton_mappings` and
    /// `audio_mods` are cleared, so a pasted card is NOT mapped to the same
    /// Ableton control or audio send as its source. Per-instance modulation
    /// that has no external binding (`drivers`, `envelopes`) is kept.
    ///
    /// `group_id` is left untouched — the caller decides: a cross-chain paste
    /// drops it (the source group doesn't exist in the destination); a
    /// whole-layer duplicate remaps it to the duplicated group.
    pub fn duplicated(&self) -> Self {
        let mut copy = self.clone();
        copy.regenerate_id();
        copy.ableton_mappings = None;
        copy.audio_mods = None;
        copy
    }

    /// Number of parameters on this instance (manifest length).
    pub fn param_count(&self) -> usize {
        self.params.len()
    }

    /// Read the effective (modulated) value of param `id`. Unknown id → 0.0.
    pub fn get_param(&self, id: &str) -> f32 {
        self.params.get(id).map(|p| p.value).unwrap_or(0.0)
    }

    /// Whether param `id` is exposed (a visible card slider). Unknown id →
    /// `true`, the conservative always-visible default.
    pub fn is_param_exposed(&self, id: &str) -> bool {
        self.params.get(id).map(|p| p.exposed).unwrap_or(true)
    }

    /// Toggle a param's exposure flag. No-op if `id` is unknown.
    pub fn set_param_exposed(&mut self, id: &str, exposed: bool) {
        if let Some(p) = self.params.get_mut(id) {
            p.exposed = exposed;
        }
    }

    /// Write the effective (modulated) value of param `id`. No-op if `id` is
    /// unknown — a value write can't create a param (the manifest is seeded at
    /// instantiation/load; there is no positional grow). No registry clamp
    /// (the renderer reshape is the single place range is enforced toward the
    /// inner node).
    pub fn set_param(&mut self, id: &str, value: f32) {
        if let Some(p) = self.params.get_mut(id) {
            p.value = value;
        }
    }

    /// Read the user-set base value (before modulation) of param `id`. While
    /// base isn't tracked, `Param.base` may be stale, so fall through to the
    /// effective value.
    pub fn get_base_param(&self, id: &str) -> f32 {
        if self.base_tracked
            && let Some(p) = self.params.get(id)
        {
            return p.base;
        }
        self.get_param(id)
    }

    /// Set the user-intended base value of param `id`. **The single funnel**
    /// every live hand (UI slider, Ableton macro, OSC, macro bank, editing
    /// commands) writes through — marks the param `touched` so the
    /// automation-lane override latch (`docs/AUTOMATION_LANES_DESIGN.md` §4) can
    /// detect it. Returns whether `id` resolved. System-level seeding that is
    /// NOT a live gesture uses [`Self::write_base_param`] /
    /// [`Self::set_base_param_from_automation`] (no `touched`).
    pub fn set_base_param(&mut self, id: &str, value: f32) -> bool {
        if !self.write_base_param(id, value) {
            return false;
        }
        if let Some(p) = self.params.get_mut(id) {
            p.touched = true;
        }
        true
    }

    /// Automation-lane-only sibling of [`Self::set_base_param`]: writes
    /// `base`/`value` identically but does **not** set `touched` (using
    /// `set_base_param` from the automation evaluator would self-latch the lane
    /// as overridden the next frame). See `docs/AUTOMATION_LANES_DESIGN.md` §4.
    pub fn set_base_param_from_automation(&mut self, id: &str, value: f32) {
        self.write_base_param(id, value);
    }

    /// Shared base writer: `ensure_base_values` then write base + effective on
    /// param `id`. Does NOT set `touched` (callers decide). Returns whether
    /// `id` resolved. No generator migrate-on-touch: the manifest is fully
    /// seeded at instantiation/load, so there is no lazy positional tail to
    /// grow.
    pub(crate) fn write_base_param(&mut self, id: &str, value: f32) -> bool {
        self.ensure_base_values();
        match self.params.get_mut(id) {
            Some(p) => {
                // Setting base also sets the effective; modulation later
                // overrides value.
                p.base = value;
                p.value = value;
                true
            }
            None => false,
        }
    }

    /// Reset every param's effective value from its base value.
    pub fn reset_param_effectives(&mut self) {
        self.ensure_base_values();
        for p in self.params.iter_mut() {
            p.value = p.base;
        }
    }

    /// Begin tracking base: capture each param's current effective value as its
    /// base (when not already tracked) so subsequent modulation reads a stable
    /// pre-mod value.
    pub fn ensure_base_values(&mut self) {
        if !self.base_tracked {
            for p in self.params.iter_mut() {
                p.base = p.value;
            }
            self.base_tracked = true;
        }
    }

    pub fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        self.drivers
            .as_ref()?
            .iter()
            .find(|d| d.param_id == param_id)
    }

    /// Get drivers list reference (may be None).
    pub fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        self.drivers.as_ref()
    }

    /// Create a driver for a param id.
    pub fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver {
        let driver = ParameterDriver::new(param_id, BeatDivision::Quarter, DriverWaveform::Sine);
        self.drivers_mut().push(driver);
        self.drivers.as_ref().unwrap().last().unwrap()
    }

    /// Remove driver by param id.
    pub fn remove_driver(&mut self, param_id: &str) {
        if let Some(drivers) = &mut self.drivers {
            drivers.retain(|d| d.param_id != param_id);
        }
    }

    /// The per-instance graph's user-added [`BindingDef`]s, in declaration
    /// order — the **single source of truth** for this effect's
    /// user-exposed parameters after the binding-storage unification
    /// (`PRESET_UNIFICATION_PLAN.md` step 3). User bindings no longer live
    /// in a parallel `PresetInstance.user_param_bindings` Vec; they are the
    /// `user_added` entries of `graph.preset_metadata.bindings`, exactly
    /// like the generator side. Empty when the effect has no per-instance
    /// graph or no user-added bindings. Order matches the `param_values`
    /// user-tail order (registry `param_count + j`).
    pub fn user_added_bindings(&self) -> impl Iterator<Item = &crate::effect_graph_def::BindingDef> {
        self.graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .into_iter()
            .flat_map(|m| m.bindings.iter())
            .filter(|b| b.user_added)
    }

    /// Number of user-exposed parameter bindings on this instance.
    pub fn user_param_count(&self) -> usize {
        self.user_added_bindings().count()
    }

    /// Synthesize the in-memory [`UserParamBinding`] view for each
    /// user-added binding. Routing + affine (`scale`/`offset`) come from the
    /// `user_added` [`BindingDef`]; the reshape (range / invert / curve /
    /// `is_angle`) comes from the matching `ParamSpecDef` — the single source
    /// after the per-instance reshape note was deleted. `is_angle` now has a
    /// home on the spec (seeded at expose from the inner `ParamType::Angle`),
    /// so it round-trips instead of being dead-fed `false`.
    ///
    /// Allocates a `Vec`; callers (renderer rebuild, state-sync, panels)
    /// hit this only on the boundary path (binding edit / card build), not
    /// the per-frame hot path, so the allocation is acceptable.
    pub fn user_param_bindings(&self) -> Vec<UserParamBinding> {
        self.user_added_bindings()
            .map(|b| self.synth_user_binding(b))
            .collect()
    }

    /// Build one [`UserParamBinding`] from a `user_added` [`BindingDef`]
    /// plus its matching `ParamSpecDef` reshape. Shared by
    /// [`Self::user_param_bindings`] and the single-binding lookups.
    fn synth_user_binding(&self, b: &crate::effect_graph_def::BindingDef) -> UserParamBinding {
        use crate::effect_graph_def::BindingTarget;
        let (node_id, inner_param) = match &b.target {
            BindingTarget::Node { node_id, param } => (node_id.clone(), param.clone()),
            BindingTarget::Composite { outer_name } => {
                (NodeId::default(), outer_name.clone())
            }
        };
        // The full slider surface (range + curve + invert + label) is the
        // manifest entry's live `spec` — so a recalibrated user param's range
        // reaches the renderer (PARAM_STORAGE_DESIGN.md D6). scale/offset come
        // from the binding recipe. Identity fallback when no manifest entry.
        let param = self.params.get(&b.id);
        let spec = param.map(|p| &p.spec);
        UserParamBinding {
            id: b.id.clone(),
            label: spec.map(|s| s.name.clone()).unwrap_or_else(|| b.label.clone()),
            node_id,
            legacy_node_handle: None,
            inner_param,
            min: spec.map(|s| s.min).unwrap_or(0.0),
            max: spec.map(|s| s.max).unwrap_or(1.0),
            default_value: b.default_value,
            convert: b.convert,
            is_angle: spec.map(|s| s.is_angle).unwrap_or(false),
            invert: spec.map(|s| s.invert).unwrap_or(false),
            curve: spec.map(|s| s.curve).unwrap_or_default(),
            scale: b.scale,
            offset: b.offset,
            value_labels: spec.map(|s| s.value_labels.clone()).unwrap_or_default(),
            section: spec.and_then(|s| s.section.clone()),
        }
    }

    /// Position of a user binding by stable id within the user-added tail, or
    /// `None` if not found. Index is relative to the user tail. (For the
    /// manifest entry, use [`crate::params::ParamManifest::get`] by id.)
    pub fn user_binding_index(&self, id: &str) -> Option<usize> {
        self.user_added_bindings().position(|b| b.id == id)
    }

    // `static_param_count`, `param_id_to_value_index`, and `resolve_param` are
    // DELETED (PARAM_STORAGE_DESIGN.md D3): there is no positional slot to
    // resolve an id to, and no static/user split with addressing meaning. A
    // consumer that needs a param's value/range/whole-number data reads
    // `self.params.get(id)` and takes it off the entry
    // (`.value`, `.spec.min`/`.spec.max`, `.whole_numbers()`).

    /// Append a user-exposed binding and reserve its `param_values`
    /// (and `base_param_values`, if present) slot at the tail.
    ///
    /// New slot defaults come from the binding's `default_value`.
    /// Bumps `user_param_bindings_version` so the renderer's
    /// per-FX scratch rehydrates on the next frame.
    ///
    /// Calls [`Self::align_to_definition`] first so the static
    /// prefix matches the registry's `param_count` before the new
    /// user-tail slot is pushed. Without this, an effect freshly
    /// constructed via [`Self::new`] (with empty `param_values`)
    /// would land its first user-binding value at index 0 — wrong
    /// if the registry says `n_static > 0`. Align is cheap
    /// (single Vec resize); user-binding mutations are rare (one
    /// per checkbox click), so the cost is negligible.
    ///
    /// Caller is responsible for ensuring `binding.id` is unique
    /// within this instance — `generate_user_param_id` (in
    /// `manifold-editing`) provides the canonical collision-free
    /// shape.
    pub fn append_user_binding(&mut self, binding: UserParamBinding) {
        use crate::effect_graph_def::{
            BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
        };
        let whole_numbers = matches!(
            binding.convert,
            ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger
        );
        // The param descriptor: the manifest holds the live copy (the runtime
        // authority), and `meta.params` keeps a consistent shadow so the graph
        // def stays uniform with a bundled preset JSON.
        let spec = ParamSpecDef {
            id: binding.id.clone(),
            name: binding.label.clone(),
            min: binding.min,
            max: binding.max,
            default_value: binding.default_value,
            whole_numbers,
            is_toggle: matches!(binding.convert, ParamConvert::BoolThreshold),
            is_trigger: matches!(binding.convert, ParamConvert::Trigger),
            value_labels: binding.value_labels.clone(),
            format_string: None,
            osc_suffix: String::new(),
            curve: binding.curve,
            invert: binding.invert,
            // Captured from the inner param's `ParamType::Angle` at expose time
            // (rides `UserParamBinding.is_angle`). The spec is now the single
            // home for the flag, so the card reads it straight off the manifest.
            is_angle: binding.is_angle,
            // A user-exposed inner-graph param is never the trigger-gate card
            // (that's the preset-authored `clip_trigger` outer card only).
            is_trigger_gate: false,
            wraps: false,
            // Section seeding from the innermost enclosing group's display
            // name (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D5) — resolved
            // by the expose command (`mirror_effect_side`) and carried on
            // the `UserParamBinding` this fn receives.
            section: binding.section.clone(),
        };

        // The per-instance graph is the single binding-storage list.
        // The live expose command lifts the canonical graph before this
        // runs; for graph-less callers (storage unit tests) we synthesize
        // a metadata-only graph so the binding still has a home.
        let graph = self.graph.get_or_insert_with(|| EffectGraphDef {
            version: 0,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        });
        let meta = graph.preset_metadata.get_or_insert_with(|| PresetMetadata {
            id: PresetTypeId::new(""),
            display_name: String::new(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: Vec::new(),
            bindings: Vec::new(),
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });
        meta.params.push(spec.clone());
        meta.bindings.push(BindingDef {
            id: binding.id.clone(),
            label: binding.label.clone(),
            default_value: binding.default_value,
            target: BindingTarget::Node {
                node_id: binding.node_id.clone(),
                param: binding.inner_param.clone(),
            },
            convert: binding.convert,
            user_added: true,
            scale: binding.scale,
            offset: binding.offset,
        });

        // The manifest entry (id as identity, order = card order). `push`
        // bumps topology (D8). base + value both seed from the spec default.
        self.params.push(crate::params::Param::user_added(spec));
    }

    /// Remove a user-exposed binding by id and drop its `param_values`
    /// (and `base_param_values`) slot. Returns the removed binding view.
    ///
    /// Pulls the `user_added` `BindingDef` + its `ParamSpecDef` from the
    /// per-instance graph's `preset_metadata` (the single storage list)
    /// and its reshape note. Restores undo-state via the returned
    /// `UserParamBinding` plus the value caller saved before the call
    /// (use [`Self::get_param`] / [`Self::get_base_param`] at the slot
    /// returned by [`Self::param_id_to_value_index`]).
    pub fn remove_user_binding_by_id(&mut self, id: &str) -> Option<UserParamBinding> {
        let j = self.user_binding_index(id)?;

        // Synthesize the removed view BEFORE mutating the graph (it reads
        // the binding + the manifest spec).
        let removed = {
            let b = self.user_added_bindings().nth(j)?;
            self.synth_user_binding(b)
        };

        // Pull the binding + shadow spec from the graph metadata.
        if let Some(meta) = self
            .graph
            .as_mut()
            .and_then(|g| g.preset_metadata.as_mut())
        {
            if let Some(bi) = meta.bindings.iter().position(|b| b.user_added && b.id == id) {
                meta.bindings.remove(bi);
            }
            if let Some(si) = meta.params.iter().position(|p| p.id == id) {
                meta.params.remove(si);
            }
        }

        // Drop the manifest entry (id as identity; bumps topology).
        self.params.remove(id);
        Some(removed)
    }

    /// Re-insert a previously-removed user binding at its original
    /// user-tail `position`, restoring the graph metadata (binding +
    /// spec), the reshape note (if the binding carried one), and the
    /// `param_values` (+ `base_param_values`) slot. Undo counterpart to
    /// [`Self::remove_user_binding_by_id`]; keeps the user-tail order so
    /// sibling user bindings keep their slot indices.
    pub fn restore_user_binding_at(
        &mut self,
        binding: UserParamBinding,
        position: usize,
        param: crate::params::Param,
    ) {
        use crate::effect_graph_def::{
            BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
        };

        let graph = self.graph.get_or_insert_with(|| EffectGraphDef {
            version: 0,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        });
        let meta = graph.preset_metadata.get_or_insert_with(|| PresetMetadata {
            id: PresetTypeId::new(""),
            display_name: String::new(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: Vec::new(),
            bindings: Vec::new(),
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });

        // Absolute binding index of the `position`-th user-added entry
        // (or end). Static (bundled) bindings sit before / among them in
        // declaration order; we count only the user-added ones.
        let abs_binding_idx = meta
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.user_added)
            .nth(position)
            .map(|(i, _)| i)
            .unwrap_or(meta.bindings.len());
        let whole_numbers = matches!(
            binding.convert,
            ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger
        );
        meta.bindings.insert(
            abs_binding_idx,
            BindingDef {
                id: binding.id.clone(),
                label: binding.label.clone(),
                default_value: binding.default_value,
                target: BindingTarget::Node {
                    node_id: binding.node_id.clone(),
                    param: binding.inner_param.clone(),
                },
                convert: binding.convert,
                user_added: true,
                scale: binding.scale,
                offset: binding.offset,
            },
        );
        // Spec list: append (its absolute position isn't load-bearing for
        // effects — the registry drives the static prefix; the user tail
        // is keyed by id).
        meta.params.push(ParamSpecDef {
            id: binding.id.clone(),
            name: binding.label.clone(),
            min: binding.min,
            max: binding.max,
            default_value: binding.default_value,
            whole_numbers,
            is_toggle: matches!(binding.convert, ParamConvert::BoolThreshold),
            is_trigger: matches!(binding.convert, ParamConvert::Trigger),
            value_labels: binding.value_labels.clone(),
            format_string: None,
            osc_suffix: String::new(),
            curve: binding.curve,
            invert: binding.invert,
            // Position-aware reinstate (undo of an unexpose): preserve the
            // angle flag off the captured binding, same as `append_user_binding`.
            is_angle: binding.is_angle,
            is_trigger_gate: false,
            wraps: false,
            // Same carry-through as `append_user_binding` — this shadow entry
            // is inert anyway (D12 re-derives `meta.params` from the live
            // manifest at serialize time), but keeping it consistent avoids a
            // misleading `None` sitting next to a real section value on the
            // manifest entry `self.params.insert_at` restores below.
            section: binding.section.clone(),
        });

        // Re-insert the manifest entry at its original display position among
        // the user tail: the bundled prefix (unchanged by a user-param
        // removal) plus `position`. `insert_at` clamps + bumps topology (D10).
        let bundled = self
            .params
            .iter()
            .filter(|p| matches!(p.origin, crate::params::ParamOrigin::Bundled))
            .count();
        self.params.insert_at(bundled + position, param);
    }

    // `align_to_definition` is DELETED (PARAM_STORAGE_DESIGN.md D3). It existed
    // to resize the positional `param_values` array to the registry/graph param
    // count after a load or a binding edit — there is no positional array to
    // resize now. The manifest is coherent by construction: it is seeded whole
    // at instantiation/load (`build_param_manifest`) and mutated by
    // `push`/`remove`/`insert_at`, so there is never a length to reconcile.

    /// Snapshot this instance's current base (pre-modulation) param values into
    /// `def`'s preset metadata as the new defaults, so the def becomes a frozen
    /// copy of the configured card rather than a stock template. This is what
    /// makes Make Unique / Export carry the look you set: `param_values` is the
    /// live instrument and stays on the instance, but the def now defaults to
    /// the same values, so a later add/import/load reproduces them through the
    /// normal defaults path. Matched by param id; a metadata param the instance
    /// doesn't expose keeps its existing default. No-op without metadata.
    pub fn snapshot_values_into_def(&self, def: &mut EffectGraphDef) {
        let Some(meta) = def.preset_metadata.as_mut() else {
            return;
        };
        for p in meta.params.iter_mut() {
            if self.params.get(&p.id).is_some() {
                p.default_value = self.get_base_param(&p.id);
            }
        }
        for b in meta.bindings.iter_mut() {
            if self.params.get(&b.id).is_some() {
                b.default_value = self.get_base_param(&b.id);
            }
        }
    }

    /// Replace `param_values` with fresh exposed slots seeded from `def`'s
    /// preset metadata defaults (in declaration order — the same layout the
    /// registry produces). Used when retargeting to an *imported* preset, whose
    /// param structure differs from the instance's prior one: the old positional
    /// `param_values` no longer line up with the new bindings, so reusing them
    /// feeds garbage (or zero) into the graph. Re-seeding from the def both
    /// applies the imported preset's saved values and restores correct
    /// alignment. No-op without metadata.
    pub fn reseed_param_values_from_def(&mut self, def: &EffectGraphDef) {
        if let Some(meta) = def.preset_metadata.as_ref() {
            let entries = meta
                .params
                .iter()
                .map(|p| {
                    let user = meta.bindings.iter().any(|b| b.user_added && b.id == p.id);
                    if user {
                        crate::params::Param::user_added(p.clone())
                    } else {
                        crate::params::Param::bundled(p.clone())
                    }
                })
                .collect();
            self.params = crate::params::ParamManifest::from_params(entries);
        }
    }

    /// Remove every exposed card param bound to `node_id` — its binding, param
    /// spec, value slot, and any drivers / Ableton mappings / envelopes that
    /// referenced it — and return a capture for [`Self::restore_exposures`].
    /// Empty (and a no-op) when nothing was bound to the node. Used when a graph
    /// edit removes the node a slider drove, so the slider doesn't linger as a
    /// dead control. Operates on the per-instance `graph.preset_metadata`, so
    /// the caller must have lifted the graph first (the graph editor always has).
    pub fn remove_exposures_for_node(&mut self, node_id: &NodeId) -> Vec<RemovedExposure> {
        use crate::effect_graph_def::BindingTarget;
        let Some(meta) = self.graph.as_ref().and_then(|g| g.preset_metadata.as_ref()) else {
            return Vec::new();
        };
        // Capture phase (immutable): everything we'll remove, with positions.
        let mut captured: Vec<RemovedExposure> = Vec::new();
        for (bi, b) in meta.bindings.iter().enumerate() {
            let BindingTarget::Node { node_id: nid, .. } = &b.target else {
                continue;
            };
            if nid != node_id {
                continue;
            }
            let id = b.id.as_str();
            let param_position = self.params.index_of(id);
            let param = self.params.get(id).cloned();
            let drivers = self
                .drivers
                .iter()
                .flatten()
                .filter(|d| d.param_id == id)
                .cloned()
                .collect();
            let ableton_mappings = self
                .ableton_mappings
                .iter()
                .flatten()
                .filter(|m| m.param_id == id)
                .cloned()
                .collect();
            let envelopes = self
                .envelopes
                .iter()
                .flatten()
                .filter(|e| e.param_id == id)
                .cloned()
                .collect();
            let audio_mods = self
                .audio_mods
                .iter()
                .flatten()
                .filter(|a| a.param_id == id)
                .cloned()
                .collect();
            captured.push(RemovedExposure {
                param_position,
                binding_index: bi,
                param,
                binding: b.clone(),
                drivers,
                ableton_mappings,
                envelopes,
                audio_mods,
            });
        }
        if captured.is_empty() {
            return captured;
        }
        let ids: std::collections::HashSet<&str> =
            captured.iter().map(|c| c.binding.id.as_str()).collect();
        // Remove the descriptor shadow (`meta.params`) + the bindings, and the
        // manifest entries — all keyed by id, no positional indices.
        if let Some(meta) = self.graph.as_mut().and_then(|g| g.preset_metadata.as_mut()) {
            meta.params.retain(|p| !ids.contains(p.id.as_str()));
            let mut bidx: Vec<usize> = captured.iter().map(|c| c.binding_index).collect();
            bidx.sort_unstable_by(|a, b| b.cmp(a));
            for i in bidx {
                if i < meta.bindings.len() {
                    meta.bindings.remove(i);
                }
            }
        }
        for &id in &ids {
            self.params.remove(id);
        }
        prune_automation_by_ids(&mut self.drivers, &ids, |d| &*d.param_id);
        prune_automation_by_ids(&mut self.ableton_mappings, &ids, |m| &*m.param_id);
        prune_automation_by_ids(&mut self.envelopes, &ids, |e| &*e.param_id);
        prune_automation_by_ids(&mut self.audio_mods, &ids, |a| &*a.param_id);
        captured
    }

    /// Re-insert exposures removed by [`Self::remove_exposures_for_node`] at
    /// their original positions, restoring bindings, param specs, value slots,
    /// and automation. The undo half — exact inverse of the removal.
    pub fn restore_exposures(&mut self, removed: Vec<RemovedExposure>) {
        if removed.is_empty() {
            return;
        }
        // Insert in ascending original-index order so each lands where it was.
        if let Some(meta) = self.graph.as_mut().and_then(|g| g.preset_metadata.as_mut()) {
            // Restore the descriptor shadow from each removed entry's spec.
            for r in &removed {
                if let Some(p) = &r.param
                    && !meta.params.iter().any(|s| s.id == p.spec.id)
                {
                    meta.params.push(p.spec.clone());
                }
            }
            let mut binds: Vec<(usize, crate::effect_graph_def::BindingDef)> = removed
                .iter()
                .map(|r| (r.binding_index, r.binding.clone()))
                .collect();
            binds.sort_by_key(|(i, _)| *i);
            for (i, b) in binds {
                let i = i.min(meta.bindings.len());
                meta.bindings.insert(i, b);
            }
        }
        // Re-insert manifest entries at their captured display positions,
        // ascending so each lands where it was (D10). `insert_at` clamps.
        let mut params: Vec<(usize, crate::params::Param)> = removed
            .iter()
            .filter_map(|r| Some((r.param_position?, r.param.clone()?)))
            .collect();
        params.sort_by_key(|(i, _)| *i);
        for (i, param) in params {
            self.params.insert_at(i, param);
        }
        for r in &removed {
            if !r.drivers.is_empty() {
                self.drivers
                    .get_or_insert_with(Vec::new)
                    .extend(r.drivers.iter().cloned());
            }
            if !r.ableton_mappings.is_empty() {
                self.ableton_mappings
                    .get_or_insert_with(Vec::new)
                    .extend(r.ableton_mappings.iter().cloned());
            }
            if !r.envelopes.is_empty() {
                self.envelopes
                    .get_or_insert_with(Vec::new)
                    .extend(r.envelopes.iter().cloned());
            }
            if !r.audio_mods.is_empty() {
                self.audio_mods
                    .get_or_insert_with(Vec::new)
                    .extend(r.audio_mods.iter().cloned());
            }
        }
    }

    /// Drop any driver / Ableton mapping / envelope whose `param_id` no longer
    /// resolves to a live param on this instance, returning the removed rows for
    /// undo. The integrity sweep for structural edits that wipe params without
    /// going through the precise per-param prune — e.g. a Revert that clears the
    /// graph (and with it the user-added bindings automation was hung on). Rows
    /// are keyed by id and order-independent, so the restore just re-appends.
    pub fn prune_orphaned_automation(&mut self) -> RemovedAutomation {
        let mut orphans: std::collections::HashSet<String> = std::collections::HashSet::new();
        for d in self.drivers.iter().flatten() {
            if self.params.get(&d.param_id).is_none() {
                orphans.insert(d.param_id.to_string());
            }
        }
        for m in self.ableton_mappings.iter().flatten() {
            if self.params.get(&m.param_id).is_none() {
                orphans.insert(m.param_id.to_string());
            }
        }
        for e in self.envelopes.iter().flatten() {
            if self.params.get(&e.param_id).is_none() {
                orphans.insert(e.param_id.to_string());
            }
        }
        for a in self.audio_mods.iter().flatten() {
            if self.params.get(&a.param_id).is_none() {
                orphans.insert(a.param_id.to_string());
            }
        }
        for l in self.automation_lanes.iter().flatten() {
            if self.params.get(&l.param_id).is_none() {
                orphans.insert(l.param_id.to_string());
            }
        }
        if orphans.is_empty() {
            return RemovedAutomation::default();
        }
        RemovedAutomation {
            drivers: take_automation_by_ids(&mut self.drivers, &orphans, |d| &d.param_id),
            ableton_mappings: take_automation_by_ids(&mut self.ableton_mappings, &orphans, |m| {
                &m.param_id
            }),
            envelopes: take_automation_by_ids(&mut self.envelopes, &orphans, |e| &e.param_id),
            audio_mods: take_automation_by_ids(&mut self.audio_mods, &orphans, |a| &a.param_id),
            automation_lanes: take_automation_by_ids(&mut self.automation_lanes, &orphans, |l| {
                &l.param_id
            }),
        }
    }

    /// Re-attach automation removed by [`Self::prune_orphaned_automation`]. The
    /// undo half — append-restore, since automation rows carry no positional
    /// meaning (they're keyed by `param_id`).
    pub fn restore_automation(&mut self, removed: RemovedAutomation) {
        if !removed.drivers.is_empty() {
            self.drivers.get_or_insert_with(Vec::new).extend(removed.drivers);
        }
        if !removed.ableton_mappings.is_empty() {
            self.ableton_mappings
                .get_or_insert_with(Vec::new)
                .extend(removed.ableton_mappings);
        }
        if !removed.envelopes.is_empty() {
            self.envelopes
                .get_or_insert_with(Vec::new)
                .extend(removed.envelopes);
        }
        if !removed.audio_mods.is_empty() {
            self.audio_mods
                .get_or_insert_with(Vec::new)
                .extend(removed.audio_mods);
        }
        if !removed.automation_lanes.is_empty() {
            self.automation_lanes
                .get_or_insert_with(Vec::new)
                .extend(removed.automation_lanes);
        }
    }

    /// Get the drivers list, creating it if None.
    pub fn drivers_mut(&mut self) -> &mut Vec<ParameterDriver> {
        if self.drivers.is_none() {
            self.drivers = Some(Vec::new());
        }
        self.drivers.as_mut().unwrap()
    }

    /// Get the audio-mods list, creating it if None.
    pub fn audio_mods_mut(&mut self) -> &mut Vec<crate::audio_mod::ParameterAudioMod> {
        if self.audio_mods.is_none() {
            self.audio_mods = Some(Vec::new());
        }
        self.audio_mods.as_mut().unwrap()
    }

    pub fn find_audio_mod(&self, param_id: &str) -> Option<&crate::audio_mod::ParameterAudioMod> {
        self.audio_mods
            .as_ref()
            .and_then(|v| v.iter().find(|a| a.param_id == param_id))
    }

    pub fn has_audio_mods(&self) -> bool {
        self.audio_mods.as_ref().is_some_and(|v| !v.is_empty())
    }

    /// Whether this instance's clip-launch edge should count toward its own
    /// Trigger response (§9 U3, supersedes the deleted `AudioTriggerMod::
    /// clip_edge_enabled`). Finds an ENABLED `audio_mods` entry targeting a
    /// trigger-gate param (`spec.is_trigger_gate`); none found means no audio
    /// config exists for this gate, so the clip edge is unconditionally on
    /// (the pre-§8 behavior, unchanged). A DISABLED mod is semantically
    /// absent — the same "disabled means absent" rule the old per-instance
    /// config used to own (the bug that shipped a disarmed Transient config
    /// silently killing clip triggers on reload), now expressed with zero
    /// trigger-specific storage: a fire-mode mod is just a normal audio mod.
    pub fn clip_edge_enabled(&self) -> bool {
        let Some(mods) = self.audio_mods.as_ref() else {
            return true;
        };
        let gate_mod = mods.iter().find(|m| {
            m.enabled
                && self
                    .params
                    .get(m.param_id.as_ref())
                    .is_some_and(|p| p.spec.is_trigger_gate)
        });
        match gate_mod {
            None => true,
            Some(m) => m
                .trigger_mode
                .unwrap_or(crate::audio_trigger::TriggerFireMode::Both)
                .wants_clip_edge(),
        }
    }
}

/// Drop every element of an optional automation list whose key is in `ids`,
/// collapsing the list to `None` when it empties. Shared by the four
/// per-instance automation homes (drivers / Ableton mappings / envelopes /
/// audio mods).
fn prune_automation_by_ids<T>(
    opt: &mut Option<Vec<T>>,
    ids: &std::collections::HashSet<&str>,
    key: impl Fn(&T) -> &str,
) {
    if let Some(v) = opt.as_mut() {
        v.retain(|t| !ids.contains(key(t)));
        if v.is_empty() {
            *opt = None;
        }
    }
}

/// Like [`prune_automation_by_ids`] but *captures* the removed rows (for undo)
/// instead of dropping them, and matches against an owned id set.
fn take_automation_by_ids<T>(
    opt: &mut Option<Vec<T>>,
    ids: &std::collections::HashSet<String>,
    key: impl Fn(&T) -> &str,
) -> Vec<T> {
    let mut taken = Vec::new();
    if let Some(v) = opt.as_mut() {
        let mut i = 0;
        while i < v.len() {
            if ids.contains(key(&v[i])) {
                taken.push(v.remove(i));
            } else {
                i += 1;
            }
        }
        if v.is_empty() {
            *opt = None;
        }
    }
    taken
}

/// Automation rows (drivers / Ableton mappings / envelopes / automation
/// lanes) removed because their `param_id` no longer resolved to a live
/// param. Returned by [`PresetInstance::prune_orphaned_automation`] and
/// restored by [`PresetInstance::restore_automation`] on undo.
#[derive(Debug, Clone, Default)]
pub struct RemovedAutomation {
    drivers: Vec<ParameterDriver>,
    ableton_mappings: Vec<crate::ableton_mapping::AbletonParamMapping>,
    envelopes: Vec<ParamEnvelope>,
    audio_mods: Vec<crate::audio_mod::ParameterAudioMod>,
    automation_lanes: Vec<AutomationLane>,
}

/// Implement ParamSource for PresetInstance.
/// Port of Unity PresetInstance : IParamSource.
/// Generator-kind methods, ported from the former `PresetInstance`. They
/// read the generator registry via `self.effect_type` (which holds the preset
/// type for both kinds). Only ever called on generator-kind instances.
impl PresetInstance {
    // `migrate_to_registry_length` is DELETED (PARAM_STORAGE_DESIGN.md D3):
    // there is no lazy positional tail to pad. A generator's manifest is seeded
    // whole from the template at instantiation (`init_defaults_for_type`) and at
    // load (`build_param_manifest`).

    /// Generator-only home.
    pub fn find_envelope(&self, param_id: &str) -> Option<&ParamEnvelope> {
        self.envelopes.as_ref()?.iter().find(|e| e.param_id == param_id)
    }

    /// No-alloc check.
    pub fn has_envelopes(&self) -> bool {
        self.envelopes.as_ref().is_some_and(|e| !e.is_empty())
    }

    /// The instance's envelope list, auto-created on first access.
    /// Envelope-home unification: effect and generator envelopes both live
    /// here, keyed by `param_id` (no `target_effect_type` — the instance the
    /// envelope sits on is the target).
    pub fn envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope> {
        if self.envelopes.is_none() {
            self.envelopes = Some(Vec::new());
        }
        self.envelopes.as_mut().unwrap()
    }

    /// The instance's automation-lane list, auto-created on first access.
    /// Sibling of [`Self::drivers_mut`] / [`Self::envelopes_mut`] /
    /// [`Self::audio_mods_mut`] — same per-param-id-row pattern. See
    /// `docs/AUTOMATION_LANES_DESIGN.md` §2.
    pub fn automation_lanes_mut(&mut self) -> &mut Vec<AutomationLane> {
        if self.automation_lanes.is_none() {
            self.automation_lanes = Some(Vec::new());
        }
        self.automation_lanes.as_mut().unwrap()
    }

    /// No-alloc check, mirroring [`Self::has_envelopes`] / [`Self::has_audio_mods`].
    pub fn has_automation_lanes(&self) -> bool {
        self.automation_lanes.as_ref().is_some_and(|v| !v.is_empty())
    }

    /// Reset effective values to base — ONLY for params with active drivers or
    /// envelopes (generator semantics).
    pub fn reset_effectives(&mut self) {
        if self.params.is_empty() {
            return;
        }
        self.ensure_base_values();
        // Collect the ids of params with an active driver or envelope first
        // (disjoint from the `self.params` mutation below).
        let mut ids: Vec<String> = Vec::new();
        for d in self.drivers.iter().flatten() {
            if d.enabled {
                ids.push(d.param_id.to_string());
            }
        }
        for e in self.envelopes.iter().flatten() {
            if e.enabled {
                ids.push(e.param_id.to_string());
            }
        }
        for id in ids {
            if let Some(p) = self.params.get_mut(&id) {
                p.value = p.base;
            }
        }
    }

    /// Change generator type, re-initializing to the new type's defaults and
    /// clearing drivers/envelopes.
    pub fn change_type(&mut self, new_type: PresetTypeId) {
        if new_type == PresetTypeId::NONE {
            return;
        }
        self.effect_type = new_type.clone();
        self.init_defaults_for_type(new_type);
        if let Some(drivers) = &mut self.drivers {
            drivers.clear();
        }
        if let Some(envelopes) = &mut self.envelopes {
            envelopes.clear();
        }
    }

    /// Initialize base + effective arrays from the generator registry defaults,
    /// setting the type.
    pub fn init_defaults_for_type(&mut self, gen_type: PresetTypeId) {
        if let Some(def) = crate::preset_definition_registry::try_get(&gen_type) {
            self.effect_type = gen_type;
            // Seed the manifest whole from the registry template; each bundled
            // Param seeds base = value = default.
            let entries = def
                .param_defs
                .iter()
                .map(|pd| crate::params::Param::bundled(pd.spec.clone()))
                .collect();
            self.params = crate::params::ParamManifest::from_params(entries);
            self.base_tracked = true;
        }
    }

    /// Legacy init_defaults (no parameter). Uses the current generator type.
    pub fn init_defaults(&mut self) {
        let gt = self.effect_type.clone();
        self.init_defaults_for_type(gt);
    }

    /// Snapshot current base param values (for undo). When base is tracked,
    /// snapshots each slot's `base`; otherwise the effective value (the former
    /// `base_param_values: None` fall-through).
    pub fn snapshot_params(&self) -> Vec<f32> {
        if self.base_tracked {
            self.params.iter().map(|p| p.base).collect()
        } else {
            self.params.iter().map(|p| p.value).collect()
        }
    }

    /// Snapshot current drivers (for undo).
    pub fn snapshot_drivers(&self) -> Option<Vec<ParameterDriver>> {
        self.drivers
            .as_ref()
            .and_then(|d| if d.is_empty() { None } else { Some(d.clone()) })
    }

    /// Snapshot current generator envelopes (for undo).
    pub fn snapshot_envelopes(&self) -> Option<Vec<ParamEnvelope>> {
        self.envelopes
            .as_ref()
            .and_then(|e| if e.is_empty() { None } else { Some(e.clone()) })
    }

    /// Restore from a snapshot (used by undo).
    pub fn restore(
        &mut self,
        gen_type: PresetTypeId,
        params: Vec<f32>,
        drivers: Option<Vec<ParameterDriver>>,
        envelopes: Option<Vec<ParamEnvelope>>,
    ) {
        // Re-seed the manifest descriptors from the registry template, then
        // honor the snapshot's arity and overlay the snapshotted base values in
        // manifest (card) order. The undo snapshot is the authoritative param
        // set — a curated instance can carry fewer params than the current
        // registry template — so trim the reseeded template to the snapshot
        // length; without this, restoring a shorter snapshot leaves the
        // registry-default tail entries appended past the snapshot
        // (PARAM_STORAGE_DESIGN.md D-storage).
        self.init_defaults_for_type(gen_type);
        self.params.truncate(params.len());
        for (p, v) in self.params.iter_mut().zip(params.iter()) {
            p.base = *v;
            p.value = *v;
        }
        self.base_tracked = true;
        if let Some(d) = &mut self.drivers {
            d.clear();
        }
        if let Some(snapshot_drivers) = drivers {
            self.drivers_mut().extend(snapshot_drivers);
        }
        if let Some(e) = &mut self.envelopes {
            e.clear();
        }
        if let Some(snapshot_envelopes) = envelopes {
            self.envelopes_mut().extend(snapshot_envelopes);
        }
    }
}

impl ParamSource for PresetInstance {
    fn display_name(&self) -> &str {
        // The registry hands back an owned `Arc<PresetDef>` (hot-reloadable
        // since step 10), so the name is interned to `&'static str` to
        // satisfy the trait's borrowed return without rippling `String`
        // through every `ParamSource` caller. See
        // `preset_definition_registry::intern_display_name`.
        match crate::preset_definition_registry::try_get(&self.effect_type) {
            Some(def) => crate::preset_definition_registry::intern_display_name(&def.display_name),
            // Unknown type: kind-appropriate fallback label.
            None if self.is_generator() => "Generator",
            None => "?",
        }
    }

    fn param_count(&self) -> usize {
        self.params.len()
    }

    fn get_param_def(&self, id: &str) -> crate::effect_graph_def::ParamSpecDef {
        // The manifest entry's `spec` is the descriptor for every param
        // (bundled + user-added, calibrated in place). Unknown id → default.
        self.params
            .get(id)
            .map(|p| p.spec.clone())
            .unwrap_or_default()
    }

    fn get_param(&self, id: &str) -> f32 {
        PresetInstance::get_param(self, id)
    }

    fn set_param(&mut self, id: &str, value: f32) {
        PresetInstance::set_param(self, id, value);
    }

    fn get_base_param(&self, id: &str) -> f32 {
        PresetInstance::get_base_param(self, id)
    }

    fn set_base_param(&mut self, id: &str, value: f32) {
        PresetInstance::set_base_param(self, id, value);
    }

    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        PresetInstance::find_driver(self, param_id)
    }

    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        PresetInstance::get_drivers_list(self)
    }

    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver {
        PresetInstance::create_driver(self, param_id)
    }

    fn remove_driver(&mut self, param_id: &str) {
        PresetInstance::remove_driver(self, param_id);
    }
}

// ─── Effect Group ───

/// A rack group containing multiple effects with shared bypass and wet/dry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGroup {
    pub id: EffectGroupId,
    #[serde(default = "default_group_name")]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default = "default_one")]
    pub wet_dry: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_group_id: Option<EffectGroupId>,
}

impl EffectGroup {
    pub fn new(name: String) -> Self {
        Self {
            id: EffectGroupId::new(crate::short_id()),
            name,
            enabled: true,
            collapsed: false,
            wet_dry: 1.0,
            parent_group_id: None,
        }
    }

    pub fn clone_with_new_id(&self) -> Self {
        let mut cloned = self.clone();
        cloned.id = EffectGroupId::new(crate::short_id());
        cloned
    }
}

// ─── Parameter Driver (LFO) ───

/// LFO modulating a single effect or generator parameter.
///
/// Address shape: `param_id` is the canonical mapping key referenced by
/// project file storage and (by extension) any external client that
/// reads/writes saved JSON. Legacy V1 projects stored `paramIndex: i32`
/// instead — the custom [`Deserialize`] accepts either shape, parking
/// the legacy index in [`ParameterDriver::legacy_param_index`] for the
/// post-load resolver to translate via the registry.
///
/// Serialization (custom impl below): emits `paramId` when non-empty.
/// When `param_id` is empty AND `legacy_param_index` is `Some`, emits
/// `paramIndex` instead — this preserves recovery information across
/// save→load cycles when the load happened on a build whose registry
/// didn't have the effect type. See [`ParameterDriver::legacy_param_index`].
#[derive(Debug, Clone)]
pub struct ParameterDriver {
    /// Stable mapping key. After post-load resolution, every driver in
    /// memory has a non-empty `param_id`. During the brief window
    /// between `Deserialize` and the post-load pass, a legacy V1
    /// driver may have `param_id = ""` and `legacy_param_index = Some`.
    pub param_id: ParamId,
    pub beat_division: BeatDivision,
    pub waveform: DriverWaveform,
    pub enabled: bool,
    pub phase: f32,
    pub base_value: f32,
    pub trim_min: f32,
    pub trim_max: f32,
    pub reversed: bool,
    /// Free-running LFO period in beats. `None` => **sync mode** (period derives
    /// from [`beat_division`], including its dotted/triplet variants — the grid
    /// and feel segment). `Some(p)` => **free mode**: the LFO cycles every `p`
    /// beats regardless of the grid, enabling odd periods (3, 1.5, 0.375…) and
    /// polyrhythm against the bar. The type-in field writes this; clicking a grid
    /// cell or the feel segment clears it back to `None`. Serialized as
    /// `freePeriodBeats`, omitted when `None` so pre-free-mode projects round-trip
    /// unchanged.
    pub free_period_beats: Option<f32>,
    /// Parked legacy `paramIndex: i32` from V1.1 deserialization or from
    /// a load against an unregistered effect type.
    ///
    /// Set by:
    /// - Custom [`Deserialize`] when a legacy `paramIndex` field is
    ///   present and `paramId` is missing/empty.
    /// - Preserved unchanged by [`crate::project::Project::resolve_legacy_param_ids`]
    ///   when the effect type's registry def is missing
    ///   (`ResolveOutcome::RegistryMissing`).
    ///
    /// Cleared by the resolver in every other case (`Resolved` /
    /// `NoChange` / `Drop`).
    ///
    /// Re-emitted on serialize as `paramIndex` only when `param_id`
    /// is empty, completing the round-trip recovery loop: load V1.1
    /// on a build without the registry → save → reload on a build
    /// with the registry → resolver fills in `param_id` cleanly.
    ///
    /// **Invariant:** non-resolver code MUST NOT set this. Outside the
    /// `Deserialize → on_after_deserialize` window, an in-memory
    /// driver with `legacy_param_index = Some(_)` AND a non-empty
    /// `param_id` is a bug.
    pub legacy_param_index: Option<i32>,
    /// Runtime state, not serialized. Unity ParameterDriver.cs line 59.
    pub is_paused_by_user: bool,
}

// Custom Serialize: keeps the derive(Serialize) field shape but
// expresses the "emit `paramId` OR `paramIndex` (never both)" policy
// that derive can't express on its own.
impl Serialize for ParameterDriver {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let emit_param_id = !self.param_id.is_empty();
        let emit_legacy_index = !emit_param_id && self.legacy_param_index.is_some();
        let emit_free_period = self.free_period_beats.is_some();

        // 8 base fields (beat_division, waveform, enabled, phase,
        // base_value, trim_min, trim_max, reversed) + addressing field
        // + optional freePeriodBeats (only in free mode).
        let mut field_count = 8;
        if emit_param_id || emit_legacy_index {
            field_count += 1;
        }
        if emit_free_period {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("ParameterDriver", field_count)?;
        if emit_param_id {
            s.serialize_field("paramId", &self.param_id)?;
        } else if emit_legacy_index {
            // SAFETY: emit_legacy_index implies legacy_param_index.is_some().
            s.serialize_field("paramIndex", &self.legacy_param_index.unwrap())?;
        }
        s.serialize_field("beatDivision", &self.beat_division)?;
        s.serialize_field("waveform", &self.waveform)?;
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("phase", &self.phase)?;
        s.serialize_field("baseValue", &self.base_value)?;
        s.serialize_field("trimMin", &self.trim_min)?;
        s.serialize_field("trimMax", &self.trim_max)?;
        s.serialize_field("reversed", &self.reversed)?;
        if let Some(p) = self.free_period_beats {
            s.serialize_field("freePeriodBeats", &p)?;
        }
        s.end()
    }
}

impl ParameterDriver {
    pub fn new(
        param_id: impl Into<ParamId>,
        division: BeatDivision,
        waveform: DriverWaveform,
    ) -> Self {
        Self {
            param_id: param_id.into(),
            beat_division: division,
            waveform,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            free_period_beats: None,
            legacy_param_index: None,
            is_paused_by_user: false,
        }
    }

    /// Effective LFO period in beats. Free mode (`free_period_beats = Some(p)`)
    /// uses `p` directly; sync mode falls back to the `beat_division` period
    /// (which already encodes dotted/triplet via its variants).
    pub fn period_beats(&self) -> f32 {
        self.free_period_beats
            .unwrap_or_else(|| self.beat_division.beats())
    }

    /// Evaluate driver at given beat position -> [0, 1].
    /// Port of Unity DriverEvaluator.Evaluate. Sync-mode convenience: resolves
    /// the division to a period and defers to [`evaluate_with_period`].
    pub fn evaluate(
        current_beat: Beats,
        division: BeatDivision,
        waveform: DriverWaveform,
        phase_offset: f32,
    ) -> f32 {
        Self::evaluate_with_period(current_beat, division.beats(), waveform, phase_offset)
    }

    /// Evaluate the waveform at `current_beat` for an explicit `period` in beats
    /// -> [0, 1]. The shared core for both sync mode (period from the division)
    /// and free mode (period typed directly).
    pub fn evaluate_with_period(
        current_beat: Beats,
        period: f32,
        waveform: DriverWaveform,
        phase_offset: f32,
    ) -> f32 {
        if period <= 0.0 {
            return 0.5;
        }
        let beat = current_beat.as_f32();
        let p = (beat % period) / period + phase_offset;
        let phase = p - p.floor(); // wrap to [0, 1)

        match waveform {
            DriverWaveform::Sine => (phase * std::f32::consts::TAU).sin() * 0.5 + 0.5,
            DriverWaveform::Triangle => {
                if phase < 0.5 {
                    phase * 2.0
                } else {
                    2.0 - phase * 2.0
                }
            }
            DriverWaveform::Sawtooth => phase,
            DriverWaveform::Square => {
                if phase < 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            DriverWaveform::Random => {
                // Deterministic per-period hash matching Unity's HashToFloat.
                // Unity ParameterDriver.cs lines 224-236.
                let cycle = (beat / period).floor() as i32;
                hash_to_float(cycle as u32)
            }
        }
    }
}

/// Deterministic integer hash → the masked pre-normalize bits (0..=0x7FFFFF).
/// Unity's `HashToFloat` port (`ParameterDriver.cs` lines 224-236) — the
/// house random for anything that needs frame/seed-driven determinism
/// without RNG state: the same `seed` always yields the same output, so a
/// replay (e.g. offline export re-running the same fire sequence) reproduces
/// identically. Exposed separately from [`hash_to_float`] for callers that
/// want an exact integer modulo (PARAM_STEP_ACTIONS D7's discrete non-repeat
/// selection) rather than a float, avoiding float-rounding at the boundary.
pub fn hash_u32(seed: u32) -> u32 {
    let mut h = seed;
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h & 0x7FFFFF
}

/// [`hash_u32`] normalized to `[0, 1)`.
pub fn hash_to_float(seed: u32) -> f32 {
    hash_u32(seed) as f32 / 0x7FFFFF as f32
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    #[test]
    fn hash_to_float_is_a_pure_function_of_seed() {
        // Same seed → same output, always (determinism is the whole point:
        // PARAM_STEP_ACTIONS D7 leans on this for offline-export reproducibility).
        for seed in [0u32, 1, 7, 12345, u32::MAX] {
            assert_eq!(hash_to_float(seed), hash_to_float(seed));
            assert_eq!(hash_u32(seed), hash_u32(seed));
        }
    }

    #[test]
    fn hash_to_float_stays_in_unit_range() {
        for seed in 0..2000u32 {
            let f = hash_to_float(seed);
            assert!((0.0..1.0).contains(&f), "seed {seed} produced out-of-range {f}");
        }
    }

    #[test]
    fn driver_random_waveform_still_uses_the_shared_hash() {
        // Pins the extraction didn't change DriverWaveform::Random's output —
        // same cycle index, same value as calling hash_to_float directly.
        let period = 4.0;
        let cycle = 3i32;
        let beat = Beats((cycle as f32 * period) as f64);
        let v = ParameterDriver::evaluate_with_period(beat, period, DriverWaveform::Random, 0.0);
        assert_eq!(v, hash_to_float(cycle as u32));
    }
}

// Custom `Deserialize` accepting both V1.1 (`paramIndex: i32`) and V1.2+
// (`paramId: "amount"`) project file shapes. The runtime always reads
// `param_id`; legacy projects park the index in `legacy_param_index`
// for the post-load resolver to translate. See
// `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 8.
impl<'de> Deserialize<'de> for ParameterDriver {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Mirror struct with both shapes accepted. `param_id` and
        // `param_index` are both optional — the driver must carry one
        // or the other. If both are present, `param_id` wins (forward
        // migration takes precedence over legacy index).
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default)]
            param_id: Option<String>,
            #[serde(default)]
            param_index: Option<i32>,
            #[serde(default)]
            beat_division: BeatDivision,
            #[serde(default)]
            waveform: DriverWaveform,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            phase: f32,
            #[serde(default)]
            base_value: f32,
            #[serde(default)]
            trim_min: f32,
            #[serde(default = "default_one")]
            trim_max: f32,
            #[serde(default)]
            reversed: bool,
            #[serde(default)]
            free_period_beats: Option<f32>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let (param_id, legacy_param_index) = match (raw.param_id, raw.param_index) {
            // Canonical V1.2+ shape — param_id present and non-empty.
            (Some(id), _) if !id.is_empty() => (Cow::Owned(id), None),
            // Legacy V1.1 shape — only paramIndex present. Park for
            // post-load resolution.
            (_, Some(idx)) => (Cow::Borrowed(""), Some(idx)),
            // Round-tripped shape from a project saved before the
            // post-load resolver could fill in `param_id` (e.g. test
            // harness without effect registry, or a future case where
            // the effect type was unregistered at save time). Treat
            // as "unresolvable" rather than erroring — driver stays
            // present but inert until the registry has the metadata
            // again. Better than refusing to load the project at all.
            (_, None) => (Cow::Borrowed(""), None),
        };
        Ok(ParameterDriver {
            param_id,
            beat_division: raw.beat_division,
            waveform: raw.waveform,
            enabled: raw.enabled,
            phase: raw.phase,
            base_value: raw.base_value,
            trim_min: raw.trim_min,
            trim_max: raw.trim_max,
            reversed: raw.reversed,
            free_period_beats: raw.free_period_beats,
            legacy_param_index,
            is_paused_by_user: false,
        })
    }
}

// ─── BeatDivision helpers ───

/// Constants matching Unity BeatDivisionHelper.
pub mod beat_division_helper {
    use crate::types::BeatDivision;

    pub const STRAIGHT_COUNT: usize = 11;
    pub const DOTTED_COUNT: usize = 5;
    pub const TRIPLET_COUNT: usize = 4;
    pub const TOTAL_COUNT: usize = 20;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BeatModifier {
        None,
        Dotted,
        Triplet,
    }

    /// Display label for a beat division. Unity BeatDivisionHelper.ToLabel.
    pub fn to_label(div: BeatDivision) -> &'static str {
        match div {
            BeatDivision::ThirtySecond => "1/32",
            BeatDivision::Sixteenth => "1/16",
            BeatDivision::Eighth => "1/8",
            BeatDivision::Quarter => "1/4",
            BeatDivision::Half => "1/2",
            BeatDivision::Whole => "1/1",
            BeatDivision::TwoWhole => "2/1",
            BeatDivision::FourWhole => "4/1",
            BeatDivision::EightWhole => "8/1",
            BeatDivision::SixteenWhole => "16/1",
            BeatDivision::ThirtyTwoWhole => "32/1",
            BeatDivision::EighthDotted => "1/8.",
            BeatDivision::QuarterDotted => "1/4.",
            BeatDivision::HalfDotted => "1/2.",
            BeatDivision::WholeDotted => "1/1.",
            BeatDivision::TwoWholeDotted => "2/1.",
            BeatDivision::EighthTriplet => "1/8T",
            BeatDivision::QuarterTriplet => "1/4T",
            BeatDivision::HalfTriplet => "1/2T",
            BeatDivision::WholeTriplet => "1/1T",
        }
    }

    /// Decompose a BeatDivision into its straight base index (0-10) and modifier.
    /// Unity BeatDivisionHelper.Decompose lines 158-164.
    pub fn decompose(div: BeatDivision) -> (usize, BeatModifier) {
        let val = div as i32;
        if val >= 16 {
            ((val - 14) as usize, BeatModifier::Triplet)
        } else if val >= 11 {
            ((val - 9) as usize, BeatModifier::Dotted)
        } else {
            (val as usize, BeatModifier::None)
        }
    }

    /// Compose a straight base index + modifier into a BeatDivision.
    /// Returns None if the combination is invalid.
    /// Unity BeatDivisionHelper.TryCompose lines 170-184.
    pub fn try_compose(base_index: usize, modifier: BeatModifier) -> Option<BeatDivision> {
        match modifier {
            BeatModifier::Dotted if (2..=6).contains(&base_index) => {
                BeatDivision::from_i32((base_index + 9) as i32)
            }
            BeatModifier::Triplet if (2..=5).contains(&base_index) => {
                BeatDivision::from_i32((base_index + 14) as i32)
            }
            BeatModifier::None => BeatDivision::from_i32(base_index as i32),
            _ => None,
        }
    }
}

// ─── Param Envelope (triggered decay modulation) ───

/// Default decay time (beats) for a freshly-created envelope, so it modulates
/// usefully the moment it's armed. Tempo-synced because it's in beats.
pub const DEFAULT_ENVELOPE_DECAY_BEATS: f32 = 1.0;

/// Clip-triggered decay envelope modulating a single effect or generator
/// parameter.
///
/// Address shape: `param_id` is the canonical mapping key, mirroring
/// [`ParameterDriver`]. Legacy V1.1 projects stored `targetParamIndex:
/// i32` instead — the custom [`Deserialize`] accepts either shape and
/// parks legacy indices in [`ParamEnvelope::legacy_param_index`] for
/// the post-load resolver.
///
/// Serialization (custom impl below): emits `paramId` when non-empty,
/// else `targetParamIndex` when `legacy_param_index` is `Some`. Mirrors
/// the ParameterDriver round-trip recovery contract.
#[derive(Debug, Clone)]
pub struct ParamEnvelope {
    /// Stable mapping key. Empty after legacy V1.1 deserialization
    /// until the post-load resolver fills it in from the registry.
    ///
    /// Envelope-home unification (v1.6): an envelope lives **on its
    /// owning `PresetInstance`** (effect or generator), so it no longer
    /// carries a `target_effect_type` — the instance it sits on *is* the
    /// target. Pre-v1.6 projects stored effect envelopes on
    /// `Layer.envelopes` / `Clip.envelopes` keyed by `targetEffectType`;
    /// the v1.5→v1.6 load migration distributes each into the matching
    /// effect instance and drops the now-redundant key.
    pub param_id: ParamId,
    pub enabled: bool,
    /// The envelope's target (the orange handle on the slider track): the
    /// normalized 0-1 position the parameter is pulled toward on a clip's rising
    /// edge.
    pub target_normalized: f32,
    /// Decay time in beats — how long the value takes to fall back to its base
    /// after a trigger. The single ADSR stage kept (attack/sustain/release were
    /// dropped as not useful); editable per envelope via the card's one slider.
    pub decay_beats: f32,
    /// Parked legacy `targetParamIndex: i32` from V1.1 deserialization
    /// or RegistryMissing fallback during post-load resolution. See
    /// [`ParameterDriver::legacy_param_index`] for the recovery
    /// invariant — same contract here.
    pub legacy_param_index: Option<i32>,
    /// Cached decay output (0-1) for UI display. Not serialized.
    pub current_level: f32,
    /// Rising edge detection: was a clip active on the previous frame?
    pub was_clip_active: bool,
}

impl Serialize for ParamEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let emit_param_id = !self.param_id.is_empty();
        let emit_legacy_index = !emit_param_id && self.legacy_param_index.is_some();

        // 3 base fields (enabled, targetNormalized, decayBeats) + addressing
        // field (paramId XOR targetParamIndex).
        let mut field_count = 3;
        if emit_param_id || emit_legacy_index {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("ParamEnvelope", field_count)?;
        if emit_param_id {
            s.serialize_field("paramId", &self.param_id)?;
        } else if emit_legacy_index {
            s.serialize_field("targetParamIndex", &self.legacy_param_index.unwrap())?;
        }
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("targetNormalized", &self.target_normalized)?;
        s.serialize_field("decayBeats", &self.decay_beats)?;
        s.end()
    }
}

impl ParamEnvelope {
    /// Construct an envelope targeting `param_id` on the instance it will be
    /// attached to. Since envelope-home unification an envelope no longer
    /// distinguishes effect from generator — the `PresetInstance` it lives on
    /// is the target — so this is the single constructor for both kinds.
    pub fn new(param_id: impl Into<ParamId>) -> Self {
        Self {
            param_id: param_id.into(),
            enabled: true,
            target_normalized: 1.0,
            decay_beats: DEFAULT_ENVELOPE_DECAY_BEATS,
            legacy_param_index: None,
            current_level: 0.0,
            was_clip_active: false,
        }
    }

    /// Triggered decay level [0, 1] at `local_beat` into the active clip: 1.0 at
    /// the rising edge, falling linearly to 0 over `decay_beats`, then held at 0.
    /// The single envelope shape after the ADSR/Random simplification — depth is
    /// the per-envelope `target_normalized` (the orange target handle).
    pub fn decay_level(local_beat: Beats, decay_beats: f32) -> f32 {
        if local_beat < Beats::ZERO || decay_beats <= 0.0 {
            return 0.0;
        }
        (1.0 - local_beat.as_f32() / decay_beats).clamp(0.0, 1.0)
    }
}

// Custom `Deserialize` accepting both V1.1 (`targetParamIndex: i32`)
// and V1.2+ (`paramId: "amount"`) project file shapes. Mirrors the
// `ParameterDriver` impl above. See
// `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 9.
impl<'de> Deserialize<'de> for ParamEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            // `targetEffectType` from pre-v1.6 files is intentionally not read
            // here — the v1.5→v1.6 migration consumes it to place the envelope
            // on the right instance, and serde ignores the leftover key.
            //
            // The dropped ADSR/Random keys (`attackBeats`, `sustainLevel`,
            // `releaseBeats`, `mode`, `randomJump`, `rangeMin`, `rangeMax`) are
            // not read — serde ignores them, so an old ADSR or Random envelope
            // loads as a plain decay envelope keeping its depth + decay time.
            #[serde(default)]
            param_id: Option<String>,
            #[serde(default, rename = "targetParamIndex")]
            param_index: Option<i32>,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default = "default_one")]
            target_normalized: f32,
            #[serde(default = "default_decay_beats")]
            decay_beats: f32,
        }

        let raw = Raw::deserialize(deserializer)?;
        let (param_id, legacy_param_index) = match (raw.param_id, raw.param_index) {
            (Some(id), _) if !id.is_empty() => (Cow::Owned(id), None),
            (_, Some(idx)) => (Cow::Borrowed(""), Some(idx)),
            (_, None) => (Cow::Borrowed(""), None),
        };
        Ok(ParamEnvelope {
            param_id,
            enabled: raw.enabled,
            target_normalized: raw.target_normalized,
            decay_beats: raw.decay_beats,
            legacy_param_index,
            current_level: 0.0,
            was_clip_active: false,
        })
    }
}

// ─── Automation lanes ───
//
// Timeline arrangement automation — a tier-1 "hand" sampled from the
// arrangement each tick (`manifold-playback::automation`), riding on top of
// the same base/value slot every other hand writes through. See
// `docs/AUTOMATION_LANES_DESIGN.md`.

/// Per-param timeline automation, keyed by `param_id` — the exact pattern of
/// the sibling per-param automation rows (`drivers` / `envelopes` /
/// `audio_mods` / `ableton_mappings`) that already live on `PresetInstance`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationLane {
    pub param_id: ParamId,
    /// Lane on/off (Ableton: a deactivated automation lane). A disabled lane
    /// neither samples nor participates in touch/latch bookkeeping.
    pub enabled: bool,
    /// Sorted ascending by `beat` — the write-time invariant P2's editing
    /// commands enforce (mirrors `TempoMap::ensure_sorted`). [`Self::value_at`]
    /// assumes this and does not re-sort.
    pub points: Vec<AutomationPoint>,
}

/// One breakpoint on an [`AutomationLane`]. `value` is stored in param-range
/// units (not normalized) — a lane's points are only ever resolved against
/// [`resolve_param_in`]'s min/max for clamping, at write time (P2) and again
/// at sample time (defensive against a range narrowed after the point was
/// authored).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPoint {
    /// Arrangement beat, absolute (not clip-relative). Automation lanes are
    /// beat-indexed, so they stretch with tempo automatically.
    pub beat: Beats,
    pub value: f32,
    /// Shape of the segment LEAVING this point, toward the next one.
    pub shape: SegmentShape,
}

/// The interpolation shape of one automation segment.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum SegmentShape {
    Linear,
    /// Step — holds the earlier point's value for the whole segment.
    /// Required for enum/int-backed params (the sampler doesn't round; the
    /// existing param write path handles that exactly as slider writes do —
    /// authoring with `Hold` is what keeps an enum param from reading a
    /// nonsense mid-interpolation value).
    Hold,
    /// Power-curve bend, Ableton-style segment drag. `-1..1`: negative bends
    /// concave (slow start), positive bends convex (fast start), `0` is
    /// linear. Values outside `-1..1` are clamped at evaluation time.
    Curved(f32),
}

impl AutomationLane {
    /// Sample the curve at `beat`, in param-range units. Pure,
    /// allocation-free: binary-search the segment containing `beat`.
    ///
    /// - Empty lane → `0.0` (never sampled in practice — the evaluator skips
    ///   empty lanes before calling this).
    /// - Before the first point → the first point's value (Ableton
    ///   behavior: no backward extrapolation).
    /// - After the last point → the last point's value.
    /// - Between two points → the earlier point's [`SegmentShape`] decides:
    ///   `Linear` interpolates, `Hold` steps, `Curved(bend)` applies the
    ///   power-curve bend to the interpolation parameter before lerping.
    pub fn value_at(&self, beat: Beats) -> f32 {
        match self.points.as_slice() {
            [] => 0.0,
            [only] => only.value,
            points => {
                let first = &points[0];
                if beat.0 <= first.beat.0 {
                    return first.value;
                }
                let last = &points[points.len() - 1];
                if beat.0 >= last.beat.0 {
                    return last.value;
                }
                // `partial_cmp` is safe here: both operands come from
                // `Beats(f64)` values that reached the arrangement (never
                // NaN in practice), and a NaN comparison degrading to
                // `Equal` only widens the binary search, never panics.
                let idx = match points
                    .binary_search_by(|p| p.beat.0.partial_cmp(&beat.0).unwrap_or(std::cmp::Ordering::Equal))
                {
                    Ok(i) => i,
                    // `i > 0` is guaranteed: the `beat <= first.beat` check
                    // above already returned for any beat at or before index 0.
                    Err(i) => i - 1,
                };
                let a = &points[idx];
                let b = &points[idx + 1];
                let span = (b.beat.0 - a.beat.0) as f32;
                if span <= 0.0 {
                    return a.value;
                }
                let t = ((beat.0 - a.beat.0) as f32 / span).clamp(0.0, 1.0);
                match a.shape {
                    SegmentShape::Hold => a.value,
                    SegmentShape::Linear => a.value + (b.value - a.value) * t,
                    SegmentShape::Curved(bend) => {
                        let shaped = segment_bend(t, bend);
                        a.value + (b.value - a.value) * shaped
                    }
                }
            }
        }
    }
}

/// Power-curve bend for a `Curved` segment's interpolation parameter `t`
/// (already `[0, 1]`). `bend` in `-1..1`: `0` is identity (linear); positive
/// bends convex (`t^exponent`, exponent > 1, slow start / fast finish);
/// negative bends concave (exponent < 1, fast start / slow finish) — the
/// standard symmetric power-curve shape, Ableton's segment-drag feel.
/// Endpoints are exact regardless of bend: `f(0) = 0`, `f(1) = 1`.
fn segment_bend(t: f32, bend: f32) -> f32 {
    let bend = bend.clamp(-1.0, 1.0);
    if bend == 0.0 {
        return t;
    }
    let exponent = if bend > 0.0 {
        1.0 + bend * 3.0 // 1..4
    } else {
        1.0 / (1.0 - bend * 3.0) // 1..0.25
    };
    t.powf(exponent)
}

// ─── Default helpers ───

fn default_true() -> bool {
    true
}
fn default_one() -> f32 {
    1.0
}
fn default_decay_beats() -> f32 {
    DEFAULT_ENVELOPE_DECAY_BEATS
}
fn generate_effect_id() -> EffectId {
    EffectId::new(crate::math::short_id())
}
fn default_group_name() -> String {
    "Group".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_reshape_identity_and_stages() {
        use crate::macro_bank::MacroCurve;
        // Identity: passes through untouched.
        assert!((apply_card_reshape(2.5, 0.0, 10.0, false, MacroCurve::Linear, 1.0, 0.0) - 2.5).abs() < 1e-4);
        // Invert: 25% of the range becomes 75%.
        assert!((apply_card_reshape(2.5, 0.0, 10.0, true, MacroCurve::Linear, 1.0, 0.0) - 7.5).abs() < 1e-4);
        // SCurve (Hermite 3t^2-2t^3): n=0.25 -> 0.15625 -> *10 = 1.5625.
        assert!((apply_card_reshape(2.5, 0.0, 10.0, false, MacroCurve::SCurve, 1.0, 0.0) - 1.5625).abs() < 1e-3);
        // Degenerate range: passthrough, no divide-by-zero.
        assert!((apply_card_reshape(42.0, 5.0, 5.0, false, MacroCurve::Exponential, 1.0, 0.0) - 42.0).abs() < 1e-6);
        // Folded affine (deg->rad): no invert/curve, so scale/offset apply to the
        // RAW value, unclamped — a past-max 400° must NOT pin to the slider max.
        let k = std::f32::consts::PI / 180.0;
        assert!((apply_card_reshape(85.0, 0.0, 360.0, false, MacroCurve::Linear, k, 0.0) - 85.0 * k).abs() < 1e-5);
        assert!((apply_card_reshape(400.0, 0.0, 360.0, false, MacroCurve::Linear, k, 0.0) - 400.0 * k).abs() < 1e-4);
    }

    /// `PARAM_TWO_WAY_BINDING_DESIGN.md` invariant: forward and inverse
    /// cannot drift. For a grid of (min, max, invert, curve, scale, offset) ×
    /// values: `apply(invert(x)) ≈ x` within 1e-4 across all four curves;
    /// `invert(apply(x)) ≈ x` for in-range x.
    #[test]
    fn card_reshape_roundtrips() {
        use crate::macro_bank::MacroCurve;
        let curves = [
            MacroCurve::Linear,
            MacroCurve::Exponential,
            MacroCurve::Logarithmic,
            MacroCurve::SCurve,
        ];
        let ranges: [(f32, f32); 3] = [(0.0, 1.0), (0.0, 10.0), (-5.0, 5.0)];
        let affines: [(f32, f32); 2] = [(1.0, 0.0), (2.0, 3.0)];
        for curve in curves {
            for invert in [false, true] {
                for (min, max) in ranges {
                    for (scale, offset) in affines {
                        let mut x = min;
                        let step = (max - min) / 10.0;
                        while x <= max {
                            let target = apply_card_reshape(x, min, max, invert, curve, scale, offset);
                            let back = invert_card_reshape(target, min, max, invert, curve, scale, offset)
                                .expect("non-degenerate scale");
                            assert!(
                                (back - x).abs() < 1e-3,
                                "{curve:?} invert={invert} range=({min},{max}) affine=({scale},{offset}): \
                                 invert_card_reshape(apply_card_reshape({x})) = {back}, expected ~{x}"
                            );
                            x += step;
                        }
                    }
                }
            }
        }
        // Degenerate affine: no inverse representable.
        assert!(invert_card_reshape(1.0, 0.0, 1.0, false, MacroCurve::Linear, 0.0, 0.0).is_none());
    }

    #[test]
    fn duplicated_assigns_fresh_id_and_drops_hardware_bindings() {
        // BUG-001/004: a duplicated/pasted effect must be an INDEPENDENT copy —
        // a fresh EffectId (not a shared reference) and no carried-over hardware
        // bindings (Ableton mappings / audio mods). Per-instance modulation
        // (drivers) is kept; group_id is left for the caller to decide.
        let mut src = PresetInstance::new(PresetTypeId::new("Blur"));
        src.ableton_mappings = Some(Vec::new());
        src.audio_mods = Some(Vec::new());
        src.group_id = Some(EffectGroupId::new("grp"));
        src.create_driver("amount".into());
        assert!(src.has_drivers());

        let copy = src.duplicated();

        assert_ne!(copy.id, src.id, "copy must get a fresh EffectId");
        assert!(
            copy.ableton_mappings.is_none(),
            "Ableton mappings must not ride along on a copy"
        );
        assert!(
            copy.audio_mods.is_none(),
            "audio mods must not ride along on a copy"
        );
        assert!(copy.has_drivers(), "per-instance drivers are kept");
        assert_eq!(
            copy.group_id, src.group_id,
            "duplicated() leaves group_id for the caller to remap/clear"
        );
    }

    #[test]
    fn test_driver_sine() {
        let val =
            ParameterDriver::evaluate(Beats(0.0), BeatDivision::Quarter, DriverWaveform::Sine, 0.0);
        assert!((val - 0.5).abs() < 0.01);

        let val = ParameterDriver::evaluate(
            Beats(0.25),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
            0.0,
        );
        assert!((val - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_driver_square() {
        let val = ParameterDriver::evaluate(
            Beats(0.1),
            BeatDivision::Quarter,
            DriverWaveform::Square,
            0.0,
        );
        assert_eq!(val, 1.0);

        let val = ParameterDriver::evaluate(
            Beats(0.6),
            BeatDivision::Quarter,
            DriverWaveform::Square,
            0.0,
        );
        assert_eq!(val, 0.0);
    }

    #[test]
    fn test_driver_random_hash_matches_unity() {
        let val = ParameterDriver::evaluate(
            Beats(1.0),
            BeatDivision::Quarter,
            DriverWaveform::Random,
            0.0,
        );
        assert!((0.0..=1.0).contains(&val));
        // Same cycle should give same value
        let val2 = ParameterDriver::evaluate(
            Beats(1.5),
            BeatDivision::Quarter,
            DriverWaveform::Random,
            0.0,
        );
        assert_eq!(val, val2);
    }

    // ── ParameterDriver backward-compat Deserialize (step 8) ──────

    #[test]
    fn driver_deserialize_legacy_param_index() {
        // V1.1.0 shape: { paramIndex: 1, ... }. The custom Deserialize
        // parks the index in `legacy_param_index` and leaves
        // `param_id` empty. The post-load resolver fills `param_id`
        // later — this test only covers the Deserialize step.
        let json = r#"{
            "paramIndex": 2,
            "beatDivision": 4,
            "waveform": 0,
            "enabled": true,
            "phase": 0.0,
            "baseValue": 0.0,
            "trimMin": 0.0,
            "trimMax": 1.0,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert!(
            d.param_id.is_empty(),
            "legacy shape must leave param_id empty until post-load resolution"
        );
        assert_eq!(d.legacy_param_index, Some(2));
        assert_eq!(d.beat_division, BeatDivision::Half);
    }

    #[test]
    fn driver_deserialize_canonical_param_id() {
        // V1.2+ shape: { paramId: "amount", ... }. No post-load
        // resolution needed — `param_id` is already set, and
        // `legacy_param_index` stays None.
        let json = r#"{
            "paramId": "amount",
            "beatDivision": 5,
            "waveform": 1,
            "enabled": true,
            "phase": 0.5,
            "baseValue": 0.0,
            "trimMin": 0.1,
            "trimMax": 0.9,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "amount");
        assert_eq!(d.legacy_param_index, None);
        assert_eq!(d.beat_division, BeatDivision::Whole);
        assert!((d.phase - 0.5).abs() < 1e-6);
    }

    #[test]
    fn driver_deserialize_param_id_wins_when_both_present() {
        // If both fields are sent (forward-migration test fixtures or
        // a transitional save shape), `param_id` is canonical and
        // `param_index` is ignored. No legacy resolution scheduled.
        let json = r#"{
            "paramId": "threshold",
            "paramIndex": 99,
            "beatDivision": 3,
            "waveform": 0
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "threshold");
        assert_eq!(d.legacy_param_index, None);
    }

    #[test]
    fn driver_deserialize_missing_both_loads_as_unresolvable() {
        // No paramId, no paramIndex — load doesn't error; the driver
        // stays present but inert. Better than refusing the entire
        // project. Real recovery path is the post-load resolver, but
        // there's nothing for it to do here.
        let json = r#"{
            "beatDivision": 4
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "");
        assert_eq!(d.legacy_param_index, None);
    }

    #[test]
    fn driver_serialize_writes_param_id_only() {
        // After step 8, saved files always carry the new shape. The
        // legacy `paramIndex` field is never written (skipped via
        // custom Deserialize / derived Serialize on the canonical
        // field set).
        let driver = ParameterDriver::new("amount", BeatDivision::Half, DriverWaveform::Triangle);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(json.contains("\"paramId\":\"amount\""));
        assert!(
            !json.contains("paramIndex"),
            "Serialize must not write legacy paramIndex field; got: {json}"
        );
        assert!(
            !json.contains("legacyParamIndex"),
            "Serialize must not leak the runtime-only legacy_param_index field; got: {json}"
        );
    }

    #[test]
    fn driver_round_trips_through_canonical_shape() {
        let driver =
            ParameterDriver::new("threshold", BeatDivision::FourWhole, DriverWaveform::Square);
        let json = serde_json::to_string(&driver).unwrap();
        let back: ParameterDriver = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_id, driver.param_id);
        assert_eq!(back.beat_division, driver.beat_division);
        assert_eq!(back.waveform, driver.waveform);
        assert_eq!(back.legacy_param_index, None);
    }

    #[test]
    fn driver_sync_mode_omits_free_period_field() {
        // Sync mode (the default) must not write freePeriodBeats — pre-free-mode
        // projects round-trip byte-identically and stay tiny.
        let driver = ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sine);
        assert_eq!(driver.free_period_beats, None);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(
            !json.contains("freePeriodBeats"),
            "sync-mode driver must not emit freePeriodBeats; got: {json}"
        );
    }

    #[test]
    fn driver_free_period_round_trips() {
        let mut driver =
            ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sine);
        driver.free_period_beats = Some(3.0);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(json.contains("\"freePeriodBeats\":3"), "got: {json}");
        let back: ParameterDriver = serde_json::from_str(&json).unwrap();
        assert_eq!(back.free_period_beats, Some(3.0));
    }

    #[test]
    fn driver_legacy_json_loads_as_sync_mode() {
        // A project saved before free mode existed has no freePeriodBeats key.
        let json = r#"{
            "paramId": "amount",
            "beatDivision": 3,
            "waveform": 0,
            "enabled": true,
            "phase": 0.0,
            "baseValue": 0.0,
            "trimMin": 0.0,
            "trimMax": 1.0,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.free_period_beats, None, "legacy driver must default to sync mode");
        assert_eq!(d.period_beats(), BeatDivision::Quarter.beats());
    }

    #[test]
    fn free_period_overrides_division_for_evaluation() {
        // period_beats() prefers the free period; evaluate_with_period cycles on it.
        let mut d = ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sawtooth);
        d.free_period_beats = Some(3.0);
        assert_eq!(d.period_beats(), 3.0);
        // Sawtooth = phase; at beat 0 phase 0, at beat 1.5 phase 0.5 over a 3-beat period.
        let v0 = ParameterDriver::evaluate_with_period(
            Beats(0.0),
            d.period_beats(),
            d.waveform,
            d.phase,
        );
        let v_half = ParameterDriver::evaluate_with_period(
            Beats(1.5),
            d.period_beats(),
            d.waveform,
            d.phase,
        );
        assert!(v0.abs() < 1e-6, "phase 0 at beat 0");
        assert!((v_half - 0.5).abs() < 1e-6, "half phase at beat 1.5 over 3-beat period");
    }

    // ── ParamEnvelope backward-compat Deserialize (step 9) ──────

    #[test]
    fn envelope_deserialize_legacy_param_index() {
        // V1.1 shape: { targetEffectType, targetParamIndex: 1, ... }. The
        // leftover targetEffectType is ignored (the v1.5→v1.6 migration
        // consumes it to place the envelope on the right instance).
        let json = r#"{
            "targetEffectType": "Bloom",
            "targetParamIndex": 0,
            "enabled": true,
            "attackBeats": 0.25,
            "decayBeats": 0.25,
            "sustainLevel": 0.5,
            "releaseBeats": 0.25,
            "targetNormalized": 1.0
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert!(e.param_id.is_empty());
        assert_eq!(e.legacy_param_index, Some(0));
    }

    #[test]
    fn envelope_deserialize_canonical_param_id() {
        // Legacy ADSR keys (attackBeats etc.) are ignored post-simplification —
        // the envelope loads as a plain decay envelope keeping only its depth.
        let json = r#"{
            "paramId": "amount",
            "enabled": true,
            "attackBeats": 0.5,
            "targetNormalized": 0.7
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.param_id, "amount");
        assert_eq!(e.legacy_param_index, None);
        assert!((e.target_normalized - 0.7).abs() < 1e-6);
    }

    #[test]
    fn envelope_deserialize_param_id_wins_when_both_present() {
        let json = r#"{
            "paramId": "threshold",
            "targetParamIndex": 99,
            "enabled": true
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.param_id, "threshold");
        assert_eq!(e.legacy_param_index, None);
    }

    #[test]
    fn envelope_serialize_writes_param_id_only() {
        let env = ParamEnvelope::new("amount");
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"paramId\":\"amount\""));
        assert!(
            !json.contains("targetParamIndex"),
            "Serialize must not write legacy targetParamIndex; got: {json}"
        );
        assert!(!json.contains("legacyParamIndex"));
        assert!(
            !json.contains("targetEffectType"),
            "Serialize must not write targetEffectType post-unification; got: {json}"
        );
    }

    #[test]
    fn envelope_round_trips_through_canonical_shape() {
        let env = ParamEnvelope::new("amount");
        let json = serde_json::to_string(&env).unwrap();
        let back: ParamEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_id, env.param_id);
        assert_eq!(back.legacy_param_index, None);
    }

    // ── PresetInstance `params` wire format (V1.4, PARAM_STORAGE_DESIGN.md §4) ──
    //
    // The typed (de)serialize understands ONLY the id-keyed `params` map —
    // the four historical `paramValues` shapes (positional/keyed × bare-f32/
    // {value,exposed}) are deleted, not reimplemented here (D4); their
    // conversion tests now live in `manifold-io`'s
    // `migrations::param_storage_v14`, which runs before typed deserialize
    // ever sees the JSON. These tests cover what's left on this side: the
    // V1.4 shape itself, `base` folding, and unregistered-type degradation.
    //
    // "TestCreateDefaultUntouched" (registered below, single param
    // "amount", default 0.42) and "TestTwoParamRoundTrip" (registered
    // below, "alpha"/"beta") stand in for a real bundled effect — Rust
    // module items are visible regardless of declaration order.

    #[test]
    fn effect_instance_deserialize_v14_params_map() {
        let json = r#"{
            "id": "abc12345",
            "effectType": "TestCreateDefaultUntouched",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 0.75, "exposed": true, "base": 0.5 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.params.len(), 1);
        let amount = fx.params.get("amount").unwrap();
        assert!((amount.value - 0.75).abs() < f32::EPSILON);
        assert!(amount.exposed);
        // `base` present on the entry → base_tracked, folded into the entry.
        assert!(fx.base_tracked);
        assert!((amount.base - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn effect_instance_deserialize_v14_params_without_base_leaves_base_untracked() {
        let json = r#"{
            "effectType": "TestCreateDefaultUntouched",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 0.75 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert!(!fx.base_tracked);
        // exposed defaults to true when the key is absent from the entry.
        assert!(fx.params.get("amount").unwrap().exposed);
    }

    #[test]
    fn effect_instance_deserialize_params_without_registry_keeps_state() {
        // No registry def for this type → the template is UNRESOLVABLE,
        // which is not the same as "this id was deprecated by its template".
        // Dropping here was the BUG-036 class (a project-local preset's
        // template registers after layer deserialize under the wrong load
        // order); the entry is kept on a placeholder spec instead, so no
        // param state is ever lost to a missing template.
        let json = r#"{
            "effectType": "TotallyUnregisteredEffectType",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 0.7 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.params.len(), 1, "entry kept despite missing template");
        let p = fx.params.get("amount").unwrap();
        assert!((p.value - 0.7).abs() < f32::EPSILON);
        // Placeholder spec carries identity; a later load with the template
        // present reconciles the real descriptor (only state serializes for
        // a bundled-origin param).
        assert_eq!(p.spec.id, "amount");
    }

    #[test]
    fn effect_instance_serialize_omits_params_without_registry() {
        // No registry def and no user-added tail → `params` has nothing to
        // key its entries by, so it serializes empty. This is the honest
        // consequence of deleting the positional fallback (D4): an
        // unregistered type's values are not addressable, so they are not
        // written, rather than dumped into an array nothing can read back
        // by id. In production this path is unreachable (every shipping
        // effect is registered).
        let fx = PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: EffectId::new("abc12345"),
            effect_type: PresetTypeId::from_string("TotallyUnregisteredEffectType".to_string()),
            enabled: true,
            collapsed: false,
            // Post-manifest (D4): there is no "unaddressable positional values"
            // failure mode — every `Param` is self-describing by id. An
            // unregistered type seeds an EMPTY manifest (no template), so
            // `params` serializes empty; the instance is never lost.
            params: crate::params::ParamManifest::default(),
            base_tracked: false,
            pending_wire: None,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            automation_lanes: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
            relight: false,
            relight_params: RelightParams::default(),
        };
        let json = serde_json::to_string(&fx).unwrap();
        assert!(
            json.contains("\"params\":{}"),
            "unregistered type must serialize an empty params map, not lose the instance; got: {json}"
        );
    }

    #[test]
    fn effect_instance_serialize_round_trips_hidden_and_visible_params() {
        let fx = PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: EffectId::new("abc12345"),
            effect_type: PresetTypeId::from_string("TestTwoParamRoundTrip".to_string()),
            enabled: true,
            collapsed: false,
            params: crate::params::ParamManifest::from_params(vec![
                slot("alpha", 0.1, true),
                slot("beta", 0.2, false),
            ]),
            base_tracked: false,
            pending_wire: None,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            automation_lanes: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
            relight: false,
            relight_params: RelightParams::default(),
        };
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"alpha\":{\"value\":0.1,\"exposed\":true}"));
        assert!(json.contains("\"beta\":{\"value\":0.2,\"exposed\":false}"));
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.params.len(), 2);
        let a = back.params.get("alpha").unwrap();
        assert_eq!(a.value, 0.1);
        assert!(a.exposed);
        let b = back.params.get("beta").unwrap();
        assert_eq!(b.value, 0.2);
        assert!(!b.exposed);
    }

    /// `docs/DEPTH_RELIGHT_DESIGN.md` P5: a pre-P5 project file — no
    /// `relight`/`relightParams` keys at all — must load with the toggle off
    /// and every knob at its proven-recipe default (D2's "every existing
    /// project loads unchanged" contract), and a freshly-constructed instance
    /// must serialize with NEITHER key present (byte-identical old projects).
    #[test]
    fn relight_defaults_false_and_omits_from_wire_when_untouched() {
        let fx = PresetInstance::new(PresetTypeId::from_string("Mirror".to_string()));
        assert!(!fx.relight, "relight must default to false");
        assert_eq!(
            fx.relight_params,
            RelightParams::default(),
            "relight_params must default to the D3 proven recipe"
        );
        let json = serde_json::to_string(&fx).unwrap();
        assert!(!json.contains("\"relight\""), "untouched instance must not emit `relight`: {json}");
        assert!(
            !json.contains("relightParams"),
            "untouched instance must not emit `relightParams`: {json}"
        );

        // A pre-P5 project's raw JSON (no relight keys at all) still loads —
        // the field-less shape is exactly what an old saved project looks
        // like on disk.
        let legacy_json = r#"{"id":"abc12345","effectType":"Mirror","enabled":true,"collapsed":false,"params":{}}"#;
        let back: PresetInstance = serde_json::from_str(legacy_json).unwrap();
        assert!(!back.relight);
        assert_eq!(back.relight_params, RelightParams::default());

        // Toggling on + editing a knob DOES round-trip.
        let mut on = fx;
        on.relight = true;
        on.relight_params.relief = 0.8;
        let json_on = serde_json::to_string(&on).unwrap();
        assert!(json_on.contains("\"relight\":true"));
        assert!(json_on.contains("relightParams"));
        let back_on: PresetInstance = serde_json::from_str(&json_on).unwrap();
        assert!(back_on.relight);
        assert_eq!(back_on.relight_params.relief, 0.8);
    }

    #[test]
    fn effect_instance_legacy_param0_through_param3_round_trip() {
        // V1.0 had flat param0..param3 fields alongside the param wire.
        // The custom Deserialize must continue to capture them so the
        // existing align_to_definition migration sees both shapes.
        let json = r#"{
            "effectType": "TestCreateDefaultUntouched",
            "enabled": true,
            "collapsed": false,
            "params": {},
            "param0": 0.5,
            "param1": 1.0
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.legacy_param0, Some(0.5));
        assert_eq!(fx.legacy_param1, Some(1.0));
        assert_eq!(fx.legacy_param2, None);
        assert_eq!(fx.legacy_param3, None);
        // Round-trip preserves them.
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"param0\":0.5"));
        assert!(json.contains("\"param1\":1.0"));
    }

    #[test]
    fn effect_instance_skip_serializing_optional_none() {
        let fx = PresetInstance::new(PresetTypeId::from_string("TestEffect".to_string()));
        let json = serde_json::to_string(&fx).unwrap();
        // `params` always emits (even empty); `base` never appears on any
        // entry for a fresh, untouched instance.
        assert!(json.contains("\"params\":{}"));
        assert!(!json.contains("\"base\":"));
        assert!(!json.contains("\"drivers\""));
        assert!(!json.contains("\"abletonMappings\""));
        assert!(!json.contains("\"groupId\""));
        assert!(!json.contains("\"param0\""));
        // After the binding-storage unification there is no separate
        // `userParamBindings` field at all — user bindings live in the
        // graph. A fresh effect has no graph, so nothing extra emits and
        // existing fixtures round-trip byte-identically.
        assert!(!json.contains("\"userParamBindings\""));
    }

    // ── Map deserialize alias-aware path (step 15) ────────────────

    #[test]
    fn params_map_deserialize_drops_unknown_id() {
        // Without any alias entries, an unknown id is silently dropped.
        // This is the orphan policy — same as drivers/envelopes/Ableton.
        let json = r#"{
            "id": "abc12345",
            "effectType": "TestCreateDefaultUntouched",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 0.7 }, "old_phantom_param": { "value": 0.5 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        // amount resolves via the registry; old_phantom_param has nowhere
        // to go (not static, not a user-added tail id) and is dropped.
        assert_eq!(fx.params.len(), 1);
        assert!((fx.params.get("amount").unwrap().value - 0.7).abs() < f32::EPSILON);
    }

    inventory::submit! {
        crate::effect_registration::EffectMetadata {
            id: PresetTypeId::new("TestTwoParamRoundTrip"),
            display_name: "Test Two Param Round Trip",
            category: "Test",
            available: true,
            osc_prefix: "testTwoParamRoundTrip",
            legacy_discriminant: None,
            params: &[
                crate::generator_registration::ParamSpec::continuous(
                    "alpha", "Alpha", 0.0, 1.0, 0.0, "F2", "",
                ),
                crate::generator_registration::ParamSpec::continuous(
                    "beta", "Beta", 0.0, 1.0, 0.0, "F2", "",
                ),
            ],
        }
    }

    // ── User-exposed parameter bindings (Phase 3 step 20) ─────────

    fn sample_user_binding(id: &str, node: &str, inner: &str) -> UserParamBinding {
        UserParamBinding {
            id: id.to_string(),
            label: inner.to_string(),
            node_id: NodeId::new(node),
            legacy_node_handle: None,
            inner_param: inner.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.25,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        }
    }

    /// Build a bundled test [`Param`] (value == base == `value`) with the given
    /// id, exposure, and a 0..1 range. Replaces the old positional `ParamSlot`.
    fn slot(id: &str, value: f32, exposed: bool) -> crate::params::Param {
        let spec = crate::effect_graph_def::ParamSpecDef {
            id: id.to_string(),
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        };
        let mut p = crate::params::Param::bundled(spec);
        p.value = value;
        p.base = value;
        p.exposed = exposed;
        p
    }

    /// Build a manifest from positional `(value, exposed)` pairs, assigning
    /// synthetic ids `p0`, `p1`, … in card order — the value-only analogue of
    /// the old `param_values: vec![ParamSlot::exposed(..)]`.
    fn manifest(slots: &[(f32, bool)]) -> crate::params::ParamManifest {
        crate::params::ParamManifest::from_params(
            slots
                .iter()
                .enumerate()
                .map(|(i, &(v, e))| slot(&format!("p{i}"), v, e))
                .collect(),
        )
    }

    #[test]
    fn user_param_binding_serde_round_trip() {
        // A standalone UserParamBinding round-trips through JSON
        // without losing any field. Wire shape uses camelCase keys.
        let ub = sample_user_binding("user.uv_transform.translate.1", "uv_transform", "translate");
        let json = serde_json::to_string(&ub).unwrap();
        assert!(json.contains("\"id\":\"user.uv_transform.translate.1\""));
        assert!(json.contains("\"nodeId\":\"uv_transform\""));
        // The runtime addressing key is `nodeId`; the legacy `nodeHandle`
        // key only ever appears when reading a pre-node-id file and is
        // skip-serialized once cleared.
        assert!(!json.contains("nodeHandle"));
        assert!(json.contains("\"innerParam\":\"translate\""));
        assert!(json.contains("\"defaultValue\":0.25"));
        assert!(json.contains("\"convert\":{\"type\":\"Float\"}"));
        let back: UserParamBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ub);
    }

    #[test]
    fn user_param_binding_convert_default_is_float() {
        // Missing `convert` field defaults to Float — older serialized
        // bindings (if we ever ship without it) load cleanly.
        let json = r#"{
            "id": "user.x.y.1", "label": "Y",
            "nodeHandle": "x", "innerParam": "y",
            "min": 0.0, "max": 1.0, "defaultValue": 0.5
        }"#;
        let ub: UserParamBinding = serde_json::from_str(json).unwrap();
        assert_eq!(ub.convert, ParamConvert::Float);
        // Pre-node-id `nodeHandle` is captured by the load shim (node_id
        // stays empty until the renderer-layer migration resolves it).
        assert_eq!(ub.legacy_node_handle.as_deref(), Some("x"));
        assert!(ub.node_id.is_empty());
    }

    #[test]
    fn effect_instance_round_trip_with_user_bindings_against_bloom() {
        // Bloom is registered in this crate's tests with one param
        // `amount`. Add two user bindings and verify the whole
        // PresetInstance round-trips through JSON, including the
        // user-binding tail values landing in the right param_values
        // slots regardless of JSON key ordering.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]); // static prefix
        fx.append_user_binding(sample_user_binding(
            "user.uv_transform.translate.1",
            "uv_transform",
            "translate",
        ));
        fx.append_user_binding(sample_user_binding("user.mix.amount.1", "mix", "amount"));
        // After append, the manifest should carry [amount=0.7, translate=0.25, mix.amount=0.25].
        assert_eq!(fx.params.len(), 3);
        assert_eq!(fx.params.get("amount").unwrap().value, 0.7);
        assert_eq!(fx.params.get("user.uv_transform.translate.1").unwrap().value, 0.25);
        assert_eq!(fx.params.get("user.mix.amount.1").unwrap().value, 0.25);
        // Tweak the user-tail values to verify they round-trip.
        fx.params.get_mut("user.uv_transform.translate.1").unwrap().value = 0.42;
        fx.params.get_mut("user.mix.amount.1").unwrap().value = 0.91;

        let json = serde_json::to_string(&fx).unwrap();
        // User bindings now ride out inside the per-instance `graph`
        // (preset_metadata.bindings, userAdded), not a separate array.
        assert!(json.contains("\"graph\""));
        assert!(json.contains("\"userAdded\":true"));
        // V1.3 wire emits {value, exposed} objects per entry; the
        // param_values tail is keyed by the user-binding id.
        assert!(json.contains("\"amount\":{\"value\":0.7,\"exposed\":true}"));
        assert!(json.contains("\"user.uv_transform.translate.1\":{\"value\":0.42"));
        assert!(json.contains("\"user.mix.amount.1\":{\"value\":0.91"));

        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        let back_bindings = back.user_param_bindings();
        assert_eq!(back_bindings.len(), 2);
        assert_eq!(back_bindings[0].id, "user.uv_transform.translate.1");
        assert_eq!(back_bindings[1].id, "user.mix.amount.1");
        assert_eq!(back.params.len(), 3);
        assert_eq!(back.params.get("amount").unwrap().value, 0.7);
        assert_eq!(back.params.get("user.uv_transform.translate.1").unwrap().value, 0.42);
        assert_eq!(back.params.get("user.mix.amount.1").unwrap().value, 0.91);
    }

    #[test]
    fn user_exposed_angle_param_carries_is_angle_through_manifest_and_synth() {
        // Regression guard for the P5 inspector fix: before `is_angle` had a
        // home on the spec, exposing an angle inner param dropped the flag at
        // persistence and `synth_user_binding` rebuilt it as `false`, so the
        // card never showed degrees. Now the flag is seeded onto the manifest
        // spec at expose, survives a JSON round-trip, and synth reads it back.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);

        let mut angle = sample_user_binding("user.rotate.angle.1", "rotate", "angle");
        angle.is_angle = true;
        fx.append_user_binding(angle);
        let plain = sample_user_binding("user.mix.amount.1", "mix", "amount"); // is_angle: false
        fx.append_user_binding(plain);

        // Seed: the flag reached the live manifest spec (single home).
        assert!(fx.params.get("user.rotate.angle.1").unwrap().spec.is_angle);
        assert!(!fx.params.get("user.mix.amount.1").unwrap().spec.is_angle);

        // Read-back: synth (the card/renderer view) reflects the spec, not a
        // hardcoded false.
        let synth = fx.user_param_bindings();
        let a = synth.iter().find(|b| b.id == "user.rotate.angle.1").unwrap();
        let p = synth.iter().find(|b| b.id == "user.mix.amount.1").unwrap();
        assert!(a.is_angle, "angle user param must synth is_angle=true");
        assert!(!p.is_angle, "plain user param must stay is_angle=false");

        // Persistence: `is_angle: true` is emitted (skip_serializing_if only
        // skips false), so the flag survives save/load; false stays off disk.
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"isAngle\":true"), "true angle flag must serialize");
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert!(back.params.get("user.rotate.angle.1").unwrap().spec.is_angle);
        assert!(!back.params.get("user.mix.amount.1").unwrap().spec.is_angle);
        assert!(
            back.user_param_bindings()
                .iter()
                .find(|b| b.id == "user.rotate.angle.1")
                .unwrap()
                .is_angle
        );
    }

    /// Regression for PARAM_STORAGE_BOUNDARIES_DESIGN.md P2 (D12): `graph
    /// .preset_metadata.params` is derived from the live manifest ONLY at
    /// serialize time — `EditParamMappingCommand` no longer dual-writes it,
    /// so the sole way a calibrated range can reach the wire is
    /// `GraphWithDerivedParams`. This builds an instance whose graph carries
    /// a STALE (template) `amount` spec that nothing in this test ever
    /// touches again, calibrates ONLY the manifest (mirroring what the
    /// command does post-P2), and proves the serialized `graph.presetMetadata
    /// .params` entry reflects the calibration, not the stale shadow — with
    /// a byte-comparison against the manifest's own spec.
    #[test]
    fn calibrated_param_derives_meta_params_on_save_not_the_stale_shadow() {
        use crate::effect_graph_def::{
            BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
        };

        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);
        // Calibrate the manifest — the live authority (PARAM_STORAGE_DESIGN
        // D6) — diverging it from the template range the graph below still
        // carries untouched.
        {
            let p = fx.params.get_mut("amount").unwrap();
            p.spec.min = 10.0;
            p.spec.max = 20.0;
            p.spec.name = "Recalibrated Amount".to_string();
            p.calibrated = true;
        }
        // The graph's own shadow copy — STALE template range (0..1, "Amount").
        // Nothing after this construction ever writes to it directly; only
        // the derive-on-save wrapper may change what actually serializes.
        fx.graph = Some(EffectGraphDef {
            version: crate::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::BLOOM,
                display_name: String::new(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![ParamSpecDef {
                    id: "amount".to_string(),
                    name: "Amount".to_string(),
                    min: 0.0,
                    max: 1.0,
                    default_value: 0.7,
                    whole_numbers: false,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: Vec::new(),
                    format_string: None,
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
                    default_value: 0.7,
                    target: BindingTarget::Node {
                        node_id: NodeId::new("grade"),
                        param: "amount".to_string(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                }],
                skip_mode: Default::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            }),
            nodes: Vec::new(),
            wires: Vec::new(),
        });

        let json = serde_json::to_string(&fx).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let on_wire = &parsed["graph"]["presetMetadata"]["params"][0];
        assert_eq!(
            on_wire["min"], 10.0,
            "serialized graph must carry the CALIBRATED min, not the stale template 0.0",
        );
        assert_eq!(
            on_wire["max"], 20.0,
            "serialized graph must carry the CALIBRATED max, not the stale template 1.0",
        );
        assert_eq!(on_wire["name"], "Recalibrated Amount");

        // Byte-comparison guard: the derived wire entry is JSON-identical to
        // the live manifest spec, serialized independently. Round-trip both
        // sides through JSON TEXT (not `to_value` directly) so `serde_json`'s
        // float-formatting path matches on both sides of the comparison
        // (`to_value` on an f32-sourced f64 keeps its imprecise binary
        // widening, e.g. `0.7_f32` -> `0.699999988079071`, while the text
        // path prints/reparses the shortest round-tripping form, `0.7`).
        let manifest_spec_json: serde_json::Value = serde_json::from_str(
            &serde_json::to_string(&fx.params.get("amount").unwrap().spec).unwrap(),
        )
        .unwrap();
        assert_eq!(
            on_wire, &manifest_spec_json,
            "the derived meta.params entry must be byte-identical to the manifest's own spec",
        );

        // Round trip: reload and confirm the manifest — the card's
        // authority — carries the calibrated range through, not just the
        // one-shot JSON snapshot above.
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.params.get("amount").unwrap().spec.min, 10.0);
        assert_eq!(back.params.get("amount").unwrap().spec.max, 20.0);
    }

    #[test]
    fn append_user_binding_grows_param_values_with_default() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);
        fx.ensure_base_values();

        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        assert_eq!(fx.params.len(), 2);
        assert_eq!(fx.params.get("amount").unwrap().value, 0.7);
        assert_eq!(fx.params.get("user.a.b.1").unwrap().value, 0.25);
        // base rides each slot now (fork #16).
        assert!(fx.base_tracked);
        assert_eq!(fx.params.get("amount").unwrap().base, 0.7);
        assert_eq!(fx.params.get("user.a.b.1").unwrap().base, 0.25);
        // The binding now lives in the graph (the single storage list).
        assert_eq!(fx.user_param_count(), 1);
    }

    #[test]
    fn remove_user_binding_drops_corresponding_value_slot() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        fx.append_user_binding(sample_user_binding("user.c.d.1", "c", "d"));
        // A real slider edit sets base + value together (fork #16); set both so
        // the surviving slot is coherent after compaction.
        fx.set_base_param("user.a.b.1", 0.3);
        fx.set_base_param("user.c.d.1", 0.6);

        let removed = fx.remove_user_binding_by_id("user.a.b.1");
        assert!(removed.is_some());
        assert_eq!(fx.user_param_count(), 1);
        // Static prefix preserved + user tail compacted around the gap.
        // "amount" was seeded directly (never a `set_base_param` hand) so it
        // stays untouched; "user.c.d.1"'s value came from
        // `set_base_param("user.c.d.1", 0.6)` above, so it carries
        // `touched: true` — the funnel every hand (including this test's own
        // setup) writes through.
        assert_eq!(fx.params.len(), 2);
        let amount = fx.params.get("amount").unwrap();
        assert_eq!(amount.value, 0.7);
        assert!(!amount.touched);
        let cd = fx.params.get("user.c.d.1").unwrap();
        assert_eq!(cd.value, 0.6);
        assert_eq!(cd.base, 0.6);
        assert!(cd.exposed);
        assert!(cd.touched);
    }

    #[test]
    fn remove_user_binding_unknown_id_returns_none() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);
        let removed = fx.remove_user_binding_by_id("user.nope.1");
        assert!(removed.is_none());
        assert_eq!(fx.params.len(), 1);
        assert_eq!(fx.params.get("amount").unwrap().value, 0.7);
    }


    #[test]
    fn user_binding_index_lookup_by_id() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        fx.append_user_binding(sample_user_binding("user.c.d.1", "c", "d"));
        assert_eq!(fx.user_binding_index("user.a.b.1"), Some(0));
        assert_eq!(fx.user_binding_index("user.c.d.1"), Some(1));
        assert_eq!(fx.user_binding_index("user.nope.1"), None);
    }

    #[test]
    fn snapshot_values_into_def_bakes_current_base_as_default() {
        // Make Unique / Export must freeze the card's current values into the
        // def as its new defaults, so the preset reproduces the look later.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        assert!(fx.set_base_param_by_id("user.a.b.1", 0.83));

        let mut def = fx.graph.clone().expect("graph carries metadata");
        fx.snapshot_values_into_def(&mut def);

        let meta = def.preset_metadata.as_ref().unwrap();
        let p = meta.params.iter().find(|p| p.id == "user.a.b.1").unwrap();
        assert_eq!(
            p.default_value, 0.83,
            "current base value becomes the def's param default"
        );
        let b = meta.bindings.iter().find(|b| b.id == "user.a.b.1").unwrap();
        assert_eq!(b.default_value, 0.83, "the binding default tracks it too");
    }

    #[test]
    fn reseed_param_values_from_def_replaces_values_with_def_defaults() {
        // Import retargets to a def with a different param structure; the old
        // positional values can't carry over, so reseed rebuilds them from the
        // def's defaults (declaration order, all exposed).
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = manifest(&[(0.1, true), (0.2, true)]);

        let mut donor = PresetInstance::new(PresetTypeId::BLOOM);
        donor.append_user_binding(sample_user_binding("user.x.y.1", "x", "y"));
        assert!(donor.set_base_param_by_id("user.x.y.1", 0.55));
        let mut def = donor.graph.clone().expect("graph carries metadata");
        donor.snapshot_values_into_def(&mut def);

        fx.reseed_param_values_from_def(&def);
        assert_eq!(
            fx.params.len(),
            1,
            "reseed rebuilds the manifest from the def's (snapshotted) defaults",
        );
        assert_eq!(fx.params.get("user.x.y.1").unwrap().value, 0.55);
    }

    #[test]
    fn remove_exposures_for_node_prunes_then_restores_round_trip() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        // Two exposed user params on different nodes; we delete node "blur".
        fx.append_user_binding(sample_user_binding("user.blur.radius.1", "blur", "radius"));
        fx.append_user_binding(sample_user_binding("user.other.x.1", "other", "x"));
        assert!(fx.set_base_param_by_id("user.blur.radius.1", 0.66));
        // Automation on the blur param — must be pruned with it, restored on undo.
        fx.create_driver(ParamId::from("user.blur.radius.1"));
        fx.envelopes = Some(vec![ParamEnvelope::new("user.blur.radius.1")]);

        // Snapshot entry content (not the whole manifest — `topology` bumps on
        // every push/remove/insert_at, so it legitimately differs after a
        // remove+restore round trip even though every param's own state is
        // back to identical).
        let pre_entries: Vec<crate::params::Param> = fx.params.iter().cloned().collect();

        let removed = fx.remove_exposures_for_node(&NodeId::new("blur"));
        assert_eq!(removed.len(), 1, "one slider was bound to the deleted node");

        // Slider, slot, driver, envelope all gone; the unrelated slider survives.
        assert!(!fx.params.contains("user.blur.radius.1"));
        assert!(fx.find_driver("user.blur.radius.1").is_none());
        assert!(
            fx.envelopes.is_none(),
            "pruning the last envelope collapses the list to None"
        );
        assert!(fx.params.contains("user.other.x.1"));

        // Undo restores values, metadata, and automation.
        fx.restore_exposures(removed);
        let post_entries: Vec<crate::params::Param> = fx.params.iter().cloned().collect();
        assert_eq!(
            post_entries, pre_entries,
            "value slots restored at their original positions"
        );
        assert!(
            fx.params.contains("user.blur.radius.1"),
            "binding + param spec restored"
        );
        assert!(fx.find_driver("user.blur.radius.1").is_some(), "driver restored");
        assert!(
            fx.find_envelope("user.blur.radius.1").is_some(),
            "envelope restored"
        );
    }

    #[test]
    fn prune_orphaned_automation_drops_unresolvable_then_restores() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b")); // resolves
        fx.create_driver(ParamId::from("user.a.b.1")); // live
        fx.create_driver(ParamId::from("user.gone.x.1")); // orphan — never bound
        fx.envelopes = Some(vec![ParamEnvelope::new("user.gone.x.1")]); // orphan
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: ParamId::from("user.gone.x.1"),
            enabled: true,
            points: vec![AutomationPoint {
                beat: Beats(0.0),
                value: 0.5,
                shape: SegmentShape::Linear,
            }],
        }]); // orphan — same unresolvable id as the driver/envelope above

        let removed = fx.prune_orphaned_automation();
        assert!(fx.find_driver("user.a.b.1").is_some(), "live driver kept");
        assert!(fx.find_driver("user.gone.x.1").is_none(), "orphan driver pruned");
        assert!(
            fx.envelopes.is_none(),
            "sole orphan envelope pruned, list collapses to None"
        );
        assert!(
            fx.automation_lanes.is_none(),
            "sole orphan automation lane pruned, list collapses to None"
        );

        fx.restore_automation(removed);
        assert!(
            fx.find_driver("user.gone.x.1").is_some(),
            "orphan driver restored on undo"
        );
        assert!(
            fx.find_envelope("user.gone.x.1").is_some(),
            "orphan envelope restored on undo"
        );
        assert_eq!(
            fx.automation_lanes.as_ref().map(|v| v.len()),
            Some(1),
            "orphan automation lane restored on undo"
        );
    }

    #[test]
    fn remove_exposures_for_node_is_noop_when_nothing_bound() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.blur.radius.1", "blur", "radius"));
        let before = fx.params.clone();
        let removed = fx.remove_exposures_for_node(&NodeId::new("nonexistent"));
        assert!(removed.is_empty(), "no binding targets that node");
        assert_eq!(fx.params, before, "nothing changed");
    }

    #[test]
    fn get_param_def_synthesizes_user_binding_def() {
        // ParamSource::get_param_def must return a ParamSpecDef shaped from
        // the user binding for indices past the static count, so UI code
        // (slider rendering, OSC formatting) gets correct min/max/label.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(UserParamBinding {
            id: "user.uv.translate.1".to_string(),
            label: "Translate".to_string(),
            node_id: NodeId::new("uv_transform"),
            legacy_node_handle: None,
            inner_param: "translate".to_string(),
            min: -2.0,
            max: 2.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        });
        let pd = ParamSource::get_param_def(&fx, "user.uv.translate.1");
        assert_eq!(pd.id, "user.uv.translate.1");
        assert_eq!(pd.name, "Translate");
        assert!((pd.min + 2.0).abs() < f32::EPSILON);
        assert!((pd.max - 2.0).abs() < f32::EPSILON);
        assert!(!pd.whole_numbers);
        assert!(!pd.is_toggle);
    }

    #[test]
    fn deserialize_keyed_param_values_routes_user_ids_to_tail() {
        // The key insight: `params` comes in as a Map. The custom
        // Deserialize must consult the graph's `user_added` bindings (the
        // single storage list after the unification) to route user ids to
        // the right tail slots — regardless of JSON key order in the Map.
        let json = r#"{
            "id": "abc12345",
            "effectType": "Bloom",
            "enabled": true,
            "collapsed": false,
            "params": {
                "amount": { "value": 0.7 },
                "user.foo.bar.1": { "value": 0.3 },
                "user.baz.qux.1": { "value": 0.9 }
            },
            "graph": {
                "version": 0,
                "nodes": [],
                "wires": [],
                "presetMetadata": {
                    "id": "",
                    "displayName": "",
                    "category": "",
                    "oscPrefix": "",
                    "params": [
                        { "id": "user.foo.bar.1", "name": "Foo", "min": 0.0, "max": 1.0, "defaultValue": 0.5 },
                        { "id": "user.baz.qux.1", "name": "Baz", "min": 0.0, "max": 1.0, "defaultValue": 0.5 }
                    ],
                    "bindings": [
                        { "id": "user.foo.bar.1", "label": "Foo", "defaultValue": 0.5, "userAdded": true, "target": { "kind": "node", "nodeId": "foo", "param": "bar" } },
                        { "id": "user.baz.qux.1", "label": "Baz", "defaultValue": 0.5, "userAdded": true, "target": { "kind": "node", "nodeId": "baz", "param": "qux" } }
                    ]
                }
            }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.user_param_count(), 2);
        assert_eq!(fx.params.len(), 3);
        assert!((fx.params.get("amount").unwrap().value - 0.7).abs() < f32::EPSILON);
        assert!((fx.params.get("user.foo.bar.1").unwrap().value - 0.3).abs() < f32::EPSILON);
        assert!((fx.params.get("user.baz.qux.1").unwrap().value - 0.9).abs() < f32::EPSILON);
    }

    // ─── Per-instance graph override (Phase 1) ──────────────────

    #[test]
    fn new_effect_instance_has_no_graph_override() {
        let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        assert!(fx.graph.is_none());
        assert_eq!(fx.graph_version, 0);
    }

    #[test]
    fn graph_field_skipped_when_none() {
        // Existing fixtures (Liveschool, Burn, WAYPOINTS) must
        // continue to round-trip byte-identically — the new field
        // must not appear in their JSON unless explicitly set.
        let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        let json = serde_json::to_string(&fx).unwrap();
        assert!(
            !json.contains("\"graph\""),
            "graph field must be skipped when None — got: {json}"
        );
    }

    #[test]
    fn graph_field_round_trips_when_present() {
        use crate::effect_graph_def::{
            EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire,
            SerializedParamValue,
        };

        let mut params = std::collections::BTreeMap::new();
        params.insert("mode".to_string(), SerializedParamValue::Enum { value: 7 });

        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                EffectGraphNode {
                    id: 0,
                    node_id: crate::NodeId::default(),
                    type_id: "system.source".to_string(),
                    handle: Some("source".to_string()),
                    params: Default::default(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: std::collections::BTreeMap::new(),
                    output_canvas_scales: std::collections::BTreeMap::new(),
                    group: None,
                },
                EffectGraphNode {
                    id: 1,
                    node_id: crate::NodeId::default(),
                    type_id: "node.transform".to_string(),
                    handle: Some("uv_transform".to_string()),
                    params,
                    exposed_params: Default::default(),
                    editor_pos: Some((100.0, 200.0)),
                    wgsl_source: None,
                    title: None,
                    output_formats: std::collections::BTreeMap::new(),
                    output_canvas_scales: std::collections::BTreeMap::new(),
                    group: None,
                },
            ],
            wires: vec![EffectGraphWire {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "source".to_string(),
            }],
        };

        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.graph = Some(def.clone());

        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"graph\""));

        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.graph, Some(def));
        // `graph_version` is not serialized — it resets on load.
        assert_eq!(back.graph_version, 0);
    }

    #[test]
    fn legacy_fixture_without_graph_field_still_loads() {
        // Pre-Phase-1 fixtures have no `graph` field at all. Loading
        // them must succeed with `graph: None`.
        let json = r#"{
            "id": "abc12345",
            "effectType": "Mirror",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 1.0, "exposed": true } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert!(fx.graph.is_none());
    }

    // ─── Automation lane curve evaluation ───

    fn pt(beat: f64, value: f32, shape: SegmentShape) -> AutomationPoint {
        AutomationPoint {
            beat: Beats(beat),
            value,
            shape,
        }
    }

    #[test]
    fn automation_lane_empty_returns_zero() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: Vec::new(),
        };
        assert_eq!(lane.value_at(Beats(4.0)), 0.0);
    }

    #[test]
    fn automation_lane_single_point_holds_everywhere() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![pt(4.0, 0.7, SegmentShape::Linear)],
        };
        assert_eq!(lane.value_at(Beats(-10.0)), 0.7);
        assert_eq!(lane.value_at(Beats(4.0)), 0.7);
        assert_eq!(lane.value_at(Beats(100.0)), 0.7);
    }

    #[test]
    fn automation_lane_before_first_point_holds_first_value() {
        // Ableton behavior: no backward extrapolation.
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(4.0, 0.2, SegmentShape::Linear),
                pt(8.0, 0.8, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(0.0)), 0.2);
        assert_eq!(lane.value_at(Beats(4.0)), 0.2);
    }

    #[test]
    fn automation_lane_after_last_point_holds_last_value() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(4.0, 0.2, SegmentShape::Linear),
                pt(8.0, 0.8, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(8.0)), 0.8);
        assert_eq!(lane.value_at(Beats(1000.0)), 0.8);
    }

    #[test]
    fn automation_lane_linear_segment_interpolates() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Linear),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        assert!((lane.value_at(Beats(2.0)) - 0.5).abs() < 1e-6);
        assert!((lane.value_at(Beats(1.0)) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn automation_lane_hold_segment_steps() {
        // `Hold` on the earlier point: the segment holds that point's value
        // for its whole span, then jumps at the next point — required for
        // enum/int-backed params.
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Hold),
                pt(4.0, 1.0, SegmentShape::Hold),
                pt(8.0, 2.0, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(0.0)), 0.0);
        assert_eq!(lane.value_at(Beats(3.9)), 0.0, "holds through the segment");
        assert_eq!(lane.value_at(Beats(4.0)), 1.0, "steps exactly at the next point");
        assert_eq!(lane.value_at(Beats(7.9)), 1.0);
    }

    #[test]
    fn automation_lane_curved_segment_bends_but_keeps_endpoints() {
        let convex = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        let concave = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(-1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        // Endpoints exact regardless of bend.
        assert_eq!(convex.value_at(Beats(0.0)), 0.0);
        assert_eq!(convex.value_at(Beats(4.0)), 1.0);
        // Midpoint: positive bend (convex) sits BELOW the linear midpoint
        // (slow start); negative bend (concave) sits ABOVE it (fast start).
        let mid_linear = 0.5;
        let mid_convex = convex.value_at(Beats(2.0));
        let mid_concave = concave.value_at(Beats(2.0));
        assert!(mid_convex < mid_linear, "convex bend lags at the midpoint");
        assert!(mid_concave > mid_linear, "concave bend leads at the midpoint");
    }

    #[test]
    fn automation_lane_bend_out_of_range_is_clamped() {
        // `Curved` bends are only meaningful in -1..1; anything past that
        // clamps rather than producing a wild exponent.
        let over = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(5.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        let clamped = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        assert!((over.value_at(Beats(2.0)) - clamped.value_at(Beats(2.0))).abs() < 1e-6);
    }

    #[test]
    fn automation_lane_three_points_binary_search_finds_middle_segment() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Linear),
                pt(4.0, 1.0, SegmentShape::Linear),
                pt(8.0, 0.0, SegmentShape::Linear),
            ],
        };
        assert!((lane.value_at(Beats(6.0)) - 0.5).abs() < 1e-6);
    }

    // ─── PresetInstance.automation_lanes serde (skip-when-empty) ───

    #[test]
    fn preset_instance_without_automation_lanes_serializes_byte_identical() {
        // No lanes → no `automationLanes` key at all, and round-tripping a
        // fixture that never had lanes must not introduce one. Same
        // skip-when-empty convention as `drivers`/`envelopes`/`audioMods`.
        let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        assert!(fx.automation_lanes.is_none());
        let json = serde_json::to_string(&fx).unwrap();
        assert!(
            !json.contains("automationLanes"),
            "no automation_lanes → no key on the wire; got: {json}"
        );
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert!(back.automation_lanes.is_none());
    }

    #[test]
    fn preset_instance_automation_lanes_roundtrip_when_present() {
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![pt(0.0, 0.25, SegmentShape::Curved(0.5))],
        }]);
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("automationLanes"));
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        let lanes = back.automation_lanes.expect("lanes round-trip");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].param_id, ParamId::from("amount"));
        assert!(lanes[0].enabled);
        assert_eq!(lanes[0].points.len(), 1);
        assert_eq!(lanes[0].points[0].value, 0.25);
        assert_eq!(lanes[0].points[0].shape, SegmentShape::Curved(0.5));
    }

    // ─── touched flag: the automation self-trigger footgun ───

    #[test]
    fn set_base_param_marks_touched() {
        // The single funnel every live hand writes through — the automation
        // evaluator's touch-detection relies on this.
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.params = manifest(&[(0.0, true)]);
        fx.set_base_param("p0", 0.5);
        assert!(fx.params.get("p0").unwrap().touched, "set_base_param marks touched");
    }

    #[test]
    fn write_base_param_does_not_mark_touched() {
        // System-level seeding (registry defaults) must not look like a hand
        // touch — see `preset_definition_registry::create_default`.
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.params = manifest(&[(0.0, true)]);
        fx.write_base_param("p0", 0.5);
        assert!(
            !fx.params.get("p0").unwrap().touched,
            "write_base_param must not set touched"
        );
        assert_eq!(fx.params.get("p0").unwrap().base, 0.5);
        assert_eq!(fx.params.get("p0").unwrap().value, 0.5);
    }

    #[test]
    fn set_base_param_from_automation_does_not_mark_touched() {
        // The automation evaluator's own write path — using the public
        // set_base_param here would self-latch the very next frame.
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.params = manifest(&[(0.0, true)]);
        fx.set_base_param_from_automation("p0", 0.5);
        assert!(
            !fx.params.get("p0").unwrap().touched,
            "set_base_param_from_automation must not set touched"
        );
        assert_eq!(fx.params.get("p0").unwrap().base, 0.5);
        assert_eq!(fx.params.get("p0").unwrap().value, 0.5);
    }

    // Registered via `inventory::submit!` at module scope (mirrors
    // `manifold-playback`'s `modulation::tests` fixture pattern) — the
    // registry is normally populated by manifold-renderer's effect
    // implementations, which manifold-core's own test binary doesn't link.
    inventory::submit! {
        crate::effect_registration::EffectMetadata {
            id: PresetTypeId::new("TestCreateDefaultUntouched"),
            display_name: "Test Create Default Untouched",
            category: "Test",
            available: true,
            osc_prefix: "testCreateDefaultUntouched",
            legacy_discriminant: None,
            params: &[crate::generator_registration::ParamSpec::continuous(
                "amount", "Amount", 0.0, 1.0, 0.42, "F2", "",
            )],
        }
    }

    #[test]
    fn create_default_does_not_mark_params_touched() {
        // The exact bug this phase's call-site audit found: `create_default`
        // used to seed via the public `set_base_param`, which would have
        // marked every freshly-created effect's params `touched` before any
        // lane or hand ever existed — pre-latching any lane authored on them
        // later.
        let inst = crate::preset_definition_registry::create_default(&PresetTypeId::new(
            "TestCreateDefaultUntouched",
        ));
        assert!(
            !inst.params.get("amount").unwrap().touched,
            "create_default must not mark freshly-seeded params touched"
        );
        assert_eq!(inst.params.get("amount").unwrap().base, 0.42);
    }

    // `bundled_slider_delete_does_not_misroute_survivor_drivers` (and its
    // `TestBundledSliderMisroute` fixture registration) was DELETED
    // (PARAM_STORAGE_DESIGN.md D3): it existed to prove a fix for a bug
    // that only the OLD dual-resolution scheme could have — a live
    // per-instance `meta.params` position (`param_id_to_value_index`)
    // disagreeing with a frozen-registry position (`resolve_param_in`)
    // after a bundled slider was deleted mid-array. Both mechanisms are
    // gone; every param is now addressed by stable id everywhere (card
    // display, pruning, and runtime modulation resolution alike), so
    // there is no positional index to disagree in the first place.

    // ── §9 U1: unified trigger-gate mods ─────────────────────────────────

    /// A bundled `is_trigger_gate` param — mirrors [`slot`] but flips the
    /// gate flag, the same way a `clip_trigger` toggle card ships on the 11
    /// trigger-responsive generator presets.
    fn gate_slot(id: &str) -> crate::params::Param {
        let mut p = slot(id, 0.0, true);
        p.spec.is_toggle = true;
        p.spec.is_trigger_gate = true;
        p
    }

    #[test]
    fn clip_edge_enabled_matrix() {
        use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
        use crate::audio_trigger::TriggerFireMode;
        use crate::id::AudioSendId;

        let mut inst = PresetInstance::new(PresetTypeId::new("TestGate"));
        inst.params.push(gate_slot("clip_trigger"));

        // No mod at all → clip edge unconditionally on (pre-§8 behavior).
        assert!(inst.clip_edge_enabled());

        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-1"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.trigger_mode = Some(TriggerFireMode::Transient);
        m.enabled = false;
        inst.audio_mods_mut().push(m);

        // Disabled mod → disabled-means-absent, clip edge stays on.
        assert!(inst.clip_edge_enabled(), "disabled mod must be inert");

        inst.audio_mods.as_mut().unwrap()[0].enabled = true;
        assert!(!inst.clip_edge_enabled(), "armed Transient mode gates the clip edge");

        inst.audio_mods.as_mut().unwrap()[0].trigger_mode = Some(TriggerFireMode::ClipEdge);
        assert!(inst.clip_edge_enabled());

        inst.audio_mods.as_mut().unwrap()[0].trigger_mode = Some(TriggerFireMode::Both);
        assert!(inst.clip_edge_enabled());
    }

    #[test]
    fn legacy_audio_trigger_migrates_onto_a_parameter_audio_mod_on_the_gate_param() {
        // The exact `audioTrigger` shape a project saved during the one day
        // §8's `AudioTriggerMod` shipped (see
        // `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md` §9 U5). A generator-kind
        // instance's `graph.presetMetadata.params` is the only route to an
        // `is_trigger_gate` param outside the JSON preset path (the
        // compile-time `ParamSpec` inventory format has no field for it —
        // see `generator_registration::ParamSpec::to_param_def`), so the
        // fixture carries its own minimal per-instance graph.
        let json = r#"{
            "generatorType": "TestGenTrig",
            "graph": {
                "version": 2,
                "presetMetadata": {
                    "id": "TestGenTrig",
                    "displayName": "Test Gen Trig",
                    "category": "Test",
                    "oscPrefix": "testGenTrig",
                    "params": [
                        {
                            "id": "clip_trigger",
                            "name": "Clip Trigger",
                            "min": 0.0,
                            "max": 1.0,
                            "defaultValue": 0.0,
                            "isToggle": true,
                            "isTriggerGate": true
                        }
                    ],
                    "bindings": []
                },
                "nodes": [],
                "wires": []
            },
            "audioTrigger": {
                "enabled": false,
                "source": {
                    "sendId": "e14b42f8",
                    "feature": { "kind": "transients", "band": "full" }
                },
                "sensitivity": 1.0,
                "mode": "transient"
            }
        }"#;

        let mut de = serde_json::Deserializer::from_str(json);
        let inst = deserialize_generator_instance(&mut de).unwrap();

        assert_eq!(inst.kind, crate::preset_def::PresetKind::Generator);
        let mods = inst
            .audio_mods
            .as_ref()
            .expect("legacy audioTrigger must migrate onto audio_mods");
        assert_eq!(mods.len(), 1);
        let m = &mods[0];
        assert_eq!(m.param_id.as_ref(), "clip_trigger", "targets the gate param");
        assert!(!m.enabled, "legacy enabled=false carries over");
        assert_eq!(m.source.send_id, crate::id::AudioSendId::new("e14b42f8"));
        assert_eq!(
            m.source.feature,
            crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Transients,
                crate::audio_mod::AudioBand::Full
            )
        );
        assert_eq!(
            m.trigger_mode,
            Some(crate::audio_trigger::TriggerFireMode::Transient)
        );
        assert_eq!(m.shape.sensitivity, 1.0, "sensitivity approximates onto Amount (U5)");
    }

    #[test]
    fn legacy_audio_trigger_with_no_gate_param_is_dropped_not_guessed() {
        // No `isTriggerGate` param anywhere on the instance → the migration
        // has no target to attach to and must drop the config rather than
        // guess one (a hand-edited file, or an instance saved before the
        // flag existed).
        let json = r#"{
            "generatorType": "TestGenTrigNoGate",
            "audioTrigger": {
                "enabled": true,
                "source": {
                    "sendId": "send-1",
                    "feature": { "kind": "transients", "band": "full" }
                },
                "sensitivity": 0.5,
                "mode": "both"
            }
        }"#;

        let mut de = serde_json::Deserializer::from_str(json);
        let inst = deserialize_generator_instance(&mut de).unwrap();
        assert!(inst.audio_mods.is_none(), "no gate param means nothing to migrate onto");
    }
}
