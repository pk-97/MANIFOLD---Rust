// Port of Unity PercussionPipelineSettings.cs (711 lines) + PercussionImportOptionsFactory.cs (127 lines).
// All tunable parameters for the percussion analysis pipeline.

use serde::{Deserialize, Serialize};

use crate::percussion_analysis::{
    PercussionClipBinding, PercussionImportOptions, PercussionTriggerType,
};
use crate::project::Project;
use crate::settings::ProjectSettings;
use crate::types::QuantizeMode;
use crate::generator_type_id::GeneratorTypeId;

// ─── StemMode ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StemMode {
    #[default]
    Auto,
    On,
    Off,
}

// ─── Nested settings sections ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    pub min_bpm: f32,
    pub max_bpm: f32,
    pub onset_compensation_seconds: f32,
    pub quantize_to_grid: bool,
    pub bpm_auto_apply_confidence: f32,
    pub default_clip_duration_beats: f32,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            min_bpm: 55.0,
            max_bpm: 215.0,
            onset_compensation_seconds: 0.010,
            quantize_to_grid: true,
            bpm_auto_apply_confidence: 0.72,
            default_clip_duration_beats: 0.75,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemucsSettings {
    pub model: String,
    pub shifts: i32,
    pub overlap: f32,
    pub jobs: i32,
    pub no_split: bool,
    pub drum_stem_mode: StemMode,
    pub emit_bass: bool,
    pub bass_stem_mode: StemMode,
    pub vocal_stem_mode: StemMode,
}

impl Default for DemucsSettings {
    fn default() -> Self {
        Self {
            model: "htdemucs".to_string(),
            shifts: 1,
            overlap: 0.25,
            jobs: 0,
            no_split: false,
            drum_stem_mode: StemMode::On,
            emit_bass: true,
            bass_stem_mode: StemMode::Auto,
            vocal_stem_mode: StemMode::On,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KickSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub clip_duration_beats: f32,
    pub min_confidence: f32,
    pub band_hz: [f32; 2],
    pub hat_suppression_window: f32,
}

impl Default for KickSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::WIREFRAME_ZOO,
            layer_index: 0,
            clip_duration_beats: 0.5,
            min_confidence: 0.0,
            band_hz: [28.0, 180.0],
            hat_suppression_window: 0.020,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnareSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub clip_duration_beats: f32,
    pub min_confidence: f32,
    pub band_hz: [f32; 2],
    pub hat_suppression_window: f32,
    pub perc_conflict_window: f32,
    pub snare_dominance_ratio: f32,
    pub perc_dominance_ratio: f32,
}

impl Default for SnareSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::BASIC_SHAPES_SNAP,
            layer_index: 1,
            clip_duration_beats: 0.75,
            min_confidence: 0.0,
            band_hz: [180.0, 2800.0],
            hat_suppression_window: 0.010,
            perc_conflict_window: 0.05,
            snare_dominance_ratio: 1.15,
            perc_dominance_ratio: 1.05,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PercSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub clip_duration_beats: f32,
    pub min_confidence: f32,
    pub band_hz: [f32; 2],
    pub weight_in_mix: f32,
}

impl Default for PercSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::FLOWFIELD,
            layer_index: 3,
            clip_duration_beats: 0.50,
            min_confidence: 0.0,
            band_hz: [2800.0, 9000.0],
            weight_in_mix: 0.50,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HatSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub clip_duration_beats: f32,
    pub min_confidence: f32,
    pub band_hz: [f32; 2],
    pub weight_in_mix: f32,
}

impl Default for HatSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::OSCILLOSCOPE_XY,
            layer_index: 4,
            clip_duration_beats: 0.50,
            min_confidence: 0.0,
            band_hz: [5000.0, 16000.0],
            weight_in_mix: 0.45,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BassSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub min_confidence: f32,
    pub duration_threshold_sec: f32,
}

impl Default for BassSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::PARAMETRIC_SURFACE,
            layer_index: 8,
            min_confidence: 0.0,
            duration_threshold_sec: 1.7144,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BassSustainedSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub min_confidence: f32,
}

impl Default for BassSustainedSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::PARAMETRIC_SURFACE,
            layer_index: 9,
            min_confidence: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub min_confidence: f32,
}

