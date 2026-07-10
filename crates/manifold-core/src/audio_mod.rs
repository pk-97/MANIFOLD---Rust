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
    /// Condition `raw` into a normalized 0..1 signal: sensitivity/rate-of-change,
    /// the attack/release envelope follower, invert, and the response curve —
    /// everything EXCEPT the output range map. Advances `smoothed` (the
    /// envelope-follower state) and `prev_raw` (the rate-of-change
    /// differentiator's last sample) in place. `dt_seconds` is real wall time,
    /// so both attack/release and the rate are frame-rate independent — a
    /// 120 ms release feels the same at 60 or 144 fps.
    ///
    /// This is the signal edge detection (Step/Random fire arms, `is_trigger`,
    /// `is_trigger_gate`) must read — the trim handles (`range_min`/`range_max`)
    /// must never distort whether/when a mod fires. `invert` deliberately stays
    /// visible here (before the range map) so "fire on the quiet gaps" keeps
    /// working.
    pub fn condition(&self, raw: f32, dt_seconds: f32, smoothed: &mut f32, prev_raw: &mut f32) -> f32 {
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
        self.curve.apply(s)
    }

    /// Map a conditioned (post-`condition`) 0..1 signal onto the trim-handle
    /// output range.
    pub fn map_range(&self, conditioned: f32) -> f32 {
        self.range_min + (self.range_max - self.range_min) * conditioned
    }

    /// Smooth and map `raw` to a normalized 0..1 output within the target
    /// param's range — `map_range(condition(...))`, kept as one call for
    /// existing continuous-value callers (e.g. the `mod_harness` example).
    pub fn apply(&self, raw: f32, dt_seconds: f32, smoothed: &mut f32, prev_raw: &mut f32) -> f32 {
        self.map_range(self.condition(raw, dt_seconds, smoothed, prev_raw))
    }

    /// The param-unit travel zone the trim handles (`range_min`/`range_max`)
    /// define on `[min, max]` — the rails Step/Random advance within, so a
    /// trimmed handle bounds the fire actions exactly the way it already
    /// bounds Continuous. Defensive: if a crossed handle pair (`range_min >
    /// range_max`) ever reaches here, the rails are swapped rather than
    /// producing an inverted (lo > hi) zone — the UI shouldn't produce this,
    /// but the math must not break if it does.
    pub fn zone(&self, min: f32, max: f32) -> (f32, f32) {
        let lo = min + self.range_min * (max - min);
        let hi = min + self.range_max * (max - min);
        if lo > hi { (hi, lo) } else { (lo, hi) }
    }
}

/// PARAM_STEP_ACTIONS D2: what a fire does to the target param, evaluated by
/// the same edge chassis (`trigger_edge.advance`) `is_trigger` targets
/// already use. `Continuous` (default) is today's behavior — the shaped
/// signal overwrites the value every tick. `Step`/`Random` instead advance a
/// runtime shadow (`ParameterAudioMod::step_value`) that *replaces* the
/// param's base at evaluation time (D4) — a fire acts like the user's hand
/// moved the slider, and everything that stacks on a hand-set base (drivers,
/// continuous audio mods, envelopes) stacks on the stepped value identically.
/// No `every`/divisor field exists here (D6, retired 2026-07-08) — step size
/// is `amount` alone; every armed mod fires on every event its `trigger_mode`
/// admits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TriggerAction {
    /// Today's behavior: the shaped signal overwrites the value continuously.
    #[default]
    Continuous,
    /// Each fire moves the stepped value by `amount` (signed, param units).
    Step { amount: f32, wrap: WrapMode },
    /// Each fire jumps to a deterministic pseudo-random value in range,
    /// never repeating the current one for a discrete param (D7).
    Random,
}

/// How [`TriggerAction::Step`] behaves at the param's `min`/`max` rails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum WrapMode {
    /// min..=max is a cycle: stepping past max lands at min (and vice versa).
    #[default]
    Wrap,
    /// Ping-pong: direction reverses at min/max.
    Bounce,
    /// Saturate at the ends.
    Clamp,
}

