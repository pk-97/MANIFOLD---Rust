use crate::effect_type_id::EffectTypeId;
use crate::effects::{EffectInstance, ParamDef};
use ahash::AHashMap;
use std::collections::HashMap;
use std::sync::LazyLock;

// ─── Effect Definition ───

/// Metadata for one effect type: display name, parameter schema, OSC prefix.
/// Mechanical translation of Unity's EffectDefinitionRegistry.EffectDef.
#[derive(Debug, Clone)]
pub struct EffectDef {
    pub display_name: &'static str,
    pub param_count: usize,
    pub param_defs: Vec<ParamDef>,
    pub osc_prefix: Option<&'static str>,
    /// Stable `ParamSpec::id` → storage index, built once when this def is
    /// inserted into the registry. The lookup table for every external
    /// addressing site (drivers, envelopes, Ableton, OSC, macros, project
    /// file storage). Built from the **authoritative** `ParamSpec` list so
    /// V1 projects with empty `ParamDef.id` strings still resolve via
    /// post-load alignment + this table.
    pub id_to_index: AHashMap<String, usize>,
    /// Storage-index → param id, parallel to `param_defs`. Each entry
    /// is the same `&'static str` carried in the original `ParamSpec`
    /// at registration time. Empty for legacy slots where the spec
    /// did not carry an id. Lets reverse lookups (`param_index_to_id`)
    /// return `&'static str` without unsafe transmutes.
    pub param_ids: Vec<&'static str>,
    /// Declarative legacy id migration table. See
    /// [`crate::effect_registration::ParamAlias`].
    pub legacy_param_aliases: &'static [crate::effect_registration::ParamAlias],
    /// Declarative legacy **node-handle** migration table for V2
    /// user-exposed parameter bindings. Same shape as
    /// `legacy_param_aliases`, but addresses inner-graph node handles
    /// (set via `Graph::add_node_named` at effect construction).
    /// Submitted via [`crate::effect_registration::EffectNodeAliasMetadata`]
    /// sidecar.
    pub legacy_node_aliases: &'static [crate::effect_registration::ParamAlias],
}

/// Re-export for callers within this module's namespace. Canonical home
/// is [`crate::effect_registration::resolve_param_alias`] — it's a pure
/// slice utility with no registry coupling and lives next to the
/// [`crate::effect_registration::ParamAlias`] type definition.
pub use crate::effect_registration::resolve_param_alias;

// ─── Static Registry ───

static DEFINITIONS: LazyLock<HashMap<EffectTypeId, EffectDef>> = LazyLock::new(|| {
    let mut m = build_definitions();
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
    // Same pattern for **node-handle** aliases — the V2 user-exposed
    // parameter binding migration table. See
    // `effect_registration::EffectNodeAliasMetadata`.
    for alias_meta in inventory::iter::<crate::effect_registration::EffectNodeAliasMetadata> {
        if let Some(def) = m.get_mut(&alias_meta.id) {
            def.legacy_node_aliases = alias_meta.aliases;
        }
    }
    m
});

// ─── Public API ───

/// Get the definition for an effect type. Panics if not found.
/// Matches Unity's `EffectDefinitionRegistry.Get(EffectType)`.
pub fn get(effect_type: &EffectTypeId) -> &'static EffectDef {
    DEFINITIONS.get(effect_type).unwrap_or_else(|| {
        panic!(
            "EffectDefinitionRegistry: unknown EffectTypeId '{}'",
            effect_type
        )
    })
}

/// Try to get the definition for an effect type.
/// Matches Unity's `EffectDefinitionRegistry.TryGet(EffectType, out EffectDef)`.
pub fn try_get(effect_type: &EffectTypeId) -> Option<&'static EffectDef> {
    DEFINITIONS.get(effect_type)
}

/// Translate a stable `ParamSpec::id` into the param's storage index for the
/// given effect type. Returns `None` if the effect or id is unknown.
///
/// Hot-path: every per-frame addressing dispatch (driver, envelope,
/// Ableton update, OSC route) goes through this. The lookup is one
/// `&str → usize` `AHashMap::get` (~50ns); the map is built once when the
/// registry initializes.
pub fn param_id_to_index(effect_type: &EffectTypeId, id: &str) -> Option<usize> {
    DEFINITIONS.get(effect_type)?.id_to_index.get(id).copied()
}

/// Reverse of [`param_id_to_index`]: storage index → param id. Each id
/// is the original `&'static str` from the `ParamSpec` registration.
/// Returns `None` if the effect or index is out of range, or the slot
/// has an empty id (V1 fixture / pre-step-6 entry).
pub fn param_index_to_id(effect_type: &EffectTypeId, index: usize) -> Option<&'static str> {
    let def = DEFINITIONS.get(effect_type)?;
    let id = *def.param_ids.get(index)?;
    if id.is_empty() { None } else { Some(id) }
}

