//! Envelope edit commands, addressed by [`GraphTarget`].
//!
//! Envelope-home unification: an envelope lives on the `PresetInstance` it
//! modulates (an effect addressed by id, or a layer's generator), so every
//! command resolves its instance through [`Project::with_preset_graph_mut`]
//! and edits that instance's `envelopes` by index. There is no layer-scoped
//! envelope pool anymore.

use crate::command::Command;
use manifold_core::GraphTarget;
use manifold_core::effects::ParamEnvelope;
use manifold_core::project::Project;

/// Add an envelope to the instance addressed by `target`.
#[derive(Debug)]
pub struct AddEnvelopeCommand {
    target: GraphTarget,
    envelope: ParamEnvelope,
}

impl AddEnvelopeCommand {
    pub fn new(target: GraphTarget, envelope: ParamEnvelope) -> Self {
        Self { target, envelope }
    }
}

impl Command for AddEnvelopeCommand {
    fn execute(&mut self, project: &mut Project) {
        let envelope = self.envelope.clone();
        project.with_preset_graph_mut(&self.target, |inst| {
            inst.envelopes_mut().push(envelope);
        });
    }

    fn undo(&mut self, project: &mut Project) {
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(envs) = inst.envelopes.as_mut() {
                envs.pop();
            }
        });
    }

    fn description(&self) -> &str {
        "Add Envelope"
    }
}

/// Remove an envelope (by index) from the instance addressed by `target`.
#[derive(Debug)]
pub struct RemoveEnvelopeCommand {
    target: GraphTarget,
    envelope: Option<ParamEnvelope>,
    removed_index: usize,
}

impl RemoveEnvelopeCommand {
    pub fn new(target: GraphTarget, envelope: ParamEnvelope, removed_index: usize) -> Self {
        Self {
            target,
            envelope: Some(envelope),
            removed_index,
        }
    }
}

impl Command for RemoveEnvelopeCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.removed_index;
        let removed = project.with_preset_graph_mut(&self.target, |inst| {
            inst.envelopes
                .as_mut()
                .filter(|envs| idx < envs.len())
                .map(|envs| envs.remove(idx))
        });
        if let Some(Some(env)) = removed {
            self.envelope = Some(env);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(envelope) = self.envelope.clone() else {
            return;
        };
        let idx = self.removed_index;
        project.with_preset_graph_mut(&self.target, |inst| {
            let envs = inst.envelopes_mut();
            let at = idx.min(envs.len());
            envs.insert(at, envelope);
        });
    }

    fn description(&self) -> &str {
        "Remove Envelope"
    }
}

/// Flip an existing envelope's `enabled` flag (by index) — the card's
/// envelope button when an envelope already exists (the no-envelope case is
/// [`AddEnvelopeCommand`]). Mirrors `ToggleDriverEnabledCommand`.
#[derive(Debug)]
pub struct ToggleEnvelopeEnabledCommand {
    target: GraphTarget,
    env_index: usize,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleEnvelopeEnabledCommand {
    pub fn new(target: GraphTarget, env_index: usize, old_enabled: bool, new_enabled: bool) -> Self {
        Self {
            target,
            env_index,
            old_enabled,
            new_enabled,
        }
    }
}

impl Command for ToggleEnvelopeEnabledCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.env_index;
        let val = self.new_enabled;
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(e) = inst.envelopes.as_mut().and_then(|envs| envs.get_mut(idx)) {
                e.enabled = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.env_index;
        let val = self.old_enabled;
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(e) = inst.envelopes.as_mut().and_then(|envs| envs.get_mut(idx)) {
                e.enabled = val;
            }
        });
    }

    fn description(&self) -> &str {
        "Toggle Envelope"
    }
}

/// Change an envelope's `decay_beats` — the card's single envelope slider (by index).
#[derive(Debug)]
pub struct ChangeEnvelopeDecayCommand {
    target: GraphTarget,
    env_index: usize,
    old_decay: f32,
    new_decay: f32,
}

impl ChangeEnvelopeDecayCommand {
    pub fn new(target: GraphTarget, env_index: usize, old_decay: f32, new_decay: f32) -> Self {
        Self {
            target,
            env_index,
            old_decay,
            new_decay,
        }
    }

    fn apply(project: &mut Project, target: &GraphTarget, idx: usize, value: f32) {
        project.with_preset_graph_mut(target, |inst| {
            if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(idx)) {
                env.decay_beats = value;
            }
        });
    }
}

impl Command for ChangeEnvelopeDecayCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.new_decay);
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.old_decay);
    }

    fn description(&self) -> &str {
        "Change Envelope Decay"
    }
}

/// Change an envelope's `target_normalized` — the orange target handle (by index).
#[derive(Debug)]
pub struct ChangeEnvelopeTargetCommand {
    target: GraphTarget,
    env_index: usize,
    old_target: f32,
    new_target: f32,
}

impl ChangeEnvelopeTargetCommand {
    pub fn new(target: GraphTarget, env_index: usize, old_target: f32, new_target: f32) -> Self {
        Self {
            target,
            env_index,
            old_target,
            new_target,
        }
    }

    fn apply(project: &mut Project, target: &GraphTarget, idx: usize, value: f32) {
        project.with_preset_graph_mut(target, |inst| {
            if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(idx)) {
                env.target_normalized = value;
            }
        });
    }
}

impl Command for ChangeEnvelopeTargetCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.new_target);
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.old_target);
    }

    fn description(&self) -> &str {
        "Change Envelope Target"
    }
}
