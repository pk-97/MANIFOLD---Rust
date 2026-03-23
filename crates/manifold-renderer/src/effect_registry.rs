use std::collections::HashMap;
use manifold_core::EffectTypeId;
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
use crate::effects::stylized_feedback::StylizedFeedbackFX;
use crate::effects::edge_glow::EdgeGlowFX;
use crate::effects::transform::TransformFX;
use crate::effects::infrared::InfraredFX;
use crate::effects::voronoi_prism::VoronoiPrismFX;
use crate::effects::pixel_sort::PixelSortFX;
use crate::effects::blob_tracking::BlobTrackingFX;
use crate::effects::wireframe_depth::WireframeDepthFX;

/// Factory + singleton storage for all effect processors.
/// One processor per EffectTypeId — per-owner state lives inside each processor.
pub struct EffectRegistry {
    processors: HashMap<EffectTypeId, Box<dyn PostProcessEffect>>,
}

impl EffectRegistry {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let mut processors: HashMap<EffectTypeId, Box<dyn PostProcessEffect>> = HashMap::new();
        processors.insert(EffectTypeId::INVERT_COLORS, Box::new(InvertColorsFX::new(device)));
        processors.insert(EffectTypeId::COLOR_GRADE, Box::new(ColorGradeFX::new(device)));
        processors.insert(EffectTypeId::MIRROR, Box::new(MirrorFX::new(device)));
        processors.insert(EffectTypeId::FEEDBACK, Box::new(FeedbackFX::new(device)));
        processors.insert(EffectTypeId::BLOOM, Box::new(BloomFX::new(device)));
        processors.insert(EffectTypeId::CHROMATIC_ABERRATION, Box::new(ChromaticAberrationFX::new(device)));
        processors.insert(EffectTypeId::FILM_GRAIN, Box::new(FilmGrainFX::new(device)));
        processors.insert(EffectTypeId::GLITCH, Box::new(GlitchFX::new(device)));
        processors.insert(EffectTypeId::DITHER, Box::new(DitherFX::new(device)));
        processors.insert(EffectTypeId::HALATION, Box::new(HalationFX::new(device)));
        processors.insert(EffectTypeId::KALEIDOSCOPE, Box::new(KaleidoscopeFX::new(device)));
        processors.insert(EffectTypeId::EDGE_STRETCH, Box::new(EdgeStretchFX::new(device)));
        processors.insert(EffectTypeId::QUAD_MIRROR, Box::new(QuadMirrorFX::new(device)));
        processors.insert(EffectTypeId::STROBE, Box::new(StrobeFX::new(device)));
        processors.insert(EffectTypeId::CRT, Box::new(CrtFX::new(device)));
        processors.insert(EffectTypeId::STYLIZED_FEEDBACK, Box::new(StylizedFeedbackFX::new(device)));
        processors.insert(EffectTypeId::EDGE_GLOW, Box::new(EdgeGlowFX::new(device)));
        processors.insert(EffectTypeId::TRANSFORM, Box::new(TransformFX::new(device)));
        processors.insert(EffectTypeId::INFRARED, Box::new(InfraredFX::new(device)));
        processors.insert(EffectTypeId::VORONOI_PRISM, Box::new(VoronoiPrismFX::new(device)));
        processors.insert(EffectTypeId::PIXEL_SORT, Box::new(PixelSortFX::new(device)));
        // BlobTrackingFX needs queue for font atlas upload
        processors.insert(EffectTypeId::BLOB_TRACKING, Box::new(BlobTrackingFX::new(device, queue)));
        // WireframeDepthFX needs queue for dummy texture uploads
        processors.insert(EffectTypeId::WIREFRAME_DEPTH, Box::new(WireframeDepthFX::new(device)));
        Self { processors }
    }

    /// Register an effect processor for a given type.
    pub fn register(&mut self, effect_type: EffectTypeId, processor: Box<dyn PostProcessEffect>) {
        self.processors.insert(effect_type, processor);
    }

    /// Get a mutable reference to the processor for an effect type.
    pub fn get_mut(&mut self, effect_type: &EffectTypeId) -> Option<&mut Box<dyn PostProcessEffect>> {
        self.processors.get_mut(effect_type)
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

    /// Clean up per-owner effect state for a specific clip.
    /// Called when a clip stops playback to release per-clip GPU resources.
    pub fn cleanup_clip_owner(&mut self, owner_key: i64) {
        for processor in self.processors.values_mut() {
            processor.cleanup_owner_state(owner_key);
        }
    }

    /// Check if any processor is registered.
    pub fn has_any(&self) -> bool {
        !self.processors.is_empty()
    }
}
