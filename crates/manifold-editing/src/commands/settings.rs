use crate::command::Command;
use manifold_core::project::Project;
use manifold_core::math::BeatQuantizer;

/// Change project BPM.
#[derive(Debug)]
pub struct ChangeBpmCommand {
    old_bpm: f32,
    new_bpm: f32,
}

impl ChangeBpmCommand {
    pub fn new(old_bpm: f32, new_bpm: f32) -> Self {
        Self {
            old_bpm: BeatQuantizer::quantize_bpm(old_bpm),
            new_bpm: BeatQuantizer::quantize_bpm(new_bpm),
        }
    }
}

impl Command for ChangeBpmCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.bpm = self.new_bpm;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.bpm = self.old_bpm;
    }

    fn description(&self) -> &str { "Change BPM" }
}

/// Change output resolution.
#[derive(Debug)]
pub struct ChangeResolutionCommand {
    old_preset: manifold_core::ResolutionPreset,
    new_preset: manifold_core::ResolutionPreset,
}

impl ChangeResolutionCommand {
    pub fn new(old_preset: manifold_core::ResolutionPreset, new_preset: manifold_core::ResolutionPreset) -> Self {
        Self { old_preset, new_preset }
    }

    pub fn old_preset(&self) -> manifold_core::ResolutionPreset { self.old_preset }
    pub fn new_preset(&self) -> manifold_core::ResolutionPreset { self.new_preset }
}

impl Command for ChangeResolutionCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.resolution_preset = self.new_preset;
        let (w, h) = self.new_preset.dimensions();
        project.settings.output_width = w;
        project.settings.output_height = h;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.resolution_preset = self.old_preset;
        let (w, h) = self.old_preset.dimensions();
        project.settings.output_width = w;
        project.settings.output_height = h;
    }

    fn description(&self) -> &str { "Change Resolution" }
}
