//! The unified preset definition registry — one module for effects and
//! generators.
//!
//! Step 9 of the preset unification (`docs/PRESET_UNIFICATION_PLAN.md`):
//! the two parallel modules `effect_definition_registry` and
//! `generator_definition_registry` collapsed into this one. The value
//! type was already unified to [`crate::preset_def::PresetDef`] in step 7;
//! this step merges the two modules, deduplicates the converter / leak
//! helpers that were byte-identical mirrors, and exposes two thin
//! keyed-accessor submodules — [`effect`] and [`generator`] — over the two
//! stores.
//!
//! **Why two stores, not one.** Effects and generators are keyed by
//! distinct id types ([`EffectTypeId`] / [`GeneratorTypeId`]) and — more
//! importantly — populated from two **distinct disk sources**: the
//! renderer submits effect presets (`assets/effect-presets/`) and
//! generator presets (`assets/generator-presets/`) to two separate
//! [`inventory`] buckets. Merging into one `String`-keyed store would
//! either cross-contaminate those buckets or collide an effect id with a
//! generator id sharing a name — both touch the stable-addressing path.
//! So the module is one and the helpers are shared, but the two stores and
//! the two preset-source buckets stay distinct. The duplicated glue
//! (converters, leak helpers) was the actual fork residue; that is what
//! this step removes. Call sites change the module path only
//! (`…::effect::X` / `…::generator::X`) — the function names are
//! byte-identical to the legacy surface.

use ahash::AHashMap;
use std::collections::HashMap;
use std::sync::{LazyLock, OnceLock};

use crate::effect_graph_def::{
    AliasEntry, BindingDef, ParamSpecDef, PresetMetadata, SkipModeDef, ValueAliasEntry,
};
use crate::effect_registration::{ParamAlias, ParamValueAlias};
use crate::effect_type_id::EffectTypeId;
use crate::effects::ParamDef;
use crate::generator_type_id::GeneratorTypeId;
use crate::preset_def::{PresetDef, PresetKind};

// ─── StringParamDef ───
//
// Generator-only state carried on `PresetDef::string_param_defs`. Lived
// in the old `generator_definition_registry`; moved here with the merge.

/// A string parameter definition for generators that accept text input.
#[derive(Debug, Clone)]
pub struct StringParamDef {
    /// Display name shown in inspector.
    pub name: &'static str,
    /// Key used in `TimelineClip.string_params` map.
    pub key: &'static str,
    /// Default value for new clips.
    pub default_value: &'static str,
    /// If true, the inspector shows a dropdown selector instead of text input.
    pub use_dropdown: bool,
}

// ─── Static registries ───

static EFFECT_DEFINITIONS: LazyLock<HashMap<EffectTypeId, PresetDef>> = LazyLock::new(|| {
    let mut m: HashMap<EffectTypeId, PresetDef> = HashMap::new();
    // All effects are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/effects/*.rs).
    for meta in inventory::iter::<crate::effect_registration::EffectMetadata> {
        m.insert(meta.id.clone(), meta.to_effect_def());
    }
    // Sidecar alias submissions: attach to the matching def. Built
    // separately from `EffectMetadata` so effects without aliases
    // (the common case) don't need to spell out an empty slice in
    // their primary submission. See `effect_registration::EffectAliasMetadata`.
    for alias_meta in inventory::iter::<crate::effect_registration::EffectAliasMetadata> {
        if let Some(def) = m.get_mut(&alias_meta.id) {
            def.legacy_param_aliases = alias_meta.aliases;
        }
    }
    // Same pattern for **value** aliases — slot-value migration tables
    // applied at project load time. See
    // `effect_registration::EffectValueAliasMetadata`.
    for alias_meta in inventory::iter::<crate::effect_registration::EffectValueAliasMetadata> {
        if let Some(def) = m.get_mut(&alias_meta.id) {
            def.legacy_value_aliases = alias_meta.aliases;
        }
    }
    // JSON-loaded presets (§11 unified-registry migration). Each entry in
    // `effect::loaded_preset_metadata()` is converted to a `PresetDef` and
    // inserted — a JSON-loaded preset wins over an inventory submission
    // for the same id. Post-§11 every shipping effect lives in JSON; the
    // inventory loop above only fires for tests that submit synthetic
    // `EffectMetadata` entries.
    for preset in effect::loaded_preset_metadata() {
        m.insert(
            preset.id.clone(),
            preset_metadata_to_def(preset, PresetKind::Effect),
        );
    }
    m
});

static GENERATOR_DEFINITIONS: LazyLock<HashMap<GeneratorTypeId, PresetDef>> = LazyLock::new(|| {
    let mut m: HashMap<GeneratorTypeId, PresetDef> = HashMap::new();

    // ── None ──
    m.insert(
        GeneratorTypeId::NONE,
        PresetDef {
            kind: PresetKind::Generator,
            display_name: "None".to_string(),
            is_line_based: false,
            param_count: 0,
            param_defs: Vec::new(),
            string_param_defs: Vec::new(),
            osc_prefix: None,
            id_to_index: AHashMap::new(),
            param_ids: Vec::new(),
            legacy_param_aliases: &[],
            legacy_value_aliases: &[],
        },
    );

    // All other generators are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/generators/*.rs).
    for meta in inventory::iter::<crate::generator_registration::GeneratorMetadata> {
        m.insert(meta.id.clone(), meta.to_generator_def());
    }
    // Sidecar alias submissions for generators. See parallel effect path.
    for alias_meta in inventory::iter::<crate::generator_registration::GeneratorAliasMetadata> {
        if let Some(def) = m.get_mut(&alias_meta.id) {
            def.legacy_param_aliases = alias_meta.aliases;
        }
    }
    // JSON-loaded presets (§11). A JSON-loaded preset wins over an
    // inventory submission for the same id — same dual-source pattern as
    // the effect side, so a generator that ships with a bundled JSON
    // preset *and* a legacy inventory entry uses the JSON as the
    // canonical schema. Eliminates the inventory-vs-preset positional
    // layout drift class structurally.
    for preset in generator::loaded_preset_metadata() {
        let gen_id = GeneratorTypeId::from_string(preset.id.as_str().to_string());
        m.insert(gen_id, preset_metadata_to_def(preset, PresetKind::Generator));
    }
    m
});

