use crate::command::Command;
use manifold_core::project::Project;
use manifold_core::math::BeatQuantizer;
use manifold_core::tempo::TempoPoint;
use manifold_core::types::{BlendMode, GeneratorType, QuantizeMode, TempoPointSource};
use manifold_core::effects::ParameterDriver;

/// Change project BPM with full tempo map support.
/// Matches Unity's ChangeBpmCommand exactly:
/// - Stores old/new tempo points (cloned) for full tempo map restore on undo
/// - `flatten_tempo_map` flag clears the map to a single point
/// - `tempo_point_source` propagates to AddOrReplacePoint
/// - ApplyBpm: sets BPM, adds/replaces tempo point, optionally flattens map
/// - Undo: restores the old tempo map points completely
#[derive(Debug)]
pub struct ChangeBpmCommand {
    old_bpm: f32,
    new_bpm: f32,
    tempo_point_source: TempoPointSource,
    flatten_tempo_map: bool,
    /// Cloned snapshot of tempo map points at construction time.
    /// None when constructed with the settings-only constructor.
    old_tempo_points: Option<Vec<TempoPoint>>,
    /// Whether this command has access to the project's tempo map.
    has_project: bool,
}

impl ChangeBpmCommand {
    /// Settings-only constructor (no tempo map manipulation).
    /// Equivalent to Unity's `ChangeBpmCommand(ProjectSettings, float, float)`.
    pub fn new(old_bpm: f32, new_bpm: f32) -> Self {
        Self {
            old_bpm: BeatQuantizer::quantize_bpm(old_bpm),
            new_bpm: BeatQuantizer::quantize_bpm(new_bpm),
            tempo_point_source: TempoPointSource::Manual,
            flatten_tempo_map: false,
            old_tempo_points: None,
            has_project: false,
        }
    }

    /// Full constructor with tempo map support.
    /// Equivalent to Unity's `ChangeBpmCommand(Project, float, float, TempoPointSource, bool)`.
    /// `old_tempo_points` should be `project.tempo_map.clone_points()` captured by the caller.
    pub fn with_tempo_map(
        old_bpm: f32,
        new_bpm: f32,
        tempo_point_source: TempoPointSource,
        flatten_tempo_map: bool,
        old_tempo_points: Vec<TempoPoint>,
    ) -> Self {
        Self {
            old_bpm: BeatQuantizer::quantize_bpm(old_bpm),
            new_bpm: BeatQuantizer::quantize_bpm(new_bpm),
            tempo_point_source,
            flatten_tempo_map,
            old_tempo_points: Some(old_tempo_points),
            has_project: true,
        }
    }

    fn apply_bpm(&self, project: &mut Project, bpm: f32, is_undo: bool) {
        project.settings.bpm = BeatQuantizer::quantize_bpm(bpm);
        let applied_bpm = project.settings.bpm;

        if !self.has_project {
            return;
        }

        if !self.flatten_tempo_map {
            project.tempo_map.add_or_replace_point(
                0.0, applied_bpm, self.tempo_point_source, 0.001,
            );
            project.tempo_map.ensure_default_at_beat_zero(applied_bpm, self.tempo_point_source);
            return;
        }

        if is_undo {
            self.restore_old_tempo_map(project);
            return;
        }

        project.tempo_map.clear();
        project.tempo_map.add_or_replace_point(
            0.0, applied_bpm, self.tempo_point_source, 0.001,
        );
        project.tempo_map.ensure_default_at_beat_zero(applied_bpm, self.tempo_point_source);
    }

    fn restore_old_tempo_map(&self, project: &mut Project) {
        project.tempo_map.clear();

        match &self.old_tempo_points {
            Some(points) if !points.is_empty() => {
                for p in points {
                    project.tempo_map.add_or_replace_point_with_time(
                        p.beat, p.bpm, p.source, 0.001, p.recorded_at_seconds,
                    );
                }
                project.tempo_map.ensure_default_at_beat_zero(
                    project.settings.bpm, self.tempo_point_source,
                );
            }
            _ => {
                project.tempo_map.add_or_replace_point(
                    0.0, self.old_bpm, self.tempo_point_source, 0.001,
                );
                project.tempo_map.ensure_default_at_beat_zero(
                    self.old_bpm, self.tempo_point_source,
                );
            }
        }
    }
}

impl Command for ChangeBpmCommand {
    fn execute(&mut self, project: &mut Project) {
        self.apply_bpm(project, self.new_bpm, false);
    }

    fn undo(&mut self, project: &mut Project) {
        self.apply_bpm(project, self.old_bpm, true);
    }

    fn description(&self) -> &str { "Change BPM" }
}

