use crate::command::Command;
use manifold_core::LayerId;
use manifold_core::effects::ParamEnvelope;
use manifold_core::project::Project;

/// Add a layer envelope.
#[derive(Debug)]
pub struct AddLayerEnvelopeCommand {
    layer_id: LayerId,
    envelope: ParamEnvelope,
}

impl AddLayerEnvelopeCommand {
    pub fn new(layer_id: LayerId, envelope: ParamEnvelope) -> Self {
        Self { layer_id, envelope }
    }
}

impl Command for AddLayerEnvelopeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.envelopes_mut().push(self.envelope.clone());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.envelopes_mut().pop();
        }
    }

    fn description(&self) -> &str {
        "Add Layer Envelope"
    }
}

/// Remove a layer envelope.
#[derive(Debug)]
pub struct RemoveLayerEnvelopeCommand {
    layer_id: LayerId,
    envelope: Option<ParamEnvelope>,
    removed_index: usize,
}

impl RemoveLayerEnvelopeCommand {
    pub fn new(layer_id: LayerId, envelope: ParamEnvelope, removed_index: usize) -> Self {
        Self {
            layer_id,
            envelope: Some(envelope),
            removed_index,
        }
    }
}

impl Command for RemoveLayerEnvelopeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            let envs = layer.envelopes_mut();
            if self.removed_index < envs.len() {
                self.envelope = Some(envs.remove(self.removed_index));
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(envelope) = &self.envelope
            && let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id)
        {
            let envs = layer.envelopes_mut();
            let idx = self.removed_index.min(envs.len());
            envs.insert(idx, envelope.clone());
        }
    }

    fn description(&self) -> &str {
        "Remove Layer Envelope"
    }
}

/// Change ADSR values on a layer envelope.
#[derive(Debug)]
pub struct ChangeLayerEnvelopeADSRCommand {
    layer_id: LayerId,
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

impl ChangeLayerEnvelopeADSRCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        layer_id: LayerId,
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
            layer_id,
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

    fn apply(layer: &mut manifold_core::layer::Layer, idx: usize, a: f32, d: f32, s: f32, r: f32) {
        let envs = layer.envelopes_mut();
        if let Some(env) = envs.get_mut(idx) {
            env.attack_beats = a;
            env.decay_beats = d;
            env.sustain_level = s;
            env.release_beats = r;
        }
    }
}

impl Command for ChangeLayerEnvelopeADSRCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            Self::apply(
                layer,
                self.env_index,
                self.new_attack,
                self.new_decay,
                self.new_sustain,
                self.new_release,
            );
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            Self::apply(
                layer,
                self.env_index,
                self.old_attack,
                self.old_decay,
                self.old_sustain,
                self.old_release,
            );
        }
    }

    fn description(&self) -> &str {
        "Change Layer Envelope ADSR"
    }
}

/// Change layer envelope target_normalized value.
#[derive(Debug)]
pub struct ChangeLayerEnvelopeTargetCommand {
    layer_id: LayerId,
    env_index: usize,
    old_target: f32,
    new_target: f32,
}

impl ChangeLayerEnvelopeTargetCommand {
    pub fn new(layer_id: LayerId, env_index: usize, old_target: f32, new_target: f32) -> Self {
        Self {
            layer_id,
            env_index,
            old_target,
            new_target,
        }
    }
}

impl Command for ChangeLayerEnvelopeTargetCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            let envs = layer.envelopes_mut();
            if let Some(env) = envs.get_mut(self.env_index) {
                env.target_normalized = self.new_target;
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            let envs = layer.envelopes_mut();
            if let Some(env) = envs.get_mut(self.env_index) {
                env.target_normalized = self.old_target;
            }
        }
    }

    fn description(&self) -> &str {
        "Change Layer Envelope Target"
    }
}
