use crate::command::Command;
use manifold_core::project::Project;
use manifold_core::effects::ParamEnvelope;
use manifold_core::types::EffectType;

/// Add a param envelope to a clip.
#[derive(Debug)]
pub struct AddParamEnvelopeCommand {
    clip_id: String,
    envelope: ParamEnvelope,
}

impl AddParamEnvelopeCommand {
    pub fn new(clip_id: String, envelope: ParamEnvelope) -> Self {
        Self { clip_id, envelope }
    }
}

impl Command for AddParamEnvelopeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.envelopes_mut().push(self.envelope.clone());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            clip.envelopes_mut().pop();
        }
    }

    fn description(&self) -> &str { "Add Envelope" }
}

/// Remove a param envelope from a clip.
#[derive(Debug)]
pub struct RemoveParamEnvelopeCommand {
    clip_id: String,
    envelope: Option<ParamEnvelope>,
    removed_index: usize,
}

impl RemoveParamEnvelopeCommand {
    pub fn new(clip_id: String, envelope: ParamEnvelope, removed_index: usize) -> Self {
        Self { clip_id, envelope: Some(envelope), removed_index }
    }
}

impl Command for RemoveParamEnvelopeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            let envs = clip.envelopes_mut();
            if self.removed_index < envs.len() {
                self.envelope = Some(envs.remove(self.removed_index));
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(envelope) = &self.envelope {
            if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
                let envs = clip.envelopes_mut();
                let idx = self.removed_index.min(envs.len());
                envs.insert(idx, envelope.clone());
            }
        }
    }

    fn description(&self) -> &str { "Remove Envelope" }
}

/// Change ADSR values on an envelope.
#[derive(Debug)]
pub struct ChangeEnvelopeADSRCommand {
    clip_id: String,
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
        clip_id: String,
        env_index: usize,
        old_attack: f32, old_decay: f32, old_sustain: f32, old_release: f32,
        new_attack: f32, new_decay: f32, new_sustain: f32, new_release: f32,
    ) -> Self {
        Self {
            clip_id, env_index,
            old_attack, old_decay, old_sustain, old_release,
            new_attack, new_decay, new_sustain, new_release,
        }
    }

    fn apply(clip: &mut manifold_core::clip::TimelineClip, idx: usize, a: f32, d: f32, s: f32, r: f32) {
        let envs = clip.envelopes_mut();
        if let Some(env) = envs.get_mut(idx) {
            env.attack_beats = a;
            env.decay_beats = d;
            env.sustain_level = s;
            env.release_beats = r;
        }
    }
}

impl Command for ChangeEnvelopeADSRCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            Self::apply(clip, self.env_index, self.new_attack, self.new_decay, self.new_sustain, self.new_release);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            Self::apply(clip, self.env_index, self.old_attack, self.old_decay, self.old_sustain, self.old_release);
        }
    }

    fn description(&self) -> &str { "Change Envelope ADSR" }
}

/// Change envelope target (effect type + param index).
#[derive(Debug)]
pub struct ChangeParamEnvelopeTargetCommand {
    clip_id: String,
    env_index: usize,
    old_effect_type: EffectType,
    old_param_index: i32,
    new_effect_type: EffectType,
    new_param_index: i32,
}

impl ChangeParamEnvelopeTargetCommand {
    pub fn new(
        clip_id: String,
        env_index: usize,
        old_effect_type: EffectType,
        old_param_index: i32,
        new_effect_type: EffectType,
        new_param_index: i32,
    ) -> Self {
        Self { clip_id, env_index, old_effect_type, old_param_index, new_effect_type, new_param_index }
    }
}

impl Command for ChangeParamEnvelopeTargetCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            let envs = clip.envelopes_mut();
            if let Some(env) = envs.get_mut(self.env_index) {
                env.target_effect_type = self.new_effect_type;
                env.param_index = self.new_param_index;
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            let envs = clip.envelopes_mut();
            if let Some(env) = envs.get_mut(self.env_index) {
                env.target_effect_type = self.old_effect_type;
                env.param_index = self.old_param_index;
            }
        }
    }

    fn description(&self) -> &str { "Change Envelope Target" }
}

/// Toggle envelope enabled state.
#[derive(Debug)]
pub struct ToggleEnvelopeEnabledCommand {
    clip_id: String,
    env_index: usize,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleEnvelopeEnabledCommand {
    pub fn new(clip_id: String, env_index: usize, old_enabled: bool, new_enabled: bool) -> Self {
        Self { clip_id, env_index, old_enabled, new_enabled }
    }
}

impl Command for ToggleEnvelopeEnabledCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            let envs = clip.envelopes_mut();
            if let Some(env) = envs.get_mut(self.env_index) {
                env.enabled = self.new_enabled;
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) {
            let envs = clip.envelopes_mut();
            if let Some(env) = envs.get_mut(self.env_index) {
                env.enabled = self.old_enabled;
            }
        }
    }

    fn description(&self) -> &str { "Toggle Envelope" }
}

/// Add a layer envelope.
#[derive(Debug)]
pub struct AddLayerEnvelopeCommand {
    layer_index: usize,
    envelope: ParamEnvelope,
}

impl AddLayerEnvelopeCommand {
    pub fn new(layer_index: usize, envelope: ParamEnvelope) -> Self {
        Self { layer_index, envelope }
    }
}

impl Command for AddLayerEnvelopeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.envelopes_mut().push(self.envelope.clone());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.envelopes_mut().pop();
        }
    }

    fn description(&self) -> &str { "Add Layer Envelope" }
}

/// Remove a layer envelope.
#[derive(Debug)]
pub struct RemoveLayerEnvelopeCommand {
    layer_index: usize,
    envelope: Option<ParamEnvelope>,
    removed_index: usize,
}

impl RemoveLayerEnvelopeCommand {
    pub fn new(layer_index: usize, envelope: ParamEnvelope, removed_index: usize) -> Self {
        Self { layer_index, envelope: Some(envelope), removed_index }
    }
}

impl Command for RemoveLayerEnvelopeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            let envs = layer.envelopes_mut();
            if self.removed_index < envs.len() {
                self.envelope = Some(envs.remove(self.removed_index));
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(envelope) = &self.envelope {
            if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
                let envs = layer.envelopes_mut();
                let idx = self.removed_index.min(envs.len());
                envs.insert(idx, envelope.clone());
            }
        }
    }

    fn description(&self) -> &str { "Remove Layer Envelope" }
}
