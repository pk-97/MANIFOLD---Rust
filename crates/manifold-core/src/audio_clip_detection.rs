// Per-audio-clip percussion detection state.
//
// Detection is a property of the audio clip, not a project-global singleton.
// The clip owns its detection settings (`DetectionConfig`) and caches the events
// from its last analysis (`PercussionAnalysisData`) so that changing sensitivity,
// quantize, or routing can re-plan instantly without re-running the analysis
// backend. See `docs/AUDIO_CLIP_DETECTION_DESIGN.md`.

use serde::{Deserialize, Serialize};

use crate::id::LayerId;
use crate::percussion_analysis::{PercussionAnalysisData, PercussionTriggerType};
use crate::units::{Beats, Seconds};

/// Highest confidence threshold the sensitivity slider maps to (at sensitivity 0).
/// At sensitivity 1 the threshold is 0 (accept every detected hit).
const MAX_CONFIDENCE_THRESHOLD: f32 = 0.9;

/// Per-instrument detection settings: whether to detect this trigger type, how
/// sensitive, and which layer its generated trigger clips land on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentDetect {
    pub trigger_type: PercussionTriggerType,
    pub enabled: bool,
    /// 0..1. High sensitivity = low confidence threshold (more hits accepted).
    pub sensitivity: f32,
    /// Target layer for this instrument's triggers. `None` = resolve/auto-create
    /// by trigger name (the existing import-service behaviour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_layer: Option<LayerId>,
}

impl InstrumentDetect {
    pub fn new(trigger_type: PercussionTriggerType) -> Self {
        Self {
            trigger_type,
            enabled: default_enabled(trigger_type),
            sensitivity: 0.5,
            target_layer: None,
        }
    }

    /// Map the 0..1 sensitivity slider to the planner's `min_confidence` filter.
    /// Inverted: sensitivity 1.0 → threshold 0.0 (everything passes); 0.0 →
    /// `MAX_CONFIDENCE_THRESHOLD` (only the strongest hits pass).
    pub fn min_confidence(&self) -> f32 {
        (1.0 - self.sensitivity.clamp(0.0, 1.0)) * MAX_CONFIDENCE_THRESHOLD
    }
}

/// Whether a trigger type is detected by default. Drums on, melodic/sustained off
/// — matches the inspector's default of checked drums, unchecked bass/synth/etc.
fn default_enabled(trigger_type: PercussionTriggerType) -> bool {
    matches!(
        trigger_type,
        PercussionTriggerType::Kick
            | PercussionTriggerType::Snare
            | PercussionTriggerType::Hat
            | PercussionTriggerType::Perc
    )
}

/// The default instrument set, in inspector display order. Mirrors the trigger
/// types the existing `build_import_options` factory wires.
fn default_instruments() -> Vec<InstrumentDetect> {
    [
        PercussionTriggerType::Kick,
        PercussionTriggerType::Snare,
        PercussionTriggerType::Hat,
        PercussionTriggerType::Perc,
        PercussionTriggerType::Bass,
        PercussionTriggerType::BassSustained,
        PercussionTriggerType::Synth,
        PercussionTriggerType::Pad,
        PercussionTriggerType::Vocal,
    ]
    .into_iter()
    .map(InstrumentDetect::new)
    .collect()
}

/// The detection knobs exposed in the audio clip inspector. All act at
/// plan/apply time, so changing any of them re-plans from cached events without
/// re-running the analysis backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectionConfig {
    pub quantize_on: bool,
    pub quantize_step_beats: Beats,
    pub onset_compensation: Seconds,
    pub instruments: Vec<InstrumentDetect>,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            quantize_on: true,
            quantize_step_beats: Beats(0.25),
            onset_compensation: Seconds::ZERO,
            instruments: default_instruments(),
        }
    }
}

impl DetectionConfig {
    /// The per-instrument config for a trigger type, if present and enabled.
    pub fn instrument(&self, trigger_type: PercussionTriggerType) -> Option<&InstrumentDetect> {
        self.instruments
            .iter()
            .find(|i| i.trigger_type == trigger_type)
    }
}

/// Detection state owned by one audio clip: its settings plus the cached events
/// from the last run. `analysis` is `None` until the first Detect.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioClipDetection {
    pub config: DetectionConfig,
    /// Cached events from the last analysis run. Lets sensitivity / quantize /
    /// routing re-plan without re-running the backend. `None` until first Detect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<PercussionAnalysisData>,
}

impl AudioClipDetection {
    /// A fresh detection state with default settings and no analysis yet.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_analysis(&self) -> bool {
        self.analysis.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_maps_to_inverted_confidence() {
        let mut i = InstrumentDetect::new(PercussionTriggerType::Kick);
        i.sensitivity = 1.0;
        assert_eq!(i.min_confidence(), 0.0);
        i.sensitivity = 0.0;
        assert!((i.min_confidence() - MAX_CONFIDENCE_THRESHOLD).abs() < 1e-6);
        i.sensitivity = 0.5;
        assert!((i.min_confidence() - 0.45).abs() < 1e-6);
    }

    #[test]
    fn sensitivity_clamps_out_of_range() {
        let mut i = InstrumentDetect::new(PercussionTriggerType::Snare);
        i.sensitivity = 5.0;
        assert_eq!(i.min_confidence(), 0.0);
        i.sensitivity = -3.0;
        assert!((i.min_confidence() - MAX_CONFIDENCE_THRESHOLD).abs() < 1e-6);
    }

    #[test]
    fn default_enables_drums_only() {
        let cfg = DetectionConfig::default();
        assert!(cfg.instrument(PercussionTriggerType::Kick).unwrap().enabled);
        assert!(cfg.instrument(PercussionTriggerType::Snare).unwrap().enabled);
        assert!(cfg.instrument(PercussionTriggerType::Hat).unwrap().enabled);
        assert!(cfg.instrument(PercussionTriggerType::Perc).unwrap().enabled);
        assert!(!cfg.instrument(PercussionTriggerType::Bass).unwrap().enabled);
        assert!(!cfg.instrument(PercussionTriggerType::Vocal).unwrap().enabled);
    }

    #[test]
    fn default_config_has_all_instruments() {
        let cfg = DetectionConfig::default();
        assert_eq!(cfg.instruments.len(), 9);
        assert!(cfg.quantize_on);
        assert_eq!(cfg.quantize_step_beats, Beats(0.25));
    }

    #[test]
    fn fresh_detection_has_no_analysis() {
        let d = AudioClipDetection::new();
        assert!(!d.has_analysis());
    }
}
