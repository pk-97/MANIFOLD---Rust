//! Live audio trigger evaluator — turns per-send transient impulses into
//! one-shot clip fires, in real time, with no lookahead.
//!
//! Each analysis block the engine hands this the latest
//! [`AudioFeatureSnapshot`], the project's [`AudioSetup`] (to resolve a
//! config's `AudioSendId` to a snapshot index), and the project's layers. For
//! every enabled [`LayerClipTrigger`](manifold_core::audio_trigger::LayerClipTrigger)
//! it shapes the config's source feature through the SAME
//! `AudioModShape::condition()` chassis the param-trigger evaluator uses
//! (`manifold-playback::modulation`), edge-detects the shaped signal at the
//! fixed 0.5 threshold, and emits a [`FireRequest`] naming the owning layer
//! directly. See `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
//! D2/D3/§3.3 (P2) and `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md` (the original
//! send-owned design this replaces).
//!
//! **Why this is just edge detection:** the upstream transient detector already
//! emits one decaying impulse per onset and holds its own ~106 ms refractory
//! (the audio-modulation onset detector). So this layer needs no time- or
//! beat-based refractory of its own — it only has to avoid re-firing on the
//! *same* impulse's decay. It does that with a per-config [`TransientEdge`]:
//! fire on the rising edge above the fixed 0.5 threshold, then re-arm only
//! once the shaped signal falls back below `0.5 * REARM_RATIO`. Tempo-
//! independent and pure. (§8, 2026-07-07: the edge itself moved to
//! `manifold_core::audio_trigger::TransientEdge` so the param-trigger
//! evaluator could share it; P2, 2026-07-10: this module's own state moved
//! from send×band keys to layer×index keys when clip triggers became
//! layer-owned.)

use ahash::AHashMap;

use manifold_core::audio_features::AudioFeatureSnapshot;
use manifold_core::audio_setup::AudioSetup;
use manifold_core::audio_trigger::TransientEdge;
use manifold_core::id::LayerId;
use manifold_core::layer::Layer;
use manifold_core::units::{Beats, Seconds};

/// One decided fire: a layer's clip-trigger config crossed its threshold this
/// tick. The target IS the layer that owns the config — no more send-label
/// auto-routing (that existed only because the send-owned matrix didn't know
/// which layer it should launch; a layer-owned config always knows).
#[derive(Debug, Clone, PartialEq)]
pub struct FireRequest {
    /// The layer whose clip-trigger config fired.
    pub target_layer: LayerId,
    /// How long the fired one-shot clip holds.
    pub one_shot_beats: Beats,
}

/// Runtime envelope-follower + edge state for one clip-trigger config. Mirrors
/// what `ParameterAudioMod` carries inline (`smoothed`, `prev_raw`,
/// `trigger_edge` — `audio_mod.rs`); kept out-of-line here because
/// `LayerClipTrigger` is a pure data model (§3.1 of the design doc), not a
/// struct that already carried follower state.
#[derive(Debug, Clone, Copy, Default)]
struct ClipTriggerFollower {
    edge: TransientEdge,
    smoothed: f32,
    prev_raw: f32,
}

/// Runtime edge-detection state for every live clip trigger. Owned by the
/// content thread (the engine), never serialized. Keyed by `(owning layer,
/// index within Layer::clip_triggers)`; an absent key means armed (matches
/// `TransientEdge::default()` / `ClipTriggerFollower::default()`).
#[derive(Default)]
pub struct LiveTriggerState {
    armed: AHashMap<(LayerId, usize), ClipTriggerFollower>,
}

impl LiveTriggerState {
    /// Decide which clip triggers fire this tick. Pure: reads the snapshot,
    /// setup (to resolve send ids to snapshot indices), and layers, updates
    /// only the internal follower/edge state, and returns the fires for the
    /// engine to act on. Skips configs whose send has no features this block.
    pub fn evaluate(
        &mut self,
        snapshot: &AudioFeatureSnapshot,
        setup: &AudioSetup,
        layers: &[Layer],
        dt: Seconds,
    ) -> Vec<FireRequest> {
        let mut fires = Vec::new();
        let dt_s = dt.0 as f32;
        for layer in layers {
            if layer.clip_triggers.is_empty() {
                continue;
            }
            for (idx, cfg) in layer.clip_triggers.iter().enumerate() {
                if !cfg.enabled {
                    continue;
                }
                let Some(send_idx) =
                    setup.sends.iter().position(|s| s.id == cfg.source.send_id)
                else {
                    continue;
                };
                let Some(features) = snapshot.get(send_idx) else {
                    continue;
                };
                let raw = cfg.source.feature.extract(features);
                let follower = self.armed.entry((layer.layer_id.clone(), idx)).or_default();
                // Edge-detect the pre-range-map `conditioned` signal — the
                // exact same split the trigger-gate arm in
                // `modulation::evaluate_instance_audio_mods` uses (never the
                // range-mapped value `AudioModShape::apply` would produce),
                // so *whether* a clip fires can never be distorted by a trim
                // handle even if one is ever exposed on this drawer.
                let conditioned = cfg.shape.condition(
                    raw,
                    dt_s,
                    &mut follower.smoothed,
                    &mut follower.prev_raw,
                );
                if follower.edge.advance(conditioned, 0.5) {
                    fires.push(FireRequest {
                        target_layer: layer.layer_id.clone(),
                        one_shot_beats: cfg.one_shot_beats,
                    });
                }
            }
        }
        fires
    }

