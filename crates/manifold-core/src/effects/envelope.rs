//! Clip-triggered decay envelope modulation (`ParamEnvelope`). Extracted
//! from effects.rs (P2-E, design D4).

use std::borrow::Cow;
use serde::{Deserialize, Serialize};
use crate::audio_mod::TriggerAction;
use crate::units::Beats;
use super::ParamId;
use super::{default_one, default_true};

// ─── Param Envelope (triggered decay modulation) ───

/// Default decay time (beats) for a freshly-created envelope, so it modulates
/// usefully the moment it's armed. Tempo-synced because it's in beats.
pub const DEFAULT_ENVELOPE_DECAY_BEATS: f32 = 1.0;

/// Clip-triggered decay envelope modulating a single effect or generator
/// parameter.
///
/// Address shape: `param_id` is the canonical mapping key, mirroring
/// [`ParameterDriver`]. Legacy V1.1 projects stored `targetParamIndex:
/// i32` instead — the custom [`Deserialize`] accepts either shape and
/// parks legacy indices in [`ParamEnvelope::legacy_param_index`] for
/// the post-load resolver.
///
/// Serialization (custom impl below): emits `paramId` when non-empty,
/// else `targetParamIndex` when `legacy_param_index` is `Some`. Mirrors
/// the ParameterDriver round-trip recovery contract.
#[derive(Debug, Clone)]
pub struct ParamEnvelope {
    /// Stable mapping key. Empty after legacy V1.1 deserialization
    /// until the post-load resolver fills it in from the registry.
    ///
    /// Envelope-home unification (v1.6): an envelope lives **on its
    /// owning `PresetInstance`** (effect or generator), so it no longer
    /// carries a `target_effect_type` — the instance it sits on *is* the
    /// target. Pre-v1.6 projects stored effect envelopes on
    /// `Layer.envelopes` / `Clip.envelopes` keyed by `targetEffectType`;
    /// the v1.5→v1.6 load migration distributes each into the matching
    /// effect instance and drops the now-redundant key.
    pub param_id: ParamId,
    pub enabled: bool,
    /// The envelope's target (the orange handle on the slider track): the
    /// normalized 0-1 position the parameter is pulled toward on a clip's rising
    /// edge. Meaningful only in `Continuous` mode — step/random actions hide the
    /// handle because they advance the base value on each rising edge instead.
    pub target_normalized: f32,
    /// Decay time in beats — how long the value takes to fall back to its base
    /// after a trigger. The single ADSR stage kept (attack/sustain/release were
    /// dropped as not useful); editable per envelope via the card's one slider.
    pub decay_beats: f32,
    /// Parked legacy `targetParamIndex: i32` from V1.1 deserialization
    /// or RegistryMissing fallback during post-load resolution. See
    /// [`ParameterDriver::legacy_param_index`] for the recovery
    /// invariant — same contract here.
    pub legacy_param_index: Option<i32>,
    /// PARAM_STEP_ACTIONS D8: what a clip rising edge does to the target param.
    /// `Continuous` (default) is the existing decay-envelope behavior; `Step` moves
    /// the param by `amount` with a `WrapMode`; `Random` jumps to a deterministic
    /// pseudo-random value in the param's range. Serialized only when non-default
    /// so old projects stay byte-identical.
    pub action: TriggerAction,
    /// Cached decay output (0-1) for UI display. Not serialized.
    pub current_level: f32,
    /// Rising edge detection: was a clip active on the previous frame?
    pub was_clip_active: bool,
    /// Rising edge detection: the elapsed-into-clip value on the previous frame,
    /// so a loop restart (elapsed resets while the clip stays active) is detected
    /// as a new trigger. Not serialized.
    pub prev_active_elapsed: Beats,
    /// PARAM_STEP_ACTIONS D4: monotonic fire counter for `Random` — the value
    /// sequence is deterministic by this ordinal so export reproduces identically.
    /// Not serialized; reset on load/transport stop.
    pub fire_count: u32,
    /// PARAM_STEP_ACTIONS D4: the stepped/randomized value that *replaces* the
    /// param's base for this tick. `None` until the first fire; dropped on
    /// transport stop so the param falls back to its committed base. Not serialized.
    pub step_value: Option<f32>,
    /// `WrapMode::Bounce`'s running ping-pong sign (±1, D2). Not serialized.
    pub step_dir: f32,
}

