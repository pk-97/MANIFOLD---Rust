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

/// A frequency band a feature is measured over. `Full` is the whole spectrum;
/// `Low`/`Mid`/`High` restrict the reduction to a sub-range, so any feature can
/// run on any band (e.g. `Transients` on `Low` is a kick detector).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AudioBand {
    #[default]
    Full,
    Low,
    Mid,
    High,
}

impl AudioBand {
    /// All bands in [`crate::audio_features::SendFeatures::bands`] order.
    pub const ALL: [AudioBand; 4] =
        [AudioBand::Full, AudioBand::Low, AudioBand::Mid, AudioBand::High];

    /// Index into [`crate::audio_features::SendFeatures::bands`].
    pub fn index(self) -> usize {
        match self {
            AudioBand::Full => 0,
            AudioBand::Low => 1,
            AudioBand::Mid => 2,
            AudioBand::High => 3,
        }
    }

    /// Short user-facing label.
    pub fn label(self) -> &'static str {
        match self {
            AudioBand::Full => "Full",
            AudioBand::Low => "Low",
            AudioBand::Mid => "Mid",
            AudioBand::High => "High",
        }
    }
}

/// Which detector runs over the chosen band. Each name describes the rough
/// character of the sound it responds to. All are normalized 0..1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AudioFeatureKind {
    /// Loudness of the band (RMS-like energy, dB-normalized).
    #[default]
    Amplitude,
    /// Spectral centroid — brightness.
    Centroid,
    /// Spectral flatness — tonal (0) vs noisy (1).
    Noisiness,
    /// Relative spectral flux — how much the band is changing.
    Flux,
    /// Onset trigger — transient hits in the band.
    Transients,
    /// Kick trigger — descending-FM-ridge detector, sub-bass only. A dedicated
    /// no-fallback kick detector: fires on the coherent pitch descent a kick
    /// drum makes, which general `Transients` (flux) is blind to on a
    /// bass-occupied Low band, and which a bass note's fixed-pitch attack cannot
    /// fake. Always reads the Low band regardless of the selected `band`.
    Kick,
    /// Tracked pitch of the band's dominant object, normalized to the band's
    /// bin range (P4, docs/AUDIO_OBJECT_TRACKING_DESIGN.md). HOLDS on dropout
    /// — gate with `Presence`, never read a held value as "low pitch".
    Pitch,
    /// Confidence the band's tracked pitch is a real object (0..1) — the D6
    /// display/trust signal.
    Presence,
}

impl AudioFeatureKind {
    /// All kinds in drawer-button order.
    pub const ALL: [AudioFeatureKind; 8] = [
        AudioFeatureKind::Amplitude,
        AudioFeatureKind::Centroid,
        AudioFeatureKind::Noisiness,
        AudioFeatureKind::Flux,
        AudioFeatureKind::Transients,
        AudioFeatureKind::Kick,
        AudioFeatureKind::Pitch,
        AudioFeatureKind::Presence,
    ];

    /// Index in [`Self::ALL`] order.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&k| k == self).unwrap_or(0)
    }

    /// User-facing label.
    pub fn label(self) -> &'static str {
        match self {
            AudioFeatureKind::Amplitude => "Amplitude",
            AudioFeatureKind::Centroid => "Centroid",
            AudioFeatureKind::Noisiness => "Noisiness",
            AudioFeatureKind::Flux => "Flux",
            AudioFeatureKind::Transients => "Transients",
            AudioFeatureKind::Kick => "Kick",
            AudioFeatureKind::Pitch => "Pitch",
            AudioFeatureKind::Presence => "Presence",
        }
    }
}

/// What a modulation reads: a detector (`kind`) run over a frequency band
/// (`band`). The cross-product is the feature matrix exposed in the drawer —
/// e.g. `{ Transients, Low }` is a kick detector, `{ Centroid, Full }` is
/// overall brightness. Deserialization migrates the pre-matrix flat enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioFeature {
    pub kind: AudioFeatureKind,
    pub band: AudioBand,
}

impl AudioFeature {
    pub fn new(kind: AudioFeatureKind, band: AudioBand) -> Self {
        Self { kind, band }
    }

