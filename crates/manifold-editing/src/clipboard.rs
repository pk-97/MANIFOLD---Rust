use manifold_core::effects::EffectInstance;

/// Static effect clipboard. Port of C# EffectClipboard.
pub struct EffectClipboard {
    clips: Vec<EffectInstance>,
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

    pub fn copy_single(&mut self, effect: &EffectInstance) {
        self.clips.clear();
        self.clips.push(effect.clone());
    }

    pub fn copy_all(&mut self, effects: &[EffectInstance]) {
        self.clips.clear();
        self.clips.extend(effects.iter().cloned());
    }

    /// Get fresh clones for paste.
    pub fn get_paste_clones(&self) -> Vec<EffectInstance> {
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

use manifold_core::generator::GeneratorParamState;
use manifold_core::generator_type_id::GeneratorTypeId;
use manifold_core::effects::{ParameterDriver, ParamEnvelope};

/// Snapshot of a generator's complete state for copy/paste.
#[derive(Debug, Clone)]
pub struct GeneratorSnapshot {
    pub generator_type: GeneratorTypeId,
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

    pub fn copy_from(&mut self, state: &GeneratorParamState) {
        self.snapshot = Some(GeneratorSnapshot {
            generator_type: state.generator_type().clone(),
            param_values: state.param_values.clone(),
            base_param_values: state.base_param_values.clone(),
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