impl Default for SynthSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::PLASMA,
            layer_index: 6,
            min_confidence: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VocalSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub clip_duration_beats: f32,
    pub min_confidence: f32,
    pub chest_band_hz: [f32; 2],
    pub formant_band_hz: [f32; 2],
    pub presence_band_hz: [f32; 2],
    pub chest_weight: f32,
    pub formant_weight: f32,
    pub presence_weight: f32,
}

impl Default for VocalSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::LISSAJOUS,
            layer_index: 5,
            clip_duration_beats: 0.50,
            min_confidence: 0.0,
            chest_band_hz: [80.0, 500.0],
            formant_band_hz: [500.0, 3000.0],
            presence_band_hz: [3000.0, 8000.0],
            chest_weight: 0.40,
            formant_weight: 0.85,
            presence_weight: 1.00,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PadSettings {
    pub generator: GeneratorTypeId,
    pub layer_index: i32,
    pub min_confidence: f32,
}

impl Default for PadSettings {
    fn default() -> Self {
        Self {
            generator: GeneratorTypeId::DUOCYLINDER,
            layer_index: 7,
            min_confidence: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlgorithmSettings {
    pub madmom_threshold: f32,
    pub madmom_combine: f32,
    pub madmom_pre_max: f32,
    pub madmom_post_max: f32,
    pub adaptive_window_sec: f32,
    pub local_norm_window: f32,
    pub autocorr_search_half_range: i32,
    pub autocorr_margin_threshold: f32,
    pub octave_kick_weight: f32,
    pub octave_snare_weight: f32,
    pub octave_onset_weight: f32,
    pub octave_prior_weight: f32,
    pub octave_kick_weight_no_prior: f32,
    pub octave_snare_weight_no_prior: f32,
    pub octave_onset_weight_no_prior: f32,
    pub octave_tolerance: f32,
    pub octave_tie_break_margin: f32,
    pub grid_stability_weight: f32,
    pub grid_onset_align_weight: f32,
    pub grid_event_density_weight: f32,
    pub grid_kick_bonus_weight: f32,
    pub grid_edge_coverage_weight: f32,
    pub grid_rel_var_scale: f32,
    pub synthetic_grid_penalty: f32,
    pub downbeat_tolerance: f32,
    pub downbeat_min_agreement: f32,
    pub non_downbeat_weight: f32,
    pub adtof_kick_threshold: f32,
    pub adtof_snare_threshold: f32,
    pub adtof_hihat_threshold: f32,
    pub adtof_tom_threshold: f32,
    pub adtof_cymbal_threshold: f32,
    pub bp_bass_onset_threshold: f32,
    pub bp_bass_frame_threshold: f32,
    pub bp_bass_min_note_length: f32,
    pub bp_bass_min_frequency: f32,
    pub bp_bass_max_frequency: f32,
    pub bp_bass_min_energy_db: f32,
    pub bp_synth_onset_threshold: f32,
    pub bp_synth_frame_threshold: f32,
    pub bp_synth_min_note_length: f32,
    pub bp_synth_min_frequency: f32,
    pub bp_synth_max_frequency: f32,
    pub bp_synth_min_energy_db: f32,
}

impl Default for AlgorithmSettings {
    fn default() -> Self {
        Self {
            madmom_threshold: 0.5,
            madmom_combine: 0.03,
            madmom_pre_max: 0.03,
            madmom_post_max: 0.03,
            adaptive_window_sec: 2.0,
            local_norm_window: 3.0,
            autocorr_search_half_range: 4,
            autocorr_margin_threshold: 0.01,
            octave_kick_weight: 0.35,
            octave_snare_weight: 0.25,
            octave_onset_weight: 0.20,
            octave_prior_weight: 0.20,
            octave_kick_weight_no_prior: 0.43,
            octave_snare_weight_no_prior: 0.31,
            octave_onset_weight_no_prior: 0.26,
            octave_tolerance: 0.15,
            octave_tie_break_margin: 0.05,
            grid_stability_weight: 0.36,
            grid_onset_align_weight: 0.25,
            grid_event_density_weight: 0.14,
            grid_kick_bonus_weight: 0.10,
            grid_edge_coverage_weight: 0.15,
            grid_rel_var_scale: 5.5,
            synthetic_grid_penalty: 0.72,
            downbeat_tolerance: 0.120,
            downbeat_min_agreement: 0.40,
            non_downbeat_weight: -0.18,
            adtof_kick_threshold: 0.12,
            adtof_snare_threshold: 0.14,
            adtof_hihat_threshold: 0.18,
            adtof_tom_threshold: 0.14,
            adtof_cymbal_threshold: 0.18,
            bp_bass_onset_threshold: 0.5,
            bp_bass_frame_threshold: 0.3,
            bp_bass_min_note_length: 127.7,
            bp_bass_min_frequency: 0.0,
            bp_bass_max_frequency: 0.0,
            bp_bass_min_energy_db: -40.0,
            bp_synth_onset_threshold: 0.5,
            bp_synth_frame_threshold: 0.3,
            bp_synth_min_note_length: 127.7,
            bp_synth_min_frequency: 150.0,
            bp_synth_max_frequency: 8000.0,
            bp_synth_min_energy_db: -40.0,
        }
    }
}

// ─── PercussionPipelineSettings ───

/// Port of Unity PercussionPipelineSettings ScriptableObject.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PercussionPipelineSettings {
    pub global: GlobalSettings,
    pub demucs: DemucsSettings,
    pub kick: KickSettings,
    pub snare: SnareSettings,
    pub perc: PercSettings,
    pub hat: HatSettings,
    pub bass: BassSettings,
    pub bass_sustained: BassSustainedSettings,
    pub synth: SynthSettings,
    pub vocal: VocalSettings,
    pub pad: PadSettings,
    pub algorithm: AlgorithmSettings,
}

impl PercussionPipelineSettings {
    /// Port of Unity PercussionPipelineSettings.ResetToDefaults().
    pub fn reset_to_defaults(&mut self) {
        *self = Self::default();
    }

    /// Port of Unity PercussionPipelineSettings.BuildImportOptions().
    pub fn build_import_options(
        &self,
        project: &Project,
        start_beat_offset: f32,
    ) -> PercussionImportOptions {
        let mut options = PercussionImportOptions {
            start_beat_offset: start_beat_offset.max(0.0),
            quantize_to_grid: self.global.quantize_to_grid,
            quantize_step_beats: resolve_default_quantize_step(&project.settings),
            default_clip_duration_beats: self.global.default_clip_duration_beats,
            onset_compensation_seconds: self.global.onset_compensation_seconds,
            minimum_energy_gate: 0.0,
            bindings: Vec::with_capacity(9),
        };

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Kick,
            self.kick.layer_index,
            None,
            self.kick.generator.clone(),
            self.kick.clip_duration_beats,
            self.kick.min_confidence,
        ));

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Snare,
            self.snare.layer_index,
            None,
            self.snare.generator.clone(),
            self.snare.clip_duration_beats,
            self.snare.min_confidence,
        ));

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Perc,
            self.perc.layer_index,
            None,
            self.perc.generator.clone(),
            self.perc.clip_duration_beats,
            self.perc.min_confidence,
        ));

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Hat,
            self.hat.layer_index,
            None,
            self.hat.generator.clone(),
            self.hat.clip_duration_beats,
            self.hat.min_confidence,
        ));

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Vocal,
            self.vocal.layer_index,
            None,
            self.vocal.generator.clone(),
            self.vocal.clip_duration_beats,
            self.vocal.min_confidence,
        ));

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Synth,
            self.synth.layer_index,
            None,
            self.synth.generator.clone(),
            0.0,
            self.synth.min_confidence,
        ));

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Pad,
            self.pad.layer_index,
            None,
            self.pad.generator.clone(),
            0.0,
            self.pad.min_confidence,
        ));

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Bass,
            self.bass.layer_index,
            None,
            self.bass.generator.clone(),
            0.0,
            self.bass.min_confidence,
        ));

        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::BassSustained,
            self.bass_sustained.layer_index,
            None,
            self.bass_sustained.generator.clone(),
            0.0,
            self.bass_sustained.min_confidence,
        ));

        options
    }

    /// Port of Unity PercussionPipelineSettings.SerializeToDetectionConfigJson().
    /// Builds a JSON string matching the Python detection config schema.
    pub fn serialize_to_detection_config_json(&self) -> String {
        let a = &self.algorithm;
        format!(
            r#"{{
  "drum": {{
    "kick": {{
      "bandHz": [{}, {}]
    }},
    "snare": {{
      "bandHz": [{}, {}],
      "hatSuppression": {},
      "percConflict": {{
        "window": {},
        "snareDominance": {},
        "percDominance": {}
      }}
    }},
    "hat": {{
      "bandHz": [{}, {}],
      "weight": {}
    }},
    "perc": {{
      "bandHz": [{}, {}],
      "weight": {}
    }},
    "kickHatExclusion": {}
  }},
  "bass": {{
    "durationThresholdSec": {}
  }},
  "vocal": {{
    "bands": {{
      "chest": [{}, {}],
      "formant": [{}, {}],
      "presence": [{}, {}]
    }},
    "weights": {{
      "chest": {},
      "formant": {},
      "presence": {}
    }}
  }},
  "algorithm": {{
    "madmomThreshold": {},
    "madmomCombine": {},
    "madmomPreMax": {},
    "madmomPostMax": {},
    "adaptiveWindowSec": {},
    "localNormWindow": {},
    "autocorrSearchHalfRange": {},
    "autocorrMarginThreshold": {},
    "octaveKickWeight": {},
    "octaveSnareWeight": {},
    "octaveOnsetWeight": {},
    "octavePriorWeight": {},
    "octaveKickWeightNoPrior": {},
    "octaveSnareWeightNoPrior": {},
    "octaveOnsetWeightNoPrior": {},
    "octaveTolerance": {},
    "octaveTieBreakMargin": {},
    "gridStabilityWeight": {},
    "gridOnsetAlignWeight": {},
    "gridEventDensityWeight": {},
    "gridKickBonusWeight": {},
    "gridEdgeCoverageWeight": {},
    "gridRelVarScale": {},
    "syntheticGridPenalty": {},
    "downbeatTolerance": {},
    "downbeatMinAgreement": {},
    "nonDownbeatWeight": {},
    "adtofKickThreshold": {},
    "adtofSnareThreshold": {},
    "adtofHihatThreshold": {},
    "adtofTomThreshold": {},
    "adtofCymbalThreshold": {},
    "bpBassOnsetThreshold": {},
    "bpBassFrameThreshold": {},
    "bpBassMinNoteLength": {},
    "bpBassMinFrequency": {},
    "bpBassMaxFrequency": {},
    "bpBassMinEnergyDb": {},
    "bpSynthOnsetThreshold": {},
    "bpSynthFrameThreshold": {},
    "bpSynthMinNoteLength": {},
    "bpSynthMinFrequency": {},
    "bpSynthMaxFrequency": {},
    "bpSynthMinEnergyDb": {}
  }}
}}"#,
            self.kick.band_hz[0], self.kick.band_hz[1],
            self.snare.band_hz[0], self.snare.band_hz[1],
            self.snare.hat_suppression_window,
            self.snare.perc_conflict_window,
            self.snare.snare_dominance_ratio,
            self.snare.perc_dominance_ratio,
            self.hat.band_hz[0], self.hat.band_hz[1],
            self.hat.weight_in_mix,
            self.perc.band_hz[0], self.perc.band_hz[1],
            self.perc.weight_in_mix,
            self.kick.hat_suppression_window,
            self.bass.duration_threshold_sec,
            self.vocal.chest_band_hz[0], self.vocal.chest_band_hz[1],
            self.vocal.formant_band_hz[0], self.vocal.formant_band_hz[1],
            self.vocal.presence_band_hz[0], self.vocal.presence_band_hz[1],
            self.vocal.chest_weight,
            self.vocal.formant_weight,
            self.vocal.presence_weight,
            a.madmom_threshold, a.madmom_combine, a.madmom_pre_max, a.madmom_post_max,
            a.adaptive_window_sec, a.local_norm_window,
            a.autocorr_search_half_range, a.autocorr_margin_threshold,
            a.octave_kick_weight, a.octave_snare_weight,
            a.octave_onset_weight, a.octave_prior_weight,
            a.octave_kick_weight_no_prior, a.octave_snare_weight_no_prior,
            a.octave_onset_weight_no_prior,
            a.octave_tolerance, a.octave_tie_break_margin,
            a.grid_stability_weight, a.grid_onset_align_weight,
            a.grid_event_density_weight, a.grid_kick_bonus_weight,
            a.grid_edge_coverage_weight, a.grid_rel_var_scale,
            a.synthetic_grid_penalty,
            a.downbeat_tolerance, a.downbeat_min_agreement, a.non_downbeat_weight,
            a.adtof_kick_threshold, a.adtof_snare_threshold,
            a.adtof_hihat_threshold, a.adtof_tom_threshold, a.adtof_cymbal_threshold,
            a.bp_bass_onset_threshold, a.bp_bass_frame_threshold,
            a.bp_bass_min_note_length, a.bp_bass_min_frequency,
            a.bp_bass_max_frequency, a.bp_bass_min_energy_db,
            a.bp_synth_onset_threshold, a.bp_synth_frame_threshold,
            a.bp_synth_min_note_length, a.bp_synth_min_frequency,
            a.bp_synth_max_frequency, a.bp_synth_min_energy_db,
        )
    }
}