static MAX_GEN_PARAM_COUNT: LazyLock<usize> = LazyLock::new(|| {
    GENERATOR_DEFINITIONS
        .values()
        .map(|d| d.param_count)
        .max()
        .unwrap_or(0)
});

// ─── Effect accessors ───
//
// Thin `EffectTypeId`-keyed view over [`EFFECT_DEFINITIONS`]. Function
// names match the legacy `effect_definition_registry` surface exactly —
// only the module path moved.

pub mod effect {
    use super::*;

    /// Re-export for callers within this module's namespace. Canonical
    /// home is [`crate::effect_registration::resolve_param_alias`].
    pub use crate::effect_registration::resolve_param_alias;

    /// Get the definition for an effect type. Panics if not found.
    pub fn get(effect_type: &EffectTypeId) -> &'static PresetDef {
        EFFECT_DEFINITIONS.get(effect_type).unwrap_or_else(|| {
            panic!(
                "EffectDefinitionRegistry: unknown EffectTypeId '{}'",
                effect_type
            )
        })
    }

    /// Try to get the definition for an effect type.
    pub fn try_get(effect_type: &EffectTypeId) -> Option<&'static PresetDef> {
        EFFECT_DEFINITIONS.get(effect_type)
    }

    /// Translate a stable `ParamSpec::id` into the param's storage index
    /// for the given effect type. Returns `None` if the effect or id is
    /// unknown.
    ///
    /// Hot-path: every per-frame addressing dispatch (driver, envelope,
    /// Ableton update, OSC route) goes through this. The lookup is one
    /// `&str → usize` `AHashMap::get` (~50ns); the map is built once when
    /// the registry initializes.
    pub fn param_id_to_index(effect_type: &EffectTypeId, id: &str) -> Option<usize> {
        EFFECT_DEFINITIONS
            .get(effect_type)?
            .id_to_index
            .get(id)
            .copied()
    }

    /// Reverse of [`param_id_to_index`]: storage index → param id. Each id
    /// is the original `&'static str` from the `ParamSpec` registration.
    /// Returns `None` if the effect or index is out of range, or the slot
    /// has an empty id (V1 fixture / pre-step-6 entry).
    pub fn param_index_to_id(effect_type: &EffectTypeId, index: usize) -> Option<&'static str> {
        let def = EFFECT_DEFINITIONS.get(effect_type)?;
        let id = def.param_ids.get(index)?.as_str();
        if id.is_empty() { None } else { Some(id) }
    }

    /// Create a new EffectInstance with default parameter values from the
    /// registry.
    pub fn create_default(effect_type: &EffectTypeId) -> crate::effects::EffectInstance {
        let def = get(effect_type);
        let mut inst = crate::effects::EffectInstance::new(effect_type.clone());
        for (i, pd) in def.param_defs.iter().enumerate() {
            inst.set_base_param(i, pd.default_value);
        }
        inst
    }

    /// Format a parameter value for display. Named labels take priority,
    /// then wholeNumbers round, then F2.
    pub fn format_value(effect_type: &EffectTypeId, param_index: usize, value: f32) -> String {
        let def = match try_get(effect_type) {
            Some(d) if param_index < d.param_count => d,
            _ => return format!("{:.2}", value),
        };
        let pd = &def.param_defs[param_index];
        if let Some(ref labels) = pd.value_labels {
            let idx = (value.round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
            return labels[idx].clone();
        }
        if pd.whole_numbers {
            return format!("{}", value.round() as i32);
        }
        format!("{:.2}", value)
    }

    /// Get the OSC address for a master effect parameter.
    ///
    /// Unified scheme (preset unification, 2026-05):
    /// `/master/{prefix}/{param_id}` — slash-separated path segments,
    /// stable `param_id` as the leaf. Generators share the identical shape
    /// (minus `/master`), so external senders address effects and
    /// generators with one convention. Returns `None` if the effect has no
    /// OSC prefix or the slot has no stable id.
    pub fn get_osc_address(effect_type: &EffectTypeId, param_index: usize) -> Option<String> {
        let def = try_get(effect_type)?;
        let prefix = def.osc_prefix.as_deref()?;
        let param_id = def.param_ids.get(param_index)?;
        if param_id.is_empty() {
            return None;
        }
        Some(format!("/master/{}/{}", prefix, param_id))
    }

    /// Get the OSC address for a layer effect parameter scoped to a
    /// specific layer. Unified scheme: `/layer/{layerId}/{prefix}/{param_id}`.
    pub fn get_osc_address_for_layer(
        effect_type: &EffectTypeId,
        layer_id: &str,
        param_index: usize,
    ) -> Option<String> {
        if layer_id.is_empty() {
            return None;
        }
        let def = try_get(effect_type)?;
        let prefix = def.osc_prefix.as_deref()?;
        let param_id = def.param_ids.get(param_index)?;
        if param_id.is_empty() {
            return None;
        }
        Some(format!("/layer/{}/{}/{}", layer_id, prefix, param_id))
    }

    /// Get default parameter values for an effect type as freshly-allocated
    /// `ParamSlot` entries, all `exposed: true`.
    pub fn get_defaults(effect_type: &EffectTypeId) -> Vec<crate::effects::ParamSlot> {
        let def = get(effect_type);
        def.param_defs
            .iter()
            .map(|p| crate::effects::ParamSlot::exposed(p.default_value))
            .collect()
    }

    /// Get all registered effect types (unordered).
    pub fn get_all_effect_types() -> Vec<EffectTypeId> {
        EFFECT_DEFINITIONS.keys().cloned().collect()
    }

    /// Get all registered effect types sorted by display name.
    pub fn get_all_effect_types_sorted() -> Vec<EffectTypeId> {
        let mut list: Vec<EffectTypeId> = EFFECT_DEFINITIONS.keys().cloned().collect();
        list.sort_by_key(|t| t.as_str().to_string());
        list
    }

    /// JSON-loaded **effect** preset metadata for the [`EFFECT_DEFINITIONS`]
    /// registry. Each [`PresetSource`] submission contributes a function
    /// pointer producing a `Vec<PresetMetadata>`. The renderer submits one
    /// source pointing at `loaded_presets_from_bundled` (effect preset
    /// JSON). Sources are invoked once on first access and cached for the
    /// process lifetime.
    pub fn loaded_preset_metadata() -> &'static [PresetMetadata] {
        static CACHE: OnceLock<Vec<PresetMetadata>> = OnceLock::new();
        CACHE.get_or_init(|| {
            let mut all = Vec::new();
            for source in inventory::iter::<PresetSource> {
                all.extend((source.load)());
            }
            all
        })
    }

    /// Inventory submission point for JSON-loaded **effect** preset
    /// metadata. Kept distinct from the generator bucket so an effect
    /// preset never lands in the generator store.
    ///
    /// Pattern:
    /// ```ignore
    /// inventory::submit! {
    ///     manifold_core::preset_definition_registry::effect::PresetSource {
    ///         load: my_loader_function,
    ///     }
    /// }
    /// ```
    pub struct PresetSource {
        pub load: fn() -> Vec<PresetMetadata>,
    }

    inventory::collect!(PresetSource);

    /// Convert a parsed [`PresetMetadata`] into the picker-side
    /// [`crate::effect_type_registry::EffectTypeRegistration`].
    pub fn preset_metadata_to_type_registration(
        meta: &PresetMetadata,
    ) -> crate::effect_type_registry::EffectTypeRegistration {
        crate::effect_type_registry::EffectTypeRegistration {
            id: meta.id.clone(),
            display_name: Box::leak(meta.display_name.clone().into_boxed_str()),
            category: Box::leak(meta.category.clone().into_boxed_str()),
            available: meta.available,
        }
    }

    /// Convert a parsed [`PresetMetadata`] into a `PresetDef` (kind =
    /// `Effect`). Thin wrapper over [`super::preset_metadata_to_def`] kept
    /// for call-site name-stability with the legacy
    /// `preset_metadata_to_effect_def`.
    pub fn preset_metadata_to_effect_def(meta: &PresetMetadata) -> PresetDef {
        super::preset_metadata_to_def(meta, PresetKind::Effect)
    }
}

