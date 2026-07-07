//! Undoable commands for per-parameter audio modulations
//! ([`ParameterAudioMod`]). Parallel to the driver commands — they target the
//! same instances (an effect by [`EffectId`] or a layer's generator) via the
//! shared [`DriverTarget`], and edit the instance's `audio_mods` list.
//!
//! One audio mod per param by convention (like one driver per param), so
//! commands address a mod by its `param_id`.

use crate::command::Command;
use crate::commands::effect_target::DriverTarget;
use manifold_core::audio_mod::{AudioModShape, AudioModSource, ParameterAudioMod};
use manifold_core::audio_trigger::TriggerFireMode;
use manifold_core::effects::ParamId;
use manifold_core::project::Project;

/// Resolve a target to mutable access to its audio-mods list, creating it if
/// absent (mirrors `with_drivers_mut`).
fn with_audio_mods_mut<F, R>(project: &mut Project, target: &DriverTarget, f: F) -> Option<R>
where
    F: FnOnce(&mut Vec<ParameterAudioMod>) -> R,
{
    match target {
        DriverTarget::Effect { effect_id } => {
            let effect = project.find_effect_by_id_mut(effect_id)?;
            Some(f(effect.audio_mods_mut()))
        }
        DriverTarget::GeneratorParam { layer_id } => {
            let (_, layer) = project.timeline.find_layer_by_id_mut(layer_id)?;
            let gp = layer.gen_params_or_init();
            let mods = gp.audio_mods.get_or_insert_with(Vec::new);
            Some(f(mods))
        }
    }
}

/// Add (assign) an audio modulation to a parameter.
#[derive(Debug)]
pub struct AddAudioModCommand {
    target: DriverTarget,
    audio_mod: ParameterAudioMod,
}

impl AddAudioModCommand {
    pub fn new(target: DriverTarget, audio_mod: ParameterAudioMod) -> Self {
        Self { target, audio_mod }
    }
}