impl WrapMode {
    /// Advance `current` by `delta` (signed, already sign-combined with the
    /// running `dir` for `Bounce`) at the `[min, max]` rails this mode
    /// implements. `dir` is `Bounce`'s ping-pong sign (±1) — unused, but
    /// always returned, by `Wrap`/`Clamp` so the caller can persist one
    /// uniform piece of state regardless of which wrap mode is armed (a live
    /// wrap-mode switch never needs special-casing). Returns
    /// `(new_value, new_dir)`.
    pub fn advance(self, current: f32, delta: f32, dir: f32, min: f32, max: f32) -> (f32, f32) {
        let range = (max - min).max(f32::EPSILON);
        match self {
            WrapMode::Wrap => {
                // rem_euclid keeps the result in [min, max) regardless of how
                // large `delta` is or which direction it overshoots from.
                let next = min + (current + delta - min).rem_euclid(range);
                (next, dir)
            }
            WrapMode::Bounce => {
                let mut d = dir;
                let mut next = current + delta * d;
                if next > max {
                    next = max - (next - max);
                    d = -d;
                } else if next < min {
                    next = min + (min - next);
                    d = -d;
                }
                (next.clamp(min, max), d)
            }
            WrapMode::Clamp => ((current + delta).clamp(min, max), dir),
        }
    }
}

/// D2's UI-seeding default for a fresh `Step` action's `amount`: 1.0 for a
/// discrete param (whole_numbers/value_labels — one card-step per fire), or
/// an eighth of the param's range for a continuous one (a fine-but-audible
/// jump per hit, the same "size any other knob" feel). Seeding only — once
/// the user sets `amount` this is never consulted again; the stored value is
/// whatever they left it at.
pub fn default_step_amount(min: f32, max: f32, whole_numbers: bool) -> f32 {
    if whole_numbers { 1.0 } else { (max - min) / 8.0 }
}

