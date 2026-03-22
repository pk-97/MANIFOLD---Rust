//! Static registry mapping EffectType and GeneratorType to display categories.
//! Port of Unity EffectCategoryRegistry.cs.

use crate::types::{EffectType, GeneratorType};
use std::collections::HashMap;
use std::sync::LazyLock;

pub const SPATIAL: &str = "Spatial";
pub const POST_PROCESS: &str = "Post-Process";
pub const FILMIC: &str = "Filmic";
pub const SURVEILLANCE: &str = "Surveillance";
pub const GENERATORS: &str = "Generators";

pub const ALL_CATEGORIES: &[&str] = &[SPATIAL, POST_PROCESS, FILMIC, SURVEILLANCE, GENERATORS];

static EFFECT_CATEGORIES: LazyLock<HashMap<EffectType, &'static str>> = LazyLock::new(build_effect_categories);
static GENERATOR_CATEGORIES: LazyLock<HashMap<GeneratorType, &'static str>> = LazyLock::new(build_generator_categories);

pub fn get_all_categories() -> &'static [&'static str] {
    ALL_CATEGORIES
}

/// Get the category for an effect type. Returns PostProcess as fallback.
pub fn get_category(effect_type: EffectType) -> &'static str {
    EFFECT_CATEGORIES.get(&effect_type).copied().unwrap_or(POST_PROCESS)
}

/// Get the category for a generator type. Always returns Generators.
pub fn get_category_for_generator(_gen_type: GeneratorType) -> &'static str {
    GENERATORS
}

/// Get all effect types in a given category.
pub fn get_effects_in_category(category: &str) -> Vec<EffectType> {
    EFFECT_CATEGORIES.iter()
        .filter(|(_, cat)| **cat == category)
        .map(|(et, _)| *et)
        .collect()
}

/// Get all generator types (except None).
pub fn get_generators() -> Vec<GeneratorType> {
    GENERATOR_CATEGORIES.keys().copied().collect()
}

fn build_effect_categories() -> HashMap<EffectType, &'static str> {
    let mut m = HashMap::new();
    // Spatial
    m.insert(EffectType::Transform, SPATIAL);
    m.insert(EffectType::InvertColors, SPATIAL);
    // Post-Process
    m.insert(EffectType::Feedback, POST_PROCESS);
    m.insert(EffectType::PixelSort, POST_PROCESS);
    m.insert(EffectType::Bloom, POST_PROCESS);
    m.insert(EffectType::InfiniteZoom, POST_PROCESS);
    m.insert(EffectType::Kaleidoscope, POST_PROCESS);
    m.insert(EffectType::EdgeStretch, POST_PROCESS);
    m.insert(EffectType::VoronoiPrism, POST_PROCESS);
    m.insert(EffectType::QuadMirror, POST_PROCESS);
    m.insert(EffectType::Dither, POST_PROCESS);
    m.insert(EffectType::Strobe, POST_PROCESS);
    m.insert(EffectType::StylizedFeedback, POST_PROCESS);
    m.insert(EffectType::Mirror, POST_PROCESS);
    m.insert(EffectType::BlobTracking, POST_PROCESS);
    m.insert(EffectType::CRT, POST_PROCESS);
    m.insert(EffectType::FluidDistortion, POST_PROCESS);
    m.insert(EffectType::EdgeGlow, POST_PROCESS);
    m.insert(EffectType::Datamosh, POST_PROCESS);
    m.insert(EffectType::SlitScan, POST_PROCESS);
    m.insert(EffectType::ColorGrade, POST_PROCESS);
    m.insert(EffectType::WireframeDepth, POST_PROCESS);
    // Filmic
    m.insert(EffectType::ChromaticAberration, FILMIC);
    m.insert(EffectType::GradientMap, FILMIC);
    m.insert(EffectType::Glitch, FILMIC);
    m.insert(EffectType::FilmGrain, FILMIC);
    m.insert(EffectType::Halation, FILMIC);
    // Surveillance
    m.insert(EffectType::Corruption, SURVEILLANCE);
    m.insert(EffectType::Infrared, SURVEILLANCE);
    m.insert(EffectType::Surveillance, SURVEILLANCE);
    m.insert(EffectType::Redaction, SURVEILLANCE);
    m
}

fn build_generator_categories() -> HashMap<GeneratorType, &'static str> {
    let mut m = HashMap::new();
    m.insert(GeneratorType::BasicShapesSnap, GENERATORS);
    m.insert(GeneratorType::Duocylinder, GENERATORS);
    m.insert(GeneratorType::Tesseract, GENERATORS);
    m.insert(GeneratorType::ConcentricTunnel, GENERATORS);
    m.insert(GeneratorType::Plasma, GENERATORS);
    m.insert(GeneratorType::Lissajous, GENERATORS);
    m.insert(GeneratorType::FractalZoom, GENERATORS);
    m.insert(GeneratorType::OscilloscopeXY, GENERATORS);
    m.insert(GeneratorType::WireframeZoo, GENERATORS);
    m.insert(GeneratorType::ReactionDiffusion, GENERATORS);
    m.insert(GeneratorType::Flowfield, GENERATORS);
    m.insert(GeneratorType::ParametricSurface, GENERATORS);
    m.insert(GeneratorType::StrangeAttractor, GENERATORS);
    m.insert(GeneratorType::FluidSimulation, GENERATORS);
    m.insert(GeneratorType::NumberStation, GENERATORS);
    m.insert(GeneratorType::Mycelium, GENERATORS);
    m.insert(GeneratorType::ComputeStrangeAttractor, GENERATORS);
    m.insert(GeneratorType::FluidSimulation3D, GENERATORS);
    m
}
