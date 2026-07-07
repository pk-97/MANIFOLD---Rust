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
use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
use crate::id::LayerId;
use crate::units::Beats;

/// Transient threshold at sensitivity 0 — only the strongest impulses fire.
const MAX_TRIGGER_THRESHOLD: f32 = 0.9;
/// Transient threshold at sensitivity 1 — fire on almost anything, but never 0
/// (a 0 threshold would fire on the detector's noise floor every block).
const MIN_TRIGGER_THRESHOLD: f32 = 0.05;
/// Default one-shot length (beats) — a one-beat flash; the user tunes per route.
const DEFAULT_ONE_SHOT_BEATS: f64 = 1.0;

fn default_true() -> bool {
    true
}

/// Map a 0..1 sensitivity slider to a transient fire threshold. Shared by
/// [`TriggerRoute`] (clip-launch routes) and the legacy `AudioTriggerMod`
/// wire shape ([`LegacyAudioTriggerMod`], migrated onto `ParameterAudioMod`
/// since §9) so both surfaces felt identical to tune. Inverted: sensitivity
/// 1.0 → [`MIN_TRIGGER_THRESHOLD`] (fire easily); 0.0 → [`MAX_TRIGGER_THRESHOLD`]
/// (only the strongest onsets).
pub fn sensitivity_to_threshold(sensitivity: f32) -> f32 {
    let s = sensitivity.clamp(0.0, 1.0);
    MIN_TRIGGER_THRESHOLD + (1.0 - s) * (MAX_TRIGGER_THRESHOLD - MIN_TRIGGER_THRESHOLD)
}

/// Re-arm hysteresis for [`TransientEdge`]: a fired edge re-arms once its
/// level drops below `threshold * REARM_RATIO`. Below 1.0 so a noisy plateau
/// just above threshold doesn't chatter; well above 0 so the edge re-arms
/// promptly between onsets. Shared by the live clip-trigger evaluator
/// (`manifold-playback::live_trigger::LiveTriggerState`) and the §8 param-
/// trigger evaluator.
pub const REARM_RATIO: f32 = 0.6;

/// Pure armed/re-arm edge detector for a transient impulse crossing a
/// threshold: fires once on the rising edge, then only re-arms once the level
/// falls back below `threshold * REARM_RATIO`. Tempo-independent, runtime-only
/// (never serialized) — extracted from `LiveTriggerState`'s per-route armed
/// flag (D4) so both the clip-trigger evaluator (keyed by send×band) and the
/// `ParameterAudioMod` trigger-gate evaluator (keyed by mod, §9 U1) share one
/// implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransientEdge {
    armed: bool,
}

impl Default for TransientEdge {
    fn default() -> Self {
        Self { armed: true }
    }
}

impl TransientEdge {
    /// Advance one tick: true iff `level` crosses `threshold` on this armed
    /// edge (a fire). Re-arms internally once `level` decays below
    /// `threshold * REARM_RATIO`.
    pub fn advance(&mut self, level: f32, threshold: f32) -> bool {
        if self.armed && level > threshold {
            self.armed = false;
            true
        } else {
            if !self.armed && level < threshold * REARM_RATIO {
                self.armed = true;
            }
            false
        }
    }

    /// Drop to armed — call on transport stop / project reset so a stale
    /// "fired, not yet re-armed" flag can't suppress the first onset next time
    /// (BUG-051).
    pub fn clear(&mut self) {
        self.armed = true;
    }
}

/// Which events increment a generator's/effect's Trigger response while audio
/// triggers are enabled on it (§8 D1/D2). Peter, 2026-07-07: *"if Trigger is
/// enabled we can choose if we want rising clip edge (default) OR the
/// transient trigger OR both."*
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TriggerFireMode {
    /// Only the layer's clip-launch edge counts (today's behavior, unchanged).
    #[default]
    ClipEdge,
    /// Only audio transients count — clip launches are silently ignored for
    /// this instance's trigger response (a mode a user can forget; the drawer
    /// surfaces it on the collapsed card row).
    Transient,
    /// Both the clip edge and audio transients count.
    Both,
}

impl TriggerFireMode {
    /// True if this mode counts the layer's clip-launch edge.
    pub fn wants_clip_edge(self) -> bool {
        matches!(self, TriggerFireMode::ClipEdge | TriggerFireMode::Both)
    }

    /// True if this mode counts audio transients.
    pub fn wants_transient(self) -> bool {
        matches!(self, TriggerFireMode::Transient | TriggerFireMode::Both)
    }
}

