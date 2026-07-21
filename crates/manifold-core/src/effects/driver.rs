//! Parameter driver (LFO) modulation (`ParameterDriver`), the shared
//! deterministic integer hash, and the beat-division helpers. Extracted
//! from effects.rs (P2-E, design D4).

use std::borrow::Cow;
use serde::{Deserialize, Serialize};
use crate::types::{BeatDivision, DriverWaveform};
use crate::units::Beats;
use super::ParamId;
use super::{default_one, default_true};

// ─── Parameter Driver (LFO) ───

/// LFO modulating a single effect or generator parameter.
///
/// Address shape: `param_id` is the canonical mapping key referenced by
/// project file storage and (by extension) any external client that
/// reads/writes saved JSON. Legacy V1 projects stored `paramIndex: i32`
/// instead — the custom [`Deserialize`] accepts either shape, parking
/// the legacy index in [`ParameterDriver::legacy_param_index`] for the
/// post-load resolver to translate via the registry.
///
/// Serialization (custom impl below): emits `paramId` when non-empty.
/// When `param_id` is empty AND `legacy_param_index` is `Some`, emits
/// `paramIndex` instead — this preserves recovery information across
/// save→load cycles when the load happened on a build whose registry
/// didn't have the effect type. See [`ParameterDriver::legacy_param_index`].
#[derive(Debug, Clone)]
pub struct ParameterDriver {
    /// Stable mapping key. After post-load resolution, every driver in
    /// memory has a non-empty `param_id`. During the brief window
    /// between `Deserialize` and the post-load pass, a legacy V1
    /// driver may have `param_id = ""` and `legacy_param_index = Some`.
    pub param_id: ParamId,
    pub beat_division: BeatDivision,
    pub waveform: DriverWaveform,
    pub enabled: bool,
    pub phase: f32,
    pub base_value: f32,
    pub trim_min: f32,
    pub trim_max: f32,
    pub reversed: bool,
    /// Free-running LFO period in beats. `None` => **sync mode** (period derives
    /// from [`beat_division`], including its dotted/triplet variants — the grid
    /// and feel segment). `Some(p)` => **free mode**: the LFO cycles every `p`
    /// beats regardless of the grid, enabling odd periods (3, 1.5, 0.375…) and
    /// polyrhythm against the bar. The type-in field writes this; clicking a grid
    /// cell or the feel segment clears it back to `None`. Serialized as
    /// `freePeriodBeats`, omitted when `None` so pre-free-mode projects round-trip
    /// unchanged.
    pub free_period_beats: Option<f32>,
    /// Parked legacy `paramIndex: i32` from V1.1 deserialization or from
    /// a load against an unregistered effect type.
    ///
    /// Set by:
    /// - Custom [`Deserialize`] when a legacy `paramIndex` field is
    ///   present and `paramId` is missing/empty.
    /// - Preserved unchanged by [`crate::project::Project::resolve_legacy_param_ids`]
    ///   when the effect type's registry def is missing
    ///   (`ResolveOutcome::RegistryMissing`).
    ///
    /// Cleared by the resolver in every other case (`Resolved` /
    /// `NoChange` / `Drop`).
    ///
    /// Re-emitted on serialize as `paramIndex` only when `param_id`
    /// is empty, completing the round-trip recovery loop: load V1.1
    /// on a build without the registry → save → reload on a build
    /// with the registry → resolver fills in `param_id` cleanly.
    ///
    /// **Invariant:** non-resolver code MUST NOT set this. Outside the
    /// `Deserialize → on_after_deserialize` window, an in-memory
    /// driver with `legacy_param_index = Some(_)` AND a non-empty
    /// `param_id` is a bug.
    pub legacy_param_index: Option<i32>,
    /// Runtime state, not serialized. Unity ParameterDriver.cs line 59.
    pub is_paused_by_user: bool,
}

// Custom Serialize: keeps the derive(Serialize) field shape but
// expresses the "emit `paramId` OR `paramIndex` (never both)" policy
// that derive can't express on its own.
impl Serialize for ParameterDriver {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let emit_param_id = !self.param_id.is_empty();
        let emit_legacy_index = !emit_param_id && self.legacy_param_index.is_some();
        let emit_free_period = self.free_period_beats.is_some();