// ─── Generator accessors ───
//
// Thin `GeneratorTypeId`-keyed view over [`GENERATOR_DEFINITIONS`].
// Function names match the legacy `generator_definition_registry` surface
// exactly — only the module path moved.

pub mod generator {
    use super::*;

    pub fn get(gen_type: &GeneratorTypeId) -> &'static PresetDef {
        GENERATOR_DEFINITIONS.get(gen_type).unwrap_or_else(|| {
            panic!(
                "GeneratorDefinitionRegistry: unknown GeneratorTypeId '{}'",
                gen_type
            )
        })
    }

    pub fn try_get(gen_type: &GeneratorTypeId) -> Option<&'static PresetDef> {
        GENERATOR_DEFINITIONS.get(gen_type)
    }

    /// Translate a stable `ParamSpec::id` into the param's storage index
    /// for the given generator type. Returns `None` if the generator or id
    /// is unknown. Mirrors [`super::effect::param_id_to_index`].
    pub fn param_id_to_index(gen_type: &GeneratorTypeId, id: &str) -> Option<usize> {
        GENERATOR_DEFINITIONS
            .get(gen_type)?
            .id_to_index
            .get(id)
            .copied()
    }

    /// Reverse of [`param_id_to_index`]. Returns the original `&'static
    /// str` from the `ParamSpec` registration. `None` if out of range or
    /// the slot has an empty id (V1 fixture / pre-step-6 entry).
    pub fn param_index_to_id(gen_type: &GeneratorTypeId, index: usize) -> Option<&'static str> {
        let def = GENERATOR_DEFINITIONS.get(gen_type)?;
        let id = def.param_ids.get(index)?.as_str();
        if id.is_empty() { None } else { Some(id) }
    }

    pub fn is_line_based(gen_type: &GeneratorTypeId) -> bool {
        GENERATOR_DEFINITIONS
            .get(gen_type)
            .is_some_and(|d| d.is_line_based)
    }

    pub fn get_param_def(gen_type: &GeneratorTypeId, index: usize) -> ParamDef {
        let Some(def) = GENERATOR_DEFINITIONS.get(gen_type) else {
            return ParamDef::default();
        };
        if index >= def.param_count {
            return ParamDef::default();
        }
        def.param_defs[index].clone()
    }

    pub fn get_defaults(gen_type: &GeneratorTypeId) -> Vec<f32> {
        let Some(def) = GENERATOR_DEFINITIONS.get(gen_type) else {
            return Vec::new();
        };
        def.param_defs.iter().map(|p| p.default_value).collect()
    }

    pub fn format_gen_value(gen_type: &GeneratorTypeId, index: usize, value: f32) -> String {
        let pd = get_param_def(gen_type, index);

        // Labels take priority
        if let Some(ref labels) = pd.value_labels {
            let idx = (value.round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
            return labels[idx].clone();
        }

        // Whole numbers next
        if pd.whole_numbers {
            return format!("{}", value.round() as i32);
        }

        // Format string next
        if let Some(ref fmt) = pd.format_string {
            return format_float_with_format_string(value, fmt);
        }

        // Default: F2
        format!("{:.2}", value)
    }

    pub fn get_osc_address(gen_type: &GeneratorTypeId, index: usize) -> Option<String> {
        let def = GENERATOR_DEFINITIONS.get(gen_type)?;
        let prefix = def.osc_prefix.as_deref()?;
        let param_id = def.param_ids.get(index)?;
        if param_id.is_empty() {
            return None;
        }
        Some(format!("/{}/{}", prefix, param_id))
    }

    /// Unified with the effect scheme (preset unification, 2026-05):
    /// `/layer/{layerId}/{prefix}/{param_id}`. The legacy `/gen/` namespace
    /// segment is dropped — disambiguation between an effect and a
    /// generator sharing a layer is a naming-convention concern (distinct
    /// osc_prefixes), not an addressing one.
    pub fn get_osc_address_for_layer(
        gen_type: &GeneratorTypeId,
        layer_id: &str,
        index: usize,
    ) -> Option<String> {
        if layer_id.is_empty() {
            return None;
        }
        let def = GENERATOR_DEFINITIONS.get(gen_type)?;
        let prefix = def.osc_prefix.as_deref()?;
        let param_id = def.param_ids.get(index)?;
        if param_id.is_empty() {
            return None;
        }
        Some(format!("/layer/{}/{}/{}", layer_id, prefix, param_id))
    }

    pub fn try_get_gen_param_range(gen_type: &GeneratorTypeId, index: usize) -> Option<(f32, f32)> {
        let def = GENERATOR_DEFINITIONS.get(gen_type)?;
        if index >= def.param_count {
            return None;
        }
        let pd = &def.param_defs[index];
        Some((pd.min, pd.max))
    }

    pub fn clamp_param(gen_type: &GeneratorTypeId, index: usize, value: f32) -> f32 {
        let Some(def) = GENERATOR_DEFINITIONS.get(gen_type) else {
            return value;
        };
        if index >= def.param_count {
            return value;
        }
        let pd = &def.param_defs[index];
        value.clamp(pd.min, pd.max)
    }

    pub fn max_param_count() -> usize {
        *MAX_GEN_PARAM_COUNT
    }

    /// JSON-loaded **generator** preset metadata for the
    /// [`GENERATOR_DEFINITIONS`] registry. Mirror of
    /// [`super::effect::loaded_preset_metadata`] over the generator disk
    /// bucket. The renderer submits one source pointing at
    /// `loaded_generator_presets_from_bundled`.
    pub fn loaded_preset_metadata() -> &'static [PresetMetadata] {
        static CACHE: OnceLock<Vec<PresetMetadata>> = OnceLock::new();
        CACHE.get_or_init(|| {
            let mut all = Vec::new();
            for source in inventory::iter::<PresetSource> {
                all.extend((source.load)());
            }
            all
        })
    }

    /// Inventory submission point for JSON-loaded **generator** preset
    /// metadata. Mirror of [`super::effect::PresetSource`] over the
    /// generator bucket — kept distinct so a generator preset never lands
    /// in the effect store.
    pub struct PresetSource {
        pub load: fn() -> Vec<PresetMetadata>,
    }

    inventory::collect!(PresetSource);

    /// Convert a [`PresetMetadata`] into the picker-side
    /// [`crate::generator_type_registry::GeneratorTypeRegistration`].
    pub fn preset_metadata_to_type_registration(
        meta: &PresetMetadata,
    ) -> crate::generator_type_registry::GeneratorTypeRegistration {
        crate::generator_type_registry::GeneratorTypeRegistration {
            id: GeneratorTypeId::from_string(meta.id.as_str().to_string()),
            display_name: leak_str(&meta.display_name),
            available: meta.available,
        }
    }

    /// Convert a parsed [`PresetMetadata`] into a `PresetDef` (kind =
    /// `Generator`). Thin wrapper over [`super::preset_metadata_to_def`]
    /// kept for call-site name-stability with the legacy
    /// `preset_metadata_to_generator_def`.
    pub fn preset_metadata_to_generator_def(meta: &PresetMetadata) -> PresetDef {
        super::preset_metadata_to_def(meta, PresetKind::Generator)
    }
}

