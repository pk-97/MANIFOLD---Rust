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
pub type ParamId = Cow<'static, str>;

// ─── Param Definition ───

/// Metadata for a single parameter slot.
/// Port of Unity ParamDef.cs.
///
/// `id` is the **stable mapping key** referenced by every external
/// addressing site (OSC, Ableton, modulation drivers, project file
/// storage). Once shipped, `id` is forever — renaming an `id` is a
/// breaking change for every saved project.
///
/// `name` is the display label on the slider. Free to edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamDef {
    /// Stable mapping key. `snake_case` convention. Empty for legacy
    /// `ParamDef` instances loaded from V1.0.0 project files; the
    /// post-load alignment pass fills it in from the registry.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub min: f32,
    pub max: f32,
    #[serde(default)]
    pub default_value: f32,
    #[serde(default)]
    pub whole_numbers: bool,
    #[serde(default)]
    pub is_toggle: bool,
    #[serde(default)]
    pub is_trigger: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc_suffix: Option<String>,
    /// Slider response curve applied to the normalized position before
    /// scaling back to `[min, max]`. Part of the preset-authored slider
    /// surface (Phase 2 of `docs/PRESET_INSTANCE_COLLAPSE_PLAN.md`): a
    /// preset can ship a non-Linear knob feel. Defaults to `Linear` and is
    /// skipped on serialize when Linear so existing presets stay
    /// byte-identical.
    #[serde(default, skip_serializing_if = "curve_is_linear")]
    pub curve: crate::macro_bank::MacroCurve,
    /// Slider invert: card-left drives the param max. Defaults to `false`
    /// and is skipped on serialize when false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert: bool,
}

/// serde `skip_serializing_if` for [`ParamDef::curve`] / [`ParamSpecDef::curve`].
pub(crate) fn curve_is_linear(c: &crate::macro_bank::MacroCurve) -> bool {
    matches!(c, crate::macro_bank::MacroCurve::Linear)
}

/// serde `skip_serializing_if` for a defaulted `false` bool field.
pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for ParamDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: None,
            format_string: None,
            osc_suffix: None,
            curve: crate::macro_bank::MacroCurve::Linear,
            invert: false,
        }
    }
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
    fn get_param_def(&self, index: usize) -> ParamDef;
    fn get_param(&self, index: usize) -> f32;
    fn set_param(&mut self, index: usize, value: f32);
    fn get_base_param(&self, index: usize) -> f32;
    fn set_base_param(&mut self, index: usize, value: f32);
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

/// Free-function form of [`PresetInstance::resolve_param`]. Takes the
/// `PresetDef` and the effect instance directly so callers in
/// borrow-tight closures (the modulation evaluators iterating
/// `fx.drivers`) can resolve without going through the borrowing
/// `&self` method.
///
/// The user tail is read from the instance's `graph.preset_metadata`
/// (`user_added` bindings) — the single binding-storage list after the
/// preset-unification step-3 fold-in. Allocation-free: it scans the
/// graph's binding iterator rather than materializing a Vec, so it stays
/// safe on the per-frame modulation path.
pub fn resolve_param_in(
    def: &crate::preset_def::PresetDef,
    fx: &PresetInstance,
    id: &str,
) -> Option<ResolvedParam> {
    if let Some(&idx) = def.id_to_index.get(id) {
        let pd = &def.param_defs[idx];
        return Some(ResolvedParam {
            idx,
            min: pd.min,
            max: pd.max,
            whole_numbers: pd.whole_numbers || pd.value_labels.is_some(),
        });
    }
    let (j, b) = fx.user_added_bindings().enumerate().find(|(_, b)| b.id == id)?;
    // Range comes from the binding's declared `ParamSpecDef` range (the preset
    // is the single source now — the per-instance reshape note is gone), else 0..1.
    let (min, max) = fx
        .graph
        .as_ref()
        .and_then(|g| g.preset_metadata.as_ref())
        .and_then(|m| m.params.iter().find(|p| p.id == id))
        .map(|s| (s.min, s.max))
        .unwrap_or((0.0, 1.0));
    Some(ResolvedParam {
        idx: def.param_count + j,
        min,
        max,
        whole_numbers: matches!(
            b.convert,
            ParamConvert::IntRound
                | ParamConvert::EnumRound
                | ParamConvert::BoolThreshold
                | ParamConvert::Trigger
        ),
    })
}

/// Result of [`PresetInstance::resolve_param`]: slot index plus the
/// metadata modulation evaluators need to map a normalized 0–1 driver
/// or envelope output onto the target parameter's value range.
///
/// Lives at this layer (not in `manifold-playback`) because the
/// resolution itself is pure data-model logic — it knows about static
/// vs user-tail addressing and is unrelated to playback timing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedParam {
    /// Slot in `PresetInstance.param_values` to read/write.
    pub idx: usize,
    pub min: f32,
    pub max: f32,
    /// True when the parameter is integral (registry `whole_numbers`
    /// or `value_labels` set, or user binding declares an integral
    /// conversion). Modulation evaluators round the final value when
    /// this is set.
    pub whole_numbers: bool,
}

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

// ─── Param Value (per-slot state) ───

/// A single parameter slot's runtime state on an [`PresetInstance`].
///
/// Wraps the effective (post-modulation) `value` together with the
/// `exposed` flag that gates whether this slot surfaces as a slider
/// on the effect card. `exposed` defaults to `true` — the historical
/// behavior where every static slot was always-visible. Toggling
/// `exposed` to `false` hides the slider but preserves the underlying
/// value (and any drivers/Ableton mappings addressing the slot —
/// they continue to drive the value, just without a visible slider).
///
/// Wire format:
/// - On serialize: always `{ "value": f, "exposed": b }` for clarity.
/// - On deserialize: accepts either a bare `f32` (V1.x / V1.2 wire
///   format, exposed defaults to true) or `{ value, exposed? }` object
///   (V1.3+). Polymorphic deserialization keeps loaders simple.
///
/// Type-enforced single struct per slot eliminates the parallel-array
/// footgun of separate `Vec<f32>` + `Vec<bool>` collections.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamSlot {
    /// Effective (post-modulation) value — what the renderer reads.
    pub value: f32,
    /// User-intended base (pre-modulation) value. Modulation reads `base`,
    /// computes the effective, and writes it to `value`; `reset_param_effectives`
    /// copies `base` back into `value` each frame before re-applying modulation.
    /// Folded in from the former parallel `base_param_values: Option<Vec<f32>>`
    /// (fork #16) — riding the slot eliminates the length-sync footgun. Whether
    /// base is *tracked* (emitted to the wire) is the per-instance
    /// `PresetInstance.base_tracked` bit; until tracked, `get_base_param` falls
    /// through to `value`, so a stale `base` here is never observed.
    pub base: f32,
    pub exposed: bool,
}

impl Default for ParamSlot {
    fn default() -> Self {
        Self {
            value: 0.0,
            base: 0.0,
            exposed: true,
        }
    }
}

impl ParamSlot {
    /// Convenience constructor for an exposed slot with the given value
    /// (base seeded to the same value).
    #[inline]
    pub const fn exposed(value: f32) -> Self {
        Self {
            value,
            base: value,
            exposed: true,
        }
    }
}

/// Everything removed when an exposed card param is pruned from an instance:
/// its `ParamSpecDef`, its `BindingDef`, its `param_values` slot, and any
/// drivers / Ableton mappings / envelopes that referenced its id — plus the
/// positions each occupied. Returned by
/// [`PresetInstance::remove_exposures_for_node`] and handed back to
/// [`PresetInstance::restore_exposures`] so an undo restores the pre-delete
/// state byte-for-byte. Opaque to callers (the command stack just carries it).
#[derive(Debug, Clone)]
pub struct RemovedExposure {
    /// Index in `preset_metadata.params` (where the spec lives). `None` for a
    /// binding with no matching param spec (composite/fan-out).
    meta_param_index: Option<usize>,
    /// Index in `param_values` (where the value slot lives). Resolved tier-aware
    /// via [`PresetInstance::param_id_to_value_index`], so it's correct whether
    /// the param is a static-prefix slot or a user-tail one — which is NOT
    /// generally the same as `meta_param_index`. `None` if the param has no slot.
    value_index: Option<usize>,
    /// Index in `preset_metadata.bindings`.
    binding_index: usize,
    spec: Option<crate::effect_graph_def::ParamSpecDef>,
    binding: crate::effect_graph_def::BindingDef,
    slot: Option<ParamSlot>,
    drivers: Vec<ParameterDriver>,
    ableton_mappings: Vec<crate::ableton_mapping::AbletonParamMapping>,
    envelopes: Vec<ParamEnvelope>,
    audio_mods: Vec<crate::audio_mod::ParameterAudioMod>,
}

impl Serialize for ParamSlot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ParamSlot", 2)?;
        s.serialize_field("value", &self.value)?;
        s.serialize_field("exposed", &self.exposed)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for ParamSlot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ParamValueVisitor;

        impl<'de> serde::de::Visitor<'de> for ParamValueVisitor {
            type Value = ParamSlot;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a number (legacy bare f32) or an object {value, exposed?}")
            }

            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<ParamSlot, E> {
                // base seeded to value; the separate baseParamValues array (when
                // present) overwrites base after the slots are built.
                Ok(ParamSlot::exposed(v as f32))
            }

            fn visit_f32<E: serde::de::Error>(self, v: f32) -> Result<ParamSlot, E> {
                Ok(ParamSlot::exposed(v))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<ParamSlot, E> {
                Ok(ParamSlot::exposed(v as f32))
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<ParamSlot, E> {
                Ok(ParamSlot::exposed(v as f32))
            }

            fn visit_map<M>(self, mut map: M) -> Result<ParamSlot, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut value: Option<f32> = None;
                let mut exposed: Option<bool> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "value" => value = Some(map.next_value()?),
                        "exposed" => exposed = Some(map.next_value()?),
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                let v = value.unwrap_or(0.0);
                Ok(ParamSlot {
                    value: v,
                    base: v,
                    exposed: exposed.unwrap_or(true),
                })
            }
        }

        deserializer.deserialize_any(ParamValueVisitor)
    }
}

