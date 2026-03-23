//! Static registry mapping EffectType and GeneratorType to display categories.
//! Port of Unity EffectCategoryRegistry.cs.

use crate::effect_type_id::EffectTypeId;
use crate::generator_type_id::GeneratorTypeId;
use std::collections::HashMap;
use std::sync::LazyLock;

pub const SPATIAL: &str = "Spatial";
pub const POST_PROCESS: &str = "Post-Process";
pub const FILMIC: &str = "Filmic";
pub const SURVEILLANCE: &str = "Surveillance";
pub const GENERATORS: &str = "Generators";

pub const ALL_CATEGORIES: &[&str] = &[SPATIAL, POST_PROCESS, FILMIC, SURVEILLANCE, GENERATORS];

static EFFECT_CATEGORIES: LazyLock<HashMap<EffectTypeId, &'static str>> = LazyLock::new(build_effect_categories);
static GENERATOR_CATEGORIES: LazyLock<HashMap<GeneratorTypeId, &'static str>> = LazyLock::new(build_generator_categories);

pub fn get_all_categories() -> &'static [&'static str] {
    ALL_CATEGORIES
}

/// Get the category for an effect type. Returns PostProcess as fallback.
pub fn get_category(effect_type: &EffectTypeId) -> &'static str {
    EFFECT_CATEGORIES.get(effect_type).copied().unwrap_or(POST_PROCESS)
}

/// Get the category for a generator type. Always returns Generators.
pub fn get_category_for_generator(_gen_type: &GeneratorTypeId) -> &'static str {
    GENERATORS
}

/// Get all effect types in a given category.
pub fn get_effects_in_category(category: &str) -> Vec<EffectTypeId> {
    EFFECT_CATEGORIES.iter()
        .filter(|(_, cat)| **cat == category)
        .map(|(et, _)| et.clone())
        .collect()
}

/// Get all generator types (except None).
pub fn get_generators() -> Vec<GeneratorTypeId> {
    GENERATOR_CATEGORIES.keys().cloned().collect()
}

fn build_effect_categories() -> HashMap<EffectTypeId, &'static str> {
    let mut m = HashMap::new();
    // Spatial
    m.insert(EffectTypeId::TRANSFORM, SPATIAL);
    m.insert(EffectTypeId::INVERT_COLORS, SPATIAL);
    // Post-Process
    m.insert(EffectTypeId::FEEDBACK, POST_PROCESS);
    m.insert(EffectTypeId::PIXEL_SORT, POST_PROCESS);
    m.insert(EffectTypeId::BLOOM, POST_PROCESS);
    m.insert(EffectTypeId::INFINITE_ZOOM, POST_PROCESS);
    m.insert(EffectTypeId::KALEIDOSCOPE, POST_PROCESS);
    m.insert(EffectTypeId::EDGE_STRETCH, POST_PROCESS);
    m.insert(EffectTypeId::VORONOI_PRISM, POST_PROCESS);
    m.insert(EffectTypeId::QUAD_MIRROR, POST_PROCESS);
    m.insert(EffectTypeId::DITHER, POST_PROCESS);
    m.insert(EffectTypeId::STROBE, POST_PROCESS);
    m.insert(EffectTypeId::STYLIZED_FEEDBACK, POST_PROCESS);
    m.insert(EffectTypeId::MIRROR, POST_PROCESS);
    m.insert(EffectTypeId::BLOB_TRACKING, POST_PROCESS);
    m.insert(EffectTypeId::CRT, POST_PROCESS);
    m.insert(EffectTypeId::FLUID_DISTORTION, POST_PROCESS);
    m.insert(EffectTypeId::EDGE_GLOW, POST_PROCESS);
    m.insert(EffectTypeId::DATAMOSH, POST_PROCESS);
    m.insert(EffectTypeId::SLIT_SCAN, POST_PROCESS);
    m.insert(EffectTypeId::COLOR_GRADE, POST_PROCESS);
    m.insert(EffectTypeId::WIREFRAME_DEPTH, POST_PROCESS);
    // Filmic
    m.insert(EffectTypeId::CHROMATIC_ABERRATION, FILMIC);
    m.insert(EffectTypeId::GRADIENT_MAP, FILMIC);
    m.insert(EffectTypeId::GLITCH, FILMIC);
    m.insert(EffectTypeId::FILM_GRAIN, FILMIC);
    m.insert(EffectTypeId::HALATION, FILMIC);
    // Surveillance
    m.insert(EffectTypeId::CORRUPTION, SURVEILLANCE);
    m.insert(EffectTypeId::INFRARED, SURVEILLANCE);
    m.insert(EffectTypeId::SURVEILLANCE, SURVEILLANCE);
    m.insert(EffectTypeId::REDACTION, SURVEILLANCE);
    m
}

fn build_generator_categories() -> HashMap<GeneratorTypeId, &'static str> {
    let mut m = HashMap::new();
    m.insert(GeneratorTypeId::BASIC_SHAPES_SNAP, GENERATORS);
    m.insert(GeneratorTypeId::DUOCYLINDER, GENERATORS);
    m.insert(GeneratorTypeId::TESSERACT, GENERATORS);
    m.insert(GeneratorTypeId::CONCENTRIC_TUNNEL, GENERATORS);
    m.insert(GeneratorTypeId::PLASMA, GENERATORS);
    m.insert(GeneratorTypeId::LISSAJOUS, GENERATORS);
    m.insert(GeneratorTypeId::FRACTAL_ZOOM, GENERATORS);
    m.insert(GeneratorTypeId::OSCILLOSCOPE_XY, GENERATORS);
    m.insert(GeneratorTypeId::WIREFRAME_ZOO, GENERATORS);
    m.insert(GeneratorTypeId::REACTION_DIFFUSION, GENERATORS);
    m.insert(GeneratorTypeId::FLOWFIELD, GENERATORS);
    m.insert(GeneratorTypeId::PARAMETRIC_SURFACE, GENERATORS);
    m.insert(GeneratorTypeId::STRANGE_ATTRACTOR, GENERATORS);
    m.insert(GeneratorTypeId::FLUID_SIMULATION, GENERATORS);
    m.insert(GeneratorTypeId::NUMBER_STATION, GENERATORS);
    m.insert(GeneratorTypeId::MYCELIUM, GENERATORS);
    m.insert(GeneratorTypeId::COMPUTE_STRANGE_ATTRACTOR, GENERATORS);
    m.insert(GeneratorTypeId::FLUID_SIMULATION_3D, GENERATORS);
    m
}
