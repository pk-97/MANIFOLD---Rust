//! Live audio trigger routes — realtime onset → one-shot clip routing.
//!
//! The realtime sibling of per-clip percussion detection
//! (`audio_clip_detection`). A [`TriggerRoute`] hangs off an
//! [`AudioSend`](crate::audio_setup::AudioSend): it watches the send's transient
//! detector on one frequency band and fires a fixed-length one-shot clip on a
//! target layer when an onset crosses its threshold. No lookahead, no analysis
//! backend — it reads the same `SendFeatures` the audio-modulation pipeline
//! already produces every analysis block. See `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md`.
//!
//! This module owns the **model** and the **pure threshold math**. The stateful
//! arm/fire/re-arm edge detection lives in the evaluator (`manifold-playback`),
//! because it carries per-route runtime state that is never serialized.

use serde::{Deserialize, Serialize};

use crate::audio_features::SendFeatures;
use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind};
use crate::id::LayerId;
use crate::units::Beats;

/// Transient threshold at sensitivity 0 — only the strongest impulses fire.
const MAX_TRIGGER_THRESHOLD: f32 = 0.9;
/// Transient threshold at sensitivity 1 — fire on almost anything, but never 0
/// (a 0 threshold would fire on the detector's noise floor every block).
const MIN_TRIGGER_THRESHOLD: f32 = 0.05;
/// Default one-shot length (beats) — a one-beat flash; the user tunes per route.
const DEFAULT_ONE_SHOT_BEATS: f64 = 1.0;

/// One audio → visual trigger: a send's transient on `source` fires a one-shot
/// clip on `target_layer`. All fields act at evaluation time, so editing any of
/// them takes effect on the next analysis block without restarting capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerRoute {
    /// Whether this route fires. A disabled route keeps its config (it's a row
    /// in the inspector you can toggle), it just never triggers.
    pub enabled: bool,
    /// Frequency band the transient is read from. `Full` = the whole-signal
    /// onset ("Whole" — use for a separated stem); `Low`/`Mid`/`High` split a
    /// full mix. No new detector: `Full` already runs the transient detector.
    pub source: AudioBand,
    /// Layer the fired one-shot lands on. `None` = auto-route by name (a send
    /// labeled "Kick" resolves to a layer named "Kick" at apply time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_layer: Option<LayerId>,
    /// 0..1. High sensitivity = low transient threshold (more onsets fire).
    pub sensitivity: f32,
    /// Quantize the fire to a beat grid. `None` = off (tightest latency, the
    /// default); `Some(step)` = snap to the next multiple of `step` beats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantize: Option<Beats>,
    /// How long the fired one-shot clip holds. A transient has no note-off, so
    /// the fire length is fixed here rather than by a release event.
    pub one_shot_beats: Beats,
}

impl TriggerRoute {
    /// A new route reading `source`, disabled by default (the user enables a row
    /// once they've pointed it at a layer), mid sensitivity, quantize off.
    pub fn new(source: AudioBand) -> Self {
        Self {
            enabled: false,
            source,
            target_layer: None,
            sensitivity: 0.5,
            quantize: None,
            one_shot_beats: Beats(DEFAULT_ONE_SHOT_BEATS),
        }
    }

    /// Map the 0..1 sensitivity slider to the transient fire threshold.
    /// Inverted: sensitivity 1.0 → [`MIN_TRIGGER_THRESHOLD`] (fire easily);
    /// 0.0 → [`MAX_TRIGGER_THRESHOLD`] (only the strongest onsets).
    pub fn threshold(&self) -> f32 {
        let s = self.sensitivity.clamp(0.0, 1.0);
        MIN_TRIGGER_THRESHOLD + (1.0 - s) * (MAX_TRIGGER_THRESHOLD - MIN_TRIGGER_THRESHOLD)
    }

    /// The transient impulse (0..1) for this route's band, read from a send's
    /// features. Reuses the audio-modulation feature extractor so band indexing
    /// stays in one place.
    pub fn transient(&self, features: &SendFeatures) -> f32 {
        AudioFeature::new(AudioFeatureKind::Transients, self.source).extract(features)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_route_is_disabled_full_band_mid_sensitivity() {
        let r = TriggerRoute::new(AudioBand::Full);
        assert!(!r.enabled);
        assert_eq!(r.source, AudioBand::Full);
        assert!(r.target_layer.is_none());
        assert_eq!(r.sensitivity, 0.5);
        assert!(r.quantize.is_none());
    }

    #[test]
    fn threshold_inverts_sensitivity() {
        let mut r = TriggerRoute::new(AudioBand::Low);
        r.sensitivity = 1.0;
        assert!((r.threshold() - MIN_TRIGGER_THRESHOLD).abs() < 1e-6);
        r.sensitivity = 0.0;
        assert!((r.threshold() - MAX_TRIGGER_THRESHOLD).abs() < 1e-6);
        r.sensitivity = 0.5;
        let mid = MIN_TRIGGER_THRESHOLD + 0.5 * (MAX_TRIGGER_THRESHOLD - MIN_TRIGGER_THRESHOLD);
        assert!((r.threshold() - mid).abs() < 1e-6);
    }

    #[test]
    fn threshold_clamps_out_of_range_sensitivity() {
        let mut r = TriggerRoute::new(AudioBand::Mid);
        r.sensitivity = 5.0;
        assert!((r.threshold() - MIN_TRIGGER_THRESHOLD).abs() < 1e-6);
        r.sensitivity = -2.0;
        assert!((r.threshold() - MAX_TRIGGER_THRESHOLD).abs() < 1e-6);
    }

    #[test]
    fn transient_reads_the_routes_band() {
        let mut features = SendFeatures::default();
        features.bands[AudioBand::Low.index()].transients = 0.7;
        features.bands[AudioBand::High.index()].transients = 0.2;
        let low = TriggerRoute::new(AudioBand::Low);
        let high = TriggerRoute::new(AudioBand::High);
        assert!((low.transient(&features) - 0.7).abs() < 1e-6);
        assert!((high.transient(&features) - 0.2).abs() < 1e-6);
    }
}