    /// Pull this feature's scalar out of a send's per-band features.
    pub fn extract(self, f: &SendFeatures) -> f32 {
        // Kick is a sub-bass-only detector; it always reads the Low band so a
        // `Kick` selected on any other band can't silently read zero.
        if let AudioFeatureKind::Kick = self.kind {
            return f.bands[AudioBand::Low.index()].kick;
        }
        let b = &f.bands[self.band.index()];
        match self.kind {
            AudioFeatureKind::Amplitude => b.amplitude,
            AudioFeatureKind::Centroid => b.brightness,
            AudioFeatureKind::Noisiness => b.noisiness,
            AudioFeatureKind::Flux => b.liveliness,
            AudioFeatureKind::Transients => b.transients,
            AudioFeatureKind::Kick => b.kick, // unreachable (handled above), keeps match total
            AudioFeatureKind::Pitch => b.pitch,
            AudioFeatureKind::Presence => b.presence,
        }
    }
}

// ── Load migration: pre-matrix flat feature enum → { kind, band } ──
//
// `AudioFeature` used to be a flat enum (Amplitude / BandEnergy(band) / Centroid
// / …). Saved projects carry that shape, so deserialization accepts both: the
// current `{ kind, band }` object, or the legacy enum, mapped onto the matrix.

impl<'de> Deserialize<'de> for AudioFeature {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(AudioFeatureRepr::deserialize(d)?.into())
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AudioFeatureRepr {
    /// Current shape — both keys required (no defaults), so the legacy object
    /// form (`{ "bandEnergy": … }`) can't accidentally match here.
    Matrix { kind: AudioFeatureKind, band: AudioBand },
    /// Legacy flat enum.
    Legacy(LegacyAudioFeature),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum LegacyAudioFeature {
    Amplitude,
    BandEnergy(AudioBand),
    Centroid,
    Flatness,
    Flux,
    Onset,
    Pitch,
    PitchDelta,
}

impl From<AudioFeatureRepr> for AudioFeature {
    fn from(r: AudioFeatureRepr) -> Self {
        use AudioBand::Full;
        use AudioFeatureKind::*;
        let (kind, band) = match r {
            AudioFeatureRepr::Matrix { kind, band } => (kind, band),
            AudioFeatureRepr::Legacy(l) => match l {
                LegacyAudioFeature::Amplitude => (Amplitude, Full),
                LegacyAudioFeature::BandEnergy(b) => (Amplitude, b),
                LegacyAudioFeature::Centroid => (Centroid, Full),
                LegacyAudioFeature::Flatness => (Noisiness, Full),
                LegacyAudioFeature::Flux => (Flux, Full),
                LegacyAudioFeature::Onset => (Transients, Full),
                // D3 retarget (P4): the reserved legacy pitch names finally
                // land on the real tracker. No `PitchDelta` kind exists by
                // design — `rate_of_change` on `Pitch` composes it.
                LegacyAudioFeature::Pitch | LegacyAudioFeature::PitchDelta => (Pitch, Full),
            },
        };
        AudioFeature { kind, band }
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
    /// Drive on the feature's **rate of change** (per second) instead of its
    /// level, centered so "no change" sits mid-range and rising/falling push
    /// above/below. Turns any feature into a motion signal — the literal "glue"
    /// control: the visual breathes with the sound instead of leveling with it.
    #[serde(default)]
    pub rate_of_change: bool,
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
            rate_of_change: false,
        }
    }
}

impl AudioModShape {
    /// Smooth and map `raw` to a normalized 0..1 output within the target
    /// param's range. Advances `smoothed` (the envelope-follower state) and
    /// `prev_raw` (the rate-of-change differentiator's last sample) in place.
    /// `dt_seconds` is real wall time, so both attack/release and the rate are
    /// frame-rate independent — a 120 ms release feels the same at 60 or 144 fps.
    pub fn apply(&self, raw: f32, dt_seconds: f32, smoothed: &mut f32, prev_raw: &mut f32) -> f32 {
        // Rate-of-change differentiates the feature over real time (per second),
        // scaled by sensitivity and centered at 0.5 so a steady signal reads as
        // mid-range while motion pushes it up (rising) or down (falling). The
        // level path is the plain sensitivity-scaled value. `prev_raw` always
        // tracks the last raw, so toggling the mode mid-stream stays clean.
        let target = if self.rate_of_change {
            let rate = (raw - *prev_raw) / dt_seconds.max(1e-4);
            (0.5 + rate * self.sensitivity).clamp(0.0, 1.0)
        } else {
            (raw * self.sensitivity).clamp(0.0, 1.0)
        };
        *prev_raw = raw;

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
    /// Previous raw feature value — the rate-of-change differentiator's state.
    /// Runtime state, not serialized.
    #[serde(skip)]
    pub prev_raw: f32,
    /// §8 D5b: when this mod's target param is `is_trigger`, evaluation
    /// switches from continuous overwrite to edge detection over the shaped
    /// `out_norm` (rising through 0.5). Runtime state, not serialized —
    /// resets to armed on load, matching `smoothed`/`prev_raw`.
    #[serde(skip)]
    pub trigger_edge: crate::audio_trigger::TransientEdge,
    /// §8 D5b: monotonic fire counter for an `is_trigger` target — each
    /// `trigger_edge` fire bumps this by one; the written value is
    /// `base + fire_count`, the same monotonic-counter shape every
    /// `last_count`-style consumer already edge-detects. Runtime state, not
    /// serialized.
    #[serde(skip)]
    pub fire_count: u32,
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
            prev_raw: 0.0,
            trigger_edge: crate::audio_trigger::TransientEdge::default(),
            fire_count: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_selects_the_right_scalar() {
        use crate::audio_features::BandFeatures;
        use AudioBand::*;
        use AudioFeatureKind::*;
        let f = SendFeatures {
            bands: [
                BandFeatures { amplitude: 0.42, ..Default::default() }, // Full
                BandFeatures { amplitude: 0.1, ..Default::default() },  // Low
                BandFeatures { brightness: 0.55, ..Default::default() }, // Mid
                BandFeatures { liveliness: 1.7, transients: 0.9, ..Default::default() }, // High
            ],
            ..Default::default()
        };
        assert_eq!(AudioFeature::new(Amplitude, Full).extract(&f), 0.42);
        assert_eq!(AudioFeature::new(Amplitude, Low).extract(&f), 0.1);
        assert_eq!(AudioFeature::new(Centroid, Mid).extract(&f), 0.55);
        assert_eq!(AudioFeature::new(Flux, High).extract(&f), 1.7);
        assert_eq!(AudioFeature::new(Transients, High).extract(&f), 0.9);
    }

    #[test]
    fn extract_reads_pitch_and_presence() {
        use crate::audio_features::BandFeatures;
        use AudioBand::*;
        use AudioFeatureKind::*;
        let f = SendFeatures {
            bands: [
                BandFeatures { pitch: 0.61, presence: 0.9, ..Default::default() }, // Full
                BandFeatures { pitch: 0.25, presence: 0.4, ..Default::default() }, // Low
                BandFeatures::default(),
                BandFeatures::default(),
            ],
            ..Default::default()
        };
        assert_eq!(AudioFeature::new(Pitch, Full).extract(&f), 0.61);
        assert_eq!(AudioFeature::new(Presence, Full).extract(&f), 0.9);
        assert_eq!(AudioFeature::new(Pitch, Low).extract(&f), 0.25);
        assert_eq!(AudioFeature::new(Presence, Low).extract(&f), 0.4);
    }

    #[test]
    fn amplitude_full_is_the_default_feature() {
        assert_eq!(
            AudioFeature::default(),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full)
        );
    }