// ─── PercussionImportOptionsFactory ───

/// Port of Unity PercussionImportOptionsFactory static class.
pub struct PercussionImportOptionsFactory;

impl PercussionImportOptionsFactory {
    /// Port of Unity PercussionImportOptionsFactory.CreateDefault(Project, float).
    pub fn create_default(project: &Project, start_beat_offset: f32) -> PercussionImportOptions {
        let mut options = PercussionImportOptions {
            start_beat_offset: start_beat_offset.max(0.0),
            quantize_to_grid: true,
            quantize_step_beats: resolve_default_quantize_step(&project.settings),
            default_clip_duration_beats: 0.75,
            onset_compensation_seconds: 0.010,
            minimum_energy_gate: 0.0,
            bindings: Vec::with_capacity(8),
        };

        // Kick — punchy geometric wireframes, top of stack.
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Kick, 0, None, GeneratorTypeId::WIREFRAME_ZOO, 0.5, 0.0,
        ));
        // Snare — snapping shapes.
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Snare, 1, None, GeneratorTypeId::BASIC_SHAPES_SNAP, 0.75, 0.0,
        ));
        // Perc — groove accents.
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Perc, 3, None, GeneratorTypeId::FLOWFIELD, 0.50, 0.0,
        ));
        // Hat — high-frequency shimmer.
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Hat, 4, None, GeneratorTypeId::OSCILLOSCOPE_XY, 0.50, 0.0,
        ));
        // Vocal — organic Lissajous curves.
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Vocal, 5, None, GeneratorTypeId::LISSAJOUS, 0.50, 0.0,
        ));
        // Synth — bright plasma (duration from Basic Pitch).
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Synth, 6, None, GeneratorTypeId::PLASMA, 0.0, 0.0,
        ));
        // Pad — slow ambient Duocylinder (duration from Basic Pitch).
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Pad, 7, None, GeneratorTypeId::DUOCYLINDER, 0.0, 0.0,
        ));
        // Bass — heavy parametric surfaces (duration from Basic Pitch).
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Bass, 8, None, GeneratorTypeId::PARAMETRIC_SURFACE, 0.0, 0.0,
        ));

        options
    }

    /// Port of Unity PercussionImportOptionsFactory.CreateDefault(Project, Settings, float).
    pub fn create_default_with_settings(
        project: &Project,
        settings: Option<&PercussionPipelineSettings>,
        start_beat_offset: f32,
    ) -> PercussionImportOptions {
        match settings {
            Some(s) => s.build_import_options(project, start_beat_offset),
            None => Self::create_default(project, start_beat_offset),
        }
    }
}

/// Shared helper for resolving the default quantize step from project settings.
fn resolve_default_quantize_step(settings: &ProjectSettings) -> f32 {
    match settings.quantize_mode {
        QuantizeMode::Off | QuantizeMode::QuarterBeat => 0.25,
        QuantizeMode::Beat => 1.0,
        QuantizeMode::Bar => (settings.time_signature_numerator as f32).max(1.0),
    }
}