    /// Drop all armed state — call on transport stop / project reset so a stale
    /// "fired, not yet re-armed" flag can't suppress the first onset next time
    /// (BUG-051).
    pub fn clear(&mut self) {
        for f in self.armed.values_mut() {
            f.edge.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::LayerClipTrigger;
    use manifold_core::types::LayerType;

    const DT: Seconds = Seconds(1.0 / 60.0);

    /// A setup with one send named `label`, and one layer with one enabled
    /// clip-trigger config reading that send's `band` feature at
    /// `sensitivity` — attack/release zeroed so a single `evaluate` call
    /// settles instantly (the same `AudioModShape { attack_ms: 0.0,
    /// release_ms: 0.0, .. }` pattern `modulation.rs`'s own trigger-gate
    /// tests use for deterministic single-tick firing).
    fn setup_and_layer(label: &str, band: AudioBand, sensitivity: f32) -> (AudioSetup, Vec<Layer>) {
        let send = AudioSend::new(label);
        let send_id = send.id.clone();
        let mut setup = AudioSetup::default();
        setup.sends.push(send);

        let mut layer = Layer::new(label.to_string(), LayerType::Video, 0);
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id,
            feature: AudioFeature::new(AudioFeatureKind::Transients, band),
        });
        cfg.enabled = true;
        cfg.shape.sensitivity = sensitivity;
        cfg.shape.attack_ms = 0.0;
        cfg.shape.release_ms = 0.0;
        layer.clip_triggers.push(cfg);

        (setup, vec![layer])
    }

    /// A snapshot with one send whose `band` transient is `level`.
    fn snapshot_with_transient(band: AudioBand, level: f32) -> AudioFeatureSnapshot {
        let mut f = manifold_core::SendFeatures::default();
        f.bands[band.index()].transients = level;
        AudioFeatureSnapshot { sends: vec![f] }
    }

    #[test]
    fn fires_once_on_rising_edge_then_holds_until_rearm() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        let mut state = LiveTriggerState::default();

        // Onset above the fixed 0.5 edge → one fire.
        let hot = snapshot_with_transient(AudioBand::Full, 0.9);
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT).len(), 1);

        // Impulse still high (plateau / slow decay) → no re-fire.
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT).len(), 0);

        // Impulse decays below the re-arm floor (0.5 * REARM_RATIO = 0.3) →
        // re-arms (no fire on the dip).
        let cold = snapshot_with_transient(AudioBand::Full, 0.0);
        assert_eq!(state.evaluate(&cold, &setup, &layers, DT).len(), 0);

        // Next onset fires again.
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT).len(), 1);
    }

    #[test]
    fn does_not_fire_below_the_fixed_edge() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Low, 1.0);
        let mut state = LiveTriggerState::default();
        let weak = snapshot_with_transient(AudioBand::Low, 0.3);
        assert!(state.evaluate(&weak, &setup, &layers, DT).is_empty());
    }

    #[test]
    fn sensitivity_scales_the_signal_against_the_fixed_edge() {
        // D3: Amount (sensitivity) is the tune knob against the fixed 0.5
        // edge, not a bespoke per-route threshold. A raw level that would
        // fire at sensitivity 1.0 must NOT fire when sensitivity is tuned
        // down enough to keep the conditioned signal under 0.5.
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 0.4);
        let mut state = LiveTriggerState::default();
        // raw=0.9 * sensitivity=0.4 = 0.36, under the 0.5 edge.
        let hot = snapshot_with_transient(AudioBand::Full, 0.9);
        assert!(state.evaluate(&hot, &setup, &layers, DT).is_empty());
    }

    #[test]
    fn disabled_config_never_fires() {
        let (setup, mut layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        layers[0].clip_triggers[0].enabled = false;
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.99);
        assert!(state.evaluate(&hot, &setup, &layers, DT).is_empty());
    }

    #[test]
    fn fire_carries_the_owning_layer_as_target() {
        let (setup, layers) = setup_and_layer("Snare", AudioBand::Mid, 1.0);
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Mid, 0.99);
        let fires = state.evaluate(&hot, &setup, &layers, DT);
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].target_layer, layers[0].layer_id);
    }

    #[test]
    fn clear_re_arms_so_first_onset_fires_again() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.99);
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT).len(), 1);
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT).len(), 0); // disarmed
        state.clear();
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT).len(), 1); // re-armed
    }

    #[test]
    fn skips_a_config_whose_send_id_no_longer_resolves() {
        let (mut setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        setup.sends.clear(); // the config's send_id now resolves to nothing
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.99);
        assert!(state.evaluate(&hot, &setup, &layers, DT).is_empty());
    }
}
