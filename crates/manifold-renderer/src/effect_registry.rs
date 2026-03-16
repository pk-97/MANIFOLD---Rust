use std::collections::HashMap;
use manifold_core::EffectType;
use crate::effect::PostProcessEffect;
use crate::effects::invert_colors::InvertColorsFX;
use crate::effects::color_grade::ColorGradeFX;
use crate::effects::mirror::MirrorFX;
use crate::effects::feedback::FeedbackFX;
use crate::effects::bloom::BloomFX;
use crate::effects::chromatic_aberration::ChromaticAberrationFX;
use crate::effects::film_grain::FilmGrainFX;
use crate::effects::glitch::GlitchFX;
use crate::effects::dither::DitherFX;
use crate::effects::halation::HalationFX;
use crate::effects::kaleidoscope::KaleidoscopeFX;
use crate::effects::edge_stretch::EdgeStretchFX;
use crate::effects::quad_mirror::QuadMirrorFX;
use crate::effects::strobe::StrobeFX;
use crate::effects::crt::CrtFX;

/// Factory + singleton storage for all effect processors.
/// One processor per EffectType — per-owner state lives inside each processor.
pub struct EffectRegistry {
    processors: HashMap<EffectType, Box<dyn PostProcessEffect>>,
}

impl EffectRegistry {
    pub fn new(device: &wgpu::Device) -> Self {
        let mut processors: HashMap<EffectType, Box<dyn PostProcessEffect>> = HashMap::new();
        processors.insert(EffectType::InvertColors, Box::new(InvertColorsFX::new(device)));
        processors.insert(EffectType::ColorGrade, Box::new(ColorGradeFX::new(device)));
        processors.insert(EffectType::Mirror, Box::new(MirrorFX::new(device)));
        processors.insert(EffectType::Feedback, Box::new(FeedbackFX::new(device)));
        processors.insert(EffectType::Bloom, Box::new(BloomFX::new(device)));
        processors.insert(EffectType::ChromaticAberration, Box::new(ChromaticAberrationFX::new(device)));
        processors.insert(EffectType::FilmGrain, Box::new(FilmGrainFX::new(device)));
        processors.insert(EffectType::Glitch, Box::new(GlitchFX::new(device)));
        processors.insert(EffectType::Dither, Box::new(DitherFX::new(device)));
        processors.insert(EffectType::Halation, Box::new(HalationFX::new(device)));
        processors.insert(EffectType::Kaleidoscope, Box::new(KaleidoscopeFX::new(device)));
        processors.insert(EffectType::EdgeStretch, Box::new(EdgeStretchFX::new(device)));
        processors.insert(EffectType::QuadMirror, Box::new(QuadMirrorFX::new(device)));
        processors.insert(EffectType::Strobe, Box::new(StrobeFX::new(device)));
        processors.insert(EffectType::CRT, Box::new(CrtFX::new(device)));
        Self { processors }
    }

    /// Register an effect processor for a given type.
    pub fn register(&mut self, effect_type: EffectType, processor: Box<dyn PostProcessEffect>) {
        self.processors.insert(effect_type, processor);
    }

    /// Get a mutable reference to the processor for an effect type.
    pub fn get_mut(&mut self, effect_type: EffectType) -> Option<&mut Box<dyn PostProcessEffect>> {
        self.processors.get_mut(&effect_type)
    }

    /// Clear all temporal state across all processors (e.g., on seek).
    pub fn clear_all_state(&mut self) {
        for processor in self.processors.values_mut() {
            processor.clear_state();
        }
    }

    /// Resize all processors to new dimensions.
    pub fn resize_all(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        for processor in self.processors.values_mut() {
            processor.resize(device, width, height);
        }
    }

    /// Check if any processor is registered.
    pub fn has_any(&self) -> bool {
        !self.processors.is_empty()
    }
}
