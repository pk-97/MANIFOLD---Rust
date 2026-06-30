use crate::command::Command;
use manifold_core::Beats;
use manifold_core::PresetTypeId;
use manifold_core::LayerId;
use manifold_core::effects::ParameterDriver;
use manifold_core::math::BeatQuantizer;
use manifold_core::project::Project;
use manifold_core::tempo::TempoPoint;
use manifold_core::types::{BlendMode, MidiTriggerMode, QuantizeMode, TempoPointSource};
use manifold_core::units::Bpm;

/// Change project BPM with full tempo map support.
/// Matches Unity's ChangeBpmCommand exactly:
/// - Stores old/new tempo points (cloned) for full tempo map restore on undo
/// - `flatten_tempo_map` flag clears the map to a single point
/// - `tempo_point_source` propagates to AddOrReplacePoint
/// - ApplyBpm: sets BPM, adds/replaces tempo point, optionally flattens map
/// - Undo: restores the old tempo map points completely
#[derive(Debug)]
pub struct ChangeBpmCommand {
    old_bpm: Bpm,
    new_bpm: Bpm,
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
    pub fn new(old_bpm: Bpm, new_bpm: Bpm) -> Self {
        Self {
            old_bpm: Bpm(BeatQuantizer::quantize_bpm(old_bpm.0)),
            new_bpm: Bpm(BeatQuantizer::quantize_bpm(new_bpm.0)),
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
        old_bpm: Bpm,
        new_bpm: Bpm,
        tempo_point_source: TempoPointSource,
        flatten_tempo_map: bool,
        old_tempo_points: Vec<TempoPoint>,
    ) -> Self {
        Self {
            old_bpm: Bpm(BeatQuantizer::quantize_bpm(old_bpm.0)),
            new_bpm: Bpm(BeatQuantizer::quantize_bpm(new_bpm.0)),
            tempo_point_source,
            flatten_tempo_map,
            old_tempo_points: Some(old_tempo_points),
            has_project: true,
        }
    }

    fn apply_bpm(&self, project: &mut Project, bpm: Bpm, is_undo: bool) {
        project.settings.bpm = Bpm(BeatQuantizer::quantize_bpm(bpm.0));
        let applied_bpm = project.settings.bpm;

        if !self.has_project {
            return;
        }

        if !self.flatten_tempo_map {
            project.tempo_map.add_or_replace_point(
                Beats::ZERO,
                applied_bpm,
                self.tempo_point_source,
                0.001,
            );
            project
                .tempo_map
                .ensure_default_at_beat_zero(applied_bpm, self.tempo_point_source);
            return;
        }

        if is_undo {
            self.restore_old_tempo_map(project);
            return;
        }

        project.tempo_map.clear();
        project.tempo_map.add_or_replace_point(
            Beats::ZERO,
            applied_bpm,
            self.tempo_point_source,
            0.001,
        );
        project
            .tempo_map
            .ensure_default_at_beat_zero(applied_bpm, self.tempo_point_source);
    }

    fn restore_old_tempo_map(&self, project: &mut Project) {
        project.tempo_map.clear();

        match &self.old_tempo_points {
            Some(points) if !points.is_empty() => {
                for p in points {
                    project.tempo_map.add_or_replace_point_with_time(
                        p.beat,
                        p.bpm,
                        p.source,
                        0.001,
                        p.recorded_at_seconds,
                    );
                }
                project
                    .tempo_map
                    .ensure_default_at_beat_zero(project.settings.bpm, self.tempo_point_source);
            }
            _ => {
                project.tempo_map.add_or_replace_point(
                    Beats::ZERO,
                    self.old_bpm,
                    self.tempo_point_source,
                    0.001,
                );
                project
                    .tempo_map
                    .ensure_default_at_beat_zero(self.old_bpm, self.tempo_point_source);
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

    fn description(&self) -> &str {
        "Change BPM"
    }
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
    pub fn new(
        old_preset: manifold_core::ResolutionPreset,
        new_preset: manifold_core::ResolutionPreset,
    ) -> Self {
        Self {
            old_preset,
            new_preset,
        }
    }

    pub fn old_preset(&self) -> manifold_core::ResolutionPreset {
        self.old_preset
    }
    pub fn new_preset(&self) -> manifold_core::ResolutionPreset {
        self.new_preset
    }
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

    fn description(&self) -> &str {
        "Change Resolution"
    }
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

    fn description(&self) -> &str {
        "Change Quantize Mode"
    }
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

    fn description(&self) -> &str {
        "Change Frame Rate"
    }
}

/// Change a layer's MIDI note assignment.
#[derive(Debug)]
pub struct ChangeLayerMidiNoteCommand {
    layer_id: LayerId,
    old_note: i32,
    new_note: i32,
}

impl ChangeLayerMidiNoteCommand {
    pub fn new(layer_id: LayerId, old_note: i32, new_note: i32) -> Self {
        Self {
            layer_id,
            old_note,
            new_note,
        }
    }
}

impl Command for ChangeLayerMidiNoteCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.midi_note = self.new_note;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.midi_note = self.old_note;
        }
    }

    fn description(&self) -> &str {
        "Change MIDI Note"
    }
}

/// Change a layer's blend mode.
#[derive(Debug)]
pub struct ChangeLayerBlendModeCommand {
    layer_id: LayerId,
    old_mode: BlendMode,
    new_mode: BlendMode,
}

impl ChangeLayerBlendModeCommand {
    pub fn new(layer_id: LayerId, old_mode: BlendMode, new_mode: BlendMode) -> Self {
        Self {
            layer_id,
            old_mode,
            new_mode,
        }
    }
}

impl Command for ChangeLayerBlendModeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.default_blend_mode = self.new_mode;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.default_blend_mode = self.old_mode;
        }
    }

    fn description(&self) -> &str {
        "Change Blend Mode"
    }
}