/// Create a new EffectInstance with default parameter values from the registry.
/// Matches Unity's `EffectDefinitionRegistry.CreateDefault(EffectType)`.
pub fn create_default(effect_type: &EffectTypeId) -> EffectInstance {
    let def = get(effect_type);
    let mut inst = EffectInstance::new(effect_type.clone());
    for (i, pd) in def.param_defs.iter().enumerate() {
        inst.set_base_param(i, pd.default_value);
    }
    inst
}

/// Format a parameter value for display.
/// Named labels take priority, then wholeNumbers round, then F2.
/// Matches Unity's `EffectDefinitionRegistry.FormatValue(EffectType, int, float)`.
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
/// Returns None if no address is defined.
/// Matches Unity's `EffectDefinitionRegistry.GetOscAddress(EffectType, int)`.
pub fn get_osc_address(effect_type: &EffectTypeId, param_index: usize) -> Option<String> {
    let def = try_get(effect_type)?;
    let prefix = def.osc_prefix?;
    if param_index >= def.param_count {
        return None;
    }
    if param_index == 0 {
        return Some(format!("/master/{}", prefix));
    }
    let suffix = def.param_defs[param_index].osc_suffix.as_ref()?;
    Some(format!("/master/{}{}", prefix, suffix))
}

/// Get the OSC address for a layer effect parameter scoped to a specific layer.
/// Format: /layer/{layerId}/effectName or /layer/{layerId}/effectName/paramName
/// Matches Unity's `EffectDefinitionRegistry.GetOscAddressForLayer(EffectType, string, int)`.
pub fn get_osc_address_for_layer(
    effect_type: &EffectTypeId,
    layer_id: &str,
    param_index: usize,
) -> Option<String> {
    if layer_id.is_empty() {
        return None;
    }
    let def = try_get(effect_type)?;
    let prefix = def.osc_prefix?;
    if param_index >= def.param_count {
        return None;
    }
    if param_index == 0 {
        return Some(format!("/layer/{}/{}", layer_id, prefix));
    }
    let suffix = def.param_defs[param_index].osc_suffix.as_ref()?;
    Some(format!("/layer/{}/{}{}", layer_id, prefix, suffix))
}

/// Get default parameter values for an effect type as freshly-allocated
/// `ParamSlot` entries, all `exposed: true`. Matches Unity's
/// `EffectDefinitionRegistry` usage for creating new instances.
pub fn get_defaults(effect_type: &EffectTypeId) -> Vec<crate::effects::ParamSlot> {
    let def = get(effect_type);
    def.param_defs
        .iter()
        .map(|p| crate::effects::ParamSlot::exposed(p.default_value))
        .collect()
}

/// Get all registered effect types (unordered).
/// Matches Unity's `EffectDefinitionRegistry.GetAllEffectTypes(List<EffectType>)`.
pub fn get_all_effect_types() -> Vec<EffectTypeId> {
    DEFINITIONS.keys().cloned().collect()
}

/// Get all registered effect types sorted by display name.
/// Matches Unity's `EffectDefinitionRegistry.GetAllEffectTypesSorted()`.
pub fn get_all_effect_types_sorted() -> Vec<EffectTypeId> {
    let mut list: Vec<EffectTypeId> = DEFINITIONS.keys().cloned().collect();
    list.sort_by_key(|t| t.as_str().to_string());
    list
}

// ─── Build Definitions ───

fn build_definitions() -> HashMap<EffectTypeId, EffectDef> {
    // All effects are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/effects/*.rs).
    HashMap::new()
}

#[cfg(test)]
mod tests {
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
        for (_, def) in DEFINITIONS.iter() {
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
        let addr = get_osc_address(&EffectTypeId::BLOOM, 0);
        assert_eq!(addr, Some("/master/bloom".to_string()));
    }

    #[test]
    fn test_osc_address_master_param() {
        let addr = get_osc_address(&EffectTypeId::INFINITE_ZOOM, 1);
        assert_eq!(addr, Some("/master/infiniteZoomSharpness".to_string()));
    }

    #[test]
    fn test_osc_address_no_suffix() {
        let addr = get_osc_address(&EffectTypeId::TRANSFORM, 0);
        assert_eq!(addr, Some("/master/transform".to_string()));
        let addr = get_osc_address(&EffectTypeId::TRANSFORM, 1);
        assert_eq!(addr, None);
    }

    #[test]
    fn test_osc_address_layer() {
        let addr = get_osc_address_for_layer(&EffectTypeId::BLOOM, "layer_1", 0);
        assert_eq!(addr, Some("/layer/layer_1/bloom".to_string()));
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
}
