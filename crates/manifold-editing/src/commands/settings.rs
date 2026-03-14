use crate::command::Command;
use manifold_core::project::Project;
use manifold_core::math::BeatQuantizer;
use manifold_core::tempo::TempoPoint;
use manifold_core::types::{BlendMode, GeneratorType, QuantizeMode, TempoPointSource};
use manifold_core::effects::ParameterDriver;

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

/// Change quantize mode.
#[derive(Debug)]
pub struct ChangeQuantizeModeCommand {
    old_mode: QuantizeMode,
    new_mode: QuantizeMode,
}

impl ChangeQuantizeModeCommand {
    pub fn new(old_mode: QuantizeMode, new_mode: QuantizeMode) -> Self {
        Self { old_mode, new_mode }
    }
}

impl Command for ChangeQuantizeModeCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.quantize_mode = self.new_mode;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.quantize_mode = self.old_mode;
    }

    fn description(&self) -> &str { "Change Quantize Mode" }
}

/// Change frame rate.
#[derive(Debug)]
pub struct ChangeFrameRateCommand {
    old_rate: f32,
    new_rate: f32,
}

impl ChangeFrameRateCommand {
    pub fn new(old_rate: f32, new_rate: f32) -> Self {
        Self { old_rate, new_rate }
    }
}

impl Command for ChangeFrameRateCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.frame_rate = self.new_rate;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.frame_rate = self.old_rate;
    }

    fn description(&self) -> &str { "Change Frame Rate" }
}

/// Change a layer's MIDI note assignment.
#[derive(Debug)]
pub struct ChangeLayerMidiNoteCommand {
    layer_index: usize,
    old_note: i32,
    new_note: i32,
}

impl ChangeLayerMidiNoteCommand {
    pub fn new(layer_index: usize, old_note: i32, new_note: i32) -> Self {
        Self { layer_index, old_note, new_note }
    }
}

impl Command for ChangeLayerMidiNoteCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.midi_note = self.new_note;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.midi_note = self.old_note;
        }
    }

    fn description(&self) -> &str { "Change MIDI Note" }
}

/// Change a layer's blend mode.
#[derive(Debug)]
pub struct ChangeLayerBlendModeCommand {
    layer_index: usize,
    old_mode: BlendMode,
    new_mode: BlendMode,
}

impl ChangeLayerBlendModeCommand {
    pub fn new(layer_index: usize, old_mode: BlendMode, new_mode: BlendMode) -> Self {
        Self { layer_index, old_mode, new_mode }
    }
}

impl Command for ChangeLayerBlendModeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.default_blend_mode = self.new_mode;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.default_blend_mode = self.old_mode;
        }
    }

    fn description(&self) -> &str { "Change Blend Mode" }
}

/// Change a layer's opacity.
#[derive(Debug)]
pub struct ChangeLayerOpacityCommand {
    layer_index: usize,
    old_opacity: f32,
    new_opacity: f32,
}

impl ChangeLayerOpacityCommand {
    pub fn new(layer_index: usize, old_opacity: f32, new_opacity: f32) -> Self {
        Self { layer_index, old_opacity, new_opacity }
    }
}

impl Command for ChangeLayerOpacityCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.opacity = self.new_opacity;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.opacity = self.old_opacity;
        }
    }

    fn description(&self) -> &str { "Change Layer Opacity" }
}

/// Change generator param values for a layer.
#[derive(Debug)]
pub struct ChangeGeneratorParamsCommand {
    layer_index: usize,
    old_params: Vec<f32>,
    new_params: Vec<f32>,
}

impl ChangeGeneratorParamsCommand {
    pub fn new(layer_index: usize, old_params: Vec<f32>, new_params: Vec<f32>) -> Self {
        Self { layer_index, old_params, new_params }
    }

    fn apply_params(layer: &mut manifold_core::layer::Layer, params: &[f32]) {
        for (i, &val) in params.iter().enumerate() {
            layer.set_gen_param_base(i, val);
        }
    }
}

impl Command for ChangeGeneratorParamsCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            Self::apply_params(layer, &self.new_params);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            Self::apply_params(layer, &self.old_params);
        }
    }

    fn description(&self) -> &str { "Change Generator Params" }
}

/// Change generator type for a layer (snapshots and restores params/drivers/envelopes).
#[derive(Debug)]
pub struct ChangeGeneratorTypeCommand {
    layer_index: usize,
    old_type: GeneratorType,
    new_type: GeneratorType,
    old_params: Vec<f32>,
    old_drivers: Option<Vec<ParameterDriver>>,
    old_envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
}

impl ChangeGeneratorTypeCommand {
    pub fn new(
        layer_index: usize,
        old_type: GeneratorType,
        new_type: GeneratorType,
        old_params: Vec<f32>,
        old_drivers: Option<Vec<ParameterDriver>>,
        old_envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
    ) -> Self {
        Self { layer_index, old_type, new_type, old_params, old_drivers, old_envelopes }
    }
}

impl Command for ChangeGeneratorTypeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.change_generator_type(self.new_type);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.restore_generator_state(
                self.old_type,
                self.old_params.clone(),
                self.old_drivers.clone(),
                self.old_envelopes.clone(),
            );
        }
    }

    fn description(&self) -> &str { "Change Generator Type" }
}

/// Change master opacity.
#[derive(Debug)]
pub struct ChangeMasterOpacityCommand {
    old_opacity: f32,
    new_opacity: f32,
}

impl ChangeMasterOpacityCommand {
    pub fn new(old_opacity: f32, new_opacity: f32) -> Self {
        Self { old_opacity, new_opacity }
    }
}

impl Command for ChangeMasterOpacityCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.master_opacity = self.new_opacity;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.master_opacity = self.old_opacity;
    }

    fn description(&self) -> &str { "Change Master Opacity" }
}

/// Restore a recorded tempo lane.
#[derive(Debug)]
pub struct RestoreRecordedTempoLaneCommand {
    old_bpm: f32,
    old_points: Vec<TempoPoint>,
    new_points: Vec<TempoPoint>,
}

impl RestoreRecordedTempoLaneCommand {
    pub fn new(old_bpm: f32, old_points: Vec<TempoPoint>, new_points: Vec<TempoPoint>) -> Self {
        Self { old_bpm, old_points, new_points }
    }
}

impl Command for RestoreRecordedTempoLaneCommand {
    fn execute(&mut self, project: &mut Project) {
        project.tempo_map.set_points(self.new_points.clone());
        // Update BPM to first point if available
        if let Some(first) = self.new_points.first() {
            project.settings.bpm = first.bpm;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        project.tempo_map.set_points(self.old_points.clone());
        project.settings.bpm = self.old_bpm;
    }

    fn description(&self) -> &str { "Restore Tempo Lane" }
}

/// Clear the tempo map, flattening to current BPM.
#[derive(Debug)]
pub struct ClearTempoMapCommand {
    old_points: Vec<TempoPoint>,
    current_bpm: f32,
}

impl ClearTempoMapCommand {
    pub fn new(old_points: Vec<TempoPoint>, current_bpm: f32) -> Self {
        Self { old_points, current_bpm }
    }
}

impl Command for ClearTempoMapCommand {
    fn execute(&mut self, project: &mut Project) {
        project.tempo_map.clear();
        project.tempo_map.add_or_replace_point(
            0.0,
            self.current_bpm,
            TempoPointSource::Manual,
            0.001,
        );
    }

    fn undo(&mut self, project: &mut Project) {
        project.tempo_map.set_points(self.old_points.clone());
    }

    fn description(&self) -> &str { "Clear Tempo Map" }
}
