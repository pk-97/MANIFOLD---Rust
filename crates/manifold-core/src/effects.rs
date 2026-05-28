use crate::effect_graph_def::EffectGraphDef;
use crate::effect_type_id::EffectTypeId;
use crate::id::{EffectGroupId, EffectId};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc_suffix: Option<String>,
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
            value_labels: None,
            format_string: None,
            osc_suffix: None,
        }
    }
}

// ─── Traits ───

/// Shared contract for entities that own a modular effects list.
/// Port of Unity IEffectContainer.cs.
/// Implemented by TimelineClip, Layer, and ProjectSettings.
pub trait EffectContainer {
    fn effects(&self) -> &[EffectInstance];
    fn effects_mut(&mut self) -> &mut Vec<EffectInstance>;
    fn effect_groups(&self) -> &[EffectGroup];
    fn effect_groups_mut(&mut self) -> &mut Vec<EffectGroup>;
    fn has_modular_effects(&self) -> bool;
    fn find_effect(&self, effect_type: &EffectTypeId) -> Option<&EffectInstance>;
    fn find_effect_group(&self, group_id: &str) -> Option<&EffectGroup>;
    fn envelopes(&self) -> &[ParamEnvelope];
    fn envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope>;
    fn has_envelopes(&self) -> bool;
}

/// Abstracts a "thing with named params, drivers, and ranges."
/// Port of Unity IParamSource.cs.
/// Both EffectInstance and generator params implement this.
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
}

/// Free-function form of [`EffectInstance::resolve_param`]. Takes the
/// `EffectDef` and `user_param_bindings` slice directly so callers in
/// borrow-tight closures (the modulation evaluators iterating
/// `fx.drivers`) can resolve without a second `&fx` borrow.
pub fn resolve_param_in(
    def: &crate::effect_definition_registry::EffectDef,
    user_bindings: &[UserParamBinding],
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
    let j = user_bindings.iter().position(|b| b.id == id)?;
    let ub = &user_bindings[j];
    Some(ResolvedParam {
        idx: def.param_count + j,
        min: ub.min,
        max: ub.max,
        whole_numbers: matches!(
            ub.convert,
            ParamConvert::IntRound
                | ParamConvert::EnumRound
                | ParamConvert::BoolThreshold
        ),
    })
}

/// Result of [`EffectInstance::resolve_param`]: slot index plus the
/// metadata modulation evaluators need to map a normalized 0–1 driver
/// or envelope output onto the target parameter's value range.
///
/// Lives at this layer (not in `manifold-playback`) because the
/// resolution itself is pure data-model logic — it knows about static
/// vs user-tail addressing and is unrelated to playback timing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedParam {
    /// Slot in `EffectInstance.param_values` to read/write.
    pub idx: usize,
    pub min: f32,
    pub max: f32,
    /// True when the parameter is integral (registry `whole_numbers`
    /// or `value_labels` set, or user binding declares an integral
    /// conversion). Modulation evaluators round the final value when
    /// this is set.
    pub whole_numbers: bool,
}

/// A user-exposed parameter on an [`EffectInstance`].
///
/// V2 user-exposed-params surface (see `docs/EFFECT_RUNTIME_UNIFICATION.md`
/// §7.6). Each binding is per-instance: ticking "expose UVTransform.translate"
/// on Mirror#0 doesn't affect Mirror#1.
///
/// Stable addressing across renderer refactors comes from `node_handle` —
/// an author-assigned `&'static str` registered alongside the inner node
/// at construction time (`Graph::add_node_named("uv_transform", ...)`).
/// Renames go through the same alias mechanism Phase 2 introduced for
/// param ids, via [`crate::effect_registration::EffectNodeAliasMetadata`].
///
/// Storage: each `UserParamBinding` reserves one slot at the tail of
/// the parent `EffectInstance.param_values` (positions
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
    /// Stable handle for the inner node within the effect's graph.
    /// Set by the effect's `new()` via `Graph::add_node_named(handle, …)`.
    /// Schema renames go through `EffectNodeAliasMetadata`.
    pub node_handle: String,
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
}

// ─── Param Value (per-slot state) ───

/// A single parameter slot's runtime state on an [`EffectInstance`].
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
    pub value: f32,
    pub exposed: bool,
}

impl Default for ParamSlot {
    fn default() -> Self {
        Self {
            value: 0.0,
            exposed: true,
        }
    }
}