/// Deserialize-only wire shape for the pre-§9 `audioTrigger` field (§8 D2:
/// `AudioTriggerMod`, a per-instance config parallel to `audio_mods` — deleted
/// 2026-07-07 per §9 U1, the day after it shipped, because the parallel
/// config type forced every gate/walker/drawer/command to know about two
/// things instead of one). Kept only so an old project's `audioTrigger` field
/// load-migrates onto a `ParameterAudioMod` on the instance's trigger-gate
/// param — see `effects::migrate_legacy_audio_trigger`. Never constructed
/// fresh, never serialized.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyAudioTriggerMod {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub source: AudioModSource,
    /// 0..1. High sensitivity = low transient threshold (more onsets fire).
    /// Migrates onto `AudioModShape.sensitivity` (U5 — a rough approximation,
    /// not exact-feel; the field existed in ~one project for one day).
    pub sensitivity: f32,
    #[serde(default)]
    pub mode: TriggerFireMode,
}

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
    /// How long the fired one-shot clip holds. A transient has no note-off, so
    /// the fire length is fixed here rather than by a release event.
    pub one_shot_beats: Beats,
}

impl TriggerRoute {
    /// A new route reading `source`, disabled by default (the user enables a row
    /// once they've pointed it at a layer), mid sensitivity. Fires snap to the
    /// project quantize grid (the same setting MIDI clip-launch uses).
    pub fn new(source: AudioBand) -> Self {
        Self {
            enabled: false,
            source,
            target_layer: None,
            sensitivity: 0.5,
            one_shot_beats: Beats(DEFAULT_ONE_SHOT_BEATS),
        }
    }

    /// Map the 0..1 sensitivity slider to the transient fire threshold
    /// (shared mapping — see [`sensitivity_to_threshold`]).
    pub fn threshold(&self) -> f32 {
        sensitivity_to_threshold(self.sensitivity)
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

    #[test]
    fn transient_edge_fires_once_then_rearms_below_ratio() {
        let mut edge = TransientEdge::default();
        assert!(edge.advance(0.6, 0.5)); // rising edge
        assert!(!edge.advance(0.6, 0.5)); // still hot, no re-fire
        assert!(!edge.advance(0.31, 0.5)); // above rearm floor (0.3), stays disarmed
        assert!(!edge.advance(0.29, 0.5)); // below rearm floor, rearms (no fire on the dip)
        assert!(edge.advance(0.6, 0.5)); // fires again
    }

    #[test]
    fn transient_edge_clear_forces_rearm() {
        let mut edge = TransientEdge::default();
        assert!(edge.advance(0.9, 0.5));
        assert!(!edge.advance(0.9, 0.5)); // disarmed
        edge.clear();
        assert!(edge.advance(0.9, 0.5)); // re-armed by clear()
    }

    #[test]
    fn trigger_fire_mode_wants() {
        assert!(TriggerFireMode::ClipEdge.wants_clip_edge());
        assert!(!TriggerFireMode::ClipEdge.wants_transient());
        assert!(!TriggerFireMode::Transient.wants_clip_edge());
        assert!(TriggerFireMode::Transient.wants_transient());
        assert!(TriggerFireMode::Both.wants_clip_edge());
        assert!(TriggerFireMode::Both.wants_transient());
        assert_eq!(TriggerFireMode::default(), TriggerFireMode::ClipEdge);
    }

    // The `disabled_config_never_suppresses_clip_edge` regression and the
    // `AudioTriggerMod` threshold/serde tests moved with the type: §9 U1
    // deletes `AudioTriggerMod` (a fire-mode config is now a normal
    // `ParameterAudioMod`), so the "disabled means absent" proof now lives on
    // `PresetInstance::clip_edge_enabled()` in `effects.rs`
    // (`clip_edge_enabled_matrix`), and the threshold mapping is still
    // exercised by `TriggerRoute`'s own tests above (same shared
    // `sensitivity_to_threshold`).

    #[test]
    fn legacy_audio_trigger_mod_deserializes_the_pre_unification_wire_shape() {
        // U5 migration source: the exact `audioTrigger` blob a project saved
        // during the one day §8's `AudioTriggerMod` shipped. Proves
        // `LegacyAudioTriggerMod` still parses it so
        // `effects::migrate_legacy_audio_trigger` has something to convert.
        let json = r#"{
            "enabled": false,
            "source": {
                "sendId": "e14b42f8",
                "feature": { "kind": "transients", "band": "full" }
            },
            "sensitivity": 1.0,
            "mode": "transient"
        }"#;
        let legacy: LegacyAudioTriggerMod = serde_json::from_str(json).unwrap();
        assert!(!legacy.enabled);
        assert_eq!(legacy.source.send_id, crate::id::AudioSendId::new("e14b42f8"));
        assert_eq!(
            legacy.source.feature,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full)
        );
        assert_eq!(legacy.sensitivity, 1.0);
        assert_eq!(legacy.mode, TriggerFireMode::Transient);
    }
}