        // 8 base fields (beat_division, waveform, enabled, phase,
        // base_value, trim_min, trim_max, reversed) + addressing field
        // + optional freePeriodBeats (only in free mode).
        let mut field_count = 8;
        if emit_param_id || emit_legacy_index {
            field_count += 1;
        }
        if emit_free_period {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("ParameterDriver", field_count)?;
        if emit_param_id {
            s.serialize_field("paramId", &self.param_id)?;
        } else if emit_legacy_index {
            // SAFETY: emit_legacy_index implies legacy_param_index.is_some().
            s.serialize_field("paramIndex", &self.legacy_param_index.unwrap())?;
        }
        s.serialize_field("beatDivision", &self.beat_division)?;
        s.serialize_field("waveform", &self.waveform)?;
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("phase", &self.phase)?;
        s.serialize_field("baseValue", &self.base_value)?;
        s.serialize_field("trimMin", &self.trim_min)?;
        s.serialize_field("trimMax", &self.trim_max)?;
        s.serialize_field("reversed", &self.reversed)?;
        if let Some(p) = self.free_period_beats {
            s.serialize_field("freePeriodBeats", &p)?;
        }
        s.end()
    }
}

impl ParameterDriver {
    pub fn new(
        param_id: impl Into<ParamId>,
        division: BeatDivision,
        waveform: DriverWaveform,
    ) -> Self {
        Self {
            param_id: param_id.into(),
            beat_division: division,
            waveform,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            free_period_beats: None,
            legacy_param_index: None,
            is_paused_by_user: false,
        }
    }

    /// Effective LFO period in beats. Free mode (`free_period_beats = Some(p)`)
    /// uses `p` directly; sync mode falls back to the `beat_division` period
    /// (which already encodes dotted/triplet via its variants).
    pub fn period_beats(&self) -> f32 {
        self.free_period_beats
            .unwrap_or_else(|| self.beat_division.beats())
    }

    /// Evaluate driver at given beat position -> [0, 1].
    /// Port of Unity DriverEvaluator.Evaluate. Sync-mode convenience: resolves
    /// the division to a period and defers to [`evaluate_with_period`].
    pub fn evaluate(
        current_beat: Beats,
        division: BeatDivision,
        waveform: DriverWaveform,
        phase_offset: f32,
    ) -> f32 {
        Self::evaluate_with_period(current_beat, division.beats(), waveform, phase_offset)
    }

    /// Evaluate the waveform at `current_beat` for an explicit `period` in beats
    /// -> [0, 1]. The shared core for both sync mode (period from the division)
    /// and free mode (period typed directly).
    pub fn evaluate_with_period(
        current_beat: Beats,
        period: f32,
        waveform: DriverWaveform,
        phase_offset: f32,
    ) -> f32 {
        if period <= 0.0 {
            return 0.5;
        }
        let beat = current_beat.as_f32();
        let p = (beat % period) / period + phase_offset;
        let phase = p - p.floor(); // wrap to [0, 1)

        match waveform {
            DriverWaveform::Sine => (phase * std::f32::consts::TAU).sin() * 0.5 + 0.5,
            DriverWaveform::Triangle => {
                if phase < 0.5 {
                    phase * 2.0
                } else {
                    2.0 - phase * 2.0
                }
            }
            DriverWaveform::Sawtooth => phase,
            DriverWaveform::Square => {
                if phase < 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            DriverWaveform::Random => {
                // Deterministic per-period hash matching Unity's HashToFloat.
                // Unity ParameterDriver.cs lines 224-236.
                let cycle = (beat / period).floor() as i32;
                hash_to_float(cycle as u32)
            }
        }
    }
}

/// Deterministic integer hash → the masked pre-normalize bits (0..=0x7FFFFF).
/// Unity's `HashToFloat` port (`ParameterDriver.cs` lines 224-236) — the
/// house random for anything that needs frame/seed-driven determinism
/// without RNG state: the same `seed` always yields the same output, so a
/// replay (e.g. offline export re-running the same fire sequence) reproduces
/// identically. Exposed separately from [`hash_to_float`] for callers that
/// want an exact integer modulo (PARAM_STEP_ACTIONS D7's discrete non-repeat
/// selection) rather than a float, avoiding float-rounding at the boundary.
pub fn hash_u32(seed: u32) -> u32 {
    let mut h = seed;
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9f3b);
    h ^= h >> 16;
    h & 0x7FFFFF
}