// ─── Effect Instance ───

/// A single effect applied to a clip, layer, or master chain.
///
/// Serialization (custom impls below):
///
/// - `paramValues` accepts V1.0/1.1 positional `Array<f32>`,
///   V1.2 keyed `Map<id, f32>`, V1.3 positional `Array<{value, exposed}>`,
///   and V1.3 keyed `Map<id, {value, exposed}>`. The polymorphic
///   `ParamSlot` deserializer handles the bare-vs-object distinction
///   per-element. On save, V1.3+ canonical Map form is emitted when
///   the effect's registry def is available; otherwise positional Array.
/// - `baseParamValues` stays a `Vec<f32>` *on the wire* (modulation tracking
///   only — exposure isn't meaningful for the pre-modulation snapshot). In
///   memory it's folded into `ParamSlot.base` (fork #16); the wire round-trips
///   byte-identically, emitted iff `base_tracked`.
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
    /// Positional parameter storage. The first
    /// `crate::preset_definition_registry::get(&effect_type).param_count`
    /// slots correspond to the effect's static-spec bindings in
    /// declaration order; the remaining slots correspond to
    /// [`Self::user_param_bindings`] in declaration order. After the
    /// bindings unification (Phases 1–4 of
    /// `docs/archive/BINDINGS_UNIFICATION_PLAN.md`) this layout maps directly
    /// onto the renderer's `EffectSlot.bindings[i]` — no parallel
    /// structure to keep in sync. Resolve `ParamId → index` via
    /// [`Self::param_id_to_value_index`]; that helper is the single
    /// tier-aware lookup the rest of the codebase relies on.
    pub param_values: Vec<ParamSlot>,
    /// Whether the pre-modulation base (now `ParamSlot.base`) is *tracked* —
    /// the single presence bit that replaces the former
    /// `base_param_values: Option<Vec<f32>>` (fork #16). Set on load when the
    /// JSON carried `baseParamValues`, and by any base write
    /// (`set_base_param` / `reset_param_effectives` / `init_defaults_for_type`).
    /// While `false`, `get_base_param` falls through to the effective value, so
    /// per-slot `base` is allowed to be stale; the field is also the
    /// serialize-time gate that keeps `baseParamValues` byte-identical (emitted
    /// iff tracked). Not serialized; derived on load.
    pub base_tracked: bool,
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
}

// ─── Wire-format helpers for paramValues ───

/// Wire-format shape for `PresetInstance.paramValues`. Accepts
/// V1.0/1.1 positional `Array<f32>`, V1.2 keyed `Map<id, f32>`,
/// V1.3 positional `Array<{value, exposed}>`, or V1.3 keyed
/// `Map<id, {value, exposed}>` — the polymorphic [`ParamSlot`]
/// deserializer normalizes per-element across versions.
///
/// Used only by `PresetInstance`. `PresetInstance` and
/// `PresetInstance.baseParamValues` use [`FloatValuesWire`] which
/// stays plain `Vec<f32>` (exposure is meaningless there).
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum ParamValuesWire {
    Positional(Vec<ParamSlot>),
    Keyed(std::collections::BTreeMap<String, ParamSlot>),
}

impl ParamValuesWire {
    /// Convert to the in-memory positional `Vec<ParamSlot>` form
    /// using the effect registry, with the effect instance's
    /// per-instance user bindings tail.
    ///
    /// - `Positional`: passed through unchanged. (Length is assumed
    ///   to already be `def.param_count + user_bindings.len()` if the
    ///   producer was V1.2+ aware. Not validated here — `align_to_definition`
    ///   fixes mismatches.)
    /// - `Keyed`: looked up against the effect registry first, then
    ///   against `user_bindings` for unknown ids. Each known
    ///   `param_id` lands at its slot; unknown keys are dropped
    ///   (orphan, same policy as drivers/envelopes/Ableton). Missing
    ///   slots default to the registry's `default_value` (static
    ///   prefix) or the user binding's `default_value` (user tail),
    ///   both with `exposed: true`.
    ///
    /// If the registry lacks a def for this effect (e.g., test code
    /// without `manifold-renderer` linked, or a forward-incompatible
    /// type from a future version), returns `Vec::new()` for the
    /// keyed case AND emits an `eprintln!` warning. In production this
    /// branch is unreachable — the renderer always registers all
    /// shipping effects — so a warning here means observability has
    /// caught a real bug.
    fn into_positional(
        self,
        effect_type: &PresetTypeId,
        user_binding_ids: &[&str],
        user_defaults: &[f32],
    ) -> Vec<ParamSlot> {
        match self {
            ParamValuesWire::Positional(v) => v,
            ParamValuesWire::Keyed(map) => {
                let Some(def) = crate::preset_definition_registry::try_get(effect_type) else {
                    eprintln!(
                        "[manifold-core] WARNING: dropping {} V1.2+ paramValues for unregistered \
                         effect type '{}' (Map keys: {:?}). align_to_definition will fill with \
                         registry defaults if the type is registered later, but the saved values \
                         are lost. In production this should never fire — the renderer registers \
                         every shipping effect at startup.",
                        map.len(),
                        effect_type.as_str(),
                        map.keys().collect::<Vec<_>>(),
                    );
                    return Vec::new();
                };
                let n_static = def.param_count;
                let n_user = user_binding_ids.len();
                let total = n_static + n_user;
                let mut out = vec![ParamSlot::default(); total];
                for (i, pd) in def.param_defs.iter().enumerate().take(n_static) {
                    out[i] = ParamSlot::exposed(pd.default_value);
                }
                for (j, &dv) in user_defaults.iter().enumerate() {
                    out[n_static + j] = ParamSlot::exposed(dv);
                }
                for (id, pv) in map {
                    // Direct hit via the current id_to_index table (static).
                    if let Some(&idx) = def.id_to_index.get(&id)
                        && idx < out.len()
                    {
                        out[idx] = pv;
                        continue;
                    }
                    // Static alias chain — old id (renamed) resolves to
                    // a current id; dropped ids fall through.
                    if let Some(resolved) = crate::effect_registration::resolve_param_alias(
                        def.legacy_param_aliases,
                        &id,
                    ) && let Some(&idx) = def.id_to_index.get(resolved)
                        && idx < out.len()
                    {
                        out[idx] = pv;
                        continue;
                    }
                    // Per-instance user-added binding lookup (graph tail).
                    if let Some(j) = user_binding_ids.iter().position(|bid| *bid == id) {
                        out[n_static + j] = pv;
                    }
                }
                out
            }
        }
    }

    /// Generator-registry counterpart to [`Self::into_positional`].
    /// Produces `Vec<ParamSlot>` for `PresetInstance.paramValues`.
    /// No user-binding tail parameter: generator user-added bindings
    /// live in the graph's `preset_metadata` and, when present, push
    /// `param_values.len()` past the registry count so the producer
    /// emits the positional `Array` form, which round-trips through the
    /// `Positional` arm here unchanged.
    pub(crate) fn into_positional_for_generator(
        self,
        gen_type: &crate::PresetTypeId,
    ) -> Vec<ParamSlot> {
        match self {
            ParamValuesWire::Positional(v) => v,
            ParamValuesWire::Keyed(map) => {
                let Some(def) = crate::preset_definition_registry::try_get(gen_type) else {
                    eprintln!(
                        "[manifold-core] WARNING: dropping {} V1.2+ paramValues for unregistered \
                         generator type '{}' (Map keys: {:?}). In production this should never \
                         fire — the renderer registers every shipping generator at startup.",
                        map.len(),
                        gen_type.as_str(),
                        map.keys().collect::<Vec<_>>(),
                    );
                    return Vec::new();
                };
                let mut out = vec![ParamSlot::default(); def.param_count];
                for (i, pd) in def.param_defs.iter().enumerate().take(def.param_count) {
                    out[i] = ParamSlot::exposed(pd.default_value);
                }
                for (id, pv) in map {
                    if let Some(&idx) = def.id_to_index.get(&id)
                        && idx < out.len()
                    {
                        out[idx] = pv;
                        continue;
                    }
                    if let Some(resolved) = crate::effect_registration::resolve_param_alias(
                        def.legacy_param_aliases,
                        &id,
                    ) && let Some(&idx) = def.id_to_index.get(resolved)
                        && idx < out.len()
                    {
                        out[idx] = pv;
                    }
                }
                out
            }
        }
    }
}

/// Wire-format shape for plain-float param vectors:
/// `PresetInstance.baseParamValues` and `PresetInstance.paramValues`.
/// Exposure isn't meaningful on these surfaces — base values are a
/// pre-modulation snapshot, generators don't (yet) participate in the
/// host-visible exposure surface.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum FloatValuesWire {
    Positional(Vec<f32>),
    Keyed(std::collections::BTreeMap<String, f32>),
}

impl FloatValuesWire {
    /// Effect-side `baseParamValues` conversion. Same lookup semantics
    /// as the rich [`ParamValuesWire::into_positional`] but emits plain
    /// `Vec<f32>`.
    fn into_positional_base(
        self,
        effect_type: &PresetTypeId,
        user_binding_ids: &[&str],
        user_defaults: &[f32],
    ) -> Vec<f32> {
        match self {
            FloatValuesWire::Positional(v) => v,
            FloatValuesWire::Keyed(map) => {
                let Some(def) = crate::preset_definition_registry::try_get(effect_type) else {
                    eprintln!(
                        "[manifold-core] WARNING: dropping {} V1.2+ baseParamValues for \
                         unregistered effect type '{}' (Map keys: {:?}).",
                        map.len(),
                        effect_type.as_str(),
                        map.keys().collect::<Vec<_>>(),
                    );
                    return Vec::new();
                };
                let n_static = def.param_count;
                let n_user = user_binding_ids.len();
                let total = n_static + n_user;
                let mut out = vec![0.0_f32; total];
                for (i, pd) in def.param_defs.iter().enumerate().take(n_static) {
                    out[i] = pd.default_value;
                }
                for (j, &dv) in user_defaults.iter().enumerate() {
                    out[n_static + j] = dv;
                }
                for (id, value) in map {
                    if let Some(&idx) = def.id_to_index.get(&id)
                        && idx < out.len()
                    {
                        out[idx] = value;
                        continue;
                    }
                    if let Some(resolved) = crate::effect_registration::resolve_param_alias(
                        def.legacy_param_aliases,
                        &id,
                    ) && let Some(&idx) = def.id_to_index.get(resolved)
                        && idx < out.len()
                    {
                        out[idx] = value;
                        continue;
                    }
                    if let Some(j) = user_binding_ids.iter().position(|bid| *bid == id) {
                        out[n_static + j] = value;
                    }
                }
                out
            }
        }
    }