impl Command for AddAudioModCommand {
    fn execute(&mut self, project: &mut Project) {
        let m = self.audio_mod.clone();
        with_audio_mods_mut(project, &self.target, |mods| {
            // Replace any existing mod on the same param (one-per-param).
            mods.retain(|a| a.param_id != m.param_id);
            mods.push(m);
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let pid = self.audio_mod.param_id.clone();
        with_audio_mods_mut(project, &self.target, |mods| {
            mods.retain(|a| a.param_id != pid);
        });
    }

    fn description(&self) -> &str {
        "Add Audio Modulation"
    }
}

/// Remove the audio modulation on a parameter. Captures it for undo.
#[derive(Debug)]
pub struct RemoveAudioModCommand {
    target: DriverTarget,
    param_id: ParamId,
    removed: Option<ParameterAudioMod>,
}

impl RemoveAudioModCommand {
    pub fn new(target: DriverTarget, param_id: ParamId) -> Self {
        Self { target, param_id, removed: None }
    }
}

impl Command for RemoveAudioModCommand {
    fn execute(&mut self, project: &mut Project) {
        let pid = self.param_id.clone();
        let removed = with_audio_mods_mut(project, &self.target, |mods| {
            mods.iter().position(|a| a.param_id == pid).map(|i| mods.remove(i))
        });
        self.removed = removed.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(m) = self.removed.take() {
            with_audio_mods_mut(project, &self.target, |mods| mods.push(m));
        }
    }

    fn description(&self) -> &str {
        "Remove Audio Modulation"
    }
}

/// Toggle an audio modulation's enabled state.
#[derive(Debug)]
pub struct ToggleAudioModEnabledCommand {
    target: DriverTarget,
    param_id: ParamId,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleAudioModEnabledCommand {
    pub fn new(target: DriverTarget, param_id: ParamId, old_enabled: bool, new_enabled: bool) -> Self {
        Self { target, param_id, old_enabled, new_enabled }
    }
}

impl Command for ToggleAudioModEnabledCommand {
    fn execute(&mut self, project: &mut Project) {
        set_enabled(project, &self.target, &self.param_id, self.new_enabled);
    }

    fn undo(&mut self, project: &mut Project) {
        set_enabled(project, &self.target, &self.param_id, self.old_enabled);
    }

    fn description(&self) -> &str {
        "Toggle Audio Modulation"
    }
}

fn set_enabled(project: &mut Project, target: &DriverTarget, param_id: &ParamId, val: bool) {
    with_audio_mods_mut(project, target, |mods| {
        if let Some(m) = mods.iter_mut().find(|a| &a.param_id == param_id) {
            m.enabled = val;
        }
    });
}

/// Change an audio modulation's source (send + feature).
#[derive(Debug)]
pub struct SetAudioModSourceCommand {
    target: DriverTarget,
    param_id: ParamId,
    old: AudioModSource,
    new: AudioModSource,
}

impl SetAudioModSourceCommand {
    pub fn new(target: DriverTarget, param_id: ParamId, old: AudioModSource, new: AudioModSource) -> Self {
        Self { target, param_id, old, new }
    }
}

impl Command for SetAudioModSourceCommand {
    fn execute(&mut self, project: &mut Project) {
        let v = self.new.clone();
        with_audio_mods_mut(project, &self.target, |mods| {
            if let Some(m) = mods.iter_mut().find(|a| a.param_id == self.param_id) {
                m.source = v;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let v = self.old.clone();
        with_audio_mods_mut(project, &self.target, |mods| {
            if let Some(m) = mods.iter_mut().find(|a| a.param_id == self.param_id) {
                m.source = v;
            }
        });
    }

    fn description(&self) -> &str {
        "Set Audio Modulation Source"
    }
}

/// Change an audio modulation's shaping (sensitivity / attack / release /
/// range / curve / invert).
#[derive(Debug)]
pub struct SetAudioModShapeCommand {
    target: DriverTarget,
    param_id: ParamId,
    old: AudioModShape,
    new: AudioModShape,
}

impl SetAudioModShapeCommand {
    pub fn new(target: DriverTarget, param_id: ParamId, old: AudioModShape, new: AudioModShape) -> Self {
        Self { target, param_id, old, new }
    }
}

impl Command for SetAudioModShapeCommand {
    fn execute(&mut self, project: &mut Project) {
        let v = self.new;
        with_audio_mods_mut(project, &self.target, |mods| {
            if let Some(m) = mods.iter_mut().find(|a| a.param_id == self.param_id) {
                m.shape = v;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let v = self.old;
        with_audio_mods_mut(project, &self.target, |mods| {
            if let Some(m) = mods.iter_mut().find(|a| a.param_id == self.param_id) {
                m.shape = v;
            }
        });
    }

    fn description(&self) -> &str {
        "Set Audio Modulation Shape"
    }
}

/// Change a trigger-gate mod's fire mode (§9 U3 — `ClipEdge`/`Transient`/
/// `Both`, the drawer's Mode row). Mirrors [`SetAudioModSourceCommand`]'s
/// shape: whole-field old/new capture, addressed by `param_id` like every
/// other audio-mod command. Meaningless on a non-gate target (`trigger_mode`
/// stays `None` there and nothing reads it), but the command doesn't need to
/// know which — the drawer only ever emits this for an `is_trigger_gate` row.
#[derive(Debug)]
pub struct SetAudioModTriggerModeCommand {
    target: DriverTarget,
    param_id: ParamId,
    old: Option<TriggerFireMode>,
    new: Option<TriggerFireMode>,
}

impl SetAudioModTriggerModeCommand {
    pub fn new(
        target: DriverTarget,
        param_id: ParamId,
        old: Option<TriggerFireMode>,
        new: Option<TriggerFireMode>,
    ) -> Self {
        Self { target, param_id, old, new }
    }
}

impl Command for SetAudioModTriggerModeCommand {
    fn execute(&mut self, project: &mut Project) {
        let v = self.new;
        with_audio_mods_mut(project, &self.target, |mods| {
            if let Some(m) = mods.iter_mut().find(|a| a.param_id == self.param_id) {
                m.trigger_mode = v;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let v = self.old;
        with_audio_mods_mut(project, &self.target, |mods| {
            if let Some(m) = mods.iter_mut().find(|a| a.param_id == self.param_id) {
                m.trigger_mode = v;
            }
        });
    }

    fn description(&self) -> &str {
        "Set Audio Trigger Mode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind};
    use manifold_core::id::{AudioSendId, EffectId};

    fn effect_target() -> DriverTarget {
        DriverTarget::Effect { effect_id: EffectId::new("fx-1") }
    }

    fn make_mod(param: &str) -> ParameterAudioMod {
        ParameterAudioMod::new(
            param.to_string().into(),
            AudioSendId::new("send-1"),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Low),
        )
    }

    /// A project with one master effect whose id is "fx-1". Built without the
    /// registry (manifold-renderer isn't linked into this test binary) — audio
    /// mods don't need a registered def.
    fn project_with_effect() -> Project {
        let mut project = Project::default();
        let mut fx =
            manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::new("Bloom"));
        fx.id = EffectId::new("fx-1");
        project.settings.master_effects.push(fx);
        project
    }

    #[test]
    fn add_replaces_existing_on_same_param_and_undo_removes() {
        let mut project = project_with_effect();
        let mut cmd = AddAudioModCommand::new(effect_target(), make_mod("intensity"));
        cmd.execute(&mut project);
        let fx = &project.settings.master_effects[0];
        assert_eq!(fx.audio_mods.as_ref().unwrap().len(), 1);
        cmd.undo(&mut project);
        assert!(!project.settings.master_effects[0].has_audio_mods());
    }

    #[test]
    fn toggle_round_trips() {
        let mut project = project_with_effect();
        AddAudioModCommand::new(effect_target(), make_mod("intensity")).execute(&mut project);
        let mut cmd =
            ToggleAudioModEnabledCommand::new(effect_target(), "intensity".into(), true, false);
        cmd.execute(&mut project);
        assert!(!project.settings.master_effects[0].find_audio_mod("intensity").unwrap().enabled);
        cmd.undo(&mut project);
        assert!(project.settings.master_effects[0].find_audio_mod("intensity").unwrap().enabled);
    }

    #[test]
    fn set_trigger_mode_round_trips() {
        let mut project = project_with_effect();
        AddAudioModCommand::new(effect_target(), make_mod("clip_trigger")).execute(&mut project);
        assert_eq!(
            project.settings.master_effects[0].find_audio_mod("clip_trigger").unwrap().trigger_mode,
            None
        );
        let mut cmd = SetAudioModTriggerModeCommand::new(
            effect_target(),
            "clip_trigger".into(),
            None,
            Some(TriggerFireMode::Transient),
        );
        cmd.execute(&mut project);
        assert_eq!(
            project.settings.master_effects[0].find_audio_mod("clip_trigger").unwrap().trigger_mode,
            Some(TriggerFireMode::Transient)
        );
        cmd.undo(&mut project);
        assert_eq!(
            project.settings.master_effects[0].find_audio_mod("clip_trigger").unwrap().trigger_mode,
            None
        );
    }
}
