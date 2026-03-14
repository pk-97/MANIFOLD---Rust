use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::project::Project;
use manifold_core::effects::EffectInstance;

/// Add an effect to a target's effect chain.
#[derive(Debug)]
pub struct AddEffectCommand {
    target: EffectTarget,
    effect: EffectInstance,
    insert_index: usize,
}

impl AddEffectCommand {
    pub fn new(target: EffectTarget, effect: EffectInstance, insert_index: usize) -> Self {
        Self { target, effect, insert_index }
    }
}

impl Command for AddEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        with_effects_mut(project, &self.target, |effects, _groups| {
            let idx = self.insert_index.min(effects.len());
            effects.insert(idx, self.effect.clone());
        });
    }

    fn undo(&mut self, project: &mut Project) {
        with_effects_mut(project, &self.target, |effects, _groups| {
            let idx = self.insert_index.min(effects.len().saturating_sub(1));
            if idx < effects.len() {
                effects.remove(idx);
            }
        });
    }

    fn description(&self) -> &str { "Add Effect" }
}

/// Remove an effect from a target's effect chain.
#[derive(Debug)]
pub struct RemoveEffectCommand {
    target: EffectTarget,
    effect: Option<EffectInstance>,
    removed_index: usize,
}

impl RemoveEffectCommand {
    pub fn new(target: EffectTarget, effect: EffectInstance, removed_index: usize) -> Self {
        Self { target, effect: Some(effect), removed_index }
    }
}

impl Command for RemoveEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        with_effects_mut(project, &self.target, |effects, _groups| {
            if self.removed_index < effects.len() {
                self.effect = Some(effects.remove(self.removed_index));
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(effect) = &self.effect {
            let effect = effect.clone();
            let idx = self.removed_index;
            with_effects_mut(project, &self.target, |effects, _groups| {
                let insert_idx = idx.min(effects.len());
                effects.insert(insert_idx, effect);
            });
        }
    }

    fn description(&self) -> &str { "Remove Effect" }
}

/// Reorder an effect within a target's effect chain.
#[derive(Debug)]
pub struct ReorderEffectCommand {
    target: EffectTarget,
    from_index: usize,
    to_index: usize,
}

impl ReorderEffectCommand {
    pub fn new(target: EffectTarget, from_index: usize, to_index: usize) -> Self {
        Self { target, from_index, to_index }
    }
}

impl Command for ReorderEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        let from = self.from_index;
        let to = self.to_index;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if from < effects.len() {
                let effect = effects.remove(from);
                let insert_idx = to.min(effects.len());
                effects.insert(insert_idx, effect);
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        // Reverse: move from to_index back to from_index
        let to = self.to_index;
        let from = self.from_index;
        with_effects_mut(project, &self.target, |effects, _groups| {
            let actual_to = to.min(effects.len().saturating_sub(1));
            if actual_to < effects.len() {
                let effect = effects.remove(actual_to);
                let insert_idx = from.min(effects.len());
                effects.insert(insert_idx, effect);
            }
        });
    }

    fn description(&self) -> &str { "Reorder Effect" }
}

/// Toggle an effect's enabled state.
#[derive(Debug)]
pub struct ToggleEffectCommand {
    target: EffectTarget,
    effect_index: usize,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleEffectCommand {
    pub fn new(target: EffectTarget, effect_index: usize, old_enabled: bool, new_enabled: bool) -> Self {
        Self { target, effect_index, old_enabled, new_enabled }
    }
}

impl Command for ToggleEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.effect_index;
        let new_val = self.new_enabled;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(idx) {
                effect.enabled = new_val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.effect_index;
        let old_val = self.old_enabled;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(idx) {
                effect.enabled = old_val;
            }
        });
    }

    fn description(&self) -> &str { "Toggle Effect" }
}

/// Change a single parameter value on an effect.
#[derive(Debug)]
pub struct ChangeEffectParamCommand {
    target: EffectTarget,
    effect_index: usize,
    param_index: usize,
    old_value: f32,
    new_value: f32,
}

impl ChangeEffectParamCommand {
    pub fn new(target: EffectTarget, effect_index: usize, param_index: usize, old_value: f32, new_value: f32) -> Self {
        Self { target, effect_index, param_index, old_value, new_value }
    }
}

impl Command for ChangeEffectParamCommand {
    fn execute(&mut self, project: &mut Project) {
        let eidx = self.effect_index;
        let pidx = self.param_index;
        let val = self.new_value;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(eidx) {
                effect.set_base_param(pidx, val);
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let eidx = self.effect_index;
        let pidx = self.param_index;
        let val = self.old_value;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(eidx) {
                effect.set_base_param(pidx, val);
            }
        });
    }

    fn description(&self) -> &str { "Change Effect Param" }
}
