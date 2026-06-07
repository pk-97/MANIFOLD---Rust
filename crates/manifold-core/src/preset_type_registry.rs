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

use crate::preset_def::PresetKind;
use crate::preset_type_id::PresetTypeId;
use std::sync::LazyLock;

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

static REGISTRY: LazyLock<Vec<PresetTypeRegistration>> = LazyLock::new(|| {
    let mut v: Vec<PresetTypeRegistration> = Vec::new();

    // Effects: inventory submissions, then JSON-loaded presets.
    for meta in inventory::iter::<crate::effect_registration::EffectMetadata> {
        if !v.iter().any(|r| r.id == meta.id) {
            v.push(meta.to_type_registration());
        }
    }
    for preset in crate::preset_definition_registry::effect::loaded_preset_metadata() {
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
    for preset in crate::preset_definition_registry::generator::loaded_preset_metadata() {
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
});

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

// ── Public API ─────────────────────────────────────────────────────────

/// All registered preset types (both kinds), effects first.
pub fn all() -> &'static [PresetTypeRegistration] {
    &REGISTRY
}

/// All registered types of a given kind, in registry order.
pub fn all_of_kind(kind: PresetKind) -> Vec<&'static PresetTypeRegistration> {
    REGISTRY.iter().filter(|r| r.kind == kind).collect()
}

/// Display name for a preset type. The generator `None` sentinel → "None";
/// an unknown id → the id string.
pub fn display_name(id: &PresetTypeId) -> &str {
    if id.is_none() {
        return "None";
    }
    REGISTRY
        .iter()
        .find(|r| r.id == *id)
        .map(|r| r.display_name)
        .unwrap_or(id.as_str())
}

/// Effect category bucket for a type. `Post-Process` fallback (matches the
/// former effect registry). Generators have no category.
pub fn category(id: &PresetTypeId) -> &str {
    REGISTRY
        .iter()
        .find(|r| r.id == *id)
        .and_then(|r| r.category)
        .unwrap_or(POST_PROCESS)
}

/// Types of a given kind available for the browser popup, in registry order.
pub fn available_of_kind(kind: PresetKind) -> Vec<&'static PresetTypeRegistration> {
    REGISTRY
        .iter()
        .filter(|r| r.available && r.kind == kind)
        .collect()
}

/// Whether a type id is registered. The generator `None` sentinel counts as
/// registered (preserves the former generator-registry behavior).
pub fn is_registered(id: &PresetTypeId) -> bool {
    id.is_none() || REGISTRY.iter().any(|r| r.id == *id)
}
