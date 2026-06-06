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

/// Change ADSR values on an envelope (by index).
#[derive(Debug)]
pub struct ChangeEnvelopeADSRCommand {
    target: GraphTarget,
    env_index: usize,
    old_attack: f32,
    old_decay: f32,
    old_sustain: f32,
    old_release: f32,
    new_attack: f32,
    new_decay: f32,
    new_sustain: f32,
    new_release: f32,
}

impl ChangeEnvelopeADSRCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: GraphTarget,
        env_index: usize,
        old_attack: f32,
        old_decay: f32,
        old_sustain: f32,
        old_release: f32,
        new_attack: f32,
        new_decay: f32,
        new_sustain: f32,
        new_release: f32,
    ) -> Self {
        Self {
            target,
            env_index,
            old_attack,
            old_decay,
            old_sustain,
            old_release,
            new_attack,
            new_decay,
            new_sustain,
            new_release,
        }
    }

    fn apply(project: &mut Project, target: &GraphTarget, idx: usize, a: f32, d: f32, s: f32, r: f32) {
        project.with_preset_graph_mut(target, |inst| {
            if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(idx)) {
                env.attack_beats = a;
                env.decay_beats = d;
                env.sustain_level = s;
                env.release_beats = r;
            }
        });
    }
}

impl Command for ChangeEnvelopeADSRCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply(
            project,
            &self.target,
            self.env_index,
            self.new_attack,
            self.new_decay,
            self.new_sustain,
            self.new_release,
        );
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply(
            project,
            &self.target,
            self.env_index,
            self.old_attack,
            self.old_decay,
            self.old_sustain,
            self.old_release,
        );
    }

    fn description(&self) -> &str {
        "Change Envelope ADSR"
    }
}

/// Change an envelope's `range_min`/`range_max` (by index).
#[derive(Debug)]
pub struct ChangeEnvelopeRangeCommand {
    target: GraphTarget,
    env_index: usize,
    old_min: f32,
    old_max: f32,
    new_min: f32,
    new_max: f32,
}

impl ChangeEnvelopeRangeCommand {
    pub fn new(
        target: GraphTarget,
        env_index: usize,
        old_min: f32,
        old_max: f32,
        new_min: f32,
        new_max: f32,
    ) -> Self {
        Self {
            target,
            env_index,
            old_min,
            old_max,
            new_min,
            new_max,
        }
    }

    fn apply(project: &mut Project, target: &GraphTarget, idx: usize, rmin: f32, rmax: f32) {
        project.with_preset_graph_mut(target, |inst| {
            if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(idx)) {
                env.range_min = rmin;
                env.range_max = rmax;
            }
        });
    }
}

impl Command for ChangeEnvelopeRangeCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.new_min, self.new_max);
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, self.env_index, self.old_min, self.old_max);
    }

    fn description(&self) -> &str {
        "Change Envelope Range"
    }
}

/// Change an envelope's `target_normalized` (by index).
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