    #[test]
    fn legacy_flat_feature_migrates_to_matrix() {
        // Old flat-enum JSON forms must load onto the { kind, band } matrix.
        let cases = [
            ("\"amplitude\"", AudioFeatureKind::Amplitude, AudioBand::Full),
            ("{\"bandEnergy\":\"low\"}", AudioFeatureKind::Amplitude, AudioBand::Low),
            ("\"centroid\"", AudioFeatureKind::Centroid, AudioBand::Full),
            ("\"flatness\"", AudioFeatureKind::Noisiness, AudioBand::Full),
            ("\"flux\"", AudioFeatureKind::Flux, AudioBand::Full),
            ("\"onset\"", AudioFeatureKind::Transients, AudioBand::Full),
            // D3 retarget (P4): the reserved legacy pitch names land on the
            // real tracker; PitchDelta composes as rate_of_change on Pitch.
            ("\"pitch\"", AudioFeatureKind::Pitch, AudioBand::Full),
            ("\"pitchDelta\"", AudioFeatureKind::Pitch, AudioBand::Full),
        ];
        for (json, kind, band) in cases {
            let f: AudioFeature = serde_json::from_str(json).unwrap();
            assert_eq!(f, AudioFeature::new(kind, band), "migrating {json}");
        }
        // Current shape round-trips — including the P4 kinds, whose serde
        // names ("pitch"/"presence") are load-bearing for saved projects.
        for kind in AudioFeatureKind::ALL {
            let cur = AudioFeature::new(kind, AudioBand::High);
            let json = serde_json::to_string(&cur).unwrap();
            assert_eq!(serde_json::from_str::<AudioFeature>(&json).unwrap(), cur, "round-trip {json}");
        }
        assert!(
            serde_json::to_string(&AudioFeatureKind::Pitch).unwrap().contains("pitch")
                && serde_json::to_string(&AudioFeatureKind::Presence).unwrap().contains("presence"),
            "serde names are the file-format contract"
        );
    }

