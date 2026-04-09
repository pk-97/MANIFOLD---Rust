//! Single source of truth for generator type metadata.
//!
//! Replaces the scattered `display_name()`, `ALL` const, and category registry
//! entries with one registration table. Adding/removing = add/remove a row.

use crate::generator_type_id::GeneratorTypeId;
use std::sync::LazyLock;

/// Metadata for a registered generator type.
#[derive(Debug, Clone)]
pub struct GeneratorTypeRegistration {
    pub id: GeneratorTypeId,
    pub display_name: &'static str,
    /// Whether this generator appears in the "Set Generator" browser popup.
    pub available: bool,
}

// ── Registry ────────────────────────────────────────────────────────────

static REGISTRY: LazyLock<Vec<GeneratorTypeRegistration>> = LazyLock::new(|| {
    let mut v = build_registry();
    for meta in inventory::iter::<crate::generator_registration::GeneratorMetadata> {
        // Skip if already registered by legacy path
        if !v.iter().any(|r| r.id == meta.id) {
            v.push(meta.to_type_registration());
        }
    }
    v
});

fn build_registry() -> Vec<GeneratorTypeRegistration> {
    // All generators are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/generators/*.rs).
    vec![]
}

// ── Public API ──────────────────────────────────────────────────────────

/// All registered generator types (excluding None).
pub fn all() -> &'static [GeneratorTypeRegistration] {
    &REGISTRY
}

/// Get the display name for a generator type. Returns the ID string as fallback.
pub fn display_name(id: &GeneratorTypeId) -> &str {
    if id.is_none() {
        return "None";
    }
    REGISTRY
        .iter()
        .find(|r| r.id == *id)
        .map(|r| r.display_name)
        .unwrap_or(id.as_str())
}

/// Generators available for the browser popup, in registration order.
pub fn available_generators() -> Vec<&'static GeneratorTypeRegistration> {
    REGISTRY.iter().filter(|r| r.available).collect()
}

/// Check if a generator type ID is registered (known built-in).
pub fn is_registered(id: &GeneratorTypeId) -> bool {
    id.is_none() || REGISTRY.iter().any(|r| r.id == *id)
}