/// Change a layer's opacity.
#[derive(Debug)]
pub struct ChangeLayerOpacityCommand {
    layer_id: LayerId,
    old_opacity: f32,
    new_opacity: f32,
}

impl ChangeLayerOpacityCommand {
    pub fn new(layer_id: LayerId, old_opacity: f32, new_opacity: f32) -> Self {
        Self {
            layer_id,
            old_opacity,
            new_opacity,
        }
    }
}

impl Command for ChangeLayerOpacityCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.opacity = self.new_opacity;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.opacity = self.old_opacity;
        }
    }

    fn description(&self) -> &str {
        "Change Layer Opacity"
    }
}

/// Change generator type for a layer (snapshots and restores params/drivers/envelopes).
#[derive(Debug)]
pub struct ChangeGeneratorTypeCommand {
    layer_id: LayerId,
    old_type: PresetTypeId,
    new_type: PresetTypeId,
    old_params: Vec<f32>,
    old_drivers: Option<Vec<ParameterDriver>>,
    old_envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
    /// Snapshot of `Layer::generator_graph` captured on first execute.
    /// `Layer::change_generator_type` clears the per-layer graph
    /// override (it's shape-specific to the previous type); undo
    /// reinstates the snapshot so redos that reuse this command don't
    /// lose the user's graph edits. Set on first execute, replayed on
    /// every undo.
    old_graph: Option<manifold_core::effect_graph_def::EffectGraphDef>,
    captured_old_graph: bool,
}

impl ChangeGeneratorTypeCommand {
    pub fn new(
        layer_id: LayerId,
        old_type: PresetTypeId,
        new_type: PresetTypeId,
        old_params: Vec<f32>,
        old_drivers: Option<Vec<ParameterDriver>>,
        old_envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
    ) -> Self {
        Self {
            layer_id,
            old_type,
            new_type,
            old_params,
            old_drivers,
            old_envelopes,
            old_graph: None,
            captured_old_graph: false,
        }
    }
}

