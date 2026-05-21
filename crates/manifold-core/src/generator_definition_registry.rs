use ahash::AHashMap;
use std::collections::HashMap;
use std::sync::LazyLock;

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
    if index >= def.param_count {
        return None;
    }

    let suffix = def.param_defs[index].osc_suffix.as_ref()?;
    Some(format!("/{}/{}", prefix, suffix))
}

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
    if index >= def.param_count {
        return None;
    }

    let suffix = def.param_defs[index].osc_suffix.as_ref()?;
    Some(format!("/layer/{}/gen/{}/{}", layer_id, prefix, suffix))
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