// ─── Format helper (shared) ───

fn format_float_with_format_string(value: f32, fmt: &str) -> String {
    match fmt {
        "F0" => format!("{:.0}", value),
        "F1" => format!("{:.1}", value),
        "F2" => format!("{:.2}", value),
        "F3" => format!("{:.3}", value),
        "F4" => format!("{:.4}", value),
        _ => format!("{:.2}", value),
    }
}

// ─── Shared converters ───
//
// §11 of `docs/PRIMITIVE_LIBRARY_DESIGN.md` describes the migration from
// inventory-submitted metadata to JSON-authoritative preset files. The
// two `PresetSource` buckets (`effect::PresetSource` /
// `generator::PresetSource`) stay separate; everything below is shared.

/// Convert a parsed [`PresetMetadata`] (JSON wire shape) into the unified
/// [`PresetDef`]. The `kind` argument is the only branch: a generator def
/// carries `is_line_based` from the metadata and an empty value-alias
/// table; an effect def forces `is_line_based = false` and leaks its
/// value-alias table. Both leak only the `'static` alias tables via
/// `Box::leak`, bounded by the (finite) shipping preset count, done once
/// at startup when the registries initialise.
pub fn preset_metadata_to_def(meta: &PresetMetadata, kind: PresetKind) -> PresetDef {
    let param_defs: Vec<ParamDef> = meta.params.iter().map(param_spec_def_to_param_def).collect();
    let param_count = param_defs.len();
    let id_to_index: AHashMap<String, usize> = meta
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.id.is_empty())
        .map(|(i, p)| (p.id.clone(), i))
        .collect();
    let param_ids: Vec<String> = meta.params.iter().map(|p| p.id.clone()).collect();
    let (is_line_based, legacy_value_aliases): (
        bool,
        &'static [(&'static str, &'static [ParamValueAlias])],
    ) = match kind {
        // Effects carry the slot-value migration table; effect presets are
        // never line-based.
        PresetKind::Effect => (false, leak_value_alias_table(&meta.value_aliases)),
        // Generators may be line-based; they carry no value-alias table yet
        // (capability gap, see PRESET_UNIFICATION_PLAN Step 9 follow-ups).
        PresetKind::Generator => (meta.is_line_based, &[]),
    };
    PresetDef {
        kind,
        display_name: meta.display_name.clone(),
        param_count,
        param_defs,
        // String params live outside the v2 PresetMetadata schema for now.
        // Generators that need them (Text, NumberStation, …) keep their
        // inventory submission; the §11 path applies to graph-backed
        // presets without a string-param surface.
        string_param_defs: Vec::new(),
        osc_prefix: Some(meta.osc_prefix.clone()),
        is_line_based,
        id_to_index,
        param_ids,
        legacy_param_aliases: leak_alias_table(&meta.param_aliases),
        legacy_value_aliases,
    }
}