/// D7: a deterministic pseudo-random step value keyed by `ordinal` (the
/// mod's own monotonic fire count — reused from `fire_count`, never a wall
/// clock or RNG state, so replaying the same fire sequence — e.g. offline
/// export — reproduces the identical value sequence every time).
/// `discrete_count = Some(n)` for a whole-numbers/value-labels param with `n`
/// reachable integer positions on `[min, max]`; `current_index` is excluded
/// so adjacent fires never repeat (the `ClipTriggerCycle` non-repeat
/// invariant, relocated here). `None` = continuous full-range random with no
/// exclusion (repeat probability is negligible, so none is enforced).
pub fn random_step_value(
    ordinal: u32,
    min: f32,
    max: f32,
    discrete_count: Option<u32>,
    current_index: u32,
) -> f32 {
    match discrete_count {
        Some(n) if n > 1 => {
            // Pick from the n-1 buckets that exclude `current_index`, then
            // shift the bucket index past it so every other value is
            // reachable and the current one never repeats back-to-back.
            let bucket = crate::effects::hash_u32(ordinal) % (n - 1);
            let idx = if bucket >= current_index { bucket + 1 } else { bucket };
            min + idx as f32
        }
        // n <= 1: nothing to randomize between (a single-value discrete
        // param, degenerate but not a panic case).
        Some(_) => min,
        None => min + (max - min) * crate::effects::hash_to_float(ordinal),
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
    /// §9 U1/U3: when this mod's target param is a trigger-gate card
    /// (`spec.is_trigger_gate`), which events fire the gate's trigger
    /// response — `ClipEdge`/`Transient`/`Both`. `None` on every non-gate
    /// target (the vast majority of mods); serde skip-none so ordinary audio
    /// mods stay byte-identical. Supersedes the deleted `AudioTriggerMod`
    /// (§8 D2) — a fire-mode mod is now a normal `ParameterAudioMod`, not a
    /// parallel per-instance config type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_mode: Option<crate::audio_trigger::TriggerFireMode>,
    /// PARAM_STEP_ACTIONS D1/D2: what a fire does to the target param.
    /// `Continuous` (default) is skip-when-default so old projects and every
    /// ordinary (non-stepping) mod stay byte-identical on disk.
    #[serde(default, skip_serializing_if = "is_continuous_action")]
    pub action: TriggerAction,
    /// D4: the stepped/randomized value, which *replaces* `base` at
    /// evaluation time (`apply_step_values`, `modulation.rs` Phase 1.5).
    /// `None` until this mod's first fire (which seeds from the param's
    /// `base`, per D4's lifecycle); a disarm (disabled/deleted mod) or a
    /// project reload drops it back to `None`, so the param falls back to
    /// the committed base — deliberate live behavior, not a bug. Runtime
    /// state, not serialized.
    #[serde(skip)]
    pub step_value: Option<f32>,
    /// `WrapMode::Bounce`'s running ping-pong sign (±1, D2). Unused by
    /// `Wrap`/`Clamp` but always carried so switching wrap mode mid-
    /// performance never needs special-casing. Starts at +1 (the first step
    /// follows `amount`'s authored sign). Runtime state, not serialized.
    #[serde(skip, default = "one")]
    pub step_dir: f32,
}

fn default_true() -> bool {
    true
}

fn is_continuous_action(action: &TriggerAction) -> bool {
    matches!(action, TriggerAction::Continuous)
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
            trigger_mode: None,
            action: TriggerAction::default(),
            step_value: None,
            step_dir: 1.0,
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
    fn map_range_of_condition_equals_apply() {
        // condition() then map_range() must reproduce apply() exactly, for a
        // handful of shapes/raws — the split is a pure refactor.
        let shapes = [
            AudioModShape::default(),
            AudioModShape { range_min: 0.2, range_max: 0.8, ..Default::default() },
            AudioModShape { invert: true, range_min: 0.1, range_max: 0.4, ..Default::default() },
            AudioModShape { rate_of_change: true, sensitivity: 2.0, ..Default::default() },
        ];
        for shape in shapes {
            for raw in [0.0_f32, 0.3, 0.5, 0.7, 1.0] {
                let mut s1 = 0.0;
                let mut p1 = 0.0;
                let apply_out = shape.apply(raw, 0.016, &mut s1, &mut p1);

                let mut s2 = 0.0;
                let mut p2 = 0.0;
                let conditioned = shape.condition(raw, 0.016, &mut s2, &mut p2);
                let mapped = shape.map_range(conditioned);

                assert!(
                    (apply_out - mapped).abs() < 1e-6,
                    "apply({raw}) = {apply_out}, map_range(condition({raw})) = {mapped}"
                );
                assert_eq!(s1, s2, "condition() must advance smoothed identically to apply()");
                assert_eq!(p1, p2, "condition() must advance prev_raw identically to apply()");
            }
        }
    }

    #[test]
    fn condition_does_not_leak_the_range_map() {
        // range 0.2..0.8 must not appear in condition()'s output — a
        // full-scale signal must still reach 1.0 pre-map (what edge detection
        // sees), while apply() (post-map) caps at 0.8.
        let shape = AudioModShape {
            attack_ms: 0.0,
            release_ms: 0.0,
            range_min: 0.2,
            range_max: 0.8,
            ..Default::default()
        };
        let mut s = 0.0;
        let mut p = 0.0;
        let conditioned = shape.condition(1.0, 0.016, &mut s, &mut p);
        assert!((conditioned - 1.0).abs() < 1e-6, "conditioned signal ignores the range map, got {conditioned}");

        let mut s2 = 0.0;
        let mut p2 = 0.0;
        let applied = shape.apply(1.0, 0.016, &mut s2, &mut p2);
        assert!((applied - 0.8).abs() < 1e-6, "apply() caps at the range ceiling, got {applied}");
    }

    #[test]
    fn zone_maps_range_handles_into_param_units() {
        let shape = AudioModShape { range_min: 0.2, range_max: 0.8, ..Default::default() };
        let (lo, hi) = shape.zone(0.0, 10.0);
        assert!((lo - 2.0).abs() < 1e-6, "lo = {lo}");
        assert!((hi - 8.0).abs() < 1e-6, "hi = {hi}");
    }

    #[test]
    fn zone_swaps_crossed_handles() {
        // Defensive: a crossed range_min > range_max must not produce an
        // inverted (lo > hi) zone.
        let shape = AudioModShape { range_min: 0.9, range_max: 0.1, ..Default::default() };
        let (lo, hi) = shape.zone(0.0, 10.0);
        assert!(lo <= hi, "zone() must never return an inverted (lo > hi) pair, got ({lo}, {hi})");
        assert!((lo - 1.0).abs() < 1e-6);
        assert!((hi - 9.0).abs() < 1e-6);
    }

    #[test]
    fn zone_default_handles_span_the_full_param_range() {
        let shape = AudioModShape::default();
        let (lo, hi) = shape.zone(-5.0, 5.0);
        assert_eq!(lo, -5.0);
        assert_eq!(hi, 5.0);
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
        // §9 U3: `trigger_mode` is `None` on an ordinary (non-gate) mod and
        // must not appear on the wire — old projects stay byte-identical.
        assert!(!json.contains("triggerMode"));
    }

    #[test]
    fn trigger_mode_round_trips_when_set() {
        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-1"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.trigger_mode = Some(crate::audio_trigger::TriggerFireMode::Both);
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("triggerMode"));
        let back: ParameterAudioMod = serde_json::from_str(&json).unwrap();
        assert_eq!(back.trigger_mode, Some(crate::audio_trigger::TriggerFireMode::Both));
    }

    // ── PARAM_STEP_ACTIONS D2/D7 pure helpers ───────────────────────────────

    #[test]
    fn default_step_amount_is_one_for_discrete_and_an_eighth_range_for_continuous() {
        assert_eq!(default_step_amount(0.0, 8.0, true), 1.0);
        assert_eq!(default_step_amount(0.0, 1.0, false), 0.125);
        assert_eq!(default_step_amount(-10.0, 10.0, false), 2.5);
    }

    #[test]
    fn wrap_mode_wrap_cycles_at_the_rails() {
        let (v, _) = WrapMode::Wrap.advance(9.0, 1.0, 1.0, 0.0, 10.0);
        assert_eq!(v, 0.0, "one step past the top of a 0..10 cycle lands on 0");
        // A continuous cycle is half-open [min, max) — max and min are the
        // SAME point (like 360° == 0° on a phase wheel), so stepping below
        // min wraps to just under max, not exactly to max. (The Step arm in
        // `manifold-playback` widens the modulus to max-min+1 for a
        // *discrete* param specifically so its true, distinct max stays
        // reachable — see `step_wrap_mode_cycles_past_max_to_min_for_
        // discrete_param` in `modulation.rs`.)
        let (v, _) = WrapMode::Wrap.advance(0.0, -1.0, 1.0, 0.0, 10.0);
        assert_eq!(v, 9.0, "one step before the bottom wraps to just under the top");
    }

    #[test]
    fn wrap_mode_clamp_never_exceeds_the_rails() {
        let (v, dir) = WrapMode::Clamp.advance(9.0, 5.0, 1.0, 0.0, 10.0);
        assert_eq!(v, 10.0);
        assert_eq!(dir, 1.0, "Clamp never flips direction");
    }

    #[test]
    fn wrap_mode_bounce_flips_direction_at_a_rail() {
        let (v, dir) = WrapMode::Bounce.advance(9.0, 5.0, 1.0, 0.0, 10.0);
        assert_eq!(v, 6.0, "overshoot by 4 reflects back from the max rail");
        assert_eq!(dir, -1.0, "direction flips after hitting the rail");
    }

    #[test]
    fn random_step_value_continuous_spans_the_full_range() {
        // No discrete_count => full-range hash, no exclusion.
        let v = random_step_value(42, 0.0, 10.0, None, 0);
        assert!((0.0..10.0).contains(&v), "got {v}");
    }

    #[test]
    fn random_step_value_discrete_excludes_the_current_index() {
        // Sweep a wide range of ordinals against every possible current
        // index on a small (N=4) discrete range — the result must never
        // equal the excluded index.
        for current_index in 0..4u32 {
            for ordinal in 0..500u32 {
                let v = random_step_value(ordinal, 0.0, 3.0, Some(4), current_index);
                let idx = v.round() as u32;
                assert_ne!(idx, current_index, "ordinal {ordinal} picked the excluded current index");
                assert!(idx < 4, "index {idx} out of range");
            }
        }
    }

    #[test]
    fn random_step_value_single_discrete_position_has_nothing_to_exclude() {
        // N=1: no other value exists to pick — degenerate but must not panic.
        let v = random_step_value(7, 5.0, 5.0, Some(1), 0);
        assert_eq!(v, 5.0);
    }
}