    #[test]
    fn shape_attack_rises_toward_target_over_time() {
        let shape = AudioModShape {
            attack_ms: 50.0,
            release_ms: 50.0,
            ..Default::default()
        };
        let mut s = 0.0;
        let mut p = 0.0;
        // One 16 ms step toward 1.0 should move partway, not all the way.
        let out = shape.apply(1.0, 0.016, &mut s, &mut p);
        assert!(s > 0.0 && s < 1.0, "partial rise, got {s}");
        assert!((out - s).abs() < 1e-6, "linear curve, full range → out == smoothed");

        // Many steps converge to ~1.0.
        for _ in 0..100 {
            shape.apply(1.0, 0.016, &mut s, &mut p);
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
        let mut p = 0.0;
        // raw 1.0 → smoothed 1.0 → invert → 0.0 → range map → 0.2.
        let out = shape.apply(1.0, 0.016, &mut s, &mut p);
        assert!((out - 0.2).abs() < 1e-6, "inverted full signal maps to range floor, got {out}");
    }

    #[test]
    fn rate_of_change_drives_on_motion_not_level() {
        let shape = AudioModShape {
            attack_ms: 0.0,
            release_ms: 0.0, // snap, so output == target
            rate_of_change: true,
            ..Default::default()
        };
        let mut s = 0.0;
        let mut prev = 0.0;
        // Steady level (same raw twice) → no change → centered at 0.5.
        shape.apply(0.7, 0.016, &mut s, &mut prev);
        let steady = shape.apply(0.7, 0.016, &mut s, &mut prev);
        assert!((steady - 0.5).abs() < 1e-6, "no change reads mid-range, got {steady}");
        // A rise pushes above 0.5; a fall pushes below.
        let rising = shape.apply(0.9, 0.016, &mut s, &mut prev);
        assert!(rising > 0.5, "rising feature pushes above center, got {rising}");
        let falling = shape.apply(0.5, 0.016, &mut s, &mut prev);
        assert!(falling < 0.5, "falling feature pushes below center, got {falling}");
    }

    #[test]
    fn round_trips_through_json() {
        let m = ParameterAudioMod::new(
            "amount".into(),
            AudioSendId::new("send-1"),
            AudioFeature::new(AudioFeatureKind::Flux, AudioBand::Mid),
        );
        let json = serde_json::to_string(&m).unwrap();
        let back: ParameterAudioMod = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // Runtime state is not serialized.
        assert!(!json.contains("smoothed"));
    }
}