fn param_spec_def_to_param_def(p: &ParamSpecDef) -> ParamDef {
    ParamDef {
        id: p.id.clone(),
        name: p.name.clone(),
        min: p.min,
        max: p.max,
        default_value: p.default_value,
        whole_numbers: p.whole_numbers,
        is_toggle: p.is_toggle,
        is_trigger: p.is_trigger,
        value_labels: if p.value_labels.is_empty() {
            None
        } else {
            Some(p.value_labels.clone())
        },
        format_string: p.format_string.clone(),
        osc_suffix: if p.osc_suffix.is_empty() {
            None
        } else {
            Some(p.osc_suffix.clone())
        },
    }
}

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn leak_alias_table(entries: &[AliasEntry]) -> &'static [ParamAlias] {
    let v: Vec<ParamAlias> = entries
        .iter()
        .map(|e| {
            let old: &'static str = leak_str(&e.old);
            let new: Option<&'static str> = e.new.as_deref().map(leak_str);
            (old, new)
        })
        .collect();
    Box::leak(v.into_boxed_slice())
}

fn leak_value_alias_table(
    entries: &[ValueAliasEntry],
) -> &'static [(&'static str, &'static [ParamValueAlias])] {
    let v: Vec<(&'static str, &'static [ParamValueAlias])> = entries
        .iter()
        .map(|e| {
            let param_id: &'static str = leak_str(&e.param_id);
            let mapping: &'static [ParamValueAlias] =
                Box::leak(e.mapping.clone().into_boxed_slice());
            (param_id, mapping)
        })
        .collect();
    Box::leak(v.into_boxed_slice())
}

// Silence unused-warnings for items still in plumbing. The `#[allow]` is
// removed once the items are wired to a non-test consumer.
#[allow(dead_code)]
fn _phase_b_keepalive(_: &BindingDef, _: &SkipModeDef) {}

#[cfg(test)]
mod tests {
    use super::effect::*;
    use super::generator;
    use super::*;
    use crate::effect_registration::EffectMetadata;
    use crate::generator_registration::ParamSpec;

    // Test-only inventory submissions — manifold-renderer isn't linked in
    // manifold-core unit tests, so we register minimal test fixtures here.
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::TRANSFORM,
            display_name: "Transform",
            category: "Spatial",
            available: true,
            osc_prefix: "transform",
            legacy_discriminant: Some(0),
            params: &[
                ParamSpec::continuous("x", "X", -1.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("y", "Y", -1.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("zoom", "Zoom", 0.1, 5.0, 1.0, "F2", ""),
                ParamSpec::continuous("rot", "Rot", -180.0, 180.0, 0.0, "F2", ""),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::BLOOM,
            display_name: "Bloom",
            category: "Post-Process",
            available: true,
            osc_prefix: "bloom",
            legacy_discriminant: Some(12),
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 5.0, 0.187, "F2", ""),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::DITHER,
            display_name: "Dither",
            category: "Post-Process",
            available: true,
            osc_prefix: "dither",
            legacy_discriminant: Some(18),
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::whole_labels("algo", "Algo", 0.0, 5.0, 0.0, &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"], "Algorithm"),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::KALEIDOSCOPE,
            display_name: "Kaleidoscope",
            category: "Post-Process",
            available: true,
            osc_prefix: "kaleidoscope",
            legacy_discriminant: Some(14),
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::whole("segs", "Segs", 2.0, 16.0, 6.0, "Segments"),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: EffectTypeId::INFINITE_ZOOM,
            display_name: "Infinite Zoom",
            category: "Post-Process",
            available: false,
            osc_prefix: "infiniteZoom",
            legacy_discriminant: Some(13),
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("sharp", "Sharp", 0.0, 1.0, 0.5, "F2", "Sharpness"),
            ],
        }
    }

