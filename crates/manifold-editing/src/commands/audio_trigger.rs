//! Undoable command for the per-instance [`AudioTriggerMod`] (§8 D2/D6,
//! `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md`). Unlike [`super::audio_mod`]'s
//! per-param `ParameterAudioMod` list (one command per field: add / remove /
//! toggle-enabled / set-source / set-shape), `PresetInstance.audio_trigger` is
//! a single `Option` field — every edit (arm/disarm, send, band, sensitivity,
//! mode) replaces the whole value in one undo step, mirroring
//! [`super::audio_setup::SetAudioSendTriggersCommand`]'s whole-field-capture
//! shape rather than the per-param audio-mod command family.

use crate::command::Command;
use crate::commands::effect_target::DriverTarget;
use manifold_core::audio_trigger::AudioTriggerMod;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;

/// Resolve a [`DriverTarget`] to its backing [`PresetInstance`] — the same
/// two-arm addressing every other single-instance modulation command uses
/// (effect by stable `EffectId`, generator by the layer's own params).
fn resolve_instance_mut<'p>(
    project: &'p mut Project,
    target: &DriverTarget,
) -> Option<&'p mut PresetInstance> {
    match target {
        DriverTarget::Effect { effect_id } => project.find_effect_by_id_mut(effect_id),
        DriverTarget::GeneratorParam { layer_id } => {
            let (_, layer) = project.timeline.find_layer_by_id_mut(layer_id)?;
            Some(layer.gen_params_or_init())
        }
    }
}

/// Replace an instance's whole `audio_trigger` config. Captures the prior
/// value (`None` for a never-configured instance) so undo restores it exactly
/// — arming, disarming, and every drawer edit (send / band / sensitivity /
/// mode) route through this one command.
#[derive(Debug)]
pub struct SetAudioTriggerModCommand {
    target: DriverTarget,
    old: Option<AudioTriggerMod>,
    new: Option<AudioTriggerMod>,
}

impl SetAudioTriggerModCommand {
    pub fn new(
        target: DriverTarget,
        old: Option<AudioTriggerMod>,
        new: Option<AudioTriggerMod>,
    ) -> Self {
        Self { target, old, new }
    }
}

impl Command for SetAudioTriggerModCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(inst) = resolve_instance_mut(project, &self.target) {
            inst.audio_trigger = self.new.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(inst) = resolve_instance_mut(project, &self.target) {
            inst.audio_trigger = self.old.clone();
        }
    }

    fn description(&self) -> &str {
        "Set Audio Trigger"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
    use manifold_core::audio_trigger::TriggerFireMode;
    use manifold_core::effects::PresetInstance;
    use manifold_core::id::AudioSendId;
    use manifold_core::layer::Layer;
    use manifold_core::project::Project;
    use manifold_core::types::LayerType;

    fn mk_mod(send: &str, band: AudioBand, sensitivity: f32, mode: TriggerFireMode) -> AudioTriggerMod {
        AudioTriggerMod {
            enabled: true,
            source: AudioModSource {
                send_id: AudioSendId::new(send),
                feature: AudioFeature::new(AudioFeatureKind::Transients, band),
            },
            sensitivity,
            mode,
            edge: Default::default(),
        }
    }

    #[test]
    fn execute_and_undo_round_trip_on_an_effect_instance() {
        let mut project = Project::default();
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let effect_id = inst.id.clone();
        inst.audio_trigger = None;
        project.settings.master_effects.push(inst);

        let target = DriverTarget::Effect { effect_id: effect_id.clone() };
        let new_cfg = mk_mod("kick", AudioBand::Full, 0.5, TriggerFireMode::Transient);
        let mut cmd = SetAudioTriggerModCommand::new(target, None, Some(new_cfg.clone()));

        cmd.execute(&mut project);
        let fx = project.find_effect_by_id(&effect_id).unwrap();
        assert_eq!(fx.audio_trigger, Some(new_cfg));

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&effect_id).unwrap();
        assert_eq!(fx.audio_trigger, None);
    }

    #[test]
    fn execute_and_undo_round_trip_on_a_generator_instance() {
        let mut project = Project::default();
        let layer = Layer::new("Layer 1".to_string(), LayerType::Video, 0);
        let layer_id = layer.layer_id.clone();
        project.timeline.layers.push(layer);

        let target = DriverTarget::GeneratorParam { layer_id: layer_id.clone() };
        let old_cfg = mk_mod("kick", AudioBand::Low, 0.3, TriggerFireMode::ClipEdge);
        let new_cfg = mk_mod("snare", AudioBand::High, 0.8, TriggerFireMode::Both);

        // Seed an existing config, then replace it — exercising the
        // non-`None` old-value path (arm-then-reconfigure, not just
        // arm-from-scratch).
        let mut seed = SetAudioTriggerModCommand::new(target.clone(), None, Some(old_cfg.clone()));
        seed.execute(&mut project);

        let mut cmd = SetAudioTriggerModCommand::new(target, Some(old_cfg.clone()), Some(new_cfg.clone()));
        cmd.execute(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.gen_params().unwrap().audio_trigger, Some(new_cfg));

        cmd.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.gen_params().unwrap().audio_trigger, Some(old_cfg));
    }
}
