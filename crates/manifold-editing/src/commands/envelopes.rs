//! Envelope edit commands, addressed by [`GraphTarget`].
//!
//! Envelope-home unification: an envelope lives on the `PresetInstance` it
//! modulates (an effect addressed by id, or a layer's generator), so every
//! command resolves its instance through [`Project::with_preset_graph_mut`]
//! and edits that instance's `envelopes` by index. There is no layer-scoped
//! envelope pool anymore.

use crate::command::Command;
use crate::commands::graph::KeepOnCard;
use manifold_core::GraphTarget;
use manifold_core::audio_mod::TriggerAction;
use manifold_core::change::{ChangeSet, Part};
use manifold_core::effects::ParamEnvelope;
use manifold_core::project::Project;

/// Add an envelope to the instance addressed by `target`.
#[derive(Debug)]
pub struct AddEnvelopeCommand {
    target: GraphTarget,
    envelope: ParamEnvelope,
    keep: KeepOnCard,
}

impl AddEnvelopeCommand {
    pub fn new(target: GraphTarget, envelope: ParamEnvelope) -> Self {
        Self {
            target,
            envelope,
            keep: KeepOnCard::default(),
        }
    }
}

impl Command for AddEnvelopeCommand {
    fn touches(&self) -> ChangeSet {
        ChangeSet::of(&[Part::Effects, Part::Shapes])
    }
    fn do_execute(&mut self, project: &mut Project) {
        let envelope = self.envelope.clone();
        project.with_preset_graph_mut(&self.target, |inst| {
            inst.envelopes_mut().push(envelope);
        });
        self.keep
            .attach(project, &self.target, &self.envelope.param_id);
    }

    fn do_undo(&mut self, project: &mut Project) {
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(envs) = inst.envelopes.as_mut() {
                envs.pop();
            }
        });
        self.keep.undo(project);
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
    fn touches(&self) -> ChangeSet {
        ChangeSet::of(&[Part::Effects, Part::Shapes])
    }
    fn do_execute(&mut self, project: &mut Project) {
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

    fn do_undo(&mut self, project: &mut Project) {
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
    keep: KeepOnCard,
}

impl ToggleEnvelopeEnabledCommand {
    pub fn new(
        target: GraphTarget,
        env_index: usize,
        old_enabled: bool,
        new_enabled: bool,
    ) -> Self {
        Self {
            target,
            env_index,
            old_enabled,
            new_enabled,
            keep: KeepOnCard::default(),
        }
    }
}

impl Command for ToggleEnvelopeEnabledCommand {
    fn touches(&self) -> ChangeSet {
        ChangeSet::of(&[Part::Effects, Part::Shapes])
    }
    fn do_execute(&mut self, project: &mut Project) {
        let idx = self.env_index;
        let val = self.new_enabled;
        let param_id = project
            .with_preset_graph_mut(&self.target, |inst| {
                inst.envelopes
                    .as_mut()
                    .and_then(|envs| envs.get_mut(idx))
                    .map(|e| {
                        e.enabled = val;
                        e.param_id.clone()
                    })
            })
            .flatten();
        if let Some(param_id) = param_id.filter(|_| val) {
            self.keep.attach(project, &self.target, &param_id);
        }
    }

    fn do_undo(&mut self, project: &mut Project) {
        let idx = self.env_index;
        let val = self.old_enabled;
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(e) = inst.envelopes.as_mut().and_then(|envs| envs.get_mut(idx)) {
                e.enabled = val;
            }
        });
        self.keep.undo(project);
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
    fn touches(&self) -> manifold_core::change::ChangeSet {
        ChangeSet::of(&[Part::Shapes])
    }
    fn do_execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.new_decay);
    }

    fn do_undo(&mut self, project: &mut Project) {
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
    fn touches(&self) -> manifold_core::change::ChangeSet {
        ChangeSet::of(&[Part::Shapes])
    }
    fn do_execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.new_target);
    }

    fn do_undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.old_target);
    }

    fn description(&self) -> &str {
        "Change Envelope Target"
    }
}

/// PARAM_STEP_ACTIONS D8: change an envelope's fire ACTION (Continuous/Step/
/// Random) — the T drawer's Action/Amount/Wrap rows. `TriggerAction::Step`
/// bundles `amount` + `wrap` in one Rust field, so one command covers all
/// three drawer controls (Action segment, Amount drag, Wrap segment), mirroring
/// `SetAudioModActionCommand`.
#[derive(Debug)]
pub struct SetEnvelopeActionCommand {
    target: GraphTarget,
    env_index: usize,
    old_action: TriggerAction,
    new_action: TriggerAction,
}

impl SetEnvelopeActionCommand {
    pub fn new(
        target: GraphTarget,
        env_index: usize,
        old_action: TriggerAction,
        new_action: TriggerAction,
    ) -> Self {
        Self {
            target,
            env_index,
            old_action,
            new_action,
        }
    }

    fn apply(project: &mut Project, target: &GraphTarget, idx: usize, action: TriggerAction) {
        project.with_preset_graph_mut(target, |inst| {
            if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(idx)) {
                env.action = action;
            }
        });
    }
}

impl Command for SetEnvelopeActionCommand {
    fn touches(&self) -> ChangeSet {
        crate::commands::audio_mod::action_touches(self.old_action, self.new_action)
    }
    fn do_execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.new_action);
    }

    fn do_undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.old_action);
    }

    fn description(&self) -> &str {
        "Set Envelope Action"
    }
}