    /// Generator-registry counterpart for `PresetInstance.paramValues`.
    pub(crate) fn into_positional_for_generator(
        self,
        gen_type: &crate::PresetTypeId,
    ) -> Vec<f32> {
        match self {
            FloatValuesWire::Positional(v) => v,
            FloatValuesWire::Keyed(map) => {
                let Some(def) = crate::preset_definition_registry::try_get(gen_type) else {
                    eprintln!(
                        "[manifold-core] WARNING: dropping {} V1.2+ paramValues for unregistered \
                         generator type '{}' (Map keys: {:?}). In production this should never \
                         fire — the renderer registers every shipping generator at startup.",
                        map.len(),
                        gen_type.as_str(),
                        map.keys().collect::<Vec<_>>(),
                    );
                    return Vec::new();
                };
                let mut out = vec![0.0_f32; def.param_count];
                for (i, pd) in def.param_defs.iter().enumerate().take(def.param_count) {
                    out[i] = pd.default_value;
                }
                for (id, value) in map {
                    if let Some(&idx) = def.id_to_index.get(&id)
                        && idx < out.len()
                    {
                        out[idx] = value;
                        continue;
                    }
                    if let Some(resolved) = crate::effect_registration::resolve_param_alias(
                        def.legacy_param_aliases,
                        &id,
                    ) && let Some(&idx) = def.id_to_index.get(resolved)
                        && idx < out.len()
                    {
                        out[idx] = value;
                    }
                }
                out
            }
        }
    }
}

/// Generator-registry counterpart to `serialize_param_values`.
/// Routes through the generator registry's `param_ids` slice.
pub(crate) fn serialize_param_values_for_generator<S>(
    values: &[f32],
    gen_type: &crate::PresetTypeId,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::{SerializeMap, SerializeSeq};

    let def = crate::preset_definition_registry::try_get(gen_type);
    let can_emit_map = def.as_ref().is_some_and(|d| {
        values.len() <= d.param_ids.len()
            && d.param_ids
                .iter()
                .take(values.len())
                .all(|id| !id.is_empty())
    });

    if can_emit_map {
        let def = def.expect("checked above");
        let mut map = serializer.serialize_map(Some(values.len()))?;
        for (i, &v) in values.iter().enumerate() {
            map.serialize_entry(&def.param_ids[i], &v)?;
        }
        map.end()
    } else {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for &v in values {
            seq.serialize_element(&v)?;
        }
        seq.end()
    }
}

/// `Vec<ParamSlot>` counterpart to [`serialize_param_values_for_generator`]
/// — emits each generator param slot as a `{value, exposed}` object keyed
/// by the generator registry's stable param id. Mirrors the effect-side
/// [`serialize_param_values`] but against the generator registry and with
/// no user-binding-tail parameter (generator user-added bindings live in
/// the graph's `preset_metadata` and ride the positional-array fallback
/// when `values.len()` exceeds the registry's param count).
pub(crate) fn serialize_param_values_for_generator_slots<S>(
    values: &[ParamSlot],
    gen_type: &crate::PresetTypeId,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::{SerializeMap, SerializeSeq};

    let def = crate::preset_definition_registry::try_get(gen_type);
    let can_emit_map = def.as_ref().is_some_and(|d| {
        values.len() <= d.param_ids.len()
            && d.param_ids
                .iter()
                .take(values.len())
                .all(|id| !id.is_empty())
    });

    if can_emit_map {
        let def = def.expect("checked above");
        let mut map = serializer.serialize_map(Some(values.len()))?;
        for (i, pv) in values.iter().enumerate() {
            map.serialize_entry(&def.param_ids[i], pv)?;
        }
        map.end()
    } else {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for pv in values {
            seq.serialize_element(pv)?;
        }
        seq.end()
    }
}

/// Serialize a positional `Vec<ParamSlot>` as the V1.3 `Object`
/// keyed by `param_id`, looking up ids via the effect registry. The
/// tail past `def.param_count` is keyed by per-instance
/// `user_bindings[j].id`. Each emitted entry is a `{value, exposed}`
/// object via [`ParamSlot`]'s `Serialize` impl.
///
/// Falls back to the positional `Array` form when the registry is
/// missing (test contexts) or any *static* `param_id` is empty.
fn serialize_param_values<S>(
    values: &[ParamSlot],
    effect_type: &PresetTypeId,
    user_binding_ids: &[&str],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::{SerializeMap, SerializeSeq};

    let def = crate::preset_definition_registry::try_get(effect_type);
    let static_count = def.as_ref().map(|d| d.param_count).unwrap_or(0);
    let static_touch = values.len().min(static_count);
    let can_emit_map = def.as_ref().is_some_and(|d| {
        d.param_ids
            .iter()
            .take(static_touch)
            .all(|id| !id.is_empty())
    });

    if can_emit_map {
        let def = def.expect("checked above");
        let mut map = serializer.serialize_map(Some(values.len()))?;
        for (i, pv) in values.iter().take(static_touch).enumerate() {
            map.serialize_entry(&def.param_ids[i], pv)?;
        }
        for (j, id) in user_binding_ids.iter().enumerate() {
            let idx = static_count + j;
            if let Some(pv) = values.get(idx) {
                map.serialize_entry(id, pv)?;
            }
        }
        map.end()
    } else {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for pv in values {
            seq.serialize_element(pv)?;
        }
        seq.end()
    }
}

/// Serialize a positional `Vec<f32>` for `baseParamValues` — same
/// addressing rules as [`serialize_param_values`] but emits raw
/// floats (exposure isn't meaningful on base values).
fn serialize_base_param_values<S>(
    values: &[f32],
    effect_type: &PresetTypeId,
    user_binding_ids: &[&str],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::{SerializeMap, SerializeSeq};

    let def = crate::preset_definition_registry::try_get(effect_type);
    let static_count = def.as_ref().map(|d| d.param_count).unwrap_or(0);
    let static_touch = values.len().min(static_count);
    let can_emit_map = def.as_ref().is_some_and(|d| {
        d.param_ids
            .iter()
            .take(static_touch)
            .all(|id| !id.is_empty())
    });

    if can_emit_map {
        let def = def.expect("checked above");
        let mut map = serializer.serialize_map(Some(values.len()))?;
        for (i, &v) in values.iter().take(static_touch).enumerate() {
            map.serialize_entry(&def.param_ids[i], &v)?;
        }
        for (j, id) in user_binding_ids.iter().enumerate() {
            let idx = static_count + j;
            if let Some(&v) = values.get(idx) {
                map.serialize_entry(id, &v)?;
            }
        }
        map.end()
    } else {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for &v in values {
            seq.serialize_element(&v)?;
        }
        seq.end()
    }
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

        // `param_values` always emits; `baseParamValues` is emitted iff base is
        // tracked (the former `base_param_values.is_some()` gate). Other optional
        // fields use the same `skip_if_none` policy as the previous
        // derive(Serialize) impl.
        let mut field_count = 5; // id, effectType, enabled, collapsed, paramValues
        if self.base_tracked {
            field_count += 1;
        }
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

        // The user-tail of `param_values` is keyed (on the wire) by each
        // user-added binding's stable id. After the storage unification
        // those ids live in `graph.preset_metadata.bindings` (user_added),
        // so collect them here for the value serializers. The bindings
        // themselves ride out inside the `graph` field — there is no
        // longer a separate `userParamBindings` array.
        let user_binding_ids: Vec<&str> = self.user_added_bindings().map(|b| b.id.as_str()).collect();

        let mut s = serializer.serialize_struct("PresetInstance", field_count)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("effectType", &self.effect_type)?;
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("collapsed", &self.collapsed)?;
        s.serialize_field(
            "paramValues",
            &ParamValuesSer {
                values: &self.param_values,
                effect_type: &self.effect_type,
                user_binding_ids: &user_binding_ids,
            },
        )?;
        if self.base_tracked {
            // Reconstruct the wire's flat base array from each slot's `base`.
            let base: Vec<f32> = self.param_values.iter().map(|s| s.base).collect();
            s.serialize_field(
                "baseParamValues",
                &BaseParamValuesSer {
                    values: &base,
                    effect_type: &self.effect_type,
                    user_binding_ids: &user_binding_ids,
                },
            )?;
        }
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
        if let Some(g) = &self.group_id {
            s.serialize_field("groupId", g)?;
        }
        // `graph` is skipped when None — same round-trip-invariance
        // policy. `None` means "use the catalog default for this
        // effect type"; only per-instance overrides emit.
        if let Some(graph) = &self.graph {
            s.serialize_field("graph", graph)?;
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

        let mut field_count = 2; // generatorType + paramValues
        if self.base_tracked {
            field_count += 1;
        }
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
        if self.graph.is_some() {
            field_count += 1;
        }
        if self.legacy_param_version.is_some() {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("PresetInstance", field_count)?;
        s.serialize_field("generatorType", &self.effect_type)?;
        s.serialize_field(
            "paramValues",
            &GenParamSlotValuesSer {
                values: &self.param_values,
                gen_type: &self.effect_type,
            },
        )?;
        if self.base_tracked {
            let base: Vec<f32> = self.param_values.iter().map(|s| s.base).collect();
            s.serialize_field(
                "baseParamValues",
                &GenParamValuesSer {
                    values: &base,
                    gen_type: &self.effect_type,
                },
            )?;
        }
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
        if let Some(g) = &self.graph {
            s.serialize_field("graph", g)?;
        }
        if let Some(v) = self.legacy_param_version {
            s.serialize_field("genParamVersion", &v)?;
        }
        s.end()
    }
}

/// Serialize-side wrapper for generator `baseParamValues` (plain `f32`).
struct GenParamValuesSer<'a> {
    values: &'a [f32],
    gen_type: &'a PresetTypeId,
}