impl Command for ChangeGeneratorTypeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            if !self.captured_old_graph {
                self.old_graph = layer.generator_graph().cloned();
                self.captured_old_graph = true;
            }
            layer.change_generator_type(self.new_type.clone());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.restore_generator_state(
                self.old_type.clone(),
                self.old_params.clone(),
                self.old_drivers.clone(),
                self.old_envelopes.clone(),
            );
            // The generator graph lives on `gen_params` now (graph-home
            // unification); restore the snapshot there and bump its version so
            // the renderer re-snapshots.
            let gp = layer.gen_params_or_init();
            gp.graph = self.old_graph.clone();
            gp.graph_version = gp.graph_version.wrapping_add(1);
        }
    }

    fn description(&self) -> &str {
        "Change Generator Type"
    }
}

/// Paste a generator setup onto a layer (replaces type + params + drivers + envelopes).
#[derive(Debug)]
pub struct PasteGeneratorCommand {
    layer_id: LayerId,
    old_type: PresetTypeId,
    old_params: Vec<f32>,
    old_drivers: Option<Vec<ParameterDriver>>,
    old_envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
    new_type: PresetTypeId,
    new_params: Vec<f32>,
    new_drivers: Option<Vec<ParameterDriver>>,
    new_envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
    /// Snapshot of the destination layer's `generator_graph` captured
    /// on first execute. Paste replaces the layer's generator state
    /// with the source's; any pre-paste graph override is shape-
    /// specific to the destination's previous type and would otherwise
    /// be reused by the renderer's per-frame override-version sweep,
    /// rendering the old generator with the pasted outer-card values.
    old_graph: Option<manifold_core::effect_graph_def::EffectGraphDef>,
    captured_old_graph: bool,
}

impl PasteGeneratorCommand {
    pub fn new(
        layer_id: LayerId,
        old_type: PresetTypeId,
        old_params: Vec<f32>,
        old_drivers: Option<Vec<ParameterDriver>>,
        old_envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
        new_type: PresetTypeId,
        new_params: Vec<f32>,
        new_drivers: Option<Vec<ParameterDriver>>,
        new_envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
    ) -> Self {
        Self {
            layer_id,
            old_type,
            old_params,
            old_drivers,
            old_envelopes,
            new_type,
            new_params,
            new_drivers,
            new_envelopes,
            old_graph: None,
            captured_old_graph: false,
        }
    }
}

impl Command for PasteGeneratorCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            if !self.captured_old_graph {
                self.old_graph = layer.generator_graph().cloned();
                self.captured_old_graph = true;
            }
            layer.restore_generator_state(
                self.new_type.clone(),
                self.new_params.clone(),
                self.new_drivers.clone(),
                self.new_envelopes.clone(),
            );
            // Drop any pre-paste graph override (shape-specific to the old
            // type). It lives on `gen_params` now, which `restore_generator_state`
            // does not clear, so clear it explicitly + bump.
            if let Some(gp) = layer.gen_params_mut()
                && gp.graph.take().is_some()
            {
                gp.graph_version = gp.graph_version.wrapping_add(1);
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.restore_generator_state(
                self.old_type.clone(),
                self.old_params.clone(),
                self.old_drivers.clone(),
                self.old_envelopes.clone(),
            );
            let gp = layer.gen_params_or_init();
            gp.graph = self.old_graph.clone();
            gp.graph_version = gp.graph_version.wrapping_add(1);
        }
    }

    fn description(&self) -> &str {
        "Paste Generator"
    }
}

/// Change master opacity.
#[derive(Debug)]
pub struct ChangeMasterOpacityCommand {
    old_opacity: f32,
    new_opacity: f32,
}

impl ChangeMasterOpacityCommand {
    pub fn new(old_opacity: f32, new_opacity: f32) -> Self {
        Self {
            old_opacity,
            new_opacity,
        }
    }
}

impl Command for ChangeMasterOpacityCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.master_opacity = self.new_opacity;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.master_opacity = self.old_opacity;
    }

    fn description(&self) -> &str {
        "Change Master Opacity"
    }
}

/// Change LED brightness.
#[derive(Debug)]
pub struct ChangeLedBrightnessCommand {
    old_brightness: f32,
    new_brightness: f32,
}

