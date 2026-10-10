//! Undoable parameter clip-trigger source edits.

use crate::command::Command;
use crate::commands::audio_mod::{
    AddAudioModCommand, SetAudioModTriggerModeCommand, ToggleAudioModEnabledCommand,
};
use crate::commands::effect_target::DriverTarget;
use crate::commands::envelopes::{AddEnvelopeCommand, ToggleEnvelopeEnabledCommand};
use crate::command::CompositeCommand;
use manifold_core::audio_mod::ParameterAudioMod;
use manifold_core::audio_trigger::TriggerFireMode;
use manifold_core::effects::ParamEnvelope;
use manifold_core::GraphTarget;
use manifold_core::effects::ParamId;
use manifold_core::params::ClipTriggerSource;
use manifold_core::project::Project;

/// Set one parameter's clip-trigger source on an effect, generator, or scene
/// modifier's public macro parameter. The first execution captures the existing
/// source so undo and redo remain deterministic. Missing targets and parameters
/// are inert.
#[derive(Debug)]
pub struct SetParamClipTriggerSourceCommand {
    target: GraphTarget,
    param_id: ParamId,
    new_source: ClipTriggerSource,
    old_source: Option<ClipTriggerSource>,
    applied: bool,
    validate_scope: bool,
    rejection: Option<String>,
    response: Option<CompositeCommand>,
}

impl SetParamClipTriggerSourceCommand {
    pub fn new(
        target: GraphTarget,
        param_id: impl Into<ParamId>,
        new_source: ClipTriggerSource,
    ) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            new_source,
            old_source: None,
            applied: false,
            validate_scope: false,
            rejection: None,
            response: None,
        }
    }

    /// User assignments are checked against the authoritative project when
    /// executed. `new` also supports restoring persisted missing references.
    pub fn for_assignment(
        target: GraphTarget,
        param_id: impl Into<ParamId>,
        source: ClipTriggerSource,
    ) -> Self {
        let mut command = Self::new(target, param_id, source);
        command.validate_scope = true;
        command
    }

    fn set_source(&mut self, project: &mut Project, source: &ClipTriggerSource) {
        let Some(owner) = project.graph_target_owner_mut(&self.target) else {
            self.applied = false;
            return;
        };
        let Some(param) = owner.params.get_mut(self.param_id.as_ref()) else {
            self.applied = false;
            return;
        };
        param.clip_trigger_source = source.clone();
        self.applied = true;
    }

    fn prepare_response(
        project: &Project,
        target: &GraphTarget,
        param_id: &ParamId,
    ) -> Option<CompositeCommand> {
        let owner = project.graph_target_owner(target)?;
        let param = owner.params.get(param_id.as_ref())?;
        let driver_target = DriverTarget::from(target);
        let response_target = target
            .host_target()
            .cloned()
            .unwrap_or_else(|| target.clone());
        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        if param.spec.is_trigger {
            if let Some(audio) = owner
                .audio_mods
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .find(|audio| audio.param_id == *param_id)
            {
                if audio.enabled {
                    let mode = audio.trigger_mode.unwrap_or(TriggerFireMode::Transient);
                    if !mode.wants_clip_edge() {
                        commands.push(Box::new(SetAudioModTriggerModeCommand::new(
                            driver_target.clone(),
                            param_id.clone(),
                            audio.trigger_mode,
                            Some(TriggerFireMode::Both),
                        )));
                    }
                } else {
                    commands.push(Box::new(SetAudioModTriggerModeCommand::new(
                        driver_target.clone(),
                        param_id.clone(),
                        audio.trigger_mode,
                        Some(TriggerFireMode::ClipEdge),
                    )));
                    commands.push(Box::new(ToggleAudioModEnabledCommand::new(
                        driver_target.clone(),
                        param_id.clone(),
                        false,
                        true,
                    )));
                }
            } else {
                let mut clip_only = ParameterAudioMod::new(
                    param_id.clone(),
                    manifold_core::AudioSendId::new(""),
                    manifold_core::AudioFeature::default(),
                );
                clip_only.trigger_mode = Some(TriggerFireMode::ClipEdge);
                commands.push(Box::new(AddAudioModCommand::new(driver_target, clip_only)));
            }
        } else {
            let enabled_envelope = owner
                .envelopes
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .any(|envelope| envelope.param_id == *param_id && envelope.enabled);
            let enabled_clip_audio = owner
                .audio_mods
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .any(|audio| {
                    audio.param_id == *param_id
                        && audio.enabled
                        && matches!(audio.action, manifold_core::audio_mod::TriggerAction::Step { .. }
                            | manifold_core::audio_mod::TriggerAction::Random)
                        && audio
                            .trigger_mode
                            .unwrap_or(TriggerFireMode::Transient)
                            .wants_clip_edge()
                });
            if !enabled_envelope && !enabled_clip_audio {
                if let Some((index, envelope)) = owner
                    .envelopes
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                    .find(|(_, envelope)| envelope.param_id == *param_id)
                {
                    commands.push(Box::new(ToggleEnvelopeEnabledCommand::new(
                        response_target.clone(),
                        index,
                        envelope.enabled,
                        true,
                    )));
                } else {
                    commands.push(Box::new(AddEnvelopeCommand::new(
                        response_target.clone(),
                        ParamEnvelope::new(param_id.clone()),
                    )));
                }
            }
        }

        (!commands.is_empty()).then(|| {
            CompositeCommand::new(commands, "Arm Clip Trigger Response".into())
        })
    }
}

impl Command for SetParamClipTriggerSourceCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        if self.validate_scope && !project.can_assign_clip_trigger_source(
            &self.target, self.param_id.as_ref(), &self.new_source,
        ) {
            self.rejection = Some("The trigger source is unavailable for this parameter".into());
            return;
        }
        if self.validate_scope
            && !matches!(&self.new_source, ClipTriggerSource::Disabled)
            && self.response.is_none()
        {
            if project
                .graph_target_owner(&self.target)
                .and_then(|owner| owner.params.get(self.param_id.as_ref()))
                .is_none()
            {
                return;
            }
            self.response = Self::prepare_response(project, &self.target, &self.param_id);
        }
        {
            let Some(owner) = project.graph_target_owner_mut(&self.target) else {
                return;
            };
            let Some(param) = owner.params.get_mut(self.param_id.as_ref()) else {
                return;
            };
            if self.old_source.is_none() {
                self.old_source = Some(param.clip_trigger_source.clone());
            }
            param.clip_trigger_source = self.new_source.clone();
        }
        self.applied = true;
        if let Some(response) = self.response.as_mut() {
            response.execute(project);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(old_source) = self.old_source.clone() else {
            self.applied = false;
            return;
        };
        if let Some(response) = self.response.as_mut() {
            response.undo(project);
        }
        self.set_source(project, &old_source);
    }

    fn description(&self) -> &str {
        "Set Clip Trigger Source"
    }

    fn was_applied(&self) -> bool {
        self.applied
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::audio_mod::ParameterAudioMod;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::{ParamEnvelope, PresetInstance};
    use manifold_core::id::{AudioSendId, EffectId, LayerId};
    use manifold_core::layer::Layer;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::{AudioBand, AudioFeature, AudioFeatureKind};

    fn slot(id: &str, value: f32) -> manifold_core::params::Param {
        Param::bundled(ParamSpecDef {
            id: id.into(),
            name: id.into(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            ..ParamSpecDef::default()
        })
    }

    fn effect_fixture() -> (Project, GraphTarget, ParamId) {
        let mut project = Project::default();
        let mut instance = PresetInstance::new(PresetTypeId::new("trigger-source-effect"));
        instance.id = EffectId::new("effect");
        instance.params =
            ParamManifest::from_params(vec![slot("trigger", 0.2), slot("other", 0.8)]);
        instance.envelopes = Some(vec![ParamEnvelope::new("other")]);
        instance.audio_mods = Some(vec![ParameterAudioMod::new(
            "other".into(),
            AudioSendId::new("send"),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
        )]);
        project.settings.master_effects.push(instance);
        (
            project,
            GraphTarget::Effect(EffectId::new("effect")),
            ParamId::Owned("trigger".into()),
        )
    }

    fn source(project: &Project, target: &GraphTarget, param: &str) -> ClipTriggerSource {
        project
            .graph_target_owner(target)
            .unwrap()
            .params
            .get(param)
            .unwrap()
            .clip_trigger_source
            .clone()
    }

    fn assignment_fixture() -> (Project, GraphTarget, LayerId) {
        let mut project = Project::default();
        let mut owner = Layer::new_video("Owner".into(), 0);
        let owner_id = owner.layer_id.clone();
        let mut instance = PresetInstance::new(PresetTypeId::new("trigger-assignment"));
        instance.id = EffectId::new("assignment-effect");
        let mut fire = slot("fire", 0.0);
        fire.spec.is_trigger = true;
        instance.params = ParamManifest::from_params(vec![
            slot("amount", 0.2),
            fire,
        ]);
        owner.effects = Some(vec![instance]);
        let target = GraphTarget::Effect(EffectId::new("assignment-effect"));
        let lane = Layer::new_trigger("Lane".into(), owner_id, 1);
        let lane_id = lane.layer_id.clone();
        project.timeline.layers.extend([owner, lane]);
        (project, target, lane_id)
    }

    #[test]
    fn trigger_source_effect_execute_undo_redo_preserves_other_parameter_and_modulation_state() {
        let (mut project, target, param_id) = effect_fixture();
        let before = project.preset_instance(&target).unwrap().clone();
        let mut command = SetParamClipTriggerSourceCommand::new(
            target.clone(),
            param_id,
            ClipTriggerSource::Disabled,
        );
        command.execute(&mut project);
        assert_eq!(
            source(&project, &target, "trigger"),
            ClipTriggerSource::Disabled
        );
        let after = project.preset_instance(&target).unwrap();
        assert_eq!(after.params.get("other"), before.params.get("other"));
        assert_eq!(after.audio_mods, before.audio_mods);
        assert_eq!(
            serde_json::to_value(&after.envelopes).unwrap(),
            serde_json::to_value(&before.envelopes).unwrap()
        );
        command.undo(&mut project);
        assert_eq!(
            serde_json::to_value(project.preset_instance(&target)).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        command.execute(&mut project);
        assert_eq!(
            source(&project, &target, "trigger"),
            ClipTriggerSource::Disabled
        );
    }

    #[test]
    fn trigger_source_generator_setter_distinguishes_disabled_from_own_layer() {
        let mut project = Project::default();
        let mut layer = Layer::new_generator("Generator".into(), PresetTypeId::new("test"), 0);
        let target = GraphTarget::Generator(layer.layer_id.clone());
        layer.gen_params_or_init().params = ParamManifest::from_params(vec![slot("trigger", 0.0)]);
        project.timeline.layers.push(layer);
        let mut command = SetParamClipTriggerSourceCommand::new(
            target.clone(),
            "trigger",
            ClipTriggerSource::Disabled,
        );
        command.execute(&mut project);
        assert_eq!(
            source(&project, &target, "trigger"),
            ClipTriggerSource::Disabled
        );
        command.undo(&mut project);
        assert_eq!(
            source(&project, &target, "trigger"),
            ClipTriggerSource::OwnLayer
        );
        command.execute(&mut project);
        assert_eq!(
            source(&project, &target, "trigger"),
            ClipTriggerSource::Disabled
        );
    }

    #[test]
    fn trigger_source_scene_modifier_uses_public_owner_parameter() {
        let mut project = Project::default();
        let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::new("test"), 0);
        let owner = GraphTarget::Generator(layer.layer_id.clone());
        layer.gen_params_or_init().params =
            ParamManifest::from_params(vec![slot("force-a.fire", 0.0), slot("force-b.fire", 0.0)]);
        project.timeline.layers.push(layer);
        let target = GraphTarget::SceneModifier {
            owner: Box::new(owner.clone()),
            modifier_id: manifold_core::NodeId::new("force-a"),
        };
        let selected = ClipTriggerSource::Lane {
            layer_id: LayerId::new("pattern"),
        };
        let mut command =
            SetParamClipTriggerSourceCommand::new(target, "force-a.fire", selected.clone());
        command.execute(&mut project);
        let params = &project.preset_instance(&owner).unwrap().params;
        assert_eq!(
            params.get("force-a.fire").unwrap().clip_trigger_source,
            selected
        );
        assert_eq!(
            params.get("force-b.fire").unwrap().clip_trigger_source,
            ClipTriggerSource::OwnLayer
        );
        command.undo(&mut project);
        assert_eq!(
            source(&project, &owner, "force-a.fire"),
            ClipTriggerSource::OwnLayer
        );
        command.execute(&mut project);
        assert_eq!(source(&project, &owner, "force-a.fire"), selected);
    }

    #[test]
    fn trigger_source_missing_target_and_parameter_are_inert() {
        let (mut project, _, param_id) = effect_fixture();
        let before = serde_json::to_value(&project).unwrap();
        let mut missing_target = SetParamClipTriggerSourceCommand::new(
            GraphTarget::Effect(EffectId::new("missing")),
            param_id.clone(),
            ClipTriggerSource::Disabled,
        );
        missing_target.execute(&mut project);
        assert!(!missing_target.was_applied());
        assert_eq!(serde_json::to_value(&project).unwrap(), before);

        let mut missing_param = SetParamClipTriggerSourceCommand::new(
            GraphTarget::Effect(EffectId::new("effect")),
            ParamId::Owned("missing".into()),
            ClipTriggerSource::Disabled,
        );
        missing_param.execute(&mut project);
        assert!(!missing_param.was_applied());
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
    }

    #[test]
    fn validated_numeric_assignment_creates_and_round_trips_envelope_response() {
        let (mut project, target, lane_id) = assignment_fixture();
        let expected_source = lane_id.clone();
        let mut command = SetParamClipTriggerSourceCommand::for_assignment(
            target.clone(),
            "amount",
            ClipTriggerSource::Lane { layer_id: lane_id },
        );
        command.execute(&mut project);
        let owner = project.graph_target_owner(&target).unwrap();
        assert_eq!(owner.params.get("amount").unwrap().clip_trigger_source,
            ClipTriggerSource::Lane { layer_id: expected_source });
        assert_eq!(owner.envelopes.as_ref().unwrap().len(), 1);
        assert!(owner.envelopes.as_ref().unwrap()[0].enabled);
        command.undo(&mut project);
        let owner = project.graph_target_owner(&target).unwrap();
        assert_eq!(owner.params.get("amount").unwrap().clip_trigger_source, ClipTriggerSource::OwnLayer);
        assert!(owner.envelopes.as_ref().is_none_or(Vec::is_empty));
        command.execute(&mut project);
        assert_eq!(project.graph_target_owner(&target).unwrap().envelopes.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn validated_fire_assignment_arms_clip_edge_without_losing_audio_binding() {
        let (mut project, target, lane_id) = assignment_fixture();
        {
            let owner = project.graph_target_owner_mut(&target).unwrap();
            let audio = ParameterAudioMod::new(
                "fire".into(),
                AudioSendId::new("send"),
                AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
            );
            owner.audio_mods_mut().push(audio);
        }
        let mut command = SetParamClipTriggerSourceCommand::for_assignment(
            target.clone(),
            "fire",
            ClipTriggerSource::Lane { layer_id: lane_id },
        );
        command.execute(&mut project);
        let audio = project.graph_target_owner(&target).unwrap().find_audio_mod("fire").unwrap();
        assert_eq!(audio.trigger_mode, Some(TriggerFireMode::Both));
        assert_eq!(audio.source.send_id, AudioSendId::new("send"));
        command.undo(&mut project);
        assert_eq!(project.graph_target_owner(&target).unwrap().find_audio_mod("fire").unwrap().trigger_mode, None);
        command.execute(&mut project);
        assert_eq!(project.graph_target_owner(&target).unwrap().find_audio_mod("fire").unwrap().trigger_mode, Some(TriggerFireMode::Both));
    }

    #[test]
    fn disabled_fire_audio_arms_clip_edge_and_disabled_source_leaves_it_alone() {
        let (mut project, target, lane_id) = assignment_fixture();
        {
            let owner = project.graph_target_owner_mut(&target).unwrap();
            let mut audio = ParameterAudioMod::new(
                "fire".into(),
                AudioSendId::new("send"),
                AudioFeature::default(),
            );
            audio.enabled = false;
            audio.trigger_mode = Some(TriggerFireMode::Both);
            owner.audio_mods_mut().push(audio);
        }
        let mut arm = SetParamClipTriggerSourceCommand::for_assignment(
            target.clone(),
            "fire",
            ClipTriggerSource::Lane { layer_id: lane_id },
        );
        arm.execute(&mut project);
        let audio = project.graph_target_owner(&target).unwrap().find_audio_mod("fire").unwrap();
        assert!(audio.enabled);
        assert_eq!(audio.trigger_mode, Some(TriggerFireMode::ClipEdge));
        arm.undo(&mut project);
        let mut disconnect = SetParamClipTriggerSourceCommand::for_assignment(
            target.clone(),
            "fire",
            ClipTriggerSource::Disabled,
        );
        disconnect.execute(&mut project);
        let audio = project.graph_target_owner(&target).unwrap().find_audio_mod("fire").unwrap();
        assert!(!audio.enabled);
        assert_eq!(audio.trigger_mode, Some(TriggerFireMode::Both));
    }
}