/// [`hash_u32`] normalized to `[0, 1)`.
pub fn hash_to_float(seed: u32) -> f32 {
    hash_u32(seed) as f32 / 0x7FFFFF as f32
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    #[test]
    fn hash_to_float_is_a_pure_function_of_seed() {
        // Same seed → same output, always (determinism is the whole point:
        // PARAM_STEP_ACTIONS D7 leans on this for offline-export reproducibility).
        for seed in [0u32, 1, 7, 12345, u32::MAX] {
            assert_eq!(hash_to_float(seed), hash_to_float(seed));
            assert_eq!(hash_u32(seed), hash_u32(seed));
        }
    }

    #[test]
    fn hash_to_float_stays_in_unit_range() {
        for seed in 0..2000u32 {
            let f = hash_to_float(seed);
            assert!((0.0..1.0).contains(&f), "seed {seed} produced out-of-range {f}");
        }
    }

    #[test]
    fn driver_random_waveform_still_uses_the_shared_hash() {
        // Pins the extraction didn't change DriverWaveform::Random's output —
        // same cycle index, same value as calling hash_to_float directly.
        let period = 4.0;
        let cycle = 3i32;
        let beat = Beats((cycle as f32 * period) as f64);
        let v = ParameterDriver::evaluate_with_period(beat, period, DriverWaveform::Random, 0.0);
        assert_eq!(v, hash_to_float(cycle as u32));
    }
}

// Custom `Deserialize` accepting both V1.1 (`paramIndex: i32`) and V1.2+
// (`paramId: "amount"`) project file shapes. The runtime always reads
// `param_id`; legacy projects park the index in `legacy_param_index`
// for the post-load resolver to translate. See
// `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 8.
impl<'de> Deserialize<'de> for ParameterDriver {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Mirror struct with both shapes accepted. `param_id` and
        // `param_index` are both optional — the driver must carry one
        // or the other. If both are present, `param_id` wins (forward
        // migration takes precedence over legacy index).
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default)]
            param_id: Option<String>,
            #[serde(default)]
            param_index: Option<i32>,
            #[serde(default)]
            beat_division: BeatDivision,
            #[serde(default)]
            waveform: DriverWaveform,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            phase: f32,
            #[serde(default)]
            base_value: f32,
            #[serde(default)]
            trim_min: f32,
            #[serde(default = "default_one")]
            trim_max: f32,
            #[serde(default)]
            reversed: bool,
            #[serde(default)]
            free_period_beats: Option<f32>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let (param_id, legacy_param_index) = match (raw.param_id, raw.param_index) {
            // Canonical V1.2+ shape — param_id present and non-empty.
            (Some(id), _) if !id.is_empty() => (Cow::Owned(id), None),
            // Legacy V1.1 shape — only paramIndex present. Park for
            // post-load resolution.
            (_, Some(idx)) => (Cow::Borrowed(""), Some(idx)),
            // Round-tripped shape from a project saved before the
            // post-load resolver could fill in `param_id` (e.g. test
            // harness without effect registry, or a future case where
            // the effect type was unregistered at save time). Treat
            // as "unresolvable" rather than erroring — driver stays
            // present but inert until the registry has the metadata
            // again. Better than refusing to load the project at all.
            (_, None) => (Cow::Borrowed(""), None),
        };
        Ok(ParameterDriver {
            param_id,
            beat_division: raw.beat_division,
            waveform: raw.waveform,
            enabled: raw.enabled,
            phase: raw.phase,
            base_value: raw.base_value,
            trim_min: raw.trim_min,
            trim_max: raw.trim_max,
            reversed: raw.reversed,
            free_period_beats: raw.free_period_beats,
            legacy_param_index,
            is_paused_by_user: false,
        })
    }
}

// ─── BeatDivision helpers ───

/// Constants matching Unity BeatDivisionHelper.
pub mod beat_division_helper {
    use crate::types::BeatDivision;

    pub const STRAIGHT_COUNT: usize = 11;
    pub const DOTTED_COUNT: usize = 5;
    pub const TRIPLET_COUNT: usize = 4;
    pub const TOTAL_COUNT: usize = 20;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BeatModifier {
        None,
        Dotted,
        Triplet,
    }