impl ChangeLedBrightnessCommand {
    pub fn new(old_brightness: f32, new_brightness: f32) -> Self {
        Self {
            old_brightness,
            new_brightness,
        }
    }
}

impl Command for ChangeLedBrightnessCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.led_brightness = self.new_brightness;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.led_brightness = self.old_brightness;
    }

    fn description(&self) -> &str {
        "Change LED Brightness"
    }
}

/// Toggle HDR export setting.
#[derive(Debug)]
pub struct ToggleExportHdrCommand {
    old_hdr: bool,
}

impl ToggleExportHdrCommand {
    pub fn new(old_hdr: bool) -> Self {
        Self { old_hdr }
    }
}

impl Command for ToggleExportHdrCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.export_hdr = !self.old_hdr;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.export_hdr = self.old_hdr;
    }

    fn description(&self) -> &str {
        "Toggle HDR Export"
    }
}

/// Change a layer's MIDI channel assignment.
#[derive(Debug)]
pub struct ChangeLayerMidiChannelCommand {
    layer_id: LayerId,
    old_channel: i32,
    new_channel: i32,
}

impl ChangeLayerMidiChannelCommand {
    pub fn new(layer_id: LayerId, old_channel: i32, new_channel: i32) -> Self {
        Self {
            layer_id,
            old_channel,
            new_channel,
        }
    }
}

impl Command for ChangeLayerMidiChannelCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.midi_channel = self.new_channel;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.midi_channel = self.old_channel;
        }
    }

    fn description(&self) -> &str {
        "Change MIDI Channel"
    }
}

/// Change a layer's MIDI device filter (None = any device).
#[derive(Debug)]
pub struct ChangeLayerMidiDeviceCommand {
    layer_id: LayerId,
    old_device: Option<String>,
    new_device: Option<String>,
}

impl ChangeLayerMidiDeviceCommand {
    pub fn new(layer_id: LayerId, old_device: Option<String>, new_device: Option<String>) -> Self {
        Self {
            layer_id,
            old_device,
            new_device,
        }
    }
}

impl Command for ChangeLayerMidiDeviceCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.set_midi_device(self.new_device.clone());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.set_midi_device(self.old_device.clone());
        }
    }

    fn description(&self) -> &str {
        "Change MIDI Device"
    }
}

/// Change a layer's MIDI trigger mode (single-note vs all-notes).
#[derive(Debug)]
pub struct ChangeLayerMidiTriggerModeCommand {
    layer_id: LayerId,
    old_mode: MidiTriggerMode,
    new_mode: MidiTriggerMode,
}

impl ChangeLayerMidiTriggerModeCommand {
    pub fn new(layer_id: LayerId, old_mode: MidiTriggerMode, new_mode: MidiTriggerMode) -> Self {
        Self {
            layer_id,
            old_mode,
            new_mode,
        }
    }
}

impl Command for ChangeLayerMidiTriggerModeCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.set_midi_trigger_mode(self.new_mode);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.set_midi_trigger_mode(self.old_mode);
        }
    }

    fn description(&self) -> &str {
        "Change MIDI Trigger Mode"
    }
}

/// Change display resolution (direct width/height, not preset-based).
#[derive(Debug)]
pub struct SetDisplayDimensionsCommand {
    old_width: i32,
    old_height: i32,
    new_width: i32,
    new_height: i32,
}

impl SetDisplayDimensionsCommand {
    pub fn new(old_width: i32, old_height: i32, new_width: i32, new_height: i32) -> Self {
        Self {
            old_width,
            old_height,
            new_width,
            new_height,
        }
    }
}

impl Command for SetDisplayDimensionsCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.output_width = self.new_width;
        project.settings.output_height = self.new_height;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.output_width = self.old_width;
        project.settings.output_height = self.old_height;
    }

    fn description(&self) -> &str {
        "Set Display Resolution"
    }
}

/// Change render scale (0.5, 0.75, or 1.0 for FSR upscaling).
#[derive(Debug)]
pub struct ChangeRenderScaleCommand {
    old_scale: f32,
    new_scale: f32,
}