    #[test]
    fn test_param_counts_match() {
        // Check all registered effects have consistent param counts
        for (_, def) in EFFECT_DEFINITIONS.iter() {
            assert_eq!(
                def.param_count,
                def.param_defs.len(),
                "param_count mismatch for {}: declared {} but has {} defs",
                def.display_name,
                def.param_count,
                def.param_defs.len()
            );
        }
    }

    #[test]
    fn test_create_default_bloom() {
        let inst = create_default(&EffectTypeId::BLOOM);
        assert_eq!(*inst.effect_type(), EffectTypeId::BLOOM);
        assert!(inst.enabled);
        assert_eq!(inst.param_values.len(), 1);
        assert!((inst.param_values[0].value - 0.187).abs() < 1e-6);
    }

    #[test]
    fn test_format_value_labels() {
        let s = format_value(&EffectTypeId::DITHER, 1, 2.0);
        assert_eq!(s, "Lines");
    }

    #[test]
    fn test_format_value_whole() {
        let s = format_value(&EffectTypeId::KALEIDOSCOPE, 1, 6.7);
        assert_eq!(s, "7");
    }

    #[test]
    fn test_format_value_continuous() {
        let s = format_value(&EffectTypeId::BLOOM, 0, 0.5);
        assert_eq!(s, "0.50");
    }

    #[test]
    fn test_osc_address_master() {
        // Unified scheme: /master/{prefix}/{param_id}. Bloom param 0 id = "amount".
        let addr = get_osc_address(&EffectTypeId::BLOOM, 0);
        assert_eq!(addr, Some("/master/bloom/amount".to_string()));
    }

    #[test]
    fn test_osc_address_master_param() {
        // InfiniteZoom param 1 id = "sharp" (slash-separated, stable id leaf —
        // not the legacy concat "/master/infiniteZoomSharpness").
        let addr = get_osc_address(&EffectTypeId::INFINITE_ZOOM, 1);
        assert_eq!(addr, Some("/master/infiniteZoom/sharp".to_string()));
    }

    #[test]
    fn test_osc_address_uniform_for_param_zero_and_beyond() {
        // Every param with a stable id gets an address now — no param-0
        // special case, no "no suffix → None". Transform ids: x, y, zoom, rot.
        assert_eq!(
            get_osc_address(&EffectTypeId::TRANSFORM, 0),
            Some("/master/transform/x".to_string())
        );
        assert_eq!(
            get_osc_address(&EffectTypeId::TRANSFORM, 1),
            Some("/master/transform/y".to_string())
        );
        // Out-of-range index still returns None.
        assert_eq!(get_osc_address(&EffectTypeId::TRANSFORM, 99), None);
    }

    #[test]
    fn test_osc_address_layer() {
        let addr = get_osc_address_for_layer(&EffectTypeId::BLOOM, "layer_1", 0);
        assert_eq!(addr, Some("/layer/layer_1/bloom/amount".to_string()));
    }

    #[test]
    fn test_sorted_types() {
        let sorted = get_all_effect_types_sorted();
        for i in 1..sorted.len() {
            assert!(sorted[i - 1].as_str() <= sorted[i].as_str());
        }
    }

    #[test]
    fn param_id_to_index_resolves_known_ids() {
        // Bloom: single param with id "amount".
        assert_eq!(
            param_id_to_index(&EffectTypeId::BLOOM, "amount"),
            Some(0),
            "bloom.amount must resolve to slot 0"
        );

        // Transform: 4 params in registration order (x, y, zoom, rot).
        assert_eq!(param_id_to_index(&EffectTypeId::TRANSFORM, "x"), Some(0));
        assert_eq!(param_id_to_index(&EffectTypeId::TRANSFORM, "y"), Some(1));
        assert_eq!(param_id_to_index(&EffectTypeId::TRANSFORM, "zoom"), Some(2));
        assert_eq!(param_id_to_index(&EffectTypeId::TRANSFORM, "rot"), Some(3));
    }

    #[test]
    fn param_id_to_index_unknown_id_returns_none() {
        assert_eq!(
            param_id_to_index(&EffectTypeId::BLOOM, "nope"),
            None,
            "unknown id must return None, not a stale or default index"
        );
    }

    #[test]
    fn param_id_to_index_unknown_effect_returns_none() {
        let phantom = EffectTypeId::from_string("not-a-real-effect-id".to_string());
        assert_eq!(param_id_to_index(&phantom, "amount"), None);
    }

    #[test]
    fn param_index_to_id_round_trips() {
        // For each test-fixture effect, every (id → index) entry must
        // round-trip back through param_index_to_id.
        for effect in [
            EffectTypeId::TRANSFORM,
            EffectTypeId::BLOOM,
            EffectTypeId::DITHER,
            EffectTypeId::KALEIDOSCOPE,
        ] {
            let def = get(&effect);
            for (i, pd) in def.param_defs.iter().enumerate() {
                if pd.id.is_empty() {
                    continue;
                }
                assert_eq!(
                    param_id_to_index(&effect, &pd.id),
                    Some(i),
                    "{}::{} must resolve to {}",
                    effect.as_str(),
                    pd.id,
                    i
                );
                assert_eq!(
                    param_index_to_id(&effect, i),
                    Some(pd.id.as_str()),
                    "{} index {} must reverse to {}",
                    effect.as_str(),
                    i,
                    pd.id
                );
            }
        }
    }

    #[test]
    fn param_id_to_index_keys_match_param_count() {
        // Map size must equal the number of params (no dupes, no empties).
        // This catches accidental collisions when adding new effects.
        for effect_type in get_all_effect_types() {
            let def = get(&effect_type);
            let non_empty_id_count = def.param_defs.iter().filter(|pd| !pd.id.is_empty()).count();
            assert_eq!(
                def.id_to_index.len(),
                non_empty_id_count,
                "{}: id_to_index size mismatch — possible duplicate or empty ids",
                effect_type.as_str()
            );
        }
    }