impl Serialize for GenParamValuesSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_param_values_for_generator(self.values, self.gen_type, serializer)
    }
}

/// Serialize-side wrapper for generator `paramValues` (`ParamSlot`).
struct GenParamSlotValuesSer<'a> {
    values: &'a [ParamSlot],
    gen_type: &'a PresetTypeId,
}

impl Serialize for GenParamSlotValuesSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_param_values_for_generator_slots(self.values, self.gen_type, serializer)
    }
}

/// Serialize-side wrapper for `paramValues` that carries the parent's
/// `effect_type` and per-instance user bindings so the field-level
/// `Serialize` can route to `serialize_param_values`.
struct ParamValuesSer<'a> {
    values: &'a [ParamSlot],
    effect_type: &'a PresetTypeId,
    user_binding_ids: &'a [&'a str],
}

impl Serialize for ParamValuesSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_param_values(
            self.values,
            self.effect_type,
            self.user_binding_ids,
            serializer,
        )
    }
}

/// Serialize-side wrapper for `baseParamValues` (plain `Vec<f32>`).
struct BaseParamValuesSer<'a> {
    values: &'a [f32],
    effect_type: &'a PresetTypeId,
    user_binding_ids: &'a [&'a str],
}

impl Serialize for BaseParamValuesSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_base_param_values(
            self.values,
            self.effect_type,
            self.user_binding_ids,
            serializer,
        )
    }
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
            param_values: Option<ParamValuesWire>,
            #[serde(default)]
            base_param_values: Option<FloatValuesWire>,
            #[serde(default)]
            drivers: Option<Vec<ParameterDriver>>,
            #[serde(default)]
            envelopes: Option<Vec<ParamEnvelope>>,
            #[serde(default)]
            ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
            #[serde(default)]
            audio_mods: Option<Vec<crate::audio_mod::ParameterAudioMod>>,
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
        }

        let raw = Raw::deserialize(deserializer)?;
        // User-added bindings are the single storage list — they live in
        // `graph.preset_metadata.bindings` (`user_added`). The legacy
        // `userParamBindings` array is folded into the graph by the
        // v1.3→v1.4 load migration before this runs, so by here the only
        // home for the user tail is the graph. Extract its ids + defaults
        // (declaration order) to drive the keyed-map → positional fold.
        let user_binding_ids: Vec<&str> = raw
            .graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .map(|m| {
                m.bindings
                    .iter()
                    .filter(|b| b.user_added)
                    .map(|b| b.id.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let user_defaults: Vec<f32> = raw
            .graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .map(|m| {
                m.bindings
                    .iter()
                    .filter(|b| b.user_added)
                    .map(|b| b.default_value)
                    .collect()
            })
            .unwrap_or_default();
        // The Map → positional fold runs before `align_to_definition`, so
        // registry-known but user-tail-empty cases land at zero and align
        // fills user-binding defaults.
        let mut param_values = raw
            .param_values
            .map(|w| w.into_positional(&raw.effect_type, &user_binding_ids, &user_defaults))
            .unwrap_or_default();
        // Fold the wire's separate `baseParamValues` into each slot's `base`
        // (fork #16). Present → tracked; absent → base stays seeded to value.
        let base_tracked = raw.base_param_values.is_some();
        if let Some(base) = raw
            .base_param_values
            .map(|w| w.into_positional_base(&raw.effect_type, &user_binding_ids, &user_defaults))
        {
            for (slot, b) in param_values.iter_mut().zip(base.iter()) {
                slot.base = *b;
            }
        }

        Ok(PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: raw.id,
            effect_type: raw.effect_type,
            enabled: raw.enabled,
            collapsed: raw.collapsed,
            param_values,
            base_tracked,
            drivers: raw.drivers,
            envelopes: raw.envelopes,
            ableton_mappings: raw.ableton_mappings,
            audio_mods: raw.audio_mods,
            group_id: raw.group_id,
            graph: raw.graph,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: raw.legacy_param0,
            legacy_param1: raw.legacy_param1,
            legacy_param2: raw.legacy_param2,
            legacy_param3: raw.legacy_param3,
            legacy_param_version: None,
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
    param_values: Option<ParamValuesWire>,
    #[serde(default)]
    base_param_values: Option<FloatValuesWire>,
    #[serde(default)]
    drivers: Option<Vec<ParameterDriver>>,
    #[serde(default)]
    envelopes: Option<Vec<ParamEnvelope>>,
    #[serde(default)]
    ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
    #[serde(default)]
    audio_mods: Option<Vec<crate::audio_mod::ParameterAudioMod>>,
    /// The generator's per-instance graph override. Lives on the generator
    /// `PresetInstance` now (graph-home unification) exactly like an effect's
    /// `graph`; older projects carried it on the layer (`generatorGraph`) and
    /// the load migration relocates it here.
    #[serde(default)]
    graph: Option<EffectGraphDef>,
    #[serde(default, rename = "genParamVersion")]
    legacy_param_version: Option<i32>,
}

impl GeneratorInstanceRaw {
    fn into_instance(self) -> PresetInstance {
        let mut param_values = self
            .param_values
            .map(|w| w.into_positional_for_generator(&self.generator_type))
            .unwrap_or_default();
        // Fold the wire's separate `baseParamValues` into each slot's `base`.
        let base_tracked = self.base_param_values.is_some();
        if let Some(base) = self
            .base_param_values
            .map(|w| w.into_positional_for_generator(&self.generator_type))
        {
            for (slot, b) in param_values.iter_mut().zip(base.iter()) {
                slot.base = *b;
            }
        }
        PresetInstance {
            kind: crate::preset_def::PresetKind::Generator,
            id: generate_effect_id(),
            effect_type: self.generator_type,
            enabled: true,
            collapsed: false,
            param_values,
            base_tracked,
            drivers: self.drivers,
            envelopes: self.envelopes,
            ableton_mappings: self.ableton_mappings,
            audio_mods: self.audio_mods,
            group_id: None,
            graph: self.graph,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: self.legacy_param_version,
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

    /// Write the user-set base value (pre-modulation) for a `param_id`,
    /// resolving the id through the static + user-binding tail. Returns `true`
    /// if the id resolved. The UI clamps upstream, so no clamp here. Used by the
    /// editing commands that drive a card param through a [`GraphTarget`].
    pub fn set_base_param_by_id(&mut self, param_id: &str, value: f32) -> bool {
        match self.param_id_to_value_index(param_id) {
            Some(idx) => {
                self.set_base_param(idx, value);
                true
            }
            None => false,
        }
    }

    /// Create a new effect-kind PresetInstance with the given type.
    pub fn new(effect_type: PresetTypeId) -> Self {
        Self {
            kind: crate::preset_def::PresetKind::Effect,
            id: generate_effect_id(),
            effect_type,
            enabled: true,
            collapsed: false,
            param_values: Vec::new(),
            base_tracked: false,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
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
            param_values: Vec::new(),
            base_tracked: false,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
        };
        s.init_defaults();
        s
    }

    /// True for a generator-kind instance.
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

    /// Number of parameters currently allocated. Unity line 84.
    pub fn param_count(&self) -> usize {
        self.param_values.len()
    }

    /// Read effective (modulated) param value. Unity lines 86-91.
    pub fn get_param(&self, index: usize) -> f32 {
        self.param_values.get(index).map(|p| p.value).unwrap_or(0.0)
    }

    /// Read whether a param slot is exposed (visible as a slider on the
    /// effect card). Unknown slots return `true` — the conservative
    /// default that preserves historical "always-visible" behavior.
    pub fn is_param_exposed(&self, index: usize) -> bool {
        self.param_values
            .get(index)
            .map(|p| p.exposed)
            .unwrap_or(true)
    }

    /// Toggle a param slot's exposure flag. No-op if `index` is out of range.
    pub fn set_param_exposed(&mut self, index: usize, exposed: bool) {
        if let Some(slot) = self.param_values.get_mut(index) {
            slot.exposed = exposed;
        }
    }

    /// Write to effective (modulated) param value. Unity lines 93-101.
    ///
    /// No registry clamp (Phase 5: the generator clamp WAS the hidden-max bug —
    /// it capped the value at the stale catalog range even when the preset's
    /// range was widened). The value is bounded by the slider range (UI) and the
    /// modulation resolver (which reads the preset range); the renderer's reshape
    /// is the single place range is enforced toward the inner node.
    pub fn set_param(&mut self, index: usize, value: f32) {
        while self.param_values.len() <= index {
            self.param_values.push(ParamSlot::default());
        }
        self.param_values[index].value = value;
    }

    /// Read the user-set base value (before modulation). Unity lines 104-110.
    pub fn get_base_param(&self, index: usize) -> f32 {
        // While base isn't tracked, `ParamSlot.base` may be stale (an effective
        // write via `set_param` doesn't touch it), so fall through to the
        // effective value — exactly the former `base_param_values: None` behavior.
        if self.base_tracked
            && let Some(slot) = self.param_values.get(index)
        {
            return slot.base;
        }
        self.get_param(index)
    }

    /// Set the user-intended base value. Unity lines 113-126.
    ///
    /// The single base setter for both kinds (absorbed the former
    /// generator-named `set_param_base`). A generator migrate-on-touch grows
    /// `param_values` to the registry length before writing — generator-only,
    /// because an effect's `param_values` is already aligned by
    /// `align_to_definition`.
    pub fn set_base_param(&mut self, index: usize, value: f32) {
        if self.is_generator()
            && let Some(def) = crate::preset_definition_registry::try_get(&self.effect_type)
            && self.param_values.len() < def.param_count
        {
            self.migrate_to_registry_length();
        }
        self.ensure_base_values();
        while self.param_values.len() <= index {
            self.param_values.push(ParamSlot::default());
        }
        // Setting base also sets the effective; modulation later overrides value.
        self.param_values[index].base = value;
        self.param_values[index].value = value;
    }

    /// Reset effective param values from base values.
    pub fn reset_param_effectives(&mut self) {
        self.ensure_base_values();
        for slot in &mut self.param_values {
            slot.value = slot.base;
        }
    }

    /// Begin tracking base: capture the current effective values as base (when
    /// not already tracked) so subsequent modulation reads a stable pre-mod
    /// value. Replaces the former lazy `Option<Vec<f32>>` create; per-slot base
    /// removes the length-sync the old version had to re-derive on mismatch.
    pub fn ensure_base_values(&mut self) {
        if !self.base_tracked {
            for slot in &mut self.param_values {
                slot.base = slot.value;
            }
            self.base_tracked = true;
        }
    }

    /// Ensure paramValues has at least 'count' slots.
    /// Unity PresetInstance.cs EnsureParamCapacity lines 152-158.
    pub fn ensure_param_capacity(&mut self, count: usize) {
        while self.param_values.len() < count {
            self.param_values.push(ParamSlot::default());
        }
    }

    /// Find the driver for a given param id, or None.
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
    /// `user_added` [`BindingDef`]; the reshape (range / invert / curve) comes
    /// from the matching `ParamSpecDef` — the single source after the
    /// per-instance reshape note was deleted. `is_angle` has no home in the
    /// unified shape (the generator side accepts the same gap), so it defaults
    /// to `false`.
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
        // The full slider surface (range + curve + invert + label) lives in
        // the matching `ParamSpecDef` (Phase 2) — the preset is the single
        // source. scale/offset come from the binding's recipe fold-in. Fall
        // back to identity only when no spec exists.
        let spec = self
            .graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .and_then(|m| m.params.iter().find(|p| p.id == b.id));
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
            is_angle: false,
            invert: spec.map(|s| s.invert).unwrap_or(false),
            curve: spec.map(|s| s.curve).unwrap_or_default(),
            scale: b.scale,
            offset: b.offset,
            value_labels: spec.map(|s| s.value_labels.clone()).unwrap_or_default(),
        }
    }

    /// Position of a user binding by stable id within the user-added
    /// tail, or `None` if not found. Index is relative to the user tail,
    /// NOT `param_values`. Use [`Self::param_id_to_value_index`] for the
    /// `param_values` slot.
    pub fn user_binding_index(&self, id: &str) -> Option<usize> {
        self.user_added_bindings().position(|b| b.id == id)
    }

    /// Translate a stable `param_id` to its slot in `param_values`.
    ///
    /// Lookup order:
    /// 1. Static registry (`def.id_to_index`).
    /// 2. Per-instance user-added bindings (linear scan; tail position
    ///    `def.param_count + j` where `j` is the binding's declaration
    ///    index among the `user_added` entries).
    ///
    /// Returns `None` for unknown ids — callers (driver evaluation,
    /// Ableton update, OSC dispatch) treat this as orphaned addressing.
    /// Boundary-frequency lookup, not a per-pixel hot path.
    /// The instance's static (registry-defined) param count — the length of
    /// the `param_values` prefix before the user-added tail. Kind-aware: an
    /// effect's prefix comes from the effect registry, a generator's from the
    /// generator registry. Used by the unified expose/unexpose mirror to place
    /// a new user-binding slot at `static_param_count() + user_position`.
    pub fn static_param_count(&self) -> usize {
        // Matches `align_to_definition`'s asymmetric authority: a generator with
        // a per-instance graph counts its non-user-added graph bindings (the
        // graph is the param authority); an effect, or a generator without a
        // graph, uses the kind's registry (an effect's graph metadata is a stub).
        if self.is_generator()
            && let Some(meta) = self.graph.as_ref().and_then(|g| g.preset_metadata.as_ref())
        {
            return meta.bindings.iter().filter(|b| !b.user_added).count();
        }
        crate::preset_definition_registry::try_get(&self.effect_type)
            .map(|d| d.param_count)
            .unwrap_or(0)
    }

    pub fn param_id_to_value_index(&self, id: &str) -> Option<usize> {
        // Generator with a per-instance graph: the graph's `preset_metadata`
        // params are the slot authority (matches `static_param_count` and the
        // former `Layer::resolve_gen_param_slot`). Effects, and generators
        // without a graph, resolve against the registry static prefix +
        // user-binding tail.
        if self.is_generator()
            && let Some(meta) = self.graph.as_ref().and_then(|g| g.preset_metadata.as_ref())
            && !meta.params.is_empty()
        {
            return meta.params.iter().position(|p| p.id == id);
        }
        if let Some(idx) = crate::preset_definition_registry::param_id_to_index(&self.effect_type, id) {
            return Some(idx);
        }
        let n_static = crate::preset_definition_registry::try_get(&self.effect_type)
            .map(|d| d.param_count)
            .unwrap_or(0);
        self.user_binding_index(id).map(|j| n_static + j)
    }

    /// Full resolution for a `param_id`: slot index plus the value
    /// range and whole-number flag the modulation evaluators need.
    ///
    /// Handles both addressing modes the host uses:
    /// - **Static** (def-declared): pulls range from the registry's
    ///   `ParamDef` for the resolved slot.
    /// - **User-tail** (per-instance `UserParamBinding`): pulls range
    ///   from the binding itself; `whole_numbers` is true when the
    ///   binding's `convert` is `IntRound` / `EnumRound` / `BoolThreshold`.
    ///
    /// Returns `None` when the registry doesn't know the effect type
    /// (test contexts) or the id matches neither a static slot nor a
    /// user binding. Cost: one `AHashMap::get` for static hits, plus
    /// one linear scan of `user_param_bindings` for user-tail hits.
    /// Suitable for the modulation hot path because the alternative
    /// (caching the resolution on the driver/envelope) would require
    /// invalidation on every `align_to_definition` and user-binding
    /// edit — at typical driver counts (<50) the scan is cheaper than
    /// the bookkeeping.
    pub fn resolve_param(&self, id: &str) -> Option<ResolvedParam> {
        let def = crate::preset_definition_registry::try_get(&self.effect_type)?;
        resolve_param_in(&def, self, id)
    }

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
        // Align FIRST (against the current user-added binding count,
        // which doesn't include the new binding yet) so the static
        // prefix is `n_static` long. Then push — the new tail slot
        // lands at exactly `n_static + old_user_count`, matching what
        // `param_id_to_value_index` will compute on lookup.
        self.align_to_definition();
        let default_v = binding.default_value;

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
        let whole_numbers = matches!(
            binding.convert,
            ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger
        );
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
        });
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

        // Reshape (range / curve / invert) is carried on the `ParamSpecDef`
        // pushed above — the preset is the single source. No per-instance note.
        // base rides the slot now (fork #16), so one push covers value + base.
        self.param_values.push(ParamSlot::exposed(default_v));
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
        let value_idx = self.static_param_count() + j;

        // Synthesize the removed view BEFORE mutating the graph (it reads
        // the binding + its reshape note).
        let removed = {
            let b = self.user_added_bindings().nth(j)?;
            self.synth_user_binding(b)
        };

        // Pull the binding + spec from the graph metadata, and the note.
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

        if value_idx < self.param_values.len() {
            // Removing the slot removes its base too (fork #16).
            self.param_values.remove(value_idx);
        }
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
        slot_value: ParamSlot,
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
        });

        // Reshape (range / curve / invert) rides on the `ParamSpecDef` pushed
        // above — the preset is the single source. No per-instance note.

        // Value slot at the original tail index `n_static + position`. The
        // just-restored binding is user-added, so it doesn't change the bundled
        // (`static_param_count`) prefix — kind-aware so generators restore at
        // the right slot too.
        let value_idx = self.static_param_count() + position;
        // The restored `slot_value` carries its own `base` now (fork #16), so a
        // single insert restores value + base together.
        if value_idx <= self.param_values.len() {
            self.param_values.insert(value_idx, slot_value);
        } else {
            self.param_values.push(slot_value);
        }
    }

    /// Resize paramValues and baseParamValues to match the current effect definition.
    /// New slots are filled with the definition's default values.
    /// Includes migration for layout changes (e.g., WireframeDepth 14→12 params).
    ///
    /// V2 user-binding awareness: the target length is
    /// `def.param_count + self.user_param_bindings.len()`. The static
    /// prefix is aligned to registry defaults; the user-binding tail
    /// pulls per-binding `default_value`. Any extra tail beyond the
    /// known user-binding count is treated as junk and truncated —
    /// the user_param_bindings vec is the single source of truth for
    /// "how many user slots exist."
    pub fn align_to_definition(&mut self) {

        // Migration: WireframeDepth 14-param (old) → 12-param (new).
        // Old: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4) Persist(5) Depth(6)
        //      Subject(7) Blend(8) WireRes(9) MeshRate(10) CVFlow(11) Lock(12) Face(13)
        // New: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4) Subject(5) Blend(6)
        //      WireRes(7) MeshRate(8) Flow(9) Lock(10) EdgeFollow(11)
        // (User bindings didn't exist in V1.0 / V1.1, so the WireframeDepth
        // legacy migration runs against the entire param_values Vec.)
        if self.effect_type == PresetTypeId::WIREFRAME_DEPTH && self.param_values.len() == 14 {
            let old = &self.param_values;
            let migrated = vec![
                old[0],                  // Amount → Amount
                old[1],                  // Density → Density
                old[2],                  // Width → Width
                old[3],                  // ZScale → ZScale
                old[4],                  // Smooth → Smooth
                old[7],                  // Subject → Subject (was index 7)
                old[8],                  // Blend → Blend (was index 8)
                old[9],                  // WireRes → WireRes (was index 9)
                old[10],                 // MeshRate → MeshRate (was index 10)
                old[11],                 // CVFlow → Flow (was index 11)
                old[12],                 // Lock → Lock (was index 12)
                ParamSlot::exposed(0.5), // EdgeFollow default (Face was discrete toggle, not transferable)
            ];
            // base rides each slot now (fork #16), so the reorder above carries
            // base alongside value; the new EdgeFollow slot seeds base = 0.5.
            self.param_values = migrated;
        }

        // Snapshot the user-added binding defaults up front (declaration
        // order) so the resize loops can pad without a borrow conflict
        // against `self.graph`.
        let user_defaults: Vec<f32> = self
            .user_added_bindings()
            .map(|b| b.default_value)
            .collect();

        // Resolve the static (bundled) param block — the `param_values` prefix
        // before the user-added tail. The authority is *asymmetric*:
        //  - **Generator with a per-instance graph:** the graph metadata is the
        //    source of truth (the generator registry can be stale or `NONE`
        //    while the graph carries the real params). `meta.params` is
        //    `[bundled | user-added]`, so the bundled prefix is the non-
        //    `user_added` bindings.
        //  - **Effect, or generator without a graph:** the kind's registry. An
        //    effect's per-instance graph metadata is only a *stub* (may carry
        //    just user-added bindings), so the registry — not the graph — is the
        //    bundled-param authority for effects.
        // Both then share the identical `[static prefix | user-added tail]`
        // alignment below, which is what lets the generator expose/unexpose
        // mirror route through the shared `append_user_binding` /
        // `remove_user_binding_by_id` helpers.
        let static_defaults: Option<Vec<f32>> = if self.is_generator()
            && let Some(meta) = self.graph.as_ref().and_then(|g| g.preset_metadata.as_ref())
        {
            // Graph-backed generator: the graph is the param authority.
            let bundled = meta.bindings.iter().filter(|b| !b.user_added).count();
            Some(meta.params.iter().take(bundled).map(|p| p.default_value).collect())
        } else {
            // Effect, or generator without a graph: the unified registry.
            crate::preset_definition_registry::try_get(&self.effect_type)
                .map(|d| d.param_defs.iter().map(|pd| pd.default_value).collect())
        };
        if let Some(static_defaults) = static_defaults {
            let static_target = static_defaults.len();
            let n_user = user_defaults.len();
            let target = static_target + n_user;
            if self.param_values.len() == target {
                return;
            }

            // Interpretation contract: the first `static_target` values
            // are static (registry-driven). Anything past `static_target`
            // is user-tail. This trades "graceful resize when the static
            // count grew between save and load" for "graceful resize
            // when the user-tail is partially or fully missing." The
            // latter is the common case — fresh in-memory flows and
            // partial JSON have absent user-tail; static-count growth
            // is rare and usually paired with a deliberate alias
            // declaration, so it's the right trade.
            let old_total = self.param_values.len();
            let static_copy = old_total.min(static_target);
            let user_tail_now: Vec<ParamSlot> = if old_total > static_target {
                self.param_values[static_target..].to_vec()
            } else {
                Vec::new()
            };

            let mut aligned = vec![ParamSlot::default(); target];
            // Static prefix — copy what we have, pad with registry defaults
            // (exposed=true to match historical always-visible behavior for
            // freshly-introduced static slots).
            aligned[..static_copy].copy_from_slice(&self.param_values[..static_copy]);
            for (i, slot) in aligned
                .iter_mut()
                .enumerate()
                .take(static_target)
                .skip(static_copy)
            {
                *slot = ParamSlot::exposed(static_defaults.get(i).copied().unwrap_or(0.0));
            }
            // User-binding tail — copy what we have, pad from binding defaults.
            for j in 0..n_user {
                aligned[static_target + j] = user_tail_now
                    .get(j)
                    .copied()
                    .unwrap_or_else(|| ParamSlot::exposed(user_defaults[j]));
            }
            // base rides each ParamSlot now (fork #16): the copies + padded
            // `ParamSlot::exposed(default)` slots above already carry the
            // aligned base, so the former parallel `aligned_base` rebuild is
            // gone (and with it the length-sync footgun this fork removes).
            self.param_values = aligned;
        }
    }

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
            if let Some(idx) = self.param_id_to_value_index(&p.id) {
                p.default_value = self.get_base_param(idx);
            }
        }
        for b in meta.bindings.iter_mut() {
            if let Some(idx) = self.param_id_to_value_index(&b.id) {
                b.default_value = self.get_base_param(idx);
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
            self.param_values = meta
                .params
                .iter()
                .map(|p| ParamSlot::exposed(p.default_value))
                .collect();
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
            let meta_param_index = meta.params.iter().position(|p| p.id == id);
            let spec = meta_param_index.map(|i| meta.params[i].clone());
            let value_index = self.param_id_to_value_index(id);
            let slot = value_index.and_then(|i| self.param_values.get(i).copied());
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
                meta_param_index,
                value_index,
                binding_index: bi,
                spec,
                binding: b.clone(),
                slot,
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
        // Remove metadata params + bindings by descending index (indices stay
        // valid mid-loop). param_values uses the same indices as meta.params.
        if let Some(meta) = self.graph.as_mut().and_then(|g| g.preset_metadata.as_mut()) {
            let mut pidx: Vec<usize> =
                captured.iter().filter_map(|c| c.meta_param_index).collect();
            pidx.sort_unstable_by(|a, b| b.cmp(a));
            for i in pidx {
                if i < meta.params.len() {
                    meta.params.remove(i);
                }
            }
            let mut bidx: Vec<usize> = captured.iter().map(|c| c.binding_index).collect();
            bidx.sort_unstable_by(|a, b| b.cmp(a));
            for i in bidx {
                if i < meta.bindings.len() {
                    meta.bindings.remove(i);
                }
            }
        }
        let mut sidx: Vec<usize> = captured.iter().filter_map(|c| c.value_index).collect();
        sidx.sort_unstable_by(|a, b| b.cmp(a));
        for i in sidx {
            if i < self.param_values.len() {
                self.param_values.remove(i);
            }
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
            let mut params: Vec<(usize, crate::effect_graph_def::ParamSpecDef)> = removed
                .iter()
                .filter_map(|r| Some((r.meta_param_index?, r.spec.clone()?)))
                .collect();
            params.sort_by_key(|(i, _)| *i);
            for (i, spec) in params {
                let i = i.min(meta.params.len());
                meta.params.insert(i, spec);
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
        let mut slots: Vec<(usize, ParamSlot)> = removed
            .iter()
            .filter_map(|r| Some((r.value_index?, r.slot?)))
            .collect();
        slots.sort_by_key(|(i, _)| *i);
        for (i, s) in slots {
            let i = i.min(self.param_values.len());
            self.param_values.insert(i, s);
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
            if self.param_id_to_value_index(&d.param_id).is_none() {
                orphans.insert(d.param_id.to_string());
            }
        }
        for m in self.ableton_mappings.iter().flatten() {
            if self.param_id_to_value_index(&m.param_id).is_none() {
                orphans.insert(m.param_id.to_string());
            }
        }
        for e in self.envelopes.iter().flatten() {
            if self.param_id_to_value_index(&e.param_id).is_none() {
                orphans.insert(e.param_id.to_string());
            }
        }
        for a in self.audio_mods.iter().flatten() {
            if self.param_id_to_value_index(&a.param_id).is_none() {
                orphans.insert(a.param_id.to_string());
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

    /// Find the audio modulation for a given param id, if any.
    pub fn find_audio_mod(&self, param_id: &str) -> Option<&crate::audio_mod::ParameterAudioMod> {
        self.audio_mods
            .as_ref()
            .and_then(|v| v.iter().find(|a| a.param_id == param_id))
    }

    /// True if this instance carries any audio modulation.
    pub fn has_audio_mods(&self) -> bool {
        self.audio_mods.as_ref().is_some_and(|v| !v.is_empty())
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

/// Automation rows (drivers / Ableton mappings / envelopes) removed because
/// their `param_id` no longer resolved to a live param. Returned by
/// [`PresetInstance::prune_orphaned_automation`] and restored by
/// [`PresetInstance::restore_automation`] on undo.
#[derive(Debug, Clone, Default)]
pub struct RemovedAutomation {
    drivers: Vec<ParameterDriver>,
    ableton_mappings: Vec<crate::ableton_mapping::AbletonParamMapping>,
    envelopes: Vec<ParamEnvelope>,
    audio_mods: Vec<crate::audio_mod::ParameterAudioMod>,
}

/// Implement ParamSource for PresetInstance.
/// Port of Unity PresetInstance : IParamSource.
/// Generator-kind methods, ported from the former `PresetInstance`. They
/// read the generator registry via `self.effect_type` (which holds the preset
/// type for both kinds). Only ever called on generator-kind instances.
impl PresetInstance {
    /// Extend-only pad of `param_values`/`base_param_values` to the generator
    /// registry's param count, filling the tail with registry defaults.
    pub fn migrate_to_registry_length(&mut self) {
        let Some(def) = crate::preset_definition_registry::try_get(&self.effect_type)
        else {
            return;
        };
        let min_target = def.param_count;
        if self.param_values.len() < min_target {
            self.param_values
                .reserve(min_target - self.param_values.len());
            // Each padded slot seeds base = default (fork #16), so the former
            // parallel base pad is gone.
            for i in self.param_values.len()..min_target {
                self.param_values
                    .push(ParamSlot::exposed(def.param_defs[i].default_value));
            }
        }
    }

    /// Find the envelope for a given param id, or None (generator-only home).
    pub fn find_envelope(&self, param_id: &str) -> Option<&ParamEnvelope> {
        self.envelopes.as_ref()?.iter().find(|e| e.param_id == param_id)
    }

    /// True if this instance has generator envelopes (no-alloc check).
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

    /// Reset effective values to base — ONLY for params with active drivers or
    /// envelopes (generator semantics).
    pub fn reset_effectives(&mut self) {
        if self.param_values.is_empty() {
            return;
        }
        self.ensure_base_values();
        let def = crate::preset_definition_registry::try_get(&self.effect_type);
        let id_to_index = def.as_ref().map(|d| &d.id_to_index);

        if let Some(drivers) = &self.drivers {
            for driver in drivers {
                if !driver.enabled {
                    continue;
                }
                let Some(&idx) = id_to_index.and_then(|m| m.get(driver.param_id.as_ref())) else {
                    continue;
                };
                if let Some(slot) = self.param_values.get_mut(idx) {
                    slot.value = slot.base;
                }
            }
        }
        if let Some(envelopes) = &self.envelopes {
            for env in envelopes {
                if !env.enabled {
                    continue;
                }
                let Some(&idx) = id_to_index.and_then(|m| m.get(env.param_id.as_ref())) else {
                    continue;
                };
                if let Some(slot) = self.param_values.get_mut(idx) {
                    slot.value = slot.base;
                }
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
            // ParamSlot::exposed seeds base = default; the instance now tracks
            // base (the former `base_param_values = Some(..)`).
            self.param_values = def
                .param_defs
                .iter()
                .map(|pd| ParamSlot::exposed(pd.default_value))
                .collect();
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
            self.param_values.iter().map(|s| s.base).collect()
        } else {
            self.param_values.iter().map(|s| s.value).collect()
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
        self.effect_type = gen_type;
        // ParamSlot::exposed seeds base = value; the snapshot is the base.
        self.param_values = params.iter().map(|v| ParamSlot::exposed(*v)).collect();
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
        self.param_values.len()
    }

    fn get_param_def(&self, index: usize) -> ParamDef {
        let Some(def) = crate::preset_definition_registry::try_get(&self.effect_type) else {
            return ParamDef::default();
        };
        if index < def.param_count {
            return def.param_defs[index].clone();
        }
        // Past the static prefix: effects synthesize a ParamDef from the
        // user-added binding tail (routing + reshape range). Generators have
        // no user-tail in this path, so they fall through to the default.
        if !self.is_generator() {
            let user_idx = index - def.param_count;
            if let Some(b) = self.user_added_bindings().nth(user_idx) {
                let ub = self.synth_user_binding(b);
                let whole_numbers = matches!(
                    ub.convert,
                    ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger
                );
                let is_toggle = matches!(ub.convert, ParamConvert::BoolThreshold);
                let is_trigger = matches!(ub.convert, ParamConvert::Trigger);
                return ParamDef {
                    id: ub.id.clone(),
                    name: ub.label.clone(),
                    min: ub.min,
                    max: ub.max,
                    default_value: ub.default_value,
                    whole_numbers,
                    is_toggle,
                    is_trigger,
                    value_labels: None,
                    format_string: None,
                    osc_suffix: None,
                    curve: ub.curve,
                    invert: ub.invert,
                };
            }
        }
        ParamDef::default()
    }

    fn get_param(&self, index: usize) -> f32 {
        PresetInstance::get_param(self, index)
    }

    fn set_param(&mut self, index: usize, value: f32) {
        PresetInstance::set_param(self, index, value);
    }

    fn get_base_param(&self, index: usize) -> f32 {
        PresetInstance::get_base_param(self, index)
    }

    fn set_base_param(&mut self, index: usize, value: f32) {
        PresetInstance::set_base_param(self, index, value);
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

        // 8 base fields (beat_division, waveform, enabled, phase,
        // base_value, trim_min, trim_max, reversed) + addressing field.
        let mut field_count = 8;
        if emit_param_id || emit_legacy_index {
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
        s.end()
    }
}

impl ParameterDriver {
    /// Constructor.
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
            legacy_param_index: None,
            is_paused_by_user: false,
        }
    }

    /// Evaluate driver at given beat position -> [0, 1].
    /// Port of Unity DriverEvaluator.Evaluate.
    pub fn evaluate(
        current_beat: Beats,
        division: BeatDivision,
        waveform: DriverWaveform,
        phase_offset: f32,
    ) -> f32 {
        let period = division.beats();
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
                let mut h = cycle as u32;
                h ^= h >> 16;
                h = h.wrapping_mul(0x45d9f3b);
                h ^= h >> 16;
                h = h.wrapping_mul(0x45d9f3b);
                h ^= h >> 16;
                (h & 0x7FFFFF) as f32 / 0x7FFFFF as f32
            }
        }
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

    // ── PresetInstance paramValues wire format (step 12) ──────────

    #[test]
    fn effect_instance_deserialize_legacy_array_param_values() {
        // V1.0 / V1.1 wire format: paramValues is an Array.
        let json = r#"{
            "id": "abc12345",
            "effectType": "ColorGrade",
            "enabled": true,
            "collapsed": false,
            "paramValues": [1.0, 1.0, 1.0, 0.0, 1.5, 0.0, 0.0, 1.0, 0.0],
            "baseParamValues": [1.0, 1.0, 1.0, 0.0, 1.5, 0.0, 0.0, 1.0, 0.0]
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.param_values.len(), 9);
        assert!((fx.param_values[4].value - 1.5).abs() < f32::EPSILON);
        // Legacy bare-f32 wire format → exposed defaults to true.
        assert!(fx.param_values.iter().all(|p| p.exposed));
        // baseParamValues present → base tracked, folded into each slot's base.
        assert!(fx.base_tracked);
        assert_eq!(fx.param_values.len(), 9);
        assert!((fx.param_values[4].base - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn effect_instance_deserialize_canonical_map_param_values_without_registry() {
        // V1.2+ wire format: paramValues is an Object keyed by param_id.
        // Without manifold-renderer linked, the registry has no def for
        // "ColorGrade" → into_positional returns empty Vec, leaving
        // align_to_definition / the resolver to fill in defaults later.
        let json = r#"{
            "id": "abc12345",
            "effectType": "ColorGrade",
            "enabled": true,
            "collapsed": false,
            "paramValues": { "amount": 0.7, "threshold": 0.5 }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        // No registry → empty Vec is the safe degraded result.
        assert!(fx.param_values.is_empty() || fx.param_values.iter().all(|p| p.value == 0.0));
    }

    #[test]
    fn effect_instance_serialize_falls_back_to_array_without_registry() {
        // No registry def → Serialize must emit Array form so the
        // value survives a round-trip through manifold-core's tests.
        let fx = PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: EffectId::new("abc12345"),
            effect_type: PresetTypeId::from_string("UnregisteredTestEffect".to_string()),
            enabled: true,
            collapsed: false,
            param_values: vec![
                ParamSlot::exposed(0.1),
                ParamSlot::exposed(0.2),
                ParamSlot::exposed(0.3),
            ],
            base_tracked: false,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
        };
        let json = serde_json::to_string(&fx).unwrap();
        // V1.3 wire emits {value, exposed} objects per element.
        assert!(
            json.contains("\"paramValues\":[{\"value\":0.1,\"exposed\":true}"),
            "Serialize without registry must emit positional Array of ParamSlot; got: {json}"
        );
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_values.len(), 3);
        assert_eq!(back.param_values[0].value, 0.1);
        assert!(back.param_values[0].exposed);
    }

    #[test]
    fn param_value_deserialize_accepts_bare_f32_or_object() {
        // Bare f32 (V1.x legacy) → exposed=true.
        let pv: ParamSlot = serde_json::from_str("0.7").unwrap();
        assert_eq!(pv.value, 0.7);
        assert!(pv.exposed);
        // Object form (V1.3 canonical).
        let pv: ParamSlot = serde_json::from_str(r#"{"value": 0.42, "exposed": false}"#).unwrap();
        assert_eq!(pv.value, 0.42);
        assert!(!pv.exposed);
        // Object with missing exposed defaults to true.
        let pv: ParamSlot = serde_json::from_str(r#"{"value": 1.0}"#).unwrap();
        assert_eq!(pv.value, 1.0);
        assert!(pv.exposed);
    }

    #[test]
    fn effect_instance_serialize_emits_v13_object_form_for_hidden_params() {
        // Hidden param round-trips through positional Array{value,exposed}.
        let fx = PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: EffectId::new("abc12345"),
            effect_type: PresetTypeId::from_string("UnregisteredTestEffect".to_string()),
            enabled: true,
            collapsed: false,
            param_values: vec![
                ParamSlot {
                    value: 0.1,
                    base: 0.1,
                    exposed: true,
                },
                ParamSlot {
                    value: 0.2,
                    base: 0.2,
                    exposed: false,
                },
            ],
            base_tracked: false,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
        };
        let json = serde_json::to_string(&fx).unwrap();
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_values.len(), 2);
        assert_eq!(back.param_values[0].value, 0.1);
        assert!(back.param_values[0].exposed);
        assert_eq!(back.param_values[1].value, 0.2);
        assert!(!back.param_values[1].exposed);
    }

    #[test]
    fn effect_instance_legacy_param0_through_param3_round_trip() {
        // V1.0 had flat param0..param3 fields alongside paramValues.
        // The custom Deserialize must continue to capture them so the
        // existing align_to_definition migration sees both shapes.
        let json = r#"{
            "effectType": "ColorGrade",
            "enabled": true,
            "collapsed": false,
            "paramValues": [],
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
        // Verify optional None fields aren't emitted.
        assert!(!json.contains("\"baseParamValues\""));
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

    // Bloom is registered in this crate's tests with a single param
    // `amount`. We use a synthetic alias table here to verify the
    // chain runs through `into_positional` even though Bloom itself
    // ships without aliases. The test mutates a static alias slice
    // via the registry build path — but the registry is `LazyLock`-
    // initialized, so mutating it post-init isn't possible. Instead,
    // verify the alias-walking behavior by directly calling
    // `resolve_param_alias` on a synthetic table — the integration
    // path is covered indirectly by `resolve_param_alias_*` tests in
    // `preset_definition_registry`.

    #[test]
    fn into_positional_keyed_drops_unknown_id() {
        // Without any alias entries, an unknown id is silently dropped.
        // This is the orphan policy — same as drivers/envelopes/Ableton.
        let json = r#"{
            "id": "abc12345",
            "effectType": "Bloom",
            "enabled": true,
            "collapsed": false,
            "paramValues": { "amount": 0.7, "old_phantom_param": 0.5 }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        // amount should land at index 0; old_phantom_param dropped.
        assert_eq!(fx.param_values.len(), 1);
        assert!((fx.param_values[0].value - 0.7).abs() < f32::EPSILON);
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
        }
    }

    /// Install a user-added binding into the effect's graph metadata
    /// WITHOUT growing `param_values` — mimics what deserialize produces
    /// (the binding lives in the graph; the value tail comes from
    /// `paramValues`). Used to exercise `align_to_definition` directly.
    fn push_user_binding_meta_only(fx: &mut PresetInstance, ub: &UserParamBinding) {
        use crate::effect_graph_def::{
            BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
        };
        let graph = fx.graph.get_or_insert_with(|| EffectGraphDef {
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
        meta.params.push(ParamSpecDef {
            id: ub.id.clone(),
            name: ub.label.clone(),
            min: ub.min,
            max: ub.max,
            default_value: ub.default_value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: ub.curve,
            invert: ub.invert,
        });
        meta.bindings.push(BindingDef {
            id: ub.id.clone(),
            label: ub.label.clone(),
            default_value: ub.default_value,
            target: BindingTarget::Node {
                node_id: ub.node_id.clone(),
                param: ub.inner_param.clone(),
            },
            convert: ub.convert,
            user_added: true,
            scale: ub.scale,
            offset: ub.offset,
        });
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
        fx.param_values = vec![ParamSlot::exposed(0.7)]; // static prefix
        fx.append_user_binding(sample_user_binding(
            "user.uv_transform.translate.1",
            "uv_transform",
            "translate",
        ));
        fx.append_user_binding(sample_user_binding("user.mix.amount.1", "mix", "amount"));
        // After append, param_values should be [0.7, 0.25, 0.25].
        assert_eq!(
            fx.param_values,
            vec![
                ParamSlot::exposed(0.7),
                ParamSlot::exposed(0.25),
                ParamSlot::exposed(0.25)
            ]
        );
        // Tweak the user-tail values to verify they round-trip.
        fx.param_values[1].value = 0.42;
        fx.param_values[2].value = 0.91;

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
        assert_eq!(
            back.param_values,
            vec![
                ParamSlot::exposed(0.7),
                ParamSlot::exposed(0.42),
                ParamSlot::exposed(0.91)
            ]
        );
    }

    #[test]
    fn append_user_binding_grows_param_values_with_default() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.7)];
        fx.ensure_base_values();

        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        assert_eq!(
            fx.param_values,
            vec![ParamSlot::exposed(0.7), ParamSlot::exposed(0.25)]
        );
        // base rides each slot now (fork #16).
        assert!(fx.base_tracked);
        assert_eq!(
            fx.param_values.iter().map(|s| s.base).collect::<Vec<_>>(),
            vec![0.7, 0.25]
        );
        // The binding now lives in the graph (the single storage list).
        assert_eq!(fx.user_param_count(), 1);
    }

    #[test]
    fn remove_user_binding_drops_corresponding_value_slot() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.7)];
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        fx.append_user_binding(sample_user_binding("user.c.d.1", "c", "d"));
        // A real slider edit sets base + value together (fork #16); set both so
        // the surviving slot is coherent after compaction.
        fx.set_base_param(1, 0.3);
        fx.set_base_param(2, 0.6);

        let removed = fx.remove_user_binding_by_id("user.a.b.1");
        assert!(removed.is_some());
        assert_eq!(fx.user_param_count(), 1);
        // Static prefix preserved + user tail compacted around the gap.
        assert_eq!(
            fx.param_values,
            vec![ParamSlot::exposed(0.7), ParamSlot::exposed(0.6)]
        );
    }

    #[test]
    fn remove_user_binding_unknown_id_returns_none() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.7)];
        let removed = fx.remove_user_binding_by_id("user.nope.1");
        assert!(removed.is_none());
        assert_eq!(fx.param_values, vec![ParamSlot::exposed(0.7)]);
    }

    #[test]
    fn param_id_to_value_index_resolves_static_then_user() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.7)];
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        fx.append_user_binding(sample_user_binding("user.c.d.1", "c", "d"));
        // Static id maps to slot 0 via the registry.
        assert_eq!(fx.param_id_to_value_index("amount"), Some(0));
        // User ids map to tail slots.
        assert_eq!(fx.param_id_to_value_index("user.a.b.1"), Some(1));
        assert_eq!(fx.param_id_to_value_index("user.c.d.1"), Some(2));
        // Unknown id returns None.
        assert_eq!(fx.param_id_to_value_index("nope"), None);
    }

    #[test]
    fn align_to_definition_preserves_user_binding_tail() {
        // Simulate: a fixture saved with 2 user bindings is loaded into
        // a build that also knows those bindings (because they're
        // per-instance — same PresetInstance), and align runs. The
        // user-binding tail values must survive.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        push_user_binding_meta_only(&mut fx, &sample_user_binding("user.a.b.1", "a", "b"));
        push_user_binding_meta_only(&mut fx, &sample_user_binding("user.c.d.1", "c", "d"));
        // Hand-build param_values to mimic what comes out of deserialize.
        fx.param_values = vec![
            ParamSlot::exposed(0.7),
            ParamSlot::exposed(0.42),
            ParamSlot::exposed(0.91),
        ];
        fx.align_to_definition();
        // Bloom static count = 1. User tail = 2. Total = 3.
        assert_eq!(
            fx.param_values,
            vec![
                ParamSlot::exposed(0.7),
                ParamSlot::exposed(0.42),
                ParamSlot::exposed(0.91)
            ]
        );
    }

    #[test]
    fn align_to_definition_pads_missing_user_tail_with_binding_defaults() {
        // Static prefix already correct, user-binding tail missing
        // (e.g., the binding was added in memory but param_values
        // hasn't grown yet). align should pad with binding defaults.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        push_user_binding_meta_only(&mut fx, &sample_user_binding("user.a.b.1", "a", "b"));
        fx.param_values = vec![ParamSlot::exposed(0.7)]; // missing tail
        fx.align_to_definition();
        assert_eq!(
            fx.param_values,
            vec![ParamSlot::exposed(0.7), ParamSlot::exposed(0.25)]
        );
    }

    #[test]
    fn align_to_definition_truncates_extra_user_tail() {
        // param_values has more user-tail slots than user bindings —
        // junk data from somewhere. align trims to the actual binding count.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        push_user_binding_meta_only(&mut fx, &sample_user_binding("user.a.b.1", "a", "b"));
        fx.param_values = vec![
            ParamSlot::exposed(0.7),
            ParamSlot::exposed(0.42),
            ParamSlot::exposed(0.91),
            ParamSlot::exposed(0.99),
        ]; // extra junk at tail
        fx.align_to_definition();
        // After: static=1 (kept) + user=1 (last value taken — split logic
        // pulls the tail count from the graph's user-added bindings).
        assert_eq!(fx.param_values.len(), 2);
        assert!((fx.param_values[0].value - 0.7).abs() < f32::EPSILON);
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
        fx.param_values = vec![ParamSlot::exposed(0.1), ParamSlot::exposed(0.2)];

        let mut donor = PresetInstance::new(PresetTypeId::BLOOM);
        donor.append_user_binding(sample_user_binding("user.x.y.1", "x", "y"));
        assert!(donor.set_base_param_by_id("user.x.y.1", 0.55));
        let mut def = donor.graph.clone().expect("graph carries metadata");
        donor.snapshot_values_into_def(&mut def);

        fx.reseed_param_values_from_def(&def);
        assert_eq!(
            fx.param_values,
            vec![ParamSlot::exposed(0.55)],
            "reseed rebuilds param_values from the def's (snapshotted) defaults",
        );
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

        let pre_params = fx.param_values.clone();

        let removed = fx.remove_exposures_for_node(&NodeId::new("blur"));
        assert_eq!(removed.len(), 1, "one slider was bound to the deleted node");

        // Slider, slot, driver, envelope all gone; the unrelated slider survives.
        assert!(fx.param_id_to_value_index("user.blur.radius.1").is_none());
        assert!(fx.find_driver("user.blur.radius.1").is_none());
        assert!(
            fx.envelopes.is_none(),
            "pruning the last envelope collapses the list to None"
        );
        assert!(fx.param_id_to_value_index("user.other.x.1").is_some());

        // Undo restores values, metadata, and automation.
        fx.restore_exposures(removed);
        assert_eq!(
            fx.param_values, pre_params,
            "value slots restored at their original positions"
        );
        assert!(
            fx.param_id_to_value_index("user.blur.radius.1").is_some(),
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

        let removed = fx.prune_orphaned_automation();
        assert!(fx.find_driver("user.a.b.1").is_some(), "live driver kept");
        assert!(fx.find_driver("user.gone.x.1").is_none(), "orphan driver pruned");
        assert!(
            fx.envelopes.is_none(),
            "sole orphan envelope pruned, list collapses to None"
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
    }

    #[test]
    fn remove_exposures_for_node_is_noop_when_nothing_bound() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.blur.radius.1", "blur", "radius"));
        let before = fx.param_values.clone();
        let removed = fx.remove_exposures_for_node(&NodeId::new("nonexistent"));
        assert!(removed.is_empty(), "no binding targets that node");
        assert_eq!(fx.param_values, before, "nothing changed");
    }

    #[test]
    fn get_param_def_synthesizes_user_binding_def() {
        // ParamSource::get_param_def must return a ParamDef shaped from
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
        });
        let pd = ParamSource::get_param_def(&fx, 1);
        assert_eq!(pd.id, "user.uv.translate.1");
        assert_eq!(pd.name, "Translate");
        assert!((pd.min + 2.0).abs() < f32::EPSILON);
        assert!((pd.max - 2.0).abs() < f32::EPSILON);
        assert!(!pd.whole_numbers);
        assert!(!pd.is_toggle);
    }

    #[test]
    fn deserialize_keyed_param_values_routes_user_ids_to_tail() {
        // The key insight: paramValues comes in as a Map. The custom
        // Deserialize must consult the graph's `user_added` bindings (the
        // single storage list after the unification) to route user ids to
        // the right tail slots — regardless of JSON key order in the Map.
        let json = r#"{
            "id": "abc12345",
            "effectType": "Bloom",
            "enabled": true,
            "collapsed": false,
            "paramValues": {
                "amount": 0.7,
                "user.foo.bar.1": 0.3,
                "user.baz.qux.1": 0.9
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
        assert_eq!(fx.param_values.len(), 3);
        assert!((fx.param_values[0].value - 0.7).abs() < f32::EPSILON);
        assert!((fx.param_values[1].value - 0.3).abs() < f32::EPSILON);
        assert!((fx.param_values[2].value - 0.9).abs() < f32::EPSILON);
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
            "paramValues": [{"value": 1.0, "exposed": true}, {"value": 0.0, "exposed": true}]
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert!(fx.graph.is_none());
    }
}