impl ChangeRenderScaleCommand {
    pub fn new(old_scale: f32, new_scale: f32) -> Self {
        Self {
            old_scale,
            new_scale,
        }
    }
}

impl Command for ChangeRenderScaleCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.render_scale = self.new_scale;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.render_scale = self.old_scale;
    }

    fn description(&self) -> &str {
        "Change Render Scale"
    }
}

/// Change the tonemapping curve (project setting, undoable).
#[derive(Debug)]
pub struct ChangeTonemapCurveCommand {
    old_curve: manifold_core::TonemapCurve,
    new_curve: manifold_core::TonemapCurve,
}

impl ChangeTonemapCurveCommand {
    pub fn new(
        old_curve: manifold_core::TonemapCurve,
        new_curve: manifold_core::TonemapCurve,
    ) -> Self {
        Self {
            old_curve,
            new_curve,
        }
    }
}

impl Command for ChangeTonemapCurveCommand {
    fn execute(&mut self, project: &mut Project) {
        project.settings.tonemap_curve = self.new_curve;
    }

    fn undo(&mut self, project: &mut Project) {
        project.settings.tonemap_curve = self.old_curve;
    }

    fn description(&self) -> &str {
        "Change Tonemap Curve"
    }
}

/// Restore a recorded tempo lane.
/// Matches Unity's RestoreRecordedTempoLaneCommand exactly.
#[derive(Debug)]
pub struct RestoreRecordedTempoLaneCommand {
    old_bpm: Bpm,
    old_points: Vec<TempoPoint>,
    new_points: Vec<TempoPoint>,
}

impl RestoreRecordedTempoLaneCommand {
    pub fn new(old_bpm: Bpm, old_points: Vec<TempoPoint>, new_points: Vec<TempoPoint>) -> Self {
        Self {
            old_bpm,
            old_points,
            new_points,
        }
    }

    fn apply_lane(project: &mut Project, lane: &[TempoPoint], fallback_bpm: Bpm) {
        project.tempo_map.clear();

        for point in lane {
            project.tempo_map.add_or_replace_point_with_time(
                point.beat,
                point.bpm,
                point.source,
                0.001,
                point.recorded_at_seconds,
            );
        }

        project
            .tempo_map
            .ensure_default_at_beat_zero(fallback_bpm, TempoPointSource::Manual);

        let bpm_at_zero = project.tempo_map.get_bpm_at_beat(Beats::ZERO, fallback_bpm);
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

    fn description(&self) -> &str {
        "Restore Tempo Lane"
    }
}

/// Clear the tempo map, flattening to current BPM.
/// Matches Unity's ClearTempoMapCommand exactly.
#[derive(Debug)]
pub struct ClearTempoMapCommand {
    old_points: Vec<TempoPoint>,
    current_bpm: Bpm,
}

impl ClearTempoMapCommand {
    pub fn new(old_points: Vec<TempoPoint>, current_bpm: Bpm) -> Self {
        Self {
            old_points,
            current_bpm,
        }
    }
}

impl Command for ClearTempoMapCommand {
    fn execute(&mut self, project: &mut Project) {
        project.tempo_map.clear();
        project.tempo_map.add_or_replace_point(
            Beats::ZERO,
            self.current_bpm,
            TempoPointSource::Manual,
            0.001,
        );
        project
            .tempo_map
            .ensure_default_at_beat_zero(self.current_bpm, TempoPointSource::Manual);
    }

    fn undo(&mut self, project: &mut Project) {
        project.tempo_map.clear();

        if !self.old_points.is_empty() {
            for p in &self.old_points {
                project.tempo_map.add_or_replace_point_with_time(
                    p.beat,
                    p.bpm,
                    p.source,
                    0.001,
                    p.recorded_at_seconds,
                );
            }
        }

        project
            .tempo_map
            .ensure_default_at_beat_zero(self.current_bpm, TempoPointSource::Manual);
    }

