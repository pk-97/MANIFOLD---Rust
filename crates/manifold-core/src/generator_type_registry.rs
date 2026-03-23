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

static REGISTRY: LazyLock<Vec<GeneratorTypeRegistration>> = LazyLock::new(build_registry);

fn build_registry() -> Vec<GeneratorTypeRegistration> {
    use GeneratorTypeId as G;
    vec![
        reg(G::PLASMA,                     "Plasma",               true),
        reg(G::CONCENTRIC_TUNNEL,           "Concentric Tunnel",    true),
        reg(G::LISSAJOUS,                   "Lissajous",            true),
        reg(G::FRACTAL_ZOOM,                "Fractal Zoom",         true),
        reg(G::FLOWFIELD,                   "Flowfield",            true),
        reg(G::REACTION_DIFFUSION,          "Reaction Diffusion",   true),
        reg(G::FLUID_SIMULATION,            "Fluid Simulation",     true),
        reg(G::FLUID_SIMULATION_3D,         "Fluid Sim 3D",        true),
        reg(G::BASIC_SHAPES_SNAP,           "Basic Shapes",         true),
        reg(G::DUOCYLINDER,                 "Duocylinder",          true),
        reg(G::TESSERACT,                   "Tesseract",            true),
        reg(G::OSCILLOSCOPE_XY,             "Oscilloscope XY",      true),
        reg(G::WIREFRAME_ZOO,               "Wireframe Zoo",        true),
        reg(G::PARAMETRIC_SURFACE,          "Parametric Surface",   true),
        reg(G::STRANGE_ATTRACTOR,           "Strange Attractor",    true),
        reg(G::COMPUTE_STRANGE_ATTRACTOR,   "Compute Attractor",    true),
        reg(G::NUMBER_STATION,              "Number Station",       true),
        reg(G::MYCELIUM,                    "Mycelium",             true),
        reg(G::MRI_VOLUME,                  "MRI Volume",           true),
    ]
}

fn reg(
    id: GeneratorTypeId,
    display_name: &'static str,
    available: bool,
) -> GeneratorTypeRegistration {
    GeneratorTypeRegistration { id, display_name, available }
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
    REGISTRY.iter()
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