impl Serialize for ParamEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let emit_param_id = !self.param_id.is_empty();
        let emit_legacy_index = !emit_param_id && self.legacy_param_index.is_some();
        let emit_action = !is_continuous_action(&self.action);

        // 3 base fields (enabled, targetNormalized, decayBeats) + addressing
        // field (paramId XOR targetParamIndex) + action field when non-default.
        let mut field_count = 3;
        if emit_param_id || emit_legacy_index {
            field_count += 1;
        }
        if emit_action {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("ParamEnvelope", field_count)?;
        if emit_param_id {
            s.serialize_field("paramId", &self.param_id)?;
        } else if emit_legacy_index {
            s.serialize_field("targetParamIndex", &self.legacy_param_index.unwrap())?;
        }
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("targetNormalized", &self.target_normalized)?;
        s.serialize_field("decayBeats", &self.decay_beats)?;
        if emit_action {
            s.serialize_field("action", &self.action)?;
        }
        s.end()
    }
}

impl ParamEnvelope {
    /// Construct an envelope targeting `param_id` on the instance it will be
    /// attached to. Since envelope-home unification an envelope no longer
    /// distinguishes effect from generator — the `PresetInstance` it lives on
    /// is the target — so this is the single constructor for both kinds.
    pub fn new(param_id: impl Into<ParamId>) -> Self {
        Self {
            param_id: param_id.into(),
            enabled: true,
            target_normalized: 1.0,
            decay_beats: DEFAULT_ENVELOPE_DECAY_BEATS,
            legacy_param_index: None,
            action: TriggerAction::Continuous,
            current_level: 0.0,
            was_clip_active: false,
            prev_active_elapsed: Beats(-1.0),
            fire_count: 0,
            step_value: None,
            step_dir: 1.0,
        }
    }

    /// Triggered decay level [0, 1] at `local_beat` into the active clip: 1.0 at
    /// the rising edge, falling linearly to 0 over `decay_beats`, then held at 0.
    /// The single envelope shape after the ADSR/Random simplification — depth is
    /// the per-envelope `target_normalized` (the orange target handle).
    pub fn decay_level(local_beat: Beats, decay_beats: f32) -> f32 {
        if local_beat < Beats::ZERO || decay_beats <= 0.0 {
            return 0.0;
        }
        (1.0 - local_beat.as_f32() / decay_beats).clamp(0.0, 1.0)
    }
}

// Custom `Deserialize` accepting both V1.1 (`targetParamIndex: i32`)
// and V1.2+ (`paramId: "amount"`) project file shapes. Mirrors the
// `ParameterDriver` impl above. See
// `docs/EFFECT_RUNTIME_UNIFICATION.md` section 7 step 9.
impl<'de> Deserialize<'de> for ParamEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            // `targetEffectType` from pre-v1.6 files is intentionally not read
            // here — the v1.5→v1.6 migration consumes it to place the envelope
            // on the right instance, and serde ignores the leftover key.
            //
            // The dropped ADSR/Random keys (`attackBeats`, `sustainLevel`,
            // `releaseBeats`, `mode`, `randomJump`, `rangeMin`, `rangeMax`) are
            // not read — serde ignores them, so an old ADSR or Random envelope
            // loads as a plain decay envelope keeping its depth + decay time.
            #[serde(default)]
            param_id: Option<String>,
            #[serde(default, rename = "targetParamIndex")]
            param_index: Option<i32>,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default = "default_one")]
            target_normalized: f32,
            #[serde(default = "default_decay_beats")]
            decay_beats: f32,
            #[serde(default)]
            action: TriggerAction,
        }

        let raw = Raw::deserialize(deserializer)?;
        let (param_id, legacy_param_index) = match (raw.param_id, raw.param_index) {
            (Some(id), _) if !id.is_empty() => (Cow::Owned(id), None),
            (_, Some(idx)) => (Cow::Borrowed(""), Some(idx)),
            (_, None) => (Cow::Borrowed(""), None),
        };
        Ok(ParamEnvelope {
            param_id,
            enabled: raw.enabled,
            target_normalized: raw.target_normalized,
            decay_beats: raw.decay_beats,
            legacy_param_index,
            action: raw.action,
            current_level: 0.0,
            was_clip_active: false,
            prev_active_elapsed: Beats(-1.0),
            fire_count: 0,
            step_value: None,
            step_dir: 1.0,
        })
    }
}

