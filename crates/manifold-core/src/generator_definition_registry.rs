use ahash::AHashMap;
use std::collections::HashMap;
use std::sync::{LazyLock, OnceLock};

use crate::effect_graph_def::{ParamSpecDef, PresetMetadata};
use crate::effect_registration::{ParamAlias, ParamValueAlias};
use crate::effects::ParamDef;
use crate::generator_type_id::GeneratorTypeId;

// ─── Generator Definition ───

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

#[derive(Debug, Clone)]
pub struct GeneratorDef {
    pub display_name: &'static str,
    pub is_line_based: bool,
    pub param_count: usize,
    pub param_defs: Vec<ParamDef>,
    pub string_param_defs: Vec<StringParamDef>,
    pub osc_prefix: Option<&'static str>,
    /// Stable `ParamSpec::id` → storage index. See
    /// [`crate::effect_definition_registry::EffectDef::id_to_index`] —
    /// same role, parallel structure for generators.
    pub id_to_index: AHashMap<String, usize>,
    /// Storage-index → static param id. See
    /// [`crate::effect_definition_registry::EffectDef::param_ids`].
    pub param_ids: Vec<&'static str>,
    /// Declarative legacy id migration table. See
    /// [`crate::effect_registration::ParamAlias`].
    pub legacy_param_aliases: &'static [crate::effect_registration::ParamAlias],
}

// ─── Static Registry ───

static DEFINITIONS: LazyLock<HashMap<GeneratorTypeId, GeneratorDef>> = LazyLock::new(|| {
    let mut m = build_definitions();
    for meta in inventory::iter::<crate::generator_registration::GeneratorMetadata> {
        m.insert(meta.id.clone(), meta.to_generator_def());
    }
    // Sidecar alias submissions for generators. See parallel impl in
    // `effect_definition_registry`.
    for alias_meta in inventory::iter::<crate::generator_registration::GeneratorAliasMetadata> {
        if let Some(def) = m.get_mut(&alias_meta.id) {
            def.legacy_param_aliases = alias_meta.aliases;
        }
    }
    // JSON-loaded presets (§11 unified-registry migration, generator
    // mirror of the effect-side path in `effect_definition_registry`).
    // A JSON-loaded preset wins over an inventory submission for the
    // same id — same dual-source pattern so a generator that ships
    // with a bundled JSON preset *and* a legacy inventory entry uses
    // the JSON as the canonical schema. Eliminates the inventory-vs-
    // preset positional layout drift class structurally: there's only
    // one positional namespace per generator (the JSON's), and the
    // inventory entry becomes dead code overridden here.
    for preset in loaded_preset_metadata() {
        let gen_id = GeneratorTypeId::from_string(preset.id.as_str().to_string());
        m.insert(gen_id, preset_metadata_to_generator_def(preset));
    }
    m
});

static MAX_PARAM_COUNT: LazyLock<usize> = LazyLock::new(|| {
    DEFINITIONS
        .values()
        .map(|d| d.param_count)
        .max()
        .unwrap_or(0)
});

// ─── Public API ───

pub fn get(gen_type: &GeneratorTypeId) -> &'static GeneratorDef {
    DEFINITIONS.get(gen_type).unwrap_or_else(|| {
        panic!(
            "GeneratorDefinitionRegistry: unknown GeneratorTypeId '{}'",
            gen_type
        )
    })
}

pub fn try_get(gen_type: &GeneratorTypeId) -> Option<&'static GeneratorDef> {
    DEFINITIONS.get(gen_type)
}

/// Translate a stable `ParamSpec::id` into the param's storage index for the
/// given generator type. Returns `None` if the generator or id is unknown.
///
/// Mirrors [`crate::effect_definition_registry::param_id_to_index`].
pub fn param_id_to_index(gen_type: &GeneratorTypeId, id: &str) -> Option<usize> {
    DEFINITIONS.get(gen_type)?.id_to_index.get(id).copied()
}

/// Reverse of [`param_id_to_index`]. Returns the original `&'static
/// str` from the `ParamSpec` registration. `None` if out of range or
/// the slot has an empty id (V1 fixture / pre-step-6 entry).
pub fn param_index_to_id(gen_type: &GeneratorTypeId, index: usize) -> Option<&'static str> {
    let def = DEFINITIONS.get(gen_type)?;
    let id = *def.param_ids.get(index)?;
    if id.is_empty() { None } else { Some(id) }
}

pub fn is_line_based(gen_type: &GeneratorTypeId) -> bool {
    DEFINITIONS.get(gen_type).is_some_and(|d| d.is_line_based)
}

pub fn get_param_def(gen_type: &GeneratorTypeId, index: usize) -> ParamDef {
    let Some(def) = DEFINITIONS.get(gen_type) else {
        return ParamDef::default();
    };
    if index >= def.param_count {
        return ParamDef::default();
    }
    def.param_defs[index].clone()
}

