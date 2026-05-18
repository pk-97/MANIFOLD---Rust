//! Single source of truth for effect type metadata.
//!
//! Replaces the scattered `display_name()`, `ALL` const, and category registry
//! with one registration table. Adding/removing an effect = add/remove a row.

use crate::effect_type_id::EffectTypeId;
use std::sync::LazyLock;

/// Metadata for a registered effect type.
#[derive(Debug, Clone)]
pub struct EffectTypeRegistration {
    pub id: EffectTypeId,
    pub display_name: &'static str,
    pub category: &'static str,
    /// Whether this effect appears in the "Add Effect" browser popup.
    pub available: bool,
}

// ── Categories ──────────────────────────────────────────────────────────
//
// Five user-facing buckets after the §9.1.4 audit:
//   - Spatial    — transforms, mirrors, kaleidoscopes (UV-space ops)
//   - Color      — invert, color grade, infrared, dither (channel ops)
//   - Stylize    — feedback, watercolor, soft focus, strobe, auto gain,
//                  voronoi prism (look-developer effects)
//   - Filmic     — bloom, halation, chromatic aberration, glitch, DoF,
//                  HDR boost (film-style optical effects)
//   - Diagnostic — edge detect, blob track, wireframe depth, node graph
//                  test (overlay / debug-style effects)
//
// `POST_PROCESS` and `SURVEILLANCE` are retained as constants only so
// unavailable-stub effects (Corruption, Datamosh, etc.) keep compiling
// against their existing category strings; they don't appear in
// `ALL_CATEGORIES` and so are excluded from the picker. Removing the
// stubs entirely happens when those effects ship.

pub const SPATIAL: &str = "Spatial";
pub const COLOR: &str = "Color";
pub const STYLIZE: &str = "Stylize";
pub const FILMIC: &str = "Filmic";
pub const DIAGNOSTIC: &str = "Diagnostic";

// Legacy buckets kept alive for compiling unavailable-stub effects.
// Not exposed in `ALL_CATEGORIES`.
pub const POST_PROCESS: &str = "Post-Process";
pub const SURVEILLANCE: &str = "Surveillance";

pub const ALL_CATEGORIES: &[&str] = &[SPATIAL, COLOR, STYLIZE, FILMIC, DIAGNOSTIC];

// ── Registry ────────────────────────────────────────────────────────────

static REGISTRY: LazyLock<Vec<EffectTypeRegistration>> = LazyLock::new(|| {
    let mut v = build_registry();
    for meta in inventory::iter::<crate::effect_registration::EffectMetadata> {
        if !v.iter().any(|r| r.id == meta.id) {
            v.push(meta.to_type_registration());
        }
    }
    // JSON-loaded presets (§11 unified-registry migration). Same
    // dual-source pattern as `effect_definition_registry::DEFINITIONS`
    // — every shipping effect post-§11 has its `presetMetadata` block
    // populated in `assets/effect-presets/*.json` and surfaces here
    // through `LoadedPresetSource`. Without this loop, the picker only
    // shows the 6 plugin-bridge effects whose legacy `EffectMetadata`
    // submissions survived block 8f.
    for preset in crate::effect_definition_registry::loaded_preset_metadata() {
        if !v.iter().any(|r| r.id == preset.id) {
            v.push(
                crate::effect_definition_registry::preset_metadata_to_type_registration(preset),
            );
        }
    }
    v
});

fn build_registry() -> Vec<EffectTypeRegistration> {
    // All effects are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/effects/*.rs).
    vec![]
}

// ── Public API ──────────────────────────────────────────────────────────

/// All registered effect types.
pub fn all() -> &'static [EffectTypeRegistration] {
    &REGISTRY
}

/// Get the display name for an effect type. Returns the ID string as fallback.
pub fn display_name(id: &EffectTypeId) -> &str {
    REGISTRY
        .iter()
        .find(|r| r.id == *id)
        .map(|r| r.display_name)
        .unwrap_or(id.as_str())
}

/// Get the category for an effect type. Returns "Post-Process" as fallback.
pub fn category(id: &EffectTypeId) -> &str {
    REGISTRY
        .iter()
        .find(|r| r.id == *id)
        .map(|r| r.category)
        .unwrap_or(POST_PROCESS)
}

/// Effects available for the "Add Effect" browser popup, in registration order.
pub fn available_effects() -> Vec<&'static EffectTypeRegistration> {
    REGISTRY.iter().filter(|r| r.available).collect()
}

/// All effect types in a given category.
pub fn effects_in_category(cat: &str) -> Vec<&'static EffectTypeRegistration> {
    REGISTRY.iter().filter(|r| r.category == cat).collect()
}

/// Check if an effect type ID is registered (known built-in).
pub fn is_registered(id: &EffectTypeId) -> bool {
    REGISTRY.iter().any(|r| r.id == *id)
}