    // ── ParamAlias resolution (step 15) ────────────────────────────

    #[test]
    fn resolve_param_alias_passes_through_current_id() {
        // No alias entry for "amount" → returns it unchanged.
        let aliases: &[crate::effect_registration::ParamAlias] =
            &[("old_thing", Some("new_thing"))];
        assert_eq!(resolve_param_alias(aliases, "amount"), Some("amount"));
    }

    #[test]
    fn resolve_param_alias_renames() {
        let aliases: &[crate::effect_registration::ParamAlias] = &[("cv_flow", Some("flow"))];
        assert_eq!(resolve_param_alias(aliases, "cv_flow"), Some("flow"));
    }

    #[test]
    fn resolve_param_alias_chains_renames() {
        // Two-hop rename: a → b → c.
        let aliases: &[crate::effect_registration::ParamAlias] =
            &[("a", Some("b")), ("b", Some("c"))];
        assert_eq!(resolve_param_alias(aliases, "a"), Some("c"));
    }

    #[test]
    fn resolve_param_alias_drop_returns_none() {
        let aliases: &[crate::effect_registration::ParamAlias] = &[("face", None)];
        assert_eq!(resolve_param_alias(aliases, "face"), None);
    }

    #[test]
    fn resolve_param_alias_chain_to_drop_returns_none() {
        // Renamed once, then dropped: a → b → None.
        let aliases: &[crate::effect_registration::ParamAlias] = &[("a", Some("b")), ("b", None)];
        assert_eq!(resolve_param_alias(aliases, "a"), None);
    }

    #[test]
    fn resolve_param_alias_breaks_cycle() {
        // Pathological: a → b → a (constructor accident). Should
        // bail rather than infinite-loop.
        let aliases: &[crate::effect_registration::ParamAlias] =
            &[("a", Some("b")), ("b", Some("a"))];
        assert_eq!(resolve_param_alias(aliases, "a"), None);
    }

    #[test]
    fn resolve_param_alias_empty_table_passes_through() {
        let aliases: &[crate::effect_registration::ParamAlias] = &[];
        assert_eq!(resolve_param_alias(aliases, "amount"), Some("amount"));
    }

    #[test]
    fn all_default_effect_defs_have_empty_alias_table() {
        // Step 15 ships with no actual renames yet — every effect's
        // alias table should be empty. New entries land via sidecar
        // `EffectAliasMetadata` submissions.
        for effect_type in get_all_effect_types() {
            let def = get(&effect_type);
            assert!(
                def.legacy_param_aliases.is_empty(),
                "{} unexpectedly has alias entries: {:?}",
                effect_type.as_str(),
                def.legacy_param_aliases
            );
        }
    }

    // ── §11 block 2: PresetMetadata → EffectDef converter ──────────

    use crate::effect_graph_def::{
        AliasEntry, BindingDef, BindingTarget, ParamSpecDef, PresetMetadata, SkipModeDef,
        ValueAliasEntry,
    };
    use crate::effects::ParamConvert;

