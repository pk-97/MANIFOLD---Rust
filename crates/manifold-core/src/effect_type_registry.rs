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

pub const SPATIAL: &str = "Spatial";
pub const POST_PROCESS: &str = "Post-Process";
pub const FILMIC: &str = "Filmic";
pub const SURVEILLANCE: &str = "Surveillance";

pub const ALL_CATEGORIES: &[&str] = &[SPATIAL, POST_PROCESS, FILMIC, SURVEILLANCE];

// ── Registry ────────────────────────────────────────────────────────────

static REGISTRY: LazyLock<Vec<EffectTypeRegistration>> = LazyLock::new(build_registry);

fn build_registry() -> Vec<EffectTypeRegistration> {
    use EffectTypeId as E;
    vec![
        // Spatial
        reg(E::TRANSFORM,            "Transform",            SPATIAL,       true),
        reg(E::INVERT_COLORS,        "Invert Colors",        SPATIAL,       true),
        // Post-Process
        reg(E::FEEDBACK,             "Feedback",             POST_PROCESS,  true),
        reg(E::PIXEL_SORT,           "Pixel Sort",           POST_PROCESS,  true),
        reg(E::BLOOM,                "Bloom",                POST_PROCESS,  true),
        reg(E::INFINITE_ZOOM,        "Infinite Zoom",        POST_PROCESS,  false),
        reg(E::KALEIDOSCOPE,         "Kaleidoscope",         POST_PROCESS,  true),
        reg(E::EDGE_STRETCH,         "Edge Stretch",         POST_PROCESS,  true),
        reg(E::VORONOI_PRISM,        "Voronoi Prism",        POST_PROCESS,  true),
        reg(E::QUAD_MIRROR,          "Quad Mirror",          POST_PROCESS,  true),
        reg(E::DITHER,               "Dither",               POST_PROCESS,  true),
        reg(E::STROBE,               "Strobe",               POST_PROCESS,  true),
        reg(E::STYLIZED_FEEDBACK,    "Stylized Feedback",    POST_PROCESS,  true),
        reg(E::MIRROR,               "Mirror",               POST_PROCESS,  true),
        reg(E::BLOB_TRACKING,        "Blob Tracking",        POST_PROCESS,  true),
        reg(E::CRT,                  "CRT",                  POST_PROCESS,  true),
        reg(E::FLUID_DISTORTION,     "Fluid Distortion",     POST_PROCESS,  false),
        reg(E::EDGE_GLOW,            "Edge Glow",            POST_PROCESS,  true),
        reg(E::DATAMOSH,             "Datamosh",             POST_PROCESS,  false),
        reg(E::SLIT_SCAN,            "Slit Scan",            POST_PROCESS,  false),
        reg(E::COLOR_GRADE,          "Color Grade",          POST_PROCESS,  true),
        reg(E::WIREFRAME_DEPTH,      "Wireframe Depth",      POST_PROCESS,  true),
        // Filmic
        reg(E::CHROMATIC_ABERRATION, "Chromatic Aberration",  FILMIC,       true),
        reg(E::GRADIENT_MAP,         "Gradient Map",          FILMIC,       false),
        reg(E::GLITCH,               "Glitch",                FILMIC,       true),
        reg(E::FILM_GRAIN,           "Film Grain",            FILMIC,       true),
        reg(E::HALATION,             "Halation",              FILMIC,       true),
        // Surveillance
        reg(E::CORRUPTION,           "Corruption",           SURVEILLANCE,  false),
        reg(E::INFRARED,             "Infrared",             SURVEILLANCE,  true),
        reg(E::SURVEILLANCE,         "Surveillance",         SURVEILLANCE,  false),
        reg(E::REDACTION,            "Redaction",             SURVEILLANCE,  false),
    ]
}

fn reg(
    id: EffectTypeId,
    display_name: &'static str,
    category: &'static str,
    available: bool,
) -> EffectTypeRegistration {
    EffectTypeRegistration { id, display_name, category, available }
}

// ── Public API ──────────────────────────────────────────────────────────

/// All registered effect types.
pub fn all() -> &'static [EffectTypeRegistration] {
    &REGISTRY
}

/// Get the display name for an effect type. Returns the ID string as fallback.
pub fn display_name(id: &EffectTypeId) -> &str {
    REGISTRY.iter()
        .find(|r| r.id == *id)
        .map(|r| r.display_name)
        .unwrap_or(id.as_str())
}

/// Get the category for an effect type. Returns "Post-Process" as fallback.
pub fn category(id: &EffectTypeId) -> &str {
    REGISTRY.iter()
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