pub fn get_defaults(gen_type: &GeneratorTypeId) -> Vec<f32> {
    let Some(def) = DEFINITIONS.get(gen_type) else {
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
    let def = DEFINITIONS.get(gen_type)?;
    let prefix = def.osc_prefix.as_ref()?;
    let &param_id = def.param_ids.get(index)?;
    if param_id.is_empty() {
        return None;
    }
    Some(format!("/{}/{}", prefix, param_id))
}

/// Unified with the effect scheme (preset unification, 2026-05):
/// `/layer/{layerId}/{prefix}/{param_id}`. The legacy `/gen/` namespace
/// segment is dropped — disambiguation between an effect and a generator
/// sharing a layer is a naming-convention concern (distinct osc_prefixes),
/// not an addressing one. External senders now use one convention for both.
pub fn get_osc_address_for_layer(
    gen_type: &GeneratorTypeId,
    layer_id: &str,
    index: usize,
) -> Option<String> {
    if layer_id.is_empty() {
        return None;
    }
    let def = DEFINITIONS.get(gen_type)?;
    let prefix = def.osc_prefix.as_ref()?;
    let &param_id = def.param_ids.get(index)?;
    if param_id.is_empty() {
        return None;
    }
    Some(format!("/layer/{}/{}/{}", layer_id, prefix, param_id))
}

pub fn try_get_gen_param_range(gen_type: &GeneratorTypeId, index: usize) -> Option<(f32, f32)> {
    let def = DEFINITIONS.get(gen_type)?;
    if index >= def.param_count {
        return None;
    }
    let pd = &def.param_defs[index];
    Some((pd.min, pd.max))
}

pub fn clamp_param(gen_type: &GeneratorTypeId, index: usize, value: f32) -> f32 {
    let Some(def) = DEFINITIONS.get(gen_type) else {
        return value;
    };
    if index >= def.param_count {
        return value;
    }
    let pd = &def.param_defs[index];
    value.clamp(pd.min, pd.max)
}

pub fn max_param_count() -> usize {
    *MAX_PARAM_COUNT
}

// ─── Format Helper ───

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

// ─── JSON-loaded preset registry ───
//
// Mirror of [`crate::effect_definition_registry::loaded_preset_metadata`]
// and the surrounding `LoadedPresetSource` infrastructure. The renderer
// crate submits one `LoadedPresetSource` covering every JSON file under
// `crates/manifold-renderer/assets/generator-presets/`, and those entries
// feed `DEFINITIONS` above with JSON-as-schema (winning over any legacy
// inventory submission for the same id).

/// JSON-loaded preset metadata for inclusion in [`DEFINITIONS`].
///
/// Each [`LoadedPresetSource`] submission in the inventory contributes a
/// function pointer that produces a `Vec<PresetMetadata>` when called.
/// The renderer crate submits one source pointing at
/// `loaded_generator_presets_from_bundled` — which parses
/// `assets/generator-presets/*.json` (via the renderer's build.rs-
/// generated table) and returns every entry whose `version` makes it
/// carry [`PresetMetadata`].
///
/// Sources are invoked once on first access and cached for the process
/// lifetime.
pub fn loaded_preset_metadata() -> &'static [PresetMetadata] {
    static CACHE: OnceLock<Vec<PresetMetadata>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut all = Vec::new();
        for source in inventory::iter::<LoadedPresetSource> {
            all.extend((source.load)());
        }
        all
    })
}

/// Inventory submission point for JSON-loaded generator preset metadata.
/// Mirrors [`crate::effect_definition_registry::LoadedPresetSource`].
pub struct LoadedPresetSource {
    pub load: fn() -> Vec<PresetMetadata>,
}

inventory::collect!(LoadedPresetSource);

/// Convert a parsed [`PresetMetadata`] (shared JSON wire shape with
/// effects) into the existing [`GeneratorDef`] consumed by the rest of
/// the codebase. String fields move from owned to `&'static str` via
/// `Box::leak`; bounded by the (finite) number of shipping presets and
/// happens once at startup.
///
/// Mirrors [`crate::effect_definition_registry::preset_metadata_to_effect_def`].
pub fn preset_metadata_to_generator_def(meta: &PresetMetadata) -> GeneratorDef {
    let param_defs: Vec<ParamDef> = meta.params.iter().map(param_spec_def_to_param_def).collect();
    let param_count = param_defs.len();
    let id_to_index: AHashMap<String, usize> = meta
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.id.is_empty())
        .map(|(i, p)| (p.id.clone(), i))
        .collect();
    let param_ids: Vec<&'static str> = meta.params.iter().map(|p| leak_str(&p.id)).collect();
    GeneratorDef {
        display_name: leak_str(&meta.display_name),
        is_line_based: meta.is_line_based,
        param_count,
        param_defs,
        // String params live outside the v2 PresetMetadata schema for
        // now. Generators that need them (Text, NumberStation, …) keep
        // their inventory submission; the §11 path applies to graph-
        // backed generators without string-param surface.
        string_param_defs: Vec::new(),
        osc_prefix: Some(leak_str(&meta.osc_prefix)),
        id_to_index,
        param_ids,
        legacy_param_aliases: leak_alias_table(&meta.param_aliases),
    }
}

