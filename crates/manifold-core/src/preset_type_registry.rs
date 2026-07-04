//! Single source of truth for preset (effect + generator) type metadata —
//! the browser-popup / picker registry.
//!
//! Replaces the former parallel `effect_type_registry` /
//! `generator_type_registry` modules. One [`PresetTypeRegistration`] carries
//! both kinds (discriminated by `kind`); effect ids and generator ids are
//! globally disjoint (verified, and the definition store asserts it), so a
//! single flat registry is unambiguous. `category` is effect-only
//! (generators carry `None`); the [`ALL_CATEGORIES`] buckets are read only by
//! the effect picker.

use crate::effect_graph_def::PresetMetadata;
use crate::preset_def::PresetKind;
use crate::preset_type_id::PresetTypeId;
use arc_swap::ArcSwap;
use std::sync::{Arc, LazyLock};

/// Picker metadata for a registered preset type (effect or generator).
#[derive(Debug, Clone)]
pub struct PresetTypeRegistration {
    pub id: PresetTypeId,
    pub display_name: &'static str,
    /// Effect category bucket; `None` for generators (no categories today).
    pub category: Option<&'static str>,
    pub kind: PresetKind,
    /// Whether this type appears in its kind's browser popup.
    pub available: bool,
}

// ── Categories (effect picker only) ──────────────────────────────────────
//
// Five user-facing buckets after the §9.1.4 audit. `POST_PROCESS` /
// `SURVEILLANCE` are retained as constants only so unavailable-stub effects
// keep compiling against their existing category strings; they aren't in
// `ALL_CATEGORIES` and so are excluded from the picker.

pub const SPATIAL: &str = "Spatial";
pub const COLOR: &str = "Color";
pub const STYLIZE: &str = "Stylize";
pub const FILMIC: &str = "Filmic";
pub const DIAGNOSTIC: &str = "Diagnostic";

pub const POST_PROCESS: &str = "Post-Process";
pub const SURVEILLANCE: &str = "Surveillance";

pub const ALL_CATEGORIES: &[&str] = &[SPATIAL, COLOR, STYLIZE, FILMIC, DIAGNOSTIC];

// ── Registry ─────────────────────────────────────────────────────────────
//
// One flat list, effects first then generators, each tagged with kind. A
// JSON-loaded preset wins over an inventory submission of the same id (same
// dual-source pattern as the definition store). Order within a kind matches
// the former per-kind registry, so position-indexed pickers
// (`ui_bridge::project::set_gen_type`) stay stable.
//
// `ArcSwap` (PRESET_LIBRARY_DESIGN P3 entry-state fix), mirroring
// `preset_definition_registry::PRESET_DEFINITIONS`: this registry backs the
// Add-effect / Add-generator browser popup's item list, and used to be a
// `LazyLock<Vec<_>>` computed once and never refreshed — a preset JSON added
// to the user dir at runtime never appeared in the browser without an app
// restart, even though the hot-reload watcher was already rebuilding the
// (separate) param-definition store. [`rebuild`] is called from
// `manifold-renderer`'s `apply_reload` right beside
// `rebuild_preset_definitions`, so both stores swap in the same reload pass.
static REGISTRY: LazyLock<ArcSwap<Vec<PresetTypeRegistration>>> = LazyLock::new(|| {
    ArcSwap::from_pointee(build_registry(
        crate::preset_definition_registry::effect::loaded_preset_metadata(),
        crate::preset_definition_registry::generator::loaded_preset_metadata(),
    ))
});

/// Build the flat registry from both kinds' JSON-loaded preset metadata (plus
/// the compiled-in `inventory` submissions). Shared by the initial `LazyLock`
/// seed and [`rebuild`] — same shape as
/// `preset_definition_registry::build_preset_definitions`.
fn build_registry(
    effect_json: &[PresetMetadata],
    generator_json: &[PresetMetadata],
) -> Vec<PresetTypeRegistration> {
    let mut v: Vec<PresetTypeRegistration> = Vec::new();

    // Effects: inventory submissions, then JSON-loaded presets.
    for meta in inventory::iter::<crate::effect_registration::EffectMetadata> {
        if !v.iter().any(|r| r.id == meta.id) {
            v.push(meta.to_type_registration());
        }
    }
    for preset in effect_json {
        if !v.iter().any(|r| r.id == preset.id) {
            v.push(PresetTypeRegistration {
                id: preset.id.clone(),
                display_name: leak(&preset.display_name),
                category: Some(leak(&preset.category)),
                kind: PresetKind::Effect,
                available: preset.available,
            });
        }
    }

    // Generators: inventory submissions, then JSON-loaded presets.
    for meta in inventory::iter::<crate::generator_registration::GeneratorMetadata> {
        if !v.iter().any(|r| r.id == meta.id) {
            v.push(meta.to_type_registration());
        }
    }
    for preset in generator_json {
        if !v.iter().any(|r| r.id == preset.id) {
            v.push(PresetTypeRegistration {
                id: preset.id.clone(),
                display_name: leak(&preset.display_name),
                category: None,
                kind: PresetKind::Generator,
                available: preset.available,
            });
        }
    }
    v
}

