//! Live audio trigger evaluator — turns per-send transient impulses into
//! one-shot clip fires, in real time, with no lookahead.
//!
//! Each content tick the engine hands this the latest [`AudioFeatureSnapshot`]
//! and the project's [`AudioSetup`]. For every enabled [`TriggerRoute`]
//! (`manifold_core::audio_trigger`) it edge-detects the route's transient and
//! emits a [`FireRequest`]; the engine resolves the request's target layer and
//! calls the one-shot sink. See `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md`.
//!
//! **Why this is just edge detection:** the upstream transient detector already
//! emits one decaying impulse per onset and holds its own ~106 ms refractory
//! (the audio-modulation onset detector). So this layer needs no time- or
//! beat-based refractory of its own — it only has to avoid re-firing on the
//! *same* impulse's decay. It does that with a per-route armed flag: fire on the
//! rising edge above the route threshold, then re-arm only once the impulse
//! falls back below `threshold * REARM_RATIO`. Tempo-independent and pure.

use ahash::AHashMap;

use manifold_core::audio_features::AudioFeatureSnapshot;
use manifold_core::audio_setup::AudioSetup;
use manifold_core::id::{AudioSendId, LayerId};
use manifold_core::units::Beats;

/// Re-arm hysteresis: a fired route re-arms once its transient drops below
/// `threshold * REARM_RATIO`. Below 1.0 so a noisy plateau just above threshold
/// doesn't chatter; well above 0 so the route re-arms promptly between onsets.
const REARM_RATIO: f32 = 0.6;

/// One decided fire: a route crossed its threshold this tick. The engine
/// resolves `target_layer` (or auto-routes by `send_label` when `None`) to a
/// layer index and fires a one-shot of `one_shot_beats`.
#[derive(Debug, Clone, PartialEq)]
pub struct FireRequest {
    /// Label of the send that fired — used to auto-route by name when
    /// `target_layer` is `None` (a "Kick" send finds a "Kick" layer).
    pub send_label: String,
    /// Explicit target layer, or `None` to auto-route by `send_label`.
    pub target_layer: Option<LayerId>,
    /// How long the fired one-shot clip holds.
    pub one_shot_beats: Beats,
}

/// Runtime edge-detection state for every live trigger route. Owned by the
/// content thread (the engine), never serialized. Keyed by `(send id, band
/// index)`; an absent key means armed.
#[derive(Default)]
pub struct LiveTriggerState {
    armed: AHashMap<(AudioSendId, usize), bool>,
}

impl LiveTriggerState {
    /// Decide which routes fire this tick. Pure: reads the snapshot + setup,
    /// updates only the internal armed flags, and returns the fires for the
    /// engine to act on. Skips sends with no enabled routes and sends with no
    /// features this block.
    pub fn evaluate(
        &mut self,
        snapshot: &AudioFeatureSnapshot,
        setup: &AudioSetup,
    ) -> Vec<FireRequest> {
        let mut fires = Vec::new();
        for (i, send) in setup.sends.iter().enumerate() {
            if !send.has_active_triggers() {
                continue;
            }
            let Some(features) = snapshot.get(i) else {
                continue;
            };
            for route in &send.triggers {
                if !route.enabled {
                    continue;
                }
                let key = (send.id.clone(), route.source.index());
                let armed = self.armed.get(&key).copied().unwrap_or(true);
                let level = route.transient(features);
                let threshold = route.threshold();

                if armed && level > threshold {
                    fires.push(FireRequest {
                        send_label: send.label.clone(),
                        target_layer: route.target_layer.clone(),
                        one_shot_beats: route.one_shot_beats,
                    });
                    self.armed.insert(key, false);
                } else if !armed && level < threshold * REARM_RATIO {
                    self.armed.insert(key, true);
                }
            }
        }
        fires
    }

    /// Drop all armed state — call on transport stop / project reset so a stale
    /// "fired, not yet re-armed" flag can't suppress the first onset next time.
    pub fn clear(&mut self) {
        self.armed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_mod::AudioBand;
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::TriggerRoute;

    /// A setup with one send named `label` carrying one enabled route on `band`
    /// at `sensitivity`.
    fn setup_with_route(label: &str, band: AudioBand, sensitivity: f32) -> AudioSetup {
        let mut send = AudioSend::new(label);
        let mut route = TriggerRoute::new(band);
        route.enabled = true;
        route.sensitivity = sensitivity;
        send.triggers.push(route);
        let mut setup = AudioSetup::default();
        setup.sends.push(send);
        setup
    }

    /// A snapshot with one send whose `band` transient is `level`.
    fn snapshot_with_transient(band: AudioBand, level: f32) -> AudioFeatureSnapshot {
        let mut f = manifold_core::SendFeatures::default();
        f.bands[band.index()].transients = level;
        AudioFeatureSnapshot { sends: vec![f] }
    }

    #[test]
    fn fires_once_on_rising_edge_then_holds_until_rearm() {
        let setup = setup_with_route("Kick", AudioBand::Full, 0.5);
        let threshold = setup.sends[0].triggers[0].threshold();
        let mut state = LiveTriggerState::default();

        // Onset above threshold → one fire.
        let hot = snapshot_with_transient(AudioBand::Full, threshold + 0.2);
        assert_eq!(state.evaluate(&hot, &setup).len(), 1);

        // Impulse still high (plateau / slow decay) → no re-fire.
        assert_eq!(state.evaluate(&hot, &setup).len(), 0);

        // Impulse decays below the re-arm floor → re-arms (no fire on the dip).
        let cold = snapshot_with_transient(AudioBand::Full, threshold * 0.3);
        assert_eq!(state.evaluate(&cold, &setup).len(), 0);

        // Next onset fires again.
        assert_eq!(state.evaluate(&hot, &setup).len(), 1);
    }

    #[test]
    fn does_not_fire_below_threshold() {
        let setup = setup_with_route("Kick", AudioBand::Low, 0.5);
        let threshold = setup.sends[0].triggers[0].threshold();
        let mut state = LiveTriggerState::default();
        let weak = snapshot_with_transient(AudioBand::Low, threshold - 0.05);
        assert!(state.evaluate(&weak, &setup).is_empty());
    }

    #[test]
    fn disabled_route_never_fires() {
        let mut setup = setup_with_route("Kick", AudioBand::Full, 0.5);
        setup.sends[0].triggers[0].enabled = false;
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.99);
        assert!(state.evaluate(&hot, &setup).is_empty());
    }

    #[test]
    fn fire_carries_send_label_and_target() {
        let mut setup = setup_with_route("Snare", AudioBand::Mid, 0.5);
        let layer = LayerId::new("layer-7");
        setup.sends[0].triggers[0].target_layer = Some(layer.clone());
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Mid, 0.99);
        let fires = state.evaluate(&hot, &setup);
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].send_label, "Snare");
        assert_eq!(fires[0].target_layer, Some(layer));
    }

    #[test]
    fn clear_re_arms_so_first_onset_fires_again() {
        let setup = setup_with_route("Kick", AudioBand::Full, 0.5);
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.99);
        assert_eq!(state.evaluate(&hot, &setup).len(), 1);
        assert_eq!(state.evaluate(&hot, &setup).len(), 0); // disarmed
        state.clear();
        assert_eq!(state.evaluate(&hot, &setup).len(), 1); // re-armed
    }
}