/// Convert a [`PresetMetadata`] into the picker-side
/// [`crate::generator_type_registry::GeneratorTypeRegistration`].
///
/// Mirrors
/// [`crate::effect_definition_registry::preset_metadata_to_type_registration`].
pub fn preset_metadata_to_type_registration(
    meta: &PresetMetadata,
) -> crate::generator_type_registry::GeneratorTypeRegistration {
    crate::generator_type_registry::GeneratorTypeRegistration {
        id: GeneratorTypeId::from_string(meta.id.as_str().to_string()),
        display_name: leak_str(&meta.display_name),
        available: meta.available,
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

fn leak_alias_table(
    entries: &[crate::effect_graph_def::AliasEntry],
) -> &'static [ParamAlias] {
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

// `ParamValueAlias` is unused by generators today (no value-alias path
// in `GeneratorDef`); held in scope to keep the conversion shape
// identical to the effect side when value aliases land for generators.
#[allow(dead_code)]
fn _value_alias_keepalive(_: ParamValueAlias) {}

// ─── Registry Builder ───

fn build_definitions() -> HashMap<GeneratorTypeId, GeneratorDef> {
    let mut m = HashMap::new();

    // ── None ──
    m.insert(
        GeneratorTypeId::NONE,
        GeneratorDef {
            display_name: "None",
            is_line_based: false,
            param_count: 0,
            param_defs: Vec::new(),
            string_param_defs: Vec::new(),
            osc_prefix: None,
            id_to_index: AHashMap::new(),
            param_ids: Vec::new(),
            legacy_param_aliases: &[],
        },
    );

    // All other generators are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/generators/*.rs).

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_id_to_index_resolves_plasma_ids() {
        // Plasma — declared in generator_metadata_submissions.rs:
        //   pattern (0), complexity (1), contrast (2), speed (3),
        //   scale (4), clip_trigger (5)
        assert_eq!(
            param_id_to_index(&GeneratorTypeId::PLASMA, "pattern"),
            Some(0)
        );
        assert_eq!(
            param_id_to_index(&GeneratorTypeId::PLASMA, "complexity"),
            Some(1)
        );
        assert_eq!(
            param_id_to_index(&GeneratorTypeId::PLASMA, "contrast"),
            Some(2)
        );
        assert_eq!(
            param_id_to_index(&GeneratorTypeId::PLASMA, "speed"),
            Some(3)
        );
        assert_eq!(
            param_id_to_index(&GeneratorTypeId::PLASMA, "scale"),
            Some(4)
        );
        assert_eq!(
            param_id_to_index(&GeneratorTypeId::PLASMA, "clip_trigger"),
            Some(5)
        );
    }

    /// Backward-compat for the `snap` → `clip_trigger` rename:
    /// projects saved before the rename store the legacy id in driver
    /// bindings; the alias table resolves it on lookup so they still
    /// route to the renamed param at index 5. Direct lookup of the
    /// raw "snap" string (without alias resolution) would return None.
    #[test]
    fn legacy_snap_id_still_resolves_via_alias() {
        use crate::effect_registration::resolve_param_alias;
        let def = get(&GeneratorTypeId::PLASMA);
        let resolved = resolve_param_alias(def.legacy_param_aliases, "snap");
        assert_eq!(resolved, Some("clip_trigger"));
        assert_eq!(
            param_id_to_index(&GeneratorTypeId::PLASMA, resolved.unwrap()),
            Some(5),
        );
    }

    #[test]
    fn param_id_to_index_unknown_id_returns_none() {
        assert_eq!(param_id_to_index(&GeneratorTypeId::PLASMA, "nope"), None);
    }

    #[test]
    fn param_id_to_index_unknown_generator_returns_none() {
        let phantom = GeneratorTypeId::from_string("not-a-real-generator-id".to_string());
        assert_eq!(param_id_to_index(&phantom, "pattern"), None);
    }

    #[test]
    fn param_id_to_index_round_trips_for_all_known_generators() {
        // Every registered generator's id_to_index map must round-trip
        // each entry through param_index_to_id.
        for (gen_id, def) in DEFINITIONS.iter() {
            for (i, pd) in def.param_defs.iter().enumerate() {
                if pd.id.is_empty() {
                    continue;
                }
                assert_eq!(
                    param_id_to_index(gen_id, &pd.id),
                    Some(i),
                    "{}::{} must resolve to {}",
                    gen_id.as_str(),
                    pd.id,
                    i
                );
                assert_eq!(
                    param_index_to_id(gen_id, i),
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