    fn description(&self) -> &str {
        "Clear Tempo Map"
    }
}

/// Rescale all clip beat positions when BPM changes.
/// Port of Unity PercussionImportOrchestrator.BuildRescaleBeatsForBpmChange.
///
/// For each clip: new_start_beat = max(0, old_start_beat * (new_bpm / old_bpm)).
/// Stores old/new positions for undo.
#[derive(Debug)]
pub struct RescaleBeatsForBpmChangeCommand {
    _old_bpm: Bpm,
    _new_bpm: Bpm,
    /// (layer_index, clip_index, old_start_beat, new_start_beat)
    clip_moves: Vec<(usize, usize, Beats, Beats)>,
}

impl RescaleBeatsForBpmChangeCommand {
    /// Build the command. Returns None if no rescaling is needed.
    pub fn build(project: &Project, old_bpm: Bpm, new_bpm: Bpm) -> Option<Self> {
        if old_bpm.0 <= 0.0 || new_bpm.0 <= 0.0 || (old_bpm.0 - new_bpm.0).abs() < 0.01 {
            return None;
        }

        let ratio = new_bpm.0 / old_bpm.0;
        let mut clip_moves = Vec::new();

        for (li, layer) in project.timeline.layers.iter().enumerate() {
            for (ci, clip) in layer.clips.iter().enumerate() {
                let old_beat = clip.start_beat;
                let new_beat = Beats((old_beat.0 * ratio as f64).max(0.0));
                if (new_beat.0 - old_beat.0).abs() >= 0.0001 {
                    clip_moves.push((li, ci, old_beat, new_beat));
                }
            }
        }

        if clip_moves.is_empty() {
            return None;
        }

        Some(Self {
            _old_bpm: old_bpm,
            _new_bpm: new_bpm,
            clip_moves,
        })
    }
}

impl Command for RescaleBeatsForBpmChangeCommand {
    fn execute(&mut self, project: &mut Project) {
        for &(li, ci, _, new_beat) in &self.clip_moves {
            if let Some(layer) = project.timeline.layers.get_mut(li)
                && let Some(clip) = layer.clips.get_mut(ci)
            {
                clip.start_beat = new_beat;
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        for &(li, ci, old_beat, _) in &self.clip_moves {
            if let Some(layer) = project.timeline.layers.get_mut(li)
                && let Some(clip) = layer.clips.get_mut(ci)
            {
                clip.start_beat = old_beat;
            }
        }
    }

    fn description(&self) -> &str {
        "Rescale beats for BPM change"
    }
}

/// Change a macro slot value. Applies the macro fan-out on execute/undo.
#[derive(Debug)]
pub struct ChangeMacroCommand {
    index: usize,
    old_value: f32,
    new_value: f32,
}

impl ChangeMacroCommand {
    pub fn new(index: usize, old_value: f32, new_value: f32) -> Self {
        Self {
            index,
            old_value,
            new_value,
        }
    }
}

impl Command for ChangeMacroCommand {
    fn execute(&mut self, project: &mut Project) {
        manifold_core::macro_bank::MacroBank::apply_macro(project, self.index, self.new_value);
    }

    fn undo(&mut self, project: &mut Project) {
        manifold_core::macro_bank::MacroBank::apply_macro(project, self.index, self.old_value);
    }

    fn description(&self) -> &str {
        "Change Macro"
    }
}

/// Rename a macro slot label.
#[derive(Debug)]
pub struct RenameMacroLabelCommand {
    index: usize,
    old_label: String,
    new_label: String,
}

impl RenameMacroLabelCommand {
    pub fn new(index: usize, old_label: String, new_label: String) -> Self {
        Self {
            index,
            old_label,
            new_label,
        }
    }

    fn apply_label(project: &mut Project, index: usize, label: &str) {
        if let Some(slot) = project.settings.macro_bank.slots.get_mut(index) {
            slot.label = label.to_string();
        }
    }
}

impl Command for RenameMacroLabelCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply_label(project, self.index, &self.new_label);
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply_label(project, self.index, &self.old_label);
    }

    fn description(&self) -> &str {
        "Rename Macro Label"
    }
}
