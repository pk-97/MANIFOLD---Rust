use std::collections::HashMap;
use manifold_core::EffectType;
use crate::effect::PostProcessEffect;

/// Factory + singleton storage for all effect processors.
/// One processor per EffectType — per-owner state lives inside each processor.
pub struct EffectRegistry {
    processors: HashMap<EffectType, Box<dyn PostProcessEffect>>,
}

impl EffectRegistry {
    pub fn new(_device: &wgpu::Device) -> Self {
        let processors = HashMap::new();
        // Concrete effects will be registered here as they are implemented:
        // processors.insert(EffectType::InvertColors, Box::new(InvertColorsFX::new(device)));
        // processors.insert(EffectType::ColorGrade, Box::new(ColorGradeFX::new(device)));
        // etc.
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
