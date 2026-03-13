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
