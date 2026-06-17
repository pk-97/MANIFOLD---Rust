//! `ParameterAudioMod` — an audio modulation source on a parameter.
//!
//! The fourth per-parameter modulation source, parallel to drivers (LFOs) and
//! envelopes. Stored on `PresetInstance.audio_mods`, keyed by `param_id` like
//! the others. References a named send in the project's `AudioSetup` by
//! [`AudioSendId`] (never a raw channel), picks a [`AudioFeature`] of that send,
//! and shapes it into a control signal via [`AudioModShape`].
//!
//! Evaluation lives in `manifold-playback` (the modulation pipeline); this
//! module owns the model and the pure shaping math. See
//! `docs/AUDIO_MODULATION_DESIGN.md` §7–§8.

use serde::{Deserialize, Serialize};

use crate::audio_features::SendFeatures;
use crate::effects::ParamId;
use crate::id::AudioSendId;
use crate::macro_bank::MacroCurve;

/// Which perceptual band of a send's energy a feature reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AudioBand {
    Low,
    Mid,
    High,
}

/// Which extracted feature of a send drives the modulation. `Amplitude` (the
/// default) is the simple overall level; `BandEnergy`, `Centroid`, `Flatness`,
/// `Flux` and `Onset` are the v1 spectral features; `Pitch`/`PitchDelta` arrive
/// with the v2 ridge tracker and slot in here without touching the plumbing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AudioFeature {
    /// Overall input level — the RMS of the analysis block, normalized 0..1
    /// (the worker computes it from the raw samples). The shaper maps this
    /// straight onto the target slider's range. The default feature.
    #[default]
    Amplitude,
    BandEnergy(AudioBand),
    /// Spectral centroid — "brightness", normalized 0..1.
    Centroid,
    /// Spectral flatness — tonal (0) vs noisy (1).
    Flatness,
    /// Spectral flux — continuous "how much is changing."
    Flux,
    Onset,
    Pitch,
    PitchDelta,
}

impl AudioFeature {
    /// Pull this feature's scalar out of a send's features.
    pub fn extract(self, f: &SendFeatures) -> f32 {
        match self {
            AudioFeature::Amplitude => f.amplitude,
            AudioFeature::BandEnergy(AudioBand::Low) => f.band_energy[0],
            AudioFeature::BandEnergy(AudioBand::Mid) => f.band_energy[1],
            AudioFeature::BandEnergy(AudioBand::High) => f.band_energy[2],
            AudioFeature::Centroid => f.centroid,
            AudioFeature::Flatness => f.flatness,
            AudioFeature::Flux => f.flux,
            AudioFeature::Onset => f.onset,
            AudioFeature::Pitch => f.pitch_hz,
            AudioFeature::PitchDelta => f.pitch_delta_st,
        }
    }
}

/// The send + feature a modulation reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioModSource {
    /// Named send in the project's `AudioSetup`. Stable across relabel/re-route.
    pub send_id: AudioSendId,
    /// Which feature of that send to read.
    #[serde(default)]
    pub feature: AudioFeature,
}

fn default_sensitivity() -> f32 {
    1.0
}
fn default_attack_ms() -> f32 {
    5.0
}
fn default_release_ms() -> f32 {
    120.0
}
fn one() -> f32 {
    1.0
}
fn default_curve() -> MacroCurve {
    MacroCurve::Linear
}

/// Shapes a raw feature value into a control signal. This is what makes audio
/// modulation feel like an instrument rather than jitter: input sensitivity, a
/// time-based attack/release envelope follower, a response curve, an output
/// sub-range, and optional inversion.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioModShape {
    /// Input gain into the 0..1 normalization (raises a small band-energy
    /// reading into useful range).
    #[serde(default = "default_sensitivity")]
    pub sensitivity: f32,
    /// Rise time constant (ms) when the signal is increasing.
    #[serde(default = "default_attack_ms")]
    pub attack_ms: f32,
    /// Fall time constant (ms) when the signal is decreasing.
    #[serde(default = "default_release_ms")]
    pub release_ms: f32,
    /// Output range floor, 0..1 within the target param's range.
    #[serde(default)]
    pub range_min: f32,
    /// Output range ceiling, 0..1 within the target param's range.
    #[serde(default = "one")]
    pub range_max: f32,
    /// Response curve applied to the shaped signal.
    #[serde(default = "default_curve")]
    pub curve: MacroCurve,
    /// Invert the signal (loud → low) before the range map.
    #[serde(default)]
    pub invert: bool,
}

impl Default for AudioModShape {
    fn default() -> Self {
        Self {
            sensitivity: default_sensitivity(),
            attack_ms: default_attack_ms(),
            release_ms: default_release_ms(),
            range_min: 0.0,
            range_max: 1.0,
            curve: default_curve(),
            invert: false,
        }
    }
}

