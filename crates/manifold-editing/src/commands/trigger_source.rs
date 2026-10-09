//! Undoable parameter clip-trigger source edits.

use crate::command::Command;
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
        }
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
}

impl Command for SetParamClipTriggerSourceCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
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
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(old_source) = self.old_source.clone() else {
            self.applied = false;
            return;
        };
        self.set_source(project, &old_source);
    }

    fn description(&self) -> &str {
        "Set Clip Trigger Source"
    }

    fn was_applied(&self) -> bool {
        self.applied
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
}