    /// Display label for a beat division. Unity BeatDivisionHelper.ToLabel.
    pub fn to_label(div: BeatDivision) -> &'static str {
        match div {
            BeatDivision::ThirtySecond => "1/32",
            BeatDivision::Sixteenth => "1/16",
            BeatDivision::Eighth => "1/8",
            BeatDivision::Quarter => "1/4",
            BeatDivision::Half => "1/2",
            BeatDivision::Whole => "1/1",
            BeatDivision::TwoWhole => "2/1",
            BeatDivision::FourWhole => "4/1",
            BeatDivision::EightWhole => "8/1",
            BeatDivision::SixteenWhole => "16/1",
            BeatDivision::ThirtyTwoWhole => "32/1",
            BeatDivision::EighthDotted => "1/8.",
            BeatDivision::QuarterDotted => "1/4.",
            BeatDivision::HalfDotted => "1/2.",
            BeatDivision::WholeDotted => "1/1.",
            BeatDivision::TwoWholeDotted => "2/1.",
            BeatDivision::EighthTriplet => "1/8T",
            BeatDivision::QuarterTriplet => "1/4T",
            BeatDivision::HalfTriplet => "1/2T",
            BeatDivision::WholeTriplet => "1/1T",
        }
    }

    /// Decompose a BeatDivision into its straight base index (0-10) and modifier.
    /// Unity BeatDivisionHelper.Decompose lines 158-164.
    pub fn decompose(div: BeatDivision) -> (usize, BeatModifier) {
        let val = div as i32;
        if val >= 16 {
            ((val - 14) as usize, BeatModifier::Triplet)
        } else if val >= 11 {
            ((val - 9) as usize, BeatModifier::Dotted)
        } else {
            (val as usize, BeatModifier::None)
        }
    }

    /// Compose a straight base index + modifier into a BeatDivision.
    /// Returns None if the combination is invalid.
    /// Unity BeatDivisionHelper.TryCompose lines 170-184.
    pub fn try_compose(base_index: usize, modifier: BeatModifier) -> Option<BeatDivision> {
        match modifier {
            BeatModifier::Dotted if (2..=6).contains(&base_index) => {
                BeatDivision::from_i32((base_index + 9) as i32)
            }
            BeatModifier::Triplet if (2..=5).contains(&base_index) => {
                BeatDivision::from_i32((base_index + 14) as i32)
            }
            BeatModifier::None => BeatDivision::from_i32(base_index as i32),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::*;
    use crate::units::Beats;

    #[test]
    fn test_driver_sine() {
        let val =
            ParameterDriver::evaluate(Beats(0.0), BeatDivision::Quarter, DriverWaveform::Sine, 0.0);
        assert!((val - 0.5).abs() < 0.01);

        let val = ParameterDriver::evaluate(
            Beats(0.25),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
            0.0,
        );
        assert!((val - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_driver_square() {
        let val = ParameterDriver::evaluate(
            Beats(0.1),
            BeatDivision::Quarter,
            DriverWaveform::Square,
            0.0,
        );
        assert_eq!(val, 1.0);

        let val = ParameterDriver::evaluate(
            Beats(0.6),
            BeatDivision::Quarter,
            DriverWaveform::Square,
            0.0,
        );
        assert_eq!(val, 0.0);
    }

    #[test]
    fn test_driver_random_hash_matches_unity() {
        let val = ParameterDriver::evaluate(
            Beats(1.0),
            BeatDivision::Quarter,
            DriverWaveform::Random,
            0.0,
        );
        assert!((0.0..=1.0).contains(&val));
        // Same cycle should give same value
        let val2 = ParameterDriver::evaluate(
            Beats(1.5),
            BeatDivision::Quarter,
            DriverWaveform::Random,
            0.0,
        );
        assert_eq!(val, val2);
    }

    // ── ParameterDriver backward-compat Deserialize (step 8) ──────

    #[test]
    fn driver_deserialize_legacy_param_index() {
        // V1.1.0 shape: { paramIndex: 1, ... }. The custom Deserialize
        // parks the index in `legacy_param_index` and leaves
        // `param_id` empty. The post-load resolver fills `param_id`
        // later — this test only covers the Deserialize step.
        let json = r#"{
            "paramIndex": 2,
            "beatDivision": 4,
            "waveform": 0,
            "enabled": true,
            "phase": 0.0,
            "baseValue": 0.0,
            "trimMin": 0.0,
            "trimMax": 1.0,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert!(
            d.param_id.is_empty(),
            "legacy shape must leave param_id empty until post-load resolution"
        );
        assert_eq!(d.legacy_param_index, Some(2));
        assert_eq!(d.beat_division, BeatDivision::Half);
    }

    #[test]
    fn driver_deserialize_canonical_param_id() {
        // V1.2+ shape: { paramId: "amount", ... }. No post-load
        // resolution needed — `param_id` is already set, and
        // `legacy_param_index` stays None.
        let json = r#"{
            "paramId": "amount",
            "beatDivision": 5,
            "waveform": 1,
            "enabled": true,
            "phase": 0.5,
            "baseValue": 0.0,
            "trimMin": 0.1,
            "trimMax": 0.9,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "amount");
        assert_eq!(d.legacy_param_index, None);
        assert_eq!(d.beat_division, BeatDivision::Whole);
        assert!((d.phase - 0.5).abs() < 1e-6);
    }

    #[test]
    fn driver_deserialize_param_id_wins_when_both_present() {
        // If both fields are sent (forward-migration test fixtures or
        // a transitional save shape), `param_id` is canonical and
        // `param_index` is ignored. No legacy resolution scheduled.
        let json = r#"{
            "paramId": "threshold",
            "paramIndex": 99,
            "beatDivision": 3,
            "waveform": 0
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "threshold");
        assert_eq!(d.legacy_param_index, None);
    }

    #[test]
    fn driver_deserialize_missing_both_loads_as_unresolvable() {
        // No paramId, no paramIndex — load doesn't error; the driver
        // stays present but inert. Better than refusing the entire
        // project. Real recovery path is the post-load resolver, but
        // there's nothing for it to do here.
        let json = r#"{
            "beatDivision": 4
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "");
        assert_eq!(d.legacy_param_index, None);
    }

    #[test]
    fn driver_serialize_writes_param_id_only() {
        // After step 8, saved files always carry the new shape. The
        // legacy `paramIndex` field is never written (skipped via
        // custom Deserialize / derived Serialize on the canonical
        // field set).
        let driver = ParameterDriver::new("amount", BeatDivision::Half, DriverWaveform::Triangle);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(json.contains("\"paramId\":\"amount\""));
        assert!(
            !json.contains("paramIndex"),
            "Serialize must not write legacy paramIndex field; got: {json}"
        );
        assert!(
            !json.contains("legacyParamIndex"),
            "Serialize must not leak the runtime-only legacy_param_index field; got: {json}"
        );
    }

    #[test]
    fn driver_round_trips_through_canonical_shape() {
        let driver =
            ParameterDriver::new("threshold", BeatDivision::FourWhole, DriverWaveform::Square);
        let json = serde_json::to_string(&driver).unwrap();
        let back: ParameterDriver = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_id, driver.param_id);
        assert_eq!(back.beat_division, driver.beat_division);
        assert_eq!(back.waveform, driver.waveform);
        assert_eq!(back.legacy_param_index, None);
    }

    #[test]
    fn driver_sync_mode_omits_free_period_field() {
        // Sync mode (the default) must not write freePeriodBeats — pre-free-mode
        // projects round-trip byte-identically and stay tiny.
        let driver = ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sine);
        assert_eq!(driver.free_period_beats, None);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(
            !json.contains("freePeriodBeats"),
            "sync-mode driver must not emit freePeriodBeats; got: {json}"
        );
    }

    #[test]
    fn driver_free_period_round_trips() {
        let mut driver =
            ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sine);
        driver.free_period_beats = Some(3.0);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(json.contains("\"freePeriodBeats\":3"), "got: {json}");
        let back: ParameterDriver = serde_json::from_str(&json).unwrap();
        assert_eq!(back.free_period_beats, Some(3.0));
    }

    #[test]
    fn driver_legacy_json_loads_as_sync_mode() {
        // A project saved before free mode existed has no freePeriodBeats key.
        let json = r#"{
            "paramId": "amount",
            "beatDivision": 3,
            "waveform": 0,
            "enabled": true,
            "phase": 0.0,
            "baseValue": 0.0,
            "trimMin": 0.0,
            "trimMax": 1.0,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.free_period_beats, None, "legacy driver must default to sync mode");
        assert_eq!(d.period_beats(), BeatDivision::Quarter.beats());
    }

    #[test]
    fn free_period_overrides_division_for_evaluation() {
        // period_beats() prefers the free period; evaluate_with_period cycles on it.
        let mut d = ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sawtooth);
        d.free_period_beats = Some(3.0);
        assert_eq!(d.period_beats(), 3.0);
        // Sawtooth = phase; at beat 0 phase 0, at beat 1.5 phase 0.5 over a 3-beat period.
        let v0 = ParameterDriver::evaluate_with_period(
            Beats(0.0),
            d.period_beats(),
            d.waveform,
            d.phase,
        );
        let v_half = ParameterDriver::evaluate_with_period(
            Beats(1.5),
            d.period_beats(),
            d.waveform,
            d.phase,
        );
        assert!(v0.abs() < 1e-6, "phase 0 at beat 0");
        assert!((v_half - 0.5).abs() < 1e-6, "half phase at beat 1.5 over 3-beat period");
    }

}