fn default_decay_beats() -> f32 {
    DEFAULT_ENVELOPE_DECAY_BEATS
}

fn is_continuous_action(action: &TriggerAction) -> bool {
    matches!(action, TriggerAction::Continuous)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_mod::WrapMode;

    // ── ParamEnvelope backward-compat Deserialize (step 9) ──────

    #[test]
    fn envelope_deserialize_legacy_param_index() {
        // V1.1 shape: { targetEffectType, targetParamIndex: 1, ... }. The
        // leftover targetEffectType is ignored (the v1.5→v1.6 migration
        // consumes it to place the envelope on the right instance).
        let json = r#"{
            "targetEffectType": "Bloom",
            "targetParamIndex": 0,
            "enabled": true,
            "attackBeats": 0.25,
            "decayBeats": 0.25,
            "sustainLevel": 0.5,
            "releaseBeats": 0.25,
            "targetNormalized": 1.0
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert!(e.param_id.is_empty());
        assert_eq!(e.legacy_param_index, Some(0));
    }

    #[test]
    fn envelope_deserialize_canonical_param_id() {
        // Legacy ADSR keys (attackBeats etc.) are ignored post-simplification —
        // the envelope loads as a plain decay envelope keeping only its depth.
        let json = r#"{
            "paramId": "amount",
            "enabled": true,
            "attackBeats": 0.5,
            "targetNormalized": 0.7
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.param_id, "amount");
        assert_eq!(e.legacy_param_index, None);
        assert!((e.target_normalized - 0.7).abs() < 1e-6);
    }

    #[test]
    fn envelope_deserialize_param_id_wins_when_both_present() {
        let json = r#"{
            "paramId": "threshold",
            "targetParamIndex": 99,
            "enabled": true
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.param_id, "threshold");
        assert_eq!(e.legacy_param_index, None);
    }

    #[test]
    fn envelope_serialize_writes_param_id_only() {
        let env = ParamEnvelope::new("amount");
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"paramId\":\"amount\""));
        assert!(
            !json.contains("targetParamIndex"),
            "Serialize must not write legacy targetParamIndex; got: {json}"
        );
        assert!(!json.contains("legacyParamIndex"));
        assert!(
            !json.contains("targetEffectType"),
            "Serialize must not write targetEffectType post-unification; got: {json}"
        );
    }

    #[test]
    fn envelope_round_trips_through_canonical_shape() {
        let env = ParamEnvelope::new("amount");
        let json = serde_json::to_string(&env).unwrap();
        let back: ParamEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_id, env.param_id);
        assert_eq!(back.legacy_param_index, None);
        assert_eq!(back.action, TriggerAction::Continuous);
    }

    #[test]
    fn envelope_serialize_skips_action_when_continuous() {
        let env = ParamEnvelope::new("amount");
        let json = serde_json::to_string(&env).unwrap();
        assert!(
            !json.contains("\"action\""),
            "Continuous action must stay off the wire; got: {json}"
        );
    }

    #[test]
    fn envelope_action_round_trips_step_and_random() {
        for action in [
            TriggerAction::Step { amount: 2.0, wrap: WrapMode::Bounce },
            TriggerAction::Random,
        ] {
            let mut env = ParamEnvelope::new("amount");
            env.action = action;
            let json = serde_json::to_string(&env).unwrap();
            assert!(json.contains("\"action\""), "non-Continuous action must serialize; got: {json}");
            let back: ParamEnvelope = serde_json::from_str(&json).unwrap();
            assert_eq!(back.action, action, "action round-trip failed for {action:?}");
            // Runtime-only stepping state must never round-trip.
            assert_eq!(back.step_value, None);
            assert_eq!(back.step_dir, 1.0);
            assert_eq!(back.fire_count, 0);
        }
    }

    #[test]
    fn envelope_old_project_without_action_loads_as_continuous() {
        let json = r#"{
            "paramId": "amount",
            "enabled": true,
            "targetNormalized": 0.8,
            "decayBeats": 0.5
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.action, TriggerAction::Continuous);
        assert!((e.target_normalized - 0.8).abs() < 1e-6);
        assert!((e.decay_beats - 0.5).abs() < 1e-6);
    }

}
