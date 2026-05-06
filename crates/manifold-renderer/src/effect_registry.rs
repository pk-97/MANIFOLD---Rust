use crate::effect::PostProcessEffect;
use manifold_core::EffectTypeId;
use manifold_gpu::GpuDevice;
use std::collections::HashMap;

/// Factory + singleton storage for all effect processors.
/// One processor per EffectTypeId — per-owner state lives inside each processor.
///
/// All effects are registered via `inventory::submit!` in their implementation files.
pub struct EffectRegistry {
    processors: HashMap<EffectTypeId, Box<dyn PostProcessEffect>>,
}

impl EffectRegistry {
    pub fn new(device: &GpuDevice) -> Self {
        let mut processors: HashMap<EffectTypeId, Box<dyn PostProcessEffect>> = HashMap::new();

        // Collect all inventory-registered effects
        for factory in inventory::iter::<crate::effects::registration::EffectFactory> {
            processors.insert(factory.id.clone(), (factory.create)(device));
        }

        Self { processors }
    }

    /// Register an effect processor for a given type.
    pub fn register(&mut self, effect_type: EffectTypeId, processor: Box<dyn PostProcessEffect>) {
        self.processors.insert(effect_type, processor);
    }

    /// Get a mutable reference to the processor for an effect type.
    pub fn get_mut(
        &mut self,
        effect_type: &EffectTypeId,
    ) -> Option<&mut Box<dyn PostProcessEffect>> {
        self.processors.get_mut(effect_type)
    }

    /// Clear all temporal state across all processors (e.g., on seek).
    pub fn clear_all_state(&mut self) {
        for processor in self.processors.values_mut() {
            processor.clear_state();
        }
    }

    /// Resize all processors to new dimensions.
    pub fn resize_all(&mut self, device: &GpuDevice, width: u32, height: u32) {
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

    /// Flush all in-flight background work across all processors.
    /// Called after each export frame for deterministic async pipeline resolution.
    pub fn flush_all_background_work(&mut self) {
        for processor in self.processors.values_mut() {
            processor.flush_background_work();
        }
    }

    /// Check if any processor is registered.
    pub fn has_any(&self) -> bool {
        !self.processors.is_empty()
    }

    /// Snapshot the graph of a specific registered effect type.
    /// Returns `None` if the type isn't registered or doesn't expose
    /// a graph. This is the lookup the editor canvas uses once the
    /// user clicks a cog on a specific effect card.
    pub fn graph_snapshot_for(
        &self,
        type_id: &EffectTypeId,
    ) -> Option<crate::node_graph::GraphSnapshot> {
        self.processors.get(type_id).and_then(|p| p.graph_snapshot())
    }
}
