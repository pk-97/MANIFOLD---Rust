// Per-audio-clip percussion detection state.
//
// Detection is a property of the audio clip, not a project-global singleton.
// The clip owns its detection settings (`DetectionConfig`) and caches the hits
// from its last Detect (`PercussionAnalysisData`) so that changing sensitivity,
// quantize, or routing can re-plan instantly without re-running the detectors.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::id::LayerId;
use crate::percussion_analysis::{PercussionAnalysisData, PercussionTriggerType};
use crate::units::{Beats, Seconds};

/// Highest confidence threshold the sensitivity slider maps to (at sensitivity 0).
/// At sensitivity 1 the threshold is 0 (accept every detected hit).
const MAX_CONFIDENCE_THRESHOLD: f32 = 0.9;
/// A new instrument row's sensitivity.
const DEFAULT_SENSITIVITY: f32 = 0.5;

/// The confidence a hit needs to be placed at `sensitivity`.
fn confidence_gate(sensitivity: f32) -> f32 {
    (1.0 - sensitivity.clamp(0.0, 1.0)) * MAX_CONFIDENCE_THRESHOLD
}

/// A trained detector's probability as a Detect confidence. The model's own cutoff lands on the default
/// sensitivity's gate, so a default Detect places exactly what the live detector fires; more sensitivity admits
/// candidates under the cutoff, less keeps only the surest hits. Linear on each side of the cutoff.
pub fn confidence_from_probability(p: f64, cutoff: f64) -> f32 {
    let gate = f64::from(confidence_gate(DEFAULT_SENSITIVITY));
    let p = p.clamp(0.0, 1.0);
    let c = if p < cutoff {
        gate * p / cutoff
    } else {
        gate + (1.0 - gate) * (p - cutoff) / (1.0 - cutoff).max(f64::EPSILON)
    };
    c as f32
}

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
            enabled: true,
            sensitivity: DEFAULT_SENSITIVITY,
            target_layer: None,
        }
    }

    /// Map the 0..1 sensitivity slider to the planner's `min_confidence` filter.
    /// Inverted: sensitivity 1.0 → threshold 0.0 (everything passes); 0.0 →
    /// `MAX_CONFIDENCE_THRESHOLD` (only the strongest hits pass).
    pub fn min_confidence(&self) -> f32 {
        confidence_gate(self.sensitivity)
    }
}

/// The default instrument set, in inspector display order: what the shipped detectors find.
fn default_instruments() -> Vec<InstrumentDetect> {
    [PercussionTriggerType::Kick, PercussionTriggerType::Snare]
        .into_iter()
        .map(InstrumentDetect::new)
        .collect()
}

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

/// The quantize-grid options the inspector dropdown offers, as `(label, step)`.
/// A `None` step means quantize off. Beats are quarter-note based (1 beat = a
/// 1/4 note), so 1/16 = 0.25 beats. Single source of truth shared by the UI
/// (label + dropdown items) so the menu and the displayed value can't drift.
pub fn quantize_grid_options() -> [(&'static str, Option<Beats>); 5] {
    [
        ("Off", None),
        ("1/4", Some(Beats(1.0))),
        ("1/8", Some(Beats(0.5))),
        ("1/16", Some(Beats(0.25))),
        ("1/32", Some(Beats(0.125))),
    ]
}

/// Display label for the current quantize setting — matches a
/// [`quantize_grid_options`] entry, else a formatted step fallback.
pub fn quantize_grid_label(on: bool, step: Beats) -> String {
    if !on {
        return "Off".to_string();
    }
    quantize_grid_options()
        .iter()
        .find_map(|(label, s)| match s {
            Some(b) if (b.0 - step.0).abs() < 1e-4 => Some(label.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| format!("{:.3}", step.0))
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
    /// Triggers placed per instrument by the last plan/replan. Drives the
    /// per-row count in the inspector ("Kick 64"). Derived from the cached
    /// analysis + config, so it is recomputed on every Detect/Replan and not
    /// persisted.
    #[serde(default, skip)]
    pub last_counts: HashMap<PercussionTriggerType, u32>,
}

impl AudioClipDetection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_analysis(&self) -> bool {
        self.analysis.is_some()
    }

    /// Triggers this clip placed for `trigger_type` on the last plan/replan.
    pub fn count(&self, trigger_type: PercussionTriggerType) -> u32 {
        self.last_counts.get(&trigger_type).copied().unwrap_or(0)
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

    /// The cutoff sits exactly on the default gate, the ends stay put, and the mapping never reorders hits.
    #[test]
    fn probability_maps_the_cutoff_onto_the_default_gate() {
        let gate = InstrumentDetect::new(PercussionTriggerType::Kick).min_confidence();
        for cutoff in [0.3, 0.62, 0.9] {
            assert!((confidence_from_probability(cutoff, cutoff) - gate).abs() < 1e-6);
            assert!(confidence_from_probability(cutoff - 0.01, cutoff) < gate);
            assert_eq!(confidence_from_probability(0.0, cutoff), 0.0);
            assert!((confidence_from_probability(1.0, cutoff) - 1.0).abs() < 1e-6);
            let ps: Vec<f32> = (0..=20)
                .map(|i| confidence_from_probability(i as f64 / 20.0, cutoff))
                .collect();
            assert!(ps.windows(2).all(|w| w[0] <= w[1]));
        }
    }

    #[test]
    fn quantize_grid_label_round_trips_options() {
        // Off + every on-step label matches its option entry.
        assert_eq!(quantize_grid_label(false, Beats(0.25)), "Off");
        for (label, step) in quantize_grid_options() {
            if let Some(beats) = step {
                assert_eq!(quantize_grid_label(true, beats), label);
            }
        }
        // The default config's step (0.25 beat) reads as 1/16.
        assert_eq!(quantize_grid_label(true, Beats(0.25)), "1/16");
    }

    #[test]
    fn count_reads_last_counts() {
        let mut d = AudioClipDetection::new();
        assert_eq!(d.count(PercussionTriggerType::Kick), 0);
        d.last_counts.insert(PercussionTriggerType::Kick, 64);
        assert_eq!(d.count(PercussionTriggerType::Kick), 64);
        assert_eq!(d.count(PercussionTriggerType::Snare), 0);
    }
}