/// Change output resolution.
/// Caller is responsible for applying the resolution to the runtime pipeline.
/// Matches Unity: only sets the preset, never width/height.
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
/// Matches Unity's RestoreRecordedTempoLaneCommand exactly.
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

    fn apply_lane(project: &mut Project, lane: &[TempoPoint], fallback_bpm: f32) {
        project.tempo_map.clear();

        for point in lane {
            project.tempo_map.add_or_replace_point_with_time(
                point.beat, point.bpm, point.source, 0.001, point.recorded_at_seconds,
            );
        }

        project.tempo_map.ensure_default_at_beat_zero(fallback_bpm, TempoPointSource::Manual);

        let bpm_at_zero = project.tempo_map.get_bpm_at_beat(0.0, fallback_bpm);
        project.settings.bpm = bpm_at_zero;
    }
}

impl Command for RestoreRecordedTempoLaneCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply_lane(project, &self.new_points.clone(), self.old_bpm);
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply_lane(project, &self.old_points.clone(), self.old_bpm);
    }

    fn description(&self) -> &str { "Restore Tempo Lane" }
}

/// Clear the tempo map, flattening to current BPM.
/// Matches Unity's ClearTempoMapCommand exactly.
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
        project.tempo_map.ensure_default_at_beat_zero(self.current_bpm, TempoPointSource::Manual);
    }

    fn undo(&mut self, project: &mut Project) {
        project.tempo_map.clear();

        if !self.old_points.is_empty() {
            for p in &self.old_points {
                project.tempo_map.add_or_replace_point_with_time(
                    p.beat, p.bpm, p.source, 0.001, p.recorded_at_seconds,
                );
            }
        }

        project.tempo_map.ensure_default_at_beat_zero(self.current_bpm, TempoPointSource::Manual);
    }

    fn description(&self) -> &str { "Clear Tempo Map" }
}

/// Rescale all clip beat positions when BPM changes.
/// Port of Unity PercussionImportOrchestrator.BuildRescaleBeatsForBpmChange.
///
/// For each clip: new_start_beat = max(0, old_start_beat * (new_bpm / old_bpm)).
/// Stores old/new positions for undo.
#[derive(Debug)]
pub struct RescaleBeatsForBpmChangeCommand {
    _old_bpm: f32,
    _new_bpm: f32,
    /// (layer_index, clip_index, old_start_beat, new_start_beat)
    clip_moves: Vec<(usize, usize, f32, f32)>,
}

impl RescaleBeatsForBpmChangeCommand {
    /// Build the command. Returns None if no rescaling is needed.
    pub fn build(project: &Project, old_bpm: f32, new_bpm: f32) -> Option<Self> {
        if old_bpm <= 0.0 || new_bpm <= 0.0 || (old_bpm - new_bpm).abs() < 0.01 {
            return None;
        }

        let ratio = new_bpm / old_bpm;
        let mut clip_moves = Vec::new();

        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for (ci, clip) in layer.clips.iter().enumerate() {
                let old_beat = clip.start_beat;
                let new_beat = (old_beat * ratio).max(0.0);
                if (new_beat - old_beat).abs() >= 0.0001 {
                    clip_moves.push((li, ci, old_beat, new_beat));
                }
            }
        }

        if clip_moves.is_empty() { return None; }

        Some(Self { _old_bpm: old_bpm, _new_bpm: new_bpm, clip_moves })
    }
}

impl Command for RescaleBeatsForBpmChangeCommand {
    fn execute(&mut self, project: &mut Project) {
        for &(li, ci, _, new_beat) in &self.clip_moves {
            if let Some(layer) = project.timeline.layers.get_mut(li)
                && let Some(clip) = layer.clips.get_mut(ci) {
                    clip.start_beat = new_beat;
                }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        for &(li, ci, old_beat, _) in &self.clip_moves {
            if let Some(layer) = project.timeline.layers.get_mut(li)
                && let Some(clip) = layer.clips.get_mut(ci) {
                    clip.start_beat = old_beat;
                }
        }
    }

    fn description(&self) -> &str { "Rescale beats for BPM change" }
}

/// Undoable command for changing the imported audio start beat.
/// Port of Unity SetImportedAudioCommand (audio_start_beat portion).
#[derive(Debug)]
pub struct SetAudioStartBeatCommand {
    old_start_beat: f32,
    new_start_beat: f32,
}

impl SetAudioStartBeatCommand {
    pub fn new(old_start_beat: f32, new_start_beat: f32) -> Self {
        Self { old_start_beat, new_start_beat }
    }
}

impl Command for SetAudioStartBeatCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(state) = project.percussion_import.as_mut() {
            state.audio_start_beat = self.new_start_beat;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(state) = project.percussion_import.as_mut() {
            state.audio_start_beat = self.old_start_beat;
        }
    }

    fn description(&self) -> &str { "Drag audio start beat" }
}