    fn bloom_preset_metadata() -> PresetMetadata {
        PresetMetadata {
            id: EffectTypeId::new("BloomFromJson"),
            display_name: "Bloom (from JSON)".to_string(),
            category: "Filmic".to_string(),
            osc_prefix: "bloom_from_json".to_string(),
            legacy_discriminant: Some(12),
            available: true,
            is_line_based: false,
            params: vec![ParamSpecDef {
                id: "amount".to_string(),
                name: "Amount".to_string(),
                min: 0.0,
                max: 5.0,
                default_value: 0.5,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: Some("F2".to_string()),
                osc_suffix: String::new(),
            }],
            bindings: vec![BindingDef {
                id: "amount".to_string(),
                label: "Amount".to_string(),
                default_value: 0.5,
                target: BindingTarget::Node {
                    node_id: crate::NodeId::new("bloom_node"),
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
            param_aliases: vec![AliasEntry {
                old: "intensity".to_string(),
                new: Some("amount".to_string()),
            }],
            value_aliases: vec![ValueAliasEntry {
                param_id: "amount".to_string(),
                mapping: vec![(0, 1)],
            }],
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        }
    }

    #[test]
    fn preset_metadata_converts_to_effect_def() {
        let meta = bloom_preset_metadata();
        let def = preset_metadata_to_def(&meta, PresetKind::Effect);

        assert_eq!(def.display_name, "Bloom (from JSON)");
        assert_eq!(def.osc_prefix.as_deref(), Some("bloom_from_json"));
        assert_eq!(def.param_count, 1);
        assert_eq!(def.param_defs.len(), 1);
        assert_eq!(def.param_defs[0].id, "amount");
        assert_eq!(def.param_defs[0].name, "Amount");
        assert!((def.param_defs[0].default_value - 0.5).abs() < 1e-6);
        assert_eq!(def.id_to_index.get("amount"), Some(&0));
        assert_eq!(def.param_ids, vec!["amount"]);

        assert_eq!(def.legacy_param_aliases.len(), 1);
        assert_eq!(def.legacy_param_aliases[0].0, "intensity");
        assert_eq!(def.legacy_param_aliases[0].1, Some("amount"));

        assert_eq!(def.legacy_value_aliases.len(), 1);
        assert_eq!(def.legacy_value_aliases[0].0, "amount");
        assert_eq!(def.legacy_value_aliases[0].1, &[(0, 1)]);
    }

    /// The JSON converter and the inventory converter must produce
    /// equivalent `EffectDef`s for the same effect shape.
    #[test]
    fn preset_metadata_and_effect_metadata_produce_equivalent_def() {
        static INV_PARAMS: [ParamSpec; 1] =
            [ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", "")];
        let inv_meta = EffectMetadata {
            id: EffectTypeId::new("ParityCheck"),
            display_name: "Parity Check",
            category: "Filmic",
            available: true,
            osc_prefix: "parity_check",
            legacy_discriminant: None,
            params: &INV_PARAMS,
        };
        let inv_def = inv_meta.to_effect_def();

        let json_meta = PresetMetadata {
            id: EffectTypeId::new("ParityCheck"),
            display_name: "Parity Check".to_string(),
            category: "Filmic".to_string(),
            osc_prefix: "parity_check".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: vec![ParamSpecDef {
                id: "amount".to_string(),
                name: "Amount".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 0.5,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: Some("F2".to_string()),
                osc_suffix: String::new(),
            }],
            bindings: Vec::new(),
            skip_mode: SkipModeDef::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        };
        let json_def = preset_metadata_to_def(&json_meta, PresetKind::Effect);

        assert_eq!(inv_def.display_name, json_def.display_name);
        assert_eq!(inv_def.param_count, json_def.param_count);
        assert_eq!(inv_def.osc_prefix, json_def.osc_prefix);
        assert_eq!(inv_def.param_defs.len(), json_def.param_defs.len());
        for (a, b) in inv_def.param_defs.iter().zip(json_def.param_defs.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.name, b.name);
            assert!((a.min - b.min).abs() < 1e-6);
            assert!((a.max - b.max).abs() < 1e-6);
            assert!((a.default_value - b.default_value).abs() < 1e-6);
            assert_eq!(a.whole_numbers, b.whole_numbers);
            assert_eq!(a.is_toggle, b.is_toggle);
            assert_eq!(a.format_string, b.format_string);
            assert_eq!(a.osc_suffix, b.osc_suffix);
        }
        assert_eq!(inv_def.id_to_index, json_def.id_to_index);
        assert_eq!(inv_def.param_ids, json_def.param_ids);
    }

    #[test]
    fn loaded_preset_metadata_returns_empty_initially() {
        // Block 2 ships with no JSON loader populated (manifold-renderer
        // isn't linked in core unit tests). Confirms the dual-source
        // registry doesn't accidentally start consuming something.
        assert!(super::effect::loaded_preset_metadata().is_empty());
        assert!(super::generator::loaded_preset_metadata().is_empty());
    }

    // ── Generator-side tests ───────────────────────────────────────

    #[test]
    fn param_id_to_index_resolves_plasma_ids() {
        // Plasma — declared in generator_metadata_submissions.rs:
        //   pattern (0), complexity (1), contrast (2), speed (3),
        //   scale (4), clip_trigger (5)
        assert_eq!(
            generator::param_id_to_index(&GeneratorTypeId::PLASMA, "pattern"),
            Some(0)
        );
        assert_eq!(
            generator::param_id_to_index(&GeneratorTypeId::PLASMA, "complexity"),
            Some(1)
        );
        assert_eq!(
            generator::param_id_to_index(&GeneratorTypeId::PLASMA, "contrast"),
            Some(2)
        );
        assert_eq!(
            generator::param_id_to_index(&GeneratorTypeId::PLASMA, "speed"),
            Some(3)
        );
        assert_eq!(
            generator::param_id_to_index(&GeneratorTypeId::PLASMA, "scale"),
            Some(4)
        );
        assert_eq!(
            generator::param_id_to_index(&GeneratorTypeId::PLASMA, "clip_trigger"),
            Some(5)
        );
    }

    /// Backward-compat for the `snap` → `clip_trigger` rename.
    #[test]
    fn legacy_snap_id_still_resolves_via_alias() {
        let def = generator::get(&GeneratorTypeId::PLASMA);
        let resolved = resolve_param_alias(def.legacy_param_aliases, "snap");
        assert_eq!(resolved, Some("clip_trigger"));
        assert_eq!(
            generator::param_id_to_index(&GeneratorTypeId::PLASMA, resolved.unwrap()),
            Some(5),
        );
    }

    #[test]
    fn gen_param_id_to_index_unknown_id_returns_none() {
        assert_eq!(
            generator::param_id_to_index(&GeneratorTypeId::PLASMA, "nope"),
            None
        );
    }

    #[test]
    fn gen_param_id_to_index_unknown_generator_returns_none() {
        let phantom = GeneratorTypeId::from_string("not-a-real-generator-id".to_string());
        assert_eq!(generator::param_id_to_index(&phantom, "pattern"), None);
    }

    #[test]
    fn gen_param_id_to_index_round_trips_for_all_known_generators() {
        // Every registered generator's id_to_index map must round-trip
        // each entry through param_index_to_id.
        for (gen_id, def) in GENERATOR_DEFINITIONS.iter() {
            for (i, pd) in def.param_defs.iter().enumerate() {
                if pd.id.is_empty() {
                    continue;
                }
                assert_eq!(
                    generator::param_id_to_index(gen_id, &pd.id),
                    Some(i),
                    "{}::{} must resolve to {}",
                    gen_id.as_str(),
                    pd.id,
                    i
                );
                assert_eq!(
                    generator::param_index_to_id(gen_id, i),
                    Some(pd.id.as_str()),
                    "{} index {} must reverse to {}",
                    gen_id.as_str(),
                    i,
                    pd.id
                );
            }
            // Map size must equal the number of non-empty ids — no dupes.
            let non_empty = def.param_defs.iter().filter(|pd| !pd.id.is_empty()).count();
            assert_eq!(
                def.id_to_index.len(),
                non_empty,
                "{}: id_to_index size mismatch",
                gen_id.as_str()
            );
        }
    }
}