impl AudioModShape {
    /// Smooth and map `raw` to a normalized 0..1 output within the target
    /// param's range. Advances `smoothed` (the envelope-follower state) in
    /// place. `dt_seconds` is real wall time, so attack/release are frame-rate
    /// independent — a 120 ms release feels the same at 60 or 144 fps.
    pub fn apply(&self, raw: f32, dt_seconds: f32, smoothed: &mut f32) -> f32 {
        let target = (raw * self.sensitivity).clamp(0.0, 1.0);

        // One-pole follower: separate attack/release time constants. A
        // non-positive tau snaps instantly (no smoothing).
        let tau_ms = if target > *smoothed { self.attack_ms } else { self.release_ms };
        let coeff = if tau_ms <= 0.0 {
            1.0
        } else {
            (1.0 - (-(dt_seconds * 1000.0) / tau_ms).exp()).clamp(0.0, 1.0)
        };
        *smoothed += (target - *smoothed) * coeff;

        let mut s = smoothed.clamp(0.0, 1.0);
        if self.invert {
            s = 1.0 - s;
        }
        let curved = self.curve.apply(s);
        self.range_min + (self.range_max - self.range_min) * curved
    }
}

/// An audio modulation bound to one parameter. Mirrors `ParameterDriver`:
/// addressed by `param_id`, with runtime-only state (`smoothed`) that is not
/// serialized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterAudioMod {
    /// Stable mapping key — the param this modulation drives.
    pub param_id: ParamId,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub source: AudioModSource,
    #[serde(default)]
    pub shape: AudioModShape,
    /// Envelope-follower accumulator. Runtime state, not serialized.
    #[serde(skip)]
    pub smoothed: f32,
}

fn default_true() -> bool {
    true
}

impl ParameterAudioMod {
    /// Create a new audio modulation with default shaping.
    pub fn new(param_id: ParamId, send_id: AudioSendId, feature: AudioFeature) -> Self {
        Self {
            param_id,
            enabled: true,
            source: AudioModSource { send_id, feature },
            shape: AudioModShape::default(),
            smoothed: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_selects_the_right_scalar() {
        let f = SendFeatures {
            amplitude: 0.42,
            band_energy: [0.1, 0.2, 0.3],
            centroid: 0.55,
            flatness: 0.15,
            flux: 1.7,
            onset: 0.9,
            pitch_hz: 110.0,
            pitch_delta_st: -2.5,
            pitch_confidence: 0.8,
        };
        assert_eq!(AudioFeature::BandEnergy(AudioBand::Low).extract(&f), 0.1);
        assert_eq!(AudioFeature::BandEnergy(AudioBand::High).extract(&f), 0.3);
        assert_eq!(AudioFeature::Centroid.extract(&f), 0.55);
        assert_eq!(AudioFeature::Flatness.extract(&f), 0.15);
        assert_eq!(AudioFeature::Flux.extract(&f), 1.7);
        assert_eq!(AudioFeature::Onset.extract(&f), 0.9);
        assert_eq!(AudioFeature::PitchDelta.extract(&f), -2.5);
        // Amplitude reads the worker's normalized 0..1 RMS level directly.
        assert_eq!(AudioFeature::Amplitude.extract(&f), 0.42);
    }

    #[test]
    fn amplitude_is_the_default_feature() {
        assert_eq!(AudioFeature::default(), AudioFeature::Amplitude);
    }

    #[test]
    fn shape_attack_rises_toward_target_over_time() {
        let shape = AudioModShape {
            attack_ms: 50.0,
            release_ms: 50.0,
            ..Default::default()
        };
        let mut s = 0.0;
        // One 16 ms step toward 1.0 should move partway, not all the way.
        let out = shape.apply(1.0, 0.016, &mut s);
        assert!(s > 0.0 && s < 1.0, "partial rise, got {s}");
        assert!((out - s).abs() < 1e-6, "linear curve, full range → out == smoothed");

        // Many steps converge to ~1.0.
        for _ in 0..100 {
            shape.apply(1.0, 0.016, &mut s);
        }
        assert!(s > 0.99, "converges to target, got {s}");
    }

    #[test]
    fn shape_range_and_invert() {
        let shape = AudioModShape {
            attack_ms: 0.0,
            release_ms: 0.0, // snap instantly
            range_min: 0.2,
            range_max: 0.8,
            invert: true,
            ..Default::default()
        };
        let mut s = 0.0;
        // raw 1.0 → smoothed 1.0 → invert → 0.0 → range map → 0.2.
        let out = shape.apply(1.0, 0.016, &mut s);
        assert!((out - 0.2).abs() < 1e-6, "inverted full signal maps to range floor, got {out}");
    }

    #[test]
    fn round_trips_through_json() {
        let m = ParameterAudioMod::new(
            "amount".into(),
            AudioSendId::new("send-1"),
            AudioFeature::BandEnergy(AudioBand::Mid),
        );
        let json = serde_json::to_string(&m).unwrap();
        let back: ParameterAudioMod = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // Runtime state is not serialized.
        assert!(!json.contains("smoothed"));
    }
}