impl ParamSlot {
    /// Convenience constructor for an exposed slot with the given value.
    #[inline]
    pub const fn exposed(value: f32) -> Self {
        Self {
            value,
            exposed: true,
        }
    }
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
                Ok(ParamSlot {
                    value: v as f32,
                    exposed: true,
                })
            }

            fn visit_f32<E: serde::de::Error>(self, v: f32) -> Result<ParamSlot, E> {
                Ok(ParamSlot {
                    value: v,
                    exposed: true,
                })
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<ParamSlot, E> {
                Ok(ParamSlot {
                    value: v as f32,
                    exposed: true,
                })
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<ParamSlot, E> {
                Ok(ParamSlot {
                    value: v as f32,
                    exposed: true,
                })
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
                Ok(ParamSlot {
                    value: value.unwrap_or(0.0),
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
/// - `baseParamValues` stays as `Vec<f32>` (modulation tracking only —
///   exposure isn't meaningful for the pre-modulation snapshot).
///
/// In-memory storage stays positional (`Vec<ParamSlot>`) — the hot
/// path reads/writes by index. The Map form only exists on the wire.
/// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 12.
#[derive(Debug, Clone)]
pub struct EffectInstance {
    /// Unique identifier for this effect instance.
    pub id: EffectId,
    effect_type: EffectTypeId,
    pub enabled: bool,
    pub collapsed: bool,
    /// Positional parameter storage. The first
    /// `effect_definition_registry::get(&effect_type).param_count`
    /// slots correspond to the effect's static-spec bindings in
    /// declaration order; the remaining slots correspond to
    /// [`Self::user_param_bindings`] in declaration order. After the
    /// bindings unification (Phases 1–4 of
    /// `docs/BINDINGS_UNIFICATION_PLAN.md`) this layout maps directly
    /// onto the renderer's `EffectSlot.bindings[i]` — no parallel
    /// structure to keep in sync. Resolve `ParamId → index` via
    /// [`Self::param_id_to_value_index`]; that helper is the single
    /// tier-aware lookup the rest of the codebase relies on.
    pub param_values: Vec<ParamSlot>,
    pub base_param_values: Option<Vec<f32>>,
    pub drivers: Option<Vec<ParameterDriver>>,
    pub ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
    pub group_id: Option<EffectGroupId>,

    /// V2 user-exposed parameters bound to inner-graph nodes.
    /// Empty for legacy and non-graph-backed effects. Serialized as
    /// `userParamBindings` and skipped when empty so existing fixtures
    /// round-trip byte-identically.
    pub user_param_bindings: Vec<UserParamBinding>,

    /// Monotonically bumped each time `user_param_bindings` is mutated
    /// in place by `append_user_binding` / `remove_user_binding_by_id` /
    /// `set_user_binding_label`. Renderer-side caches the last seen
    /// version on the FX struct singleton, rebuilds its
    /// `Vec<ParamBinding>` scratch only when the version changes —
    /// keeps the per-frame path allocation-free even when user
    /// bindings are present. Not serialized; resets to 0 on load and
    /// `u32::MAX` on the renderer side forces a first-frame rebuild.
    pub user_param_bindings_version: u32,

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

    // Legacy flat param fields (V1.0.0 format).
    pub legacy_param0: Option<f32>,
    pub legacy_param1: Option<f32>,
    pub legacy_param2: Option<f32>,
    pub legacy_param3: Option<f32>,
}

// ─── Wire-format helpers for paramValues ───

/// Wire-format shape for `EffectInstance.paramValues`. Accepts
/// V1.0/1.1 positional `Array<f32>`, V1.2 keyed `Map<id, f32>`,
/// V1.3 positional `Array<{value, exposed}>`, or V1.3 keyed
/// `Map<id, {value, exposed}>` — the polymorphic [`ParamSlot`]
/// deserializer normalizes per-element across versions.
///
/// Used only by `EffectInstance`. `GeneratorParamState` and
/// `EffectInstance.baseParamValues` use [`FloatValuesWire`] which
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
        effect_type: &EffectTypeId,
        user_bindings: &[UserParamBinding],
    ) -> Vec<ParamSlot> {
        match self {
            ParamValuesWire::Positional(v) => v,
            ParamValuesWire::Keyed(map) => {
                let Some(def) = crate::effect_definition_registry::try_get(effect_type) else {
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
                let n_user = user_bindings.len();
                let total = n_static + n_user;
                let mut out = vec![ParamSlot::default(); total];
                for (i, pd) in def.param_defs.iter().enumerate().take(n_static) {
                    out[i] = ParamSlot::exposed(pd.default_value);
                }
                for (j, ub) in user_bindings.iter().enumerate() {
                    out[n_static + j] = ParamSlot::exposed(ub.default_value);
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
                    // Per-instance user binding lookup.
                    if let Some(j) = user_bindings.iter().position(|b| b.id == id) {
                        out[n_static + j] = pv;
                    }
                }
                out
            }
        }
    }
}

/// Wire-format shape for plain-float param vectors:
/// `EffectInstance.baseParamValues` and `GeneratorParamState.paramValues`.
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
        effect_type: &EffectTypeId,
        user_bindings: &[UserParamBinding],
    ) -> Vec<f32> {
        match self {
            FloatValuesWire::Positional(v) => v,
            FloatValuesWire::Keyed(map) => {
                let Some(def) = crate::effect_definition_registry::try_get(effect_type) else {
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
                let n_user = user_bindings.len();
                let total = n_static + n_user;
                let mut out = vec![0.0_f32; total];
                for (i, pd) in def.param_defs.iter().enumerate().take(n_static) {
                    out[i] = pd.default_value;
                }
                for (j, ub) in user_bindings.iter().enumerate() {
                    out[n_static + j] = ub.default_value;
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
                    if let Some(j) = user_bindings.iter().position(|b| b.id == id) {
                        out[n_static + j] = value;
                    }
                }
                out
            }
        }
    }

    /// Generator-registry counterpart for `GeneratorParamState.paramValues`.
    pub(crate) fn into_positional_for_generator(
        self,
        gen_type: &crate::GeneratorTypeId,
    ) -> Vec<f32> {
        match self {
            FloatValuesWire::Positional(v) => v,
            FloatValuesWire::Keyed(map) => {
                let Some(def) = crate::generator_definition_registry::try_get(gen_type) else {
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
    gen_type: &crate::GeneratorTypeId,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::{SerializeMap, SerializeSeq};

    let def = crate::generator_definition_registry::try_get(gen_type);
    let can_emit_map = def.is_some_and(|d| {
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
            map.serialize_entry(def.param_ids[i], &v)?;
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
    effect_type: &EffectTypeId,
    user_bindings: &[UserParamBinding],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::{SerializeMap, SerializeSeq};

    let def = crate::effect_definition_registry::try_get(effect_type);
    let static_count = def.map(|d| d.param_count).unwrap_or(0);
    let static_touch = values.len().min(static_count);
    let can_emit_map = def.is_some_and(|d| {
        d.param_ids
            .iter()
            .take(static_touch)
            .all(|id| !id.is_empty())
    });

    if can_emit_map {
        let def = def.expect("checked above");
        let mut map = serializer.serialize_map(Some(values.len()))?;
        for (i, pv) in values.iter().take(static_touch).enumerate() {
            map.serialize_entry(def.param_ids[i], pv)?;
        }
        for (j, ub) in user_bindings.iter().enumerate() {
            let idx = static_count + j;
            if let Some(pv) = values.get(idx) {
                map.serialize_entry(&ub.id, pv)?;
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
    effect_type: &EffectTypeId,
    user_bindings: &[UserParamBinding],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::{SerializeMap, SerializeSeq};

    let def = crate::effect_definition_registry::try_get(effect_type);
    let static_count = def.map(|d| d.param_count).unwrap_or(0);
    let static_touch = values.len().min(static_count);
    let can_emit_map = def.is_some_and(|d| {
        d.param_ids
            .iter()
            .take(static_touch)
            .all(|id| !id.is_empty())
    });

    if can_emit_map {
        let def = def.expect("checked above");
        let mut map = serializer.serialize_map(Some(values.len()))?;
        for (i, &v) in values.iter().take(static_touch).enumerate() {
            map.serialize_entry(def.param_ids[i], &v)?;
        }
        for (j, ub) in user_bindings.iter().enumerate() {
            let idx = static_count + j;
            if let Some(&v) = values.get(idx) {
                map.serialize_entry(&ub.id, &v)?;
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

// ─── Custom Serialize / Deserialize for EffectInstance ───

impl Serialize for EffectInstance {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        // `param_values` always emits; `base_param_values` is optional.
        // Other optional fields use the same `skip_if_none` policy as
        // the previous derive(Serialize) impl.
        let mut field_count = 5; // id, effectType, enabled, collapsed, paramValues
        if self.base_param_values.is_some() {
            field_count += 1;
        }
        if self.drivers.is_some() {
            field_count += 1;
        }
        if self.ableton_mappings.is_some() {
            field_count += 1;
        }
        if self.group_id.is_some() {
            field_count += 1;
        }
        if !self.user_param_bindings.is_empty() {
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

        let mut s = serializer.serialize_struct("EffectInstance", field_count)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("effectType", &self.effect_type)?;
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("collapsed", &self.collapsed)?;
        s.serialize_field(
            "paramValues",
            &ParamValuesSer {
                values: &self.param_values,
                effect_type: &self.effect_type,
                user_bindings: &self.user_param_bindings,
            },
        )?;
        if let Some(base) = &self.base_param_values {
            s.serialize_field(
                "baseParamValues",
                &BaseParamValuesSer {
                    values: base,
                    effect_type: &self.effect_type,
                    user_bindings: &self.user_param_bindings,
                },
            )?;
        }
        if let Some(d) = &self.drivers {
            s.serialize_field("drivers", d)?;
        }
        if let Some(m) = &self.ableton_mappings {
            s.serialize_field("abletonMappings", m)?;
        }
        if let Some(g) = &self.group_id {
            s.serialize_field("groupId", g)?;
        }
        // `userParamBindings` is skipped when empty so existing
        // fixtures (Burn V5/V4, WAYPOINTS, Liveschool) round-trip
        // byte-identically — no new field appears in their JSON.
        if !self.user_param_bindings.is_empty() {
            s.serialize_field("userParamBindings", &self.user_param_bindings)?;
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

/// Serialize-side wrapper for `paramValues` that carries the parent's
/// `effect_type` and per-instance user bindings so the field-level
/// `Serialize` can route to `serialize_param_values`.
struct ParamValuesSer<'a> {
    values: &'a [ParamSlot],
    effect_type: &'a EffectTypeId,
    user_bindings: &'a [UserParamBinding],
}

impl Serialize for ParamValuesSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_param_values(
            self.values,
            self.effect_type,
            self.user_bindings,
            serializer,
        )
    }
}

/// Serialize-side wrapper for `baseParamValues` (plain `Vec<f32>`).
struct BaseParamValuesSer<'a> {
    values: &'a [f32],
    effect_type: &'a EffectTypeId,
    user_bindings: &'a [UserParamBinding],
}

impl Serialize for BaseParamValuesSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_base_param_values(
            self.values,
            self.effect_type,
            self.user_bindings,
            serializer,
        )
    }
}

impl<'de> Deserialize<'de> for EffectInstance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default = "generate_effect_id")]
            id: EffectId,
            effect_type: EffectTypeId,
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
            ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
            #[serde(default)]
            group_id: Option<EffectGroupId>,
            #[serde(default)]
            user_param_bindings: Option<Vec<UserParamBinding>>,
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
        let user_param_bindings = raw.user_param_bindings.unwrap_or_default();
        // `into_positional` resolves Map keys against both the static
        // registry AND the per-instance user bindings, so user-binding
        // tail values arrive in the right slot regardless of JSON key
        // ordering. The Map → positional fold runs before
        // `align_to_definition`, so registry-known but user-tail-empty
        // cases land at zero and align fills user-binding defaults.
        let param_values = raw
            .param_values
            .map(|w| w.into_positional(&raw.effect_type, &user_param_bindings))
            .unwrap_or_default();
        let base_param_values = raw
            .base_param_values
            .map(|w| w.into_positional_base(&raw.effect_type, &user_param_bindings));

        Ok(EffectInstance {
            id: raw.id,
            effect_type: raw.effect_type,
            enabled: raw.enabled,
            collapsed: raw.collapsed,
            param_values,
            base_param_values,
            drivers: raw.drivers,
            ableton_mappings: raw.ableton_mappings,
            group_id: raw.group_id,
            user_param_bindings,
            user_param_bindings_version: 0,
            graph: raw.graph,
            graph_version: 0,
            legacy_param0: raw.legacy_param0,
            legacy_param1: raw.legacy_param1,
            legacy_param2: raw.legacy_param2,
            legacy_param3: raw.legacy_param3,
        })
    }
}

impl EffectInstance {
    /// Create a new EffectInstance with the given type.
    /// Unity EffectInstance.cs lines 79-83.
    pub fn new(effect_type: EffectTypeId) -> Self {
        Self {
            id: generate_effect_id(),
            effect_type,
            enabled: true,
            collapsed: false,
            param_values: Vec::new(),
            base_param_values: None,
            drivers: None,
            ableton_mappings: None,
            group_id: None,
            user_param_bindings: Vec::new(),
            user_param_bindings_version: 0,
            graph: None,
            graph_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
        }
    }

    /// Read-only access to the effect type.
    #[inline]
    pub fn effect_type(&self) -> &EffectTypeId {
        &self.effect_type
    }

    /// Has any drivers? Unity EffectInstance.cs line 28.
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
    pub fn set_param(&mut self, index: usize, value: f32) {
        while self.param_values.len() <= index {
            self.param_values.push(ParamSlot::default());
        }
        self.param_values[index].value = value;
    }

    /// Read the user-set base value (before modulation). Unity lines 104-110.
    pub fn get_base_param(&self, index: usize) -> f32 {
        if let Some(base) = &self.base_param_values
            && index < base.len()
        {
            return base[index];
        }
        // Fall through to effective for backward compat
        self.get_param(index)
    }

    /// Set the user-intended base value. Unity lines 113-126.
    pub fn set_base_param(&mut self, index: usize, value: f32) {
        self.ensure_base_values();
        while self.param_values.len() <= index {
            self.param_values.push(ParamSlot::default());
        }
        if let Some(base) = &mut self.base_param_values {
            while base.len() <= index {
                base.push(0.0);
            }
            base[index] = value;
        }
        self.param_values[index].value = value;
    }

    /// Reset effective param values from base values.
    pub fn reset_param_effectives(&mut self) {
        self.ensure_base_values();
        if let Some(base) = &self.base_param_values {
            let len = self.param_values.len().min(base.len());
            for (i, &b) in base.iter().enumerate().take(len) {
                self.param_values[i].value = b;
            }
        }
    }

    /// Lazy migration: create baseParamValues from paramValues if missing.
    pub fn ensure_base_values(&mut self) {
        if self.base_param_values.is_none()
            || self
                .base_param_values
                .as_ref()
                .is_some_and(|b| b.len() != self.param_values.len())
        {
            self.base_param_values = Some(self.param_values.iter().map(|p| p.value).collect());
        }
    }

    /// Ensure paramValues has at least 'count' slots.
    /// Unity EffectInstance.cs EnsureParamCapacity lines 152-158.
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

    /// Number of user-exposed parameter bindings on this instance.
    pub fn user_param_count(&self) -> usize {
        self.user_param_bindings.len()
    }

    /// Position of a user binding by stable id, or `None` if not found.
    /// Returned index is relative to `user_param_bindings`, NOT
    /// `param_values`. Use [`Self::param_id_to_value_index`] for the
    /// `param_values` slot.
    pub fn user_binding_index(&self, id: &str) -> Option<usize> {
        self.user_param_bindings.iter().position(|b| b.id == id)
    }

    /// Translate a stable `param_id` to its slot in `param_values`.
    ///
    /// Lookup order:
    /// 1. Static registry (`def.id_to_index`).
    /// 2. Per-instance user bindings (linear scan; tail position
    ///    `def.param_count + j` where `j` is the binding's vec index).
    ///
    /// Returns `None` for unknown ids — callers (driver evaluation,
    /// Ableton update, OSC dispatch) treat this as orphaned addressing.
    /// Boundary-frequency lookup, not a per-pixel hot path.
    pub fn param_id_to_value_index(&self, id: &str) -> Option<usize> {
        use crate::effect_definition_registry;
        if let Some(idx) = effect_definition_registry::param_id_to_index(&self.effect_type, id) {
            return Some(idx);
        }
        let n_static = effect_definition_registry::try_get(&self.effect_type)
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
        use crate::effect_definition_registry;
        let def = effect_definition_registry::try_get(&self.effect_type)?;
        resolve_param_in(def, &self.user_param_bindings, id)
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
        // Align FIRST (against the current user_param_bindings list,
        // which doesn't include the new binding yet) so the static
        // prefix is `n_static` long. Then push — the new tail slot
        // lands at exactly `n_static + old_user_count`, matching what
        // `param_id_to_value_index` will compute on lookup.
        self.align_to_definition();
        let default_v = binding.default_value;
        self.user_param_bindings.push(binding);
        self.param_values.push(ParamSlot::exposed(default_v));
        if let Some(base) = self.base_param_values.as_mut() {
            base.push(default_v);
        }
        self.user_param_bindings_version = self.user_param_bindings_version.wrapping_add(1);
    }

    /// Remove a user-exposed binding by id and drop its `param_values`
    /// (and `base_param_values`) slot. Returns the removed binding.
    ///
    /// Bumps `user_param_bindings_version`. Restores undo-state via
    /// the returned `UserParamBinding` plus the value caller saved
    /// before the call (use [`Self::get_param`] / [`Self::get_base_param`]
    /// at the slot returned by [`Self::param_id_to_value_index`]).
    pub fn remove_user_binding_by_id(&mut self, id: &str) -> Option<UserParamBinding> {
        use crate::effect_definition_registry;
        let j = self.user_binding_index(id)?;
        let n_static = effect_definition_registry::try_get(&self.effect_type)
            .map(|d| d.param_count)
            .unwrap_or(0);
        let value_idx = n_static + j;
        let removed = self.user_param_bindings.remove(j);
        if value_idx < self.param_values.len() {
            self.param_values.remove(value_idx);
        }
        if let Some(base) = self.base_param_values.as_mut()
            && value_idx < base.len()
        {
            base.remove(value_idx);
        }
        self.user_param_bindings_version = self.user_param_bindings_version.wrapping_add(1);
        Some(removed)
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
        use crate::effect_definition_registry;

        // Migration: WireframeDepth 14-param (old) → 12-param (new).
        // Old: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4) Persist(5) Depth(6)
        //      Subject(7) Blend(8) WireRes(9) MeshRate(10) CVFlow(11) Lock(12) Face(13)
        // New: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4) Subject(5) Blend(6)
        //      WireRes(7) MeshRate(8) Flow(9) Lock(10) EdgeFollow(11)
        // (User bindings didn't exist in V1.0 / V1.1, so the WireframeDepth
        // legacy migration runs against the entire param_values Vec.)
        if self.effect_type == EffectTypeId::WIREFRAME_DEPTH && self.param_values.len() == 14 {
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
            self.param_values = migrated;
            // Migrate base values too
            if let Some(ref base) = self.base_param_values
                && base.len() == 14
            {
                let migrated_base = vec![
                    base[0], base[1], base[2], base[3], base[4], base[7], base[8], base[9],
                    base[10], base[11], base[12], 0.5,
                ];
                self.base_param_values = Some(migrated_base);
            }
        }

        if let Some(def) = effect_definition_registry::try_get(&self.effect_type) {
            let static_target = def.param_count;
            let n_user = self.user_param_bindings.len();
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
                *slot = ParamSlot::exposed(
                    def.param_defs
                        .get(i)
                        .map(|pd| pd.default_value)
                        .unwrap_or(0.0),
                );
            }
            // User-binding tail — copy what we have, pad from binding defaults.
            for j in 0..n_user {
                aligned[static_target + j] = user_tail_now.get(j).copied().unwrap_or_else(|| {
                    ParamSlot::exposed(self.user_param_bindings[j].default_value)
                });
            }
            self.param_values = aligned;

            if let Some(ref base) = self.base_param_values {
                let old_base_total = base.len();
                let base_static_copy = old_base_total.min(static_target);
                let base_user_tail_now: Vec<f32> = if old_base_total > static_target {
                    base[static_target..].to_vec()
                } else {
                    Vec::new()
                };

                let mut aligned_base = vec![0.0f32; target];
                aligned_base[..base_static_copy].copy_from_slice(&base[..base_static_copy]);
                for (i, slot) in aligned_base
                    .iter_mut()
                    .enumerate()
                    .take(static_target)
                    .skip(base_static_copy)
                {
                    *slot = def
                        .param_defs
                        .get(i)
                        .map(|pd| pd.default_value)
                        .unwrap_or(0.0);
                }
                for j in 0..n_user {
                    aligned_base[static_target + j] = base_user_tail_now
                        .get(j)
                        .copied()
                        .unwrap_or(self.user_param_bindings[j].default_value);
                }
                self.base_param_values = Some(aligned_base);
            }
        }
    }

    /// Get the drivers list, creating it if None.
    pub fn drivers_mut(&mut self) -> &mut Vec<ParameterDriver> {
        if self.drivers.is_none() {
            self.drivers = Some(Vec::new());
        }
        self.drivers.as_mut().unwrap()
    }
}

/// Implement ParamSource for EffectInstance.
/// Port of Unity EffectInstance : IParamSource.
impl ParamSource for EffectInstance {
    fn display_name(&self) -> &str {
        use crate::effect_definition_registry;
        match effect_definition_registry::try_get(&self.effect_type) {
            Some(def) => def.display_name,
            None => "?",
        }
    }

    fn param_count(&self) -> usize {
        self.param_values.len()
    }

    fn get_param_def(&self, index: usize) -> ParamDef {
        use crate::effect_definition_registry;
        if let Some(def) = effect_definition_registry::try_get(&self.effect_type) {
            if index < def.param_count {
                return def.param_defs[index].clone();
            }
            // User-binding tail: synthesize a ParamDef from the binding.
            let user_idx = index - def.param_count;
            if let Some(ub) = self.user_param_bindings.get(user_idx) {
                let whole_numbers = matches!(
                    ub.convert,
                    ParamConvert::IntRound | ParamConvert::EnumRound
                );
                let is_toggle = matches!(ub.convert, ParamConvert::BoolThreshold);
                return ParamDef {
                    id: ub.id.clone(),
                    name: ub.label.clone(),
                    min: ub.min,
                    max: ub.max,
                    default_value: ub.default_value,
                    whole_numbers,
                    is_toggle,
                    value_labels: None,
                    format_string: None,
                    osc_suffix: None,
                };
            }
        }
        ParamDef::default()
    }

    fn get_param(&self, index: usize) -> f32 {
        EffectInstance::get_param(self, index)
    }

    fn set_param(&mut self, index: usize, value: f32) {
        EffectInstance::set_param(self, index, value);
    }

    fn get_base_param(&self, index: usize) -> f32 {
        EffectInstance::get_base_param(self, index)
    }

    fn set_base_param(&mut self, index: usize, value: f32) {
        EffectInstance::set_base_param(self, index, value);
    }

    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        EffectInstance::find_driver(self, param_id)
    }

    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        EffectInstance::get_drivers_list(self)
    }

    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver {
        EffectInstance::create_driver(self, param_id)
    }

    fn remove_driver(&mut self, param_id: &str) {
        EffectInstance::remove_driver(self, param_id);
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

// ─── Param Envelope (ADSR modulation) ───

/// Envelope evaluation mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnvelopeMode {
    /// Classic ADSR envelope shape driven by clip timing.
    #[default]
    Adsr,
    /// Random value on each clip rising edge (walk or jump).
    Random,
}

/// ADSR / random envelope modulating a single effect or generator
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
    pub target_effect_type: EffectTypeId,
    /// Stable mapping key. Empty after legacy V1.1 deserialization
    /// until the post-load resolver fills it in from the registry.
    pub param_id: ParamId,
    pub enabled: bool,
    pub attack_beats: f32,
    pub decay_beats: f32,
    pub sustain_level: f32,
    pub release_beats: f32,
    pub target_normalized: f32,
    /// Envelope evaluation mode (ADSR or Random).
    pub mode: EnvelopeMode,
    /// When mode=Random: true = jump to fully random value, false = walk by step.
    pub random_jump: bool,
    /// Random mode range minimum (normalized 0-1). Walk/jump stays within this range.
    pub range_min: f32,
    /// Random mode range maximum (normalized 0-1). Walk/jump stays within this range.
    pub range_max: f32,
    /// Parked legacy `targetParamIndex: i32` from V1.1 deserialization
    /// or RegistryMissing fallback during post-load resolution. See
    /// [`ParameterDriver::legacy_param_index`] for the recovery
    /// invariant — same contract here.
    pub legacy_param_index: Option<i32>,
    /// Cached ADSR output (0-1) for UI display. Not serialized.
    pub current_level: f32,
    /// Current random walk position (normalized 0-1). Runtime only.
    pub walk_value: f32,
    /// Rising edge detection: was a clip active on the previous frame?
    pub was_clip_active: bool,
    /// Previous frame's elapsed beats within the active clip. Used by Random
    /// mode to detect clip restarts and loop points (elapsed decreases).
    pub last_elapsed: f32,
}

impl Serialize for ParamEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let emit_param_id = !self.param_id.is_empty();
        let emit_legacy_index = !emit_param_id && self.legacy_param_index.is_some();

        // 11 base fields + addressing field (paramId XOR targetParamIndex).
        let mut field_count = 11;
        if emit_param_id || emit_legacy_index {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("ParamEnvelope", field_count)?;
        s.serialize_field("targetEffectType", &self.target_effect_type)?;
        if emit_param_id {
            s.serialize_field("paramId", &self.param_id)?;
        } else if emit_legacy_index {
            s.serialize_field("targetParamIndex", &self.legacy_param_index.unwrap())?;
        }
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("attackBeats", &self.attack_beats)?;
        s.serialize_field("decayBeats", &self.decay_beats)?;
        s.serialize_field("sustainLevel", &self.sustain_level)?;
        s.serialize_field("releaseBeats", &self.release_beats)?;
        s.serialize_field("targetNormalized", &self.target_normalized)?;
        s.serialize_field("mode", &self.mode)?;
        s.serialize_field("randomJump", &self.random_jump)?;
        s.serialize_field("rangeMin", &self.range_min)?;
        s.serialize_field("rangeMax", &self.range_max)?;
        s.end()
    }
}

impl ParamEnvelope {
    /// Gen param envelope constructor.
    pub fn new_for_gen(param_id: impl Into<ParamId>) -> Self {
        Self {
            target_effect_type: EffectTypeId::TRANSFORM,
            param_id: param_id.into(),
            enabled: true,
            attack_beats: 0.0,
            decay_beats: 0.0,
            sustain_level: 0.0,
            release_beats: 0.0,
            target_normalized: 1.0,
            mode: EnvelopeMode::Adsr,
            random_jump: false,
            range_min: 0.0,
            range_max: 1.0,
            legacy_param_index: None,
            current_level: 0.0,
            walk_value: -1.0,
            was_clip_active: false,
            last_elapsed: -1.0,
        }
    }

    /// Effect envelope constructor.
    pub fn new_for_effect(effect_type: EffectTypeId, param_id: impl Into<ParamId>) -> Self {
        Self {
            target_effect_type: effect_type,
            param_id: param_id.into(),
            enabled: true,
            attack_beats: 0.0,
            decay_beats: 0.0,
            sustain_level: 0.0,
            release_beats: 0.0,
            target_normalized: 1.0,
            mode: EnvelopeMode::Adsr,
            random_jump: false,
            range_min: 0.0,
            range_max: 1.0,
            legacy_param_index: None,
            current_level: 0.0,
            walk_value: -1.0,
            was_clip_active: false,
            last_elapsed: -1.0,
        }
    }

    /// Calculate ADSR envelope level [0, 1] at given position within clip.
    /// Port of C# EnvelopeEvaluator.CalculateADSR().
    pub fn calculate_adsr(
        local_beat: Beats,
        clip_duration: Beats,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    ) -> f32 {
        if clip_duration <= Beats::ZERO || local_beat < Beats::ZERO {
            return 0.0;
        }

        let local_beat = local_beat.as_f32();
        let clip_duration = clip_duration.as_f32();

        let mut a = attack.max(0.0);
        let mut d = decay.max(0.0);
        let mut r = release.max(0.0);
        let s = sustain.clamp(0.0, 1.0);

        let total_adr = a + d + r;
        if total_adr > clip_duration && total_adr > 0.0 {
            let scale = clip_duration / total_adr;
            a *= scale;
            d *= scale;
            r *= scale;
        }

        let release_start = clip_duration - r;

        if local_beat < a {
            return if a > 0.0 { local_beat / a } else { 1.0 };
        }

        let decay_start = a;
        if local_beat < decay_start + d {
            let t = if d > 0.0 {
                (local_beat - decay_start) / d
            } else {
                1.0
            };
            return 1.0 - (1.0 - s) * t;
        }

        if local_beat >= release_start {
            let t = if r > 0.0 {
                ((local_beat - release_start) / r).min(1.0)
            } else {
                1.0
            };
            return s * (1.0 - t);
        }

        s
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
            #[serde(default)]
            target_effect_type: EffectTypeId,
            #[serde(default)]
            param_id: Option<String>,
            #[serde(default, rename = "targetParamIndex")]
            param_index: Option<i32>,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            attack_beats: f32,
            #[serde(default)]
            decay_beats: f32,
            #[serde(default)]
            sustain_level: f32,
            #[serde(default)]
            release_beats: f32,
            #[serde(default = "default_one")]
            target_normalized: f32,
            #[serde(default)]
            mode: EnvelopeMode,
            #[serde(default)]
            random_jump: bool,
            #[serde(default)]
            range_min: f32,
            #[serde(default = "default_one")]
            range_max: f32,
        }

        let raw = Raw::deserialize(deserializer)?;
        let (param_id, legacy_param_index) = match (raw.param_id, raw.param_index) {
            (Some(id), _) if !id.is_empty() => (Cow::Owned(id), None),
            (_, Some(idx)) => (Cow::Borrowed(""), Some(idx)),
            (_, None) => (Cow::Borrowed(""), None),
        };
        Ok(ParamEnvelope {
            target_effect_type: raw.target_effect_type,
            param_id,
            enabled: raw.enabled,
            attack_beats: raw.attack_beats,
            decay_beats: raw.decay_beats,
            sustain_level: raw.sustain_level,
            release_beats: raw.release_beats,
            target_normalized: raw.target_normalized,
            mode: raw.mode,
            random_jump: raw.random_jump,
            range_min: raw.range_min,
            range_max: raw.range_max,
            legacy_param_index,
            current_level: 0.0,
            walk_value: -1.0,
            was_clip_active: false,
            last_elapsed: -1.0,
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
        // V1.1 shape: { targetEffectType, targetParamIndex: 1, ... }.
        // Same parking pattern as ParameterDriver.
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
        assert_eq!(e.target_effect_type.as_str(), "Bloom");
    }

    #[test]
    fn envelope_deserialize_canonical_param_id() {
        let json = r#"{
            "targetEffectType": "Bloom",
            "paramId": "amount",
            "enabled": true,
            "attackBeats": 0.5
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.param_id, "amount");
        assert_eq!(e.legacy_param_index, None);
        assert!((e.attack_beats - 0.5).abs() < 1e-6);
    }

    #[test]
    fn envelope_deserialize_param_id_wins_when_both_present() {
        let json = r#"{
            "targetEffectType": "Bloom",
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
        let env = ParamEnvelope::new_for_effect(EffectTypeId::BLOOM, "amount");
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"paramId\":\"amount\""));
        assert!(
            !json.contains("targetParamIndex"),
            "Serialize must not write legacy targetParamIndex; got: {json}"
        );
        assert!(!json.contains("legacyParamIndex"));
    }

    #[test]
    fn envelope_round_trips_through_canonical_shape() {
        let env = ParamEnvelope::new_for_effect(EffectTypeId::BLOOM, "amount");
        let json = serde_json::to_string(&env).unwrap();
        let back: ParamEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_id, env.param_id);
        assert_eq!(back.target_effect_type, env.target_effect_type);
        assert_eq!(back.legacy_param_index, None);
    }

    // ── EffectInstance paramValues wire format (step 12) ──────────

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
        let fx: EffectInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.param_values.len(), 9);
        assert!((fx.param_values[4].value - 1.5).abs() < f32::EPSILON);
        // Legacy bare-f32 wire format → exposed defaults to true.
        assert!(fx.param_values.iter().all(|p| p.exposed));
        assert!(fx.base_param_values.is_some());
        assert_eq!(fx.base_param_values.as_ref().unwrap().len(), 9);
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
        let fx: EffectInstance = serde_json::from_str(json).unwrap();
        // No registry → empty Vec is the safe degraded result.
        assert!(fx.param_values.is_empty() || fx.param_values.iter().all(|p| p.value == 0.0));
    }

    #[test]
    fn effect_instance_serialize_falls_back_to_array_without_registry() {
        // No registry def → Serialize must emit Array form so the
        // value survives a round-trip through manifold-core's tests.
        let fx = EffectInstance {
            id: EffectId::new("abc12345"),
            effect_type: EffectTypeId::from_string("UnregisteredTestEffect".to_string()),
            enabled: true,
            collapsed: false,
            param_values: vec![
                ParamSlot::exposed(0.1),
                ParamSlot::exposed(0.2),
                ParamSlot::exposed(0.3),
            ],
            base_param_values: None,
            drivers: None,
            ableton_mappings: None,
            group_id: None,
            user_param_bindings: Vec::new(),
            user_param_bindings_version: 0,
            graph: None,
            graph_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
        };
        let json = serde_json::to_string(&fx).unwrap();
        // V1.3 wire emits {value, exposed} objects per element.
        assert!(
            json.contains("\"paramValues\":[{\"value\":0.1,\"exposed\":true}"),
            "Serialize without registry must emit positional Array of ParamSlot; got: {json}"
        );
        let back: EffectInstance = serde_json::from_str(&json).unwrap();
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
        let fx = EffectInstance {
            id: EffectId::new("abc12345"),
            effect_type: EffectTypeId::from_string("UnregisteredTestEffect".to_string()),
            enabled: true,
            collapsed: false,
            param_values: vec![
                ParamSlot {
                    value: 0.1,
                    exposed: true,
                },
                ParamSlot {
                    value: 0.2,
                    exposed: false,
                },
            ],
            base_param_values: None,
            drivers: None,
            ableton_mappings: None,
            group_id: None,
            user_param_bindings: Vec::new(),
            user_param_bindings_version: 0,
            graph: None,
            graph_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
        };
        let json = serde_json::to_string(&fx).unwrap();
        let back: EffectInstance = serde_json::from_str(&json).unwrap();
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
        let fx: EffectInstance = serde_json::from_str(json).unwrap();
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
        let fx = EffectInstance::new(EffectTypeId::from_string("TestEffect".to_string()));
        let json = serde_json::to_string(&fx).unwrap();
        // Verify optional None fields aren't emitted.
        assert!(!json.contains("\"baseParamValues\""));
        assert!(!json.contains("\"drivers\""));
        assert!(!json.contains("\"abletonMappings\""));
        assert!(!json.contains("\"groupId\""));
        assert!(!json.contains("\"param0\""));
        // Empty user_param_bindings must NOT emit `userParamBindings` —
        // existing fixtures (Burn V5/V4, WAYPOINTS, Liveschool) need to
        // round-trip byte-identically.
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
    // `effect_definition_registry`.

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
        let fx: EffectInstance = serde_json::from_str(json).unwrap();
        // amount should land at index 0; old_phantom_param dropped.
        assert_eq!(fx.param_values.len(), 1);
        assert!((fx.param_values[0].value - 0.7).abs() < f32::EPSILON);
    }

    // ── User-exposed parameter bindings (Phase 3 step 20) ─────────

    fn sample_user_binding(id: &str, node: &str, inner: &str) -> UserParamBinding {
        UserParamBinding {
            id: id.to_string(),
            label: inner.to_string(),
            node_handle: node.to_string(),
            inner_param: inner.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.25,
            convert: ParamConvert::Float,
        }
    }

    #[test]
    fn user_param_binding_serde_round_trip() {
        // A standalone UserParamBinding round-trips through JSON
        // without losing any field. Wire shape uses camelCase keys.
        let ub = sample_user_binding("user.uv_transform.translate.1", "uv_transform", "translate");
        let json = serde_json::to_string(&ub).unwrap();
        assert!(json.contains("\"id\":\"user.uv_transform.translate.1\""));
        assert!(json.contains("\"nodeHandle\":\"uv_transform\""));
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
    }

    #[test]
    fn effect_instance_round_trip_with_user_bindings_against_bloom() {
        // Bloom is registered in this crate's tests with one param
        // `amount`. Add two user bindings and verify the whole
        // EffectInstance round-trips through JSON, including the
        // user-binding tail values landing in the right param_values
        // slots regardless of JSON key ordering.
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
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
        assert!(json.contains("\"userParamBindings\""));
        // V1.3 wire emits {value, exposed} objects per entry.
        assert!(json.contains("\"amount\":{\"value\":0.7,\"exposed\":true}"));
        assert!(json.contains("\"user.uv_transform.translate.1\":{\"value\":0.42"));
        assert!(json.contains("\"user.mix.amount.1\":{\"value\":0.91"));

        let back: EffectInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.user_param_bindings.len(), 2);
        assert_eq!(
            back.user_param_bindings[0].id,
            "user.uv_transform.translate.1"
        );
        assert_eq!(back.user_param_bindings[1].id, "user.mix.amount.1");
        assert_eq!(
            back.param_values,
            vec![
                ParamSlot::exposed(0.7),
                ParamSlot::exposed(0.42),
                ParamSlot::exposed(0.91)
            ]
        );
        // user_param_bindings_version is not serialized — load resets to 0.
        assert_eq!(back.user_param_bindings_version, 0);
    }

    #[test]
    fn append_user_binding_grows_param_values_with_default() {
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.7)];
        fx.ensure_base_values();
        let v0 = fx.user_param_bindings_version;

        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        assert_eq!(
            fx.param_values,
            vec![ParamSlot::exposed(0.7), ParamSlot::exposed(0.25)]
        );
        assert_eq!(fx.base_param_values.as_ref().unwrap(), &vec![0.7, 0.25]);
        assert_ne!(fx.user_param_bindings_version, v0, "version must bump");
    }

    #[test]
    fn remove_user_binding_drops_corresponding_value_slot() {
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.7)];
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        fx.append_user_binding(sample_user_binding("user.c.d.1", "c", "d"));
        fx.param_values[1].value = 0.3;
        fx.param_values[2].value = 0.6;

        let removed = fx.remove_user_binding_by_id("user.a.b.1");
        assert!(removed.is_some());
        assert_eq!(fx.user_param_bindings.len(), 1);
        // Static prefix preserved + user tail compacted around the gap.
        assert_eq!(
            fx.param_values,
            vec![ParamSlot::exposed(0.7), ParamSlot::exposed(0.6)]
        );
    }

    #[test]
    fn remove_user_binding_unknown_id_returns_none() {
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.7)];
        let v0 = fx.user_param_bindings_version;
        let removed = fx.remove_user_binding_by_id("user.nope.1");
        assert!(removed.is_none());
        assert_eq!(fx.param_values, vec![ParamSlot::exposed(0.7)]);
        assert_eq!(
            fx.user_param_bindings_version, v0,
            "no-op remove must not bump version"
        );
    }

    #[test]
    fn param_id_to_value_index_resolves_static_then_user() {
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
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
        // per-instance — same EffectInstance), and align runs. The
        // user-binding tail values must survive.
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.user_param_bindings
            .push(sample_user_binding("user.a.b.1", "a", "b"));
        fx.user_param_bindings
            .push(sample_user_binding("user.c.d.1", "c", "d"));
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
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.user_param_bindings
            .push(sample_user_binding("user.a.b.1", "a", "b"));
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
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.user_param_bindings
            .push(sample_user_binding("user.a.b.1", "a", "b"));
        fx.param_values = vec![
            ParamSlot::exposed(0.7),
            ParamSlot::exposed(0.42),
            ParamSlot::exposed(0.91),
            ParamSlot::exposed(0.99),
        ]; // extra junk at tail
        fx.align_to_definition();
        // After: static=1 (kept) + user=1 (last value taken — split logic
        // pulls the tail count from user_param_bindings).
        assert_eq!(fx.param_values.len(), 2);
        assert!((fx.param_values[0].value - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn user_binding_index_lookup_by_id() {
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        fx.append_user_binding(sample_user_binding("user.c.d.1", "c", "d"));
        assert_eq!(fx.user_binding_index("user.a.b.1"), Some(0));
        assert_eq!(fx.user_binding_index("user.c.d.1"), Some(1));
        assert_eq!(fx.user_binding_index("user.nope.1"), None);
    }

    #[test]
    fn get_param_def_synthesizes_user_binding_def() {
        // ParamSource::get_param_def must return a ParamDef shaped from
        // the user binding for indices past the static count, so UI code
        // (slider rendering, OSC formatting) gets correct min/max/label.
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.append_user_binding(UserParamBinding {
            id: "user.uv.translate.1".to_string(),
            label: "Translate".to_string(),
            node_handle: "uv_transform".to_string(),
            inner_param: "translate".to_string(),
            min: -2.0,
            max: 2.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
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
        // Deserialize must consult `userParamBindings` (which appears
        // in the same JSON object, possibly *after* paramValues) to
        // route user ids to the right tail slots. Tests JSON ordering
        // independence by putting userParamBindings AFTER paramValues.
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
            "userParamBindings": [
                {
                    "id": "user.foo.bar.1", "label": "Foo",
                    "nodeHandle": "foo", "innerParam": "bar",
                    "min": 0.0, "max": 1.0, "defaultValue": 0.5
                },
                {
                    "id": "user.baz.qux.1", "label": "Baz",
                    "nodeHandle": "baz", "innerParam": "qux",
                    "min": 0.0, "max": 1.0, "defaultValue": 0.5
                }
            ]
        }"#;
        let fx: EffectInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.user_param_bindings.len(), 2);
        assert_eq!(fx.param_values.len(), 3);
        assert!((fx.param_values[0].value - 0.7).abs() < f32::EPSILON);
        assert!((fx.param_values[1].value - 0.3).abs() < f32::EPSILON);
        assert!((fx.param_values[2].value - 0.9).abs() < f32::EPSILON);
    }

    // ─── Per-instance graph override (Phase 1) ──────────────────

    #[test]
    fn new_effect_instance_has_no_graph_override() {
        let fx = EffectInstance::new(EffectTypeId::new("Mirror"));
        assert!(fx.graph.is_none());
        assert_eq!(fx.graph_version, 0);
    }

    #[test]
    fn graph_field_skipped_when_none() {
        // Existing fixtures (Liveschool, Burn, WAYPOINTS) must
        // continue to round-trip byte-identically — the new field
        // must not appear in their JSON unless explicitly set.
        let fx = EffectInstance::new(EffectTypeId::new("Mirror"));
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
                    type_id: "system.source".to_string(),
                    handle: Some("source".to_string()),
                    params: Default::default(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: std::collections::BTreeMap::new(),
                    output_canvas_scales: std::collections::BTreeMap::new(),
                },
                EffectGraphNode {
                    id: 1,
                    type_id: "node.transform".to_string(),
                    handle: Some("uv_transform".to_string()),
                    params,
                    exposed_params: Default::default(),
                    editor_pos: Some((100.0, 200.0)),
                    wgsl_source: None,
                    title: None,
                    output_formats: std::collections::BTreeMap::new(),
                    output_canvas_scales: std::collections::BTreeMap::new(),
                },
            ],
            wires: vec![EffectGraphWire {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "source".to_string(),
            }],
        };

        let mut fx = EffectInstance::new(EffectTypeId::new("Mirror"));
        fx.graph = Some(def.clone());

        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"graph\""));

        let back: EffectInstance = serde_json::from_str(&json).unwrap();
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
        let fx: EffectInstance = serde_json::from_str(json).unwrap();
        assert!(fx.graph.is_none());
    }
}
