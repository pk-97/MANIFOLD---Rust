//! Clip-triggered decay envelope modulation (`ParamEnvelope`). Extracted
//! from effects.rs (P2-E, design D4).

use std::borrow::Cow;
use serde::{Deserialize, Serialize};
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
    /// edge.
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
    /// Cached decay output (0-1) for UI display. Not serialized.
    pub current_level: f32,
    /// Rising edge detection: was a clip active on the previous frame?
    pub was_clip_active: bool,
}

impl Serialize for ParamEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let emit_param_id = !self.param_id.is_empty();
        let emit_legacy_index = !emit_param_id && self.legacy_param_index.is_some();

        // 3 base fields (enabled, targetNormalized, decayBeats) + addressing
        // field (paramId XOR targetParamIndex).
        let mut field_count = 3;
        if emit_param_id || emit_legacy_index {
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
            current_level: 0.0,
            was_clip_active: false,
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
// `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 9.
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
            current_level: 0.0,
            was_clip_active: false,
        })
    }
}

fn default_decay_beats() -> f32 {
    DEFAULT_ENVELOPE_DECAY_BEATS
}
