use manifold_core::effects::PresetInstance;

/// Static effect clipboard. Port of C# EffectClipboard.
pub struct EffectClipboard {
    clips: Vec<PresetInstance>,
}

impl EffectClipboard {
    pub fn new() -> Self {
        Self { clips: Vec::new() }
    }

    pub fn has_content(&self) -> bool {
        !self.clips.is_empty()
    }

    pub fn count(&self) -> usize {
        self.clips.len()
    }

    pub fn copy_single(&mut self, effect: &PresetInstance) {
        self.clips.clear();
        self.clips.push(effect.clone());
    }

    pub fn copy_all(&mut self, effects: &[PresetInstance]) {
        self.clips.clear();
        self.clips.extend(effects.iter().cloned());
    }

    /// Get fresh clones for paste.
    pub fn get_paste_clones(&self) -> Vec<PresetInstance> {
        self.clips.clone()
    }

    pub fn clear(&mut self) {
        self.clips.clear();
    }
}

impl Default for EffectClipboard {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Generator Clipboard ───

use manifold_core::effects::{ParamEnvelope, ParameterDriver};
use manifold_core::preset_type_id::PresetTypeId;

/// Snapshot of a generator's complete state for copy/paste.
#[derive(Debug, Clone)]
pub struct GeneratorSnapshot {
    pub generator_type: PresetTypeId,
    pub param_values: Vec<f32>,
    pub base_param_values: Option<Vec<f32>>,
    pub drivers: Option<Vec<ParameterDriver>>,
    pub envelopes: Option<Vec<ParamEnvelope>>,
}

/// Generator clipboard — stores one generator setup for paste.
pub struct GeneratorClipboard {
    snapshot: Option<GeneratorSnapshot>,
}

impl GeneratorClipboard {
    pub fn new() -> Self {
        Self { snapshot: None }
    }

    pub fn has_content(&self) -> bool {
        self.snapshot.is_some()
    }

    pub fn copy_from(&mut self, state: &PresetInstance) {
        self.snapshot = Some(GeneratorSnapshot {
            generator_type: state.generator_type().clone(),
            // Clipboard carries effective float values; exposure is host
            // state and doesn't travel with a copy/paste.
            param_values: state.params.iter().map(|p| p.value).collect(),
            // base rides each Param now; snapshot it as the former
            // Option<Vec<f32>> shape, present iff base is tracked.
            base_param_values: state
                .base_tracked
                .then(|| state.params.iter().map(|p| p.base).collect()),
            drivers: state.drivers.clone(),
            envelopes: state.envelopes.clone(),
        });
    }

    pub fn get_paste_snapshot(&self) -> Option<GeneratorSnapshot> {
        self.snapshot.clone()
    }
}

impl Default for GeneratorClipboard {
    fn default() -> Self {
        Self::new()
    }
}