/// Hot-reload: rebuild the registry from freshly-reloaded JSON metadata for
/// both kinds and swap it in with one atomic `ArcSwap::store`. Called by
/// `manifold-renderer`'s `apply_reload`, right beside
/// `preset_definition_registry::rebuild_preset_definitions`, so a directory
/// change (new/edited/removed preset file) reaches the browser popup's item
/// list on the same reload pass that reaches the param-definition store.
pub fn rebuild(effect_json: &[PresetMetadata], generator_json: &[PresetMetadata]) {
    REGISTRY.store(Arc::new(build_registry(effect_json, generator_json)));
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

// ── Public API ─────────────────────────────────────────────────────────
//
// Accessors return owned clones (a `PresetTypeRegistration` is cheap: a
// `PresetTypeId` — `Cow<'static, str>` — plus `Copy` fields) rather than
// `'static` references into the registry, because the registry can now be
// swapped at any time by [`rebuild`]. Every existing call site only ever
// iterated the result and read fields off it, so this is source-compatible.

/// All registered preset types (both kinds), effects first.
pub fn all() -> Vec<PresetTypeRegistration> {
    REGISTRY.load().to_vec()
}

/// All registered types of a given kind, in registry order.
pub fn all_of_kind(kind: PresetKind) -> Vec<PresetTypeRegistration> {
    REGISTRY.load().iter().filter(|r| r.kind == kind).cloned().collect()
}

/// Display name for a preset type. The generator `None` sentinel → "None";
/// an unknown id → the id string.
pub fn display_name(id: &PresetTypeId) -> &str {
    if id.is_none() {
        return "None";
    }
    REGISTRY
        .load()
        .iter()
        .find(|r| r.id == *id)
        .map(|r| r.display_name)
        .unwrap_or(id.as_str())
}

/// Effect category bucket for a type. `Post-Process` fallback (matches the
/// former effect registry). Generators have no category.
pub fn category(id: &PresetTypeId) -> &str {
    REGISTRY
        .load()
        .iter()
        .find(|r| r.id == *id)
        .and_then(|r| r.category)
        .unwrap_or(POST_PROCESS)
}

/// Types of a given kind available for the browser popup, in registry order.
pub fn available_of_kind(kind: PresetKind) -> Vec<PresetTypeRegistration> {
    REGISTRY
        .load()
        .iter()
        .filter(|r| r.available && r.kind == kind)
        .cloned()
        .collect()
}

/// Whether a type id is registered. The generator `None` sentinel counts as
/// registered (preserves the former generator-registry behavior).
pub fn is_registered(id: &PresetTypeId) -> bool {
    id.is_none() || REGISTRY.load().iter().any(|r| r.id == *id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::PresetMetadata;

    fn probe_meta(id: &str, display: &str) -> PresetMetadata {
        PresetMetadata {
            id: PresetTypeId::from_string(id.to_string()),
            display_name: display.to_string(),
            category: "Test".to_string(),
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
        }
    }

    /// PRESET_LIBRARY_DESIGN P3 entry-state fix: `rebuild` must make a
    /// newly-added preset visible immediately (no restart) AND must drop one
    /// that's no longer present (a full swap, matching a file delete on
    /// reload) — not just accrete forever. This is the exact defect the
    /// former `LazyLock<Vec<_>>` had: computed once, never refreshed.
    #[test]
    fn rebuild_swaps_in_new_entries_and_drops_stale_ones() {
        let id = PresetTypeId::from_string("__preset_type_registry_test_probe__".to_string());
        assert!(!is_registered(&id), "probe id must not pre-exist in the real registry");

        rebuild(&[probe_meta(id.as_str(), "Probe")], &[]);
        assert!(
            is_registered(&id),
            "rebuild must surface a newly-added preset without an app restart"
        );
        assert_eq!(display_name(&id), "Probe");

        rebuild(&[], &[]);
        assert!(
            !is_registered(&id),
            "rebuild must be a full swap — a preset no longer present must disappear, not linger"
        );

        // Restore the real registry: other (later-running) tests / doctests
        // in this process may read it via the public accessors.
        rebuild(
            crate::preset_definition_registry::effect::loaded_preset_metadata(),
            crate::preset_definition_registry::generator::loaded_preset_metadata(),
        );
    }
}
