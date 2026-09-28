//! Live audio trigger evaluator — turns per-send transient impulses into
//! one-shot clip fires, in real time, with no lookahead.
//!
//! Each update supplies an [`AudioFeatureSnapshot`], the project's
//! [`AudioSetup`] (to resolve a config's `AudioSendId`), and the project's
//! layers. Retained analyzed hops are consumed once per follower; legacy
//! snapshots are sampled once per update. Every enabled
//! [`LayerClipTrigger`](manifold_core::audio_trigger::LayerClipTrigger) uses
//! the shared `AudioModShape::condition()` chassis and edge-detects the
//! sensitivity-scaled raw signal at the fixed 0.5 threshold. See
//! `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` D2/D3/section 3.3
//! (P2) and `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md`.
//!
//! **Why this is just edge detection:** the upstream transient detector already
//! emits one decaying impulse per onset and holds its own ~106 ms refractory
//! (the audio-modulation onset detector). So this layer needs no time- or
//! beat-based refractory of its own — it only has to avoid re-firing on the
//! *same* impulse's decay. It does that with a per-config [`TransientEdge`]:
//! fire on the rising edge above the fixed 0.5 threshold, then re-arm only
//! once the level falls back below `0.5 * REARM_RATIO`. Tempo-
//! independent and pure. **BUG-242 (2026-07-18):** the edge reads the
//! sensitivity-scaled RAW signal, not the shape-conditioned envelope — a
//! shape's attack/release smoothing (release defaults to 120 ms) used to gate
//! `advance()` too, so a second onset landing inside the first one's decay
//! tail never re-armed the edge, deafening triggers on dense material. The
//! meter and edge both expose that same raw edge level. (section 8, 2026-07-07:
//! the edge itself moved to
//! `manifold_core::audio_trigger::TransientEdge` so the param-trigger
//! evaluator could share it; P2, 2026-07-10: this module's own state moved
//! from send×band keys to layer×index keys when clip triggers became
//! layer-owned.)

use ahash::AHashMap;

use manifold_core::audio_features::{AudioFeatureSnapshot, AudioHopCursor, AudioHopStamp};
use manifold_core::audio_mod::AudioModSource;
use manifold_core::audio_setup::AudioSetup;
use manifold_core::audio_trigger::{FireMeterCapture, TransientEdge, fire_meter_key_for_clip_trigger};
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
    /// The analyzed hop which caused this fire. Legacy snapshot-only
    /// evaluation has no source timeline and leaves this absent.
    pub audio_stamp: Option<AudioHopStamp>,
}

/// Runtime envelope-follower + edge state for one clip-trigger config. Mirrors
/// what `ParameterAudioMod` carries inline (`smoothed`, `prev_raw`,
/// `trigger_edge` — `audio_mod.rs`); kept out-of-line here because
/// `LayerClipTrigger` is a pure data model (section 3.1 of the design doc), not a
/// struct that already carried follower state.
#[derive(Debug, Clone, Default)]
struct ClipTriggerFollower {
    edge: TransientEdge,
    smoothed: f32,
    prev_raw: f32,
    cursor: AudioHopCursor,
    held_meter: f32,
    source: Option<AudioModSource>,
}

impl ClipTriggerFollower {
    fn clear_conditioning(&mut self) {
        self.edge.clear();
        self.smoothed = 0.0;
        self.prev_raw = 0.0;
        self.held_meter = 0.0;
    }
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
        fire_meters: &mut FireMeterCapture,
    ) -> Vec<FireRequest> {
        self.walk(snapshot, setup, layers, dt, fire_meters, true)
    }

    /// BUG-109 section 7.1 item 2: while the transport is stopped, clip triggers
    /// never fire (one-shot expiry is beat-based and the clock is frozen),
    /// but a performer tuning a trigger at soundcheck — transport stopped,
    /// track playing through the tap — still needs to see the shaped signal
    /// move. Runs the identical `condition()` walk [`Self::evaluate`] does —
    /// same follower state, same fixed-0.5 meter push — but never advances
    /// [`TransientEdge`] and never emits a [`FireRequest`], so resuming
    /// playback can't inherit a fire decided while stopped.
    pub fn evaluate_meter_only(
        &mut self,
        snapshot: &AudioFeatureSnapshot,
        setup: &AudioSetup,
        layers: &[Layer],
        dt: Seconds,
        fire_meters: &mut FireMeterCapture,
    ) {
        self.walk(snapshot, setup, layers, dt, fire_meters, false);
    }

    /// Shared walk behind [`Self::evaluate`] and [`Self::evaluate_meter_only`]
    /// — `fire_enabled` gates the one line that can start a clip (advancing
    /// the edge and emitting a [`FireRequest`]); everything upstream of that
    /// line (feature extraction, `condition()` shaping, the meter push) runs
    /// identically either way, so the drawer meter and the follower envelope
    /// behave the same whether or not the transport is playing.
    fn walk(
        &mut self,
        snapshot: &AudioFeatureSnapshot,
        setup: &AudioSetup,
        layers: &[Layer],
        dt: Seconds,
        fire_meters: &mut FireMeterCapture,
        fire_enabled: bool,
    ) -> Vec<FireRequest> {
        let mut fires = Vec::new();
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
                let follower = self.armed.entry((layer.layer_id.clone(), idx)).or_default();
                if follower.source.as_ref() != Some(&cfg.source) {
                    follower.source = Some(cfg.source.clone());
                    follower.cursor = AudioHopCursor::default();
                    follower.clear_conditioning();
                }

                if snapshot.hop_batches.is_empty() {
                    let Some(features) = snapshot.get(send_idx) else {
                        continue;
                    };
                    Self::sample(
                        follower,
                        cfg,
                        layer,
                        features,
                        dt,
                        None,
                        fire_enabled,
                        &mut fires,
                    );
                    Self::push_meter(layer, idx, follower.held_meter, fire_meters);
                } else {
                    let Some(batch) = snapshot.hop_batches.get(send_idx) else {
                        follower.clear_conditioning();
                        Self::push_meter(layer, idx, follower.held_meter, fire_meters);
                        continue;
                    };
                    if follower.cursor.begin_epoch(batch.epoch()) {
                        follower.clear_conditioning();
                    }
                    if batch.epoch() == 0
                        || (batch.failure().is_some() && batch.epoch() >= follower.cursor.epoch())
                    {
                        follower.clear_conditioning();
                    } else {
                        for hop in batch.hops() {
                            let Some(new_epoch) = follower.cursor.accept(hop.stamp) else {
                                continue;
                            };
                            if new_epoch {
                                follower.clear_conditioning();
                            }
                            Self::sample(
                                follower,
                                cfg,
                                layer,
                                &hop.features,
                                hop.dt,
                                Some(hop.stamp),
                                fire_enabled,
                                &mut fires,
                            );
                        }
                    }
                    Self::push_meter(layer, idx, follower.held_meter, fire_meters);
                }
            }
        }
        if fires.iter().all(|fire| fire.audio_stamp.and_then(|stamp| stamp.timeline_time).is_some()) {
            // Stable insertion sort keeps the established layer/config/hop order
            // for equal timestamps and uses no scratch allocation.
            for index in 1..fires.len() {
                let mut position = index;
                while position > 0 {
                    let left = fires[position - 1]
                        .audio_stamp
                        .and_then(|stamp| stamp.timeline_time)
                        .expect("all fires have timeline times");
                    let right = fires[position]
                        .audio_stamp
                        .and_then(|stamp| stamp.timeline_time)
                        .expect("all fires have timeline times");
                    if left.0 <= right.0 {
                        break;
                    }
                    fires.swap(position - 1, position);
                    position -= 1;
                }
            }
        }
        fires
    }

    fn sample(
        follower: &mut ClipTriggerFollower,
        cfg: &manifold_core::audio_trigger::LayerClipTrigger,
        layer: &Layer,
        features: &manifold_core::audio_features::SendFeatures,
        dt: Seconds,
        audio_stamp: Option<AudioHopStamp>,
        fire_enabled: bool,
        fires: &mut Vec<FireRequest>,
    ) {
        let raw = cfg.source.feature.extract(features);
        let prev_raw_before_condition = follower.prev_raw;
        let _conditioned = cfg.shape.condition(
            raw,
            dt.0 as f32,
            &mut follower.smoothed,
            &mut follower.prev_raw,
        );
        let edge_level = if cfg.shape.rate_of_change {
            let rate = (raw - prev_raw_before_condition) / (dt.0 as f32).max(1e-4);
            (0.5 + rate * cfg.shape.sensitivity).clamp(0.0, 1.0)
        } else {
            (raw * cfg.shape.sensitivity).clamp(0.0, 1.0)
        };
        follower.held_meter = edge_level;
        if fire_enabled && follower.edge.advance(edge_level, 0.5) {
            fires.push(FireRequest {
                target_layer: layer.layer_id.clone(),
                one_shot_beats: cfg.one_shot_beats,
                audio_stamp,
            });
        }
    }

    fn push_meter(
        layer: &Layer,
        idx: usize,
        level: f32,
        fire_meters: &mut FireMeterCapture,
    ) {
        fire_meters.push(fire_meter_key_for_clip_trigger(layer.layer_id.as_str(), idx as u64), level);
    }

    /// Re-arm fire edges on transport stop / project reset so a stale "fired,
    /// not yet re-armed" flag cannot suppress the first new onset (BUG-051).
    /// Hop cursors remain advanced so retained batches are not replayed when
    /// playback resumes.
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
    use manifold_core::audio_features::{AudioFeatureHop, AudioHopBatch, AudioHopError};
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
        AudioFeatureSnapshot { sends: vec![f], ..Default::default() }
    }

    fn snapshot_with_hops(
        band: AudioBand,
        levels: &[f32],
        epoch: u64,
        first_end_sample: u64,
        timeline_start: Option<f64>,
    ) -> AudioFeatureSnapshot {
        let mut batch = AudioHopBatch::with_capacity(levels.len().max(1));
        batch.begin(epoch);
        for (index, &level) in levels.iter().enumerate() {
            let mut features = manifold_core::SendFeatures::default();
            features.bands[band.index()].transients = level;
            let end_sample = first_end_sample + index as u64 * 480;
            batch
                .push(AudioFeatureHop {
                    stamp: AudioHopStamp {
                        epoch,
                        end_sample,
                        sample_rate: 48_000,
                        source_time: None,
                        timeline_time: timeline_start.map(|time| Seconds(time + index as f64 * 0.01)),
                    },
                    dt: Seconds(0.01),
                    features,
                })
                .unwrap();
        }
        AudioFeatureSnapshot {
            sends: vec![manifold_core::SendFeatures::default()],
            hop_batches: vec![batch],
            ..Default::default()
        }
    }

    fn empty_batch_snapshot(epoch: u64) -> AudioFeatureSnapshot {
        let mut batch = AudioHopBatch::default();
        batch.begin(epoch);
        AudioFeatureSnapshot {
            sends: vec![manifold_core::SendFeatures::default()],
            hop_batches: vec![batch],
            ..Default::default()
        }
    }

    #[test]
    fn fires_once_on_rising_edge_then_holds_until_rearm() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        let mut state = LiveTriggerState::default();

        // Onset above the fixed 0.5 edge → one fire.
        let hot = snapshot_with_transient(AudioBand::Full, 0.9);
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 1);

        // Impulse still high (plateau / slow decay) → no re-fire.
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 0);

        // Impulse decays below the re-arm floor (0.5 * REARM_RATIO = 0.3) →
        // re-arms (no fire on the dip).
        let cold = snapshot_with_transient(AudioBand::Full, 0.0);
        assert_eq!(state.evaluate(&cold, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 0);

        // Next onset fires again.
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 1);
    }

    #[test]
    fn retained_batch_consumes_each_hop_once_and_emits_each_onset() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        let snapshot = snapshot_with_hops(AudioBand::Full, &[0.9, 0.0, 0.9], 10, 480, None);
        let mut state = LiveTriggerState::default();

        let fires = state.evaluate(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default());
        assert_eq!(fires.len(), 2);
        assert!(fires.iter().all(|fire| fire.audio_stamp.is_some()));
        assert!(state.evaluate(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default()).is_empty());
    }

    #[test]
    fn retained_hops_are_partition_invariant_for_rate_of_change() {
        let (setup, mut layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        layers[0].clip_triggers[0].shape.rate_of_change = true;
        layers[0].clip_triggers[0].shape.attack_ms = 0.0;
        layers[0].clip_triggers[0].shape.release_ms = 0.0;

        let whole = snapshot_with_hops(AudioBand::Full, &[0.0, 1.0, 0.0, 1.0], 11, 480, None);
        let mut whole_state = LiveTriggerState::default();
        let whole_fires = whole_state.evaluate(&whole, &setup, &layers, DT, &mut FireMeterCapture::default());

        let mut split_state = LiveTriggerState::default();
        let mut split_fires = Vec::new();
        for (index, &level) in [0.0, 1.0, 0.0, 1.0].iter().enumerate() {
            let snapshot = snapshot_with_hops(
                AudioBand::Full,
                &[level],
                11,
                480 + index as u64 * 480,
                None,
            );
            split_fires.extend(split_state.evaluate(
                &snapshot,
                &setup,
                &layers,
                DT,
                &mut FireMeterCapture::default(),
            ));
        }
        assert_eq!(whole_fires.len(), 2);
        assert_eq!(split_fires.len(), whole_fires.len());
    }

    #[test]
    fn source_switch_resets_cursor_but_removal_and_readd_do_not_replay() {
        let send_a = AudioSend::new("A");
        let send_a_id = send_a.id.clone();
        let send_b = AudioSend::new("B");
        let send_b_id = send_b.id.clone();
        let mut setup = AudioSetup::default();
        setup.sends.extend([send_a, send_b]);
        let mut layer = Layer::new("Layer".to_string(), LayerType::Video, 0);
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id: send_a_id,
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        });
        cfg.enabled = true;
        cfg.shape.attack_ms = 0.0;
        cfg.shape.release_ms = 0.0;
        layer.clip_triggers.push(cfg);
        let mut layers = vec![layer];

        let mut snapshot = snapshot_with_hops(AudioBand::Full, &[0.9], 10, 480, None);
        let mut b_snapshot = snapshot_with_hops(AudioBand::Full, &[0.9], 2, 480, None);
        snapshot.sends.push(manifold_core::SendFeatures::default());
        snapshot.hop_batches.push(b_snapshot.hop_batches.remove(0));
        let mut state = LiveTriggerState::default();
        assert_eq!(state.evaluate(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 1);

        layers[0].clip_triggers[0].source.send_id = send_b_id;
        assert_eq!(state.evaluate(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 1);

        setup.sends.remove(1);
        assert!(state.evaluate(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default()).is_empty());
        setup.sends.push(AudioSend::new("B"));
        setup.sends[1].id = layers[0].clip_triggers[0].source.send_id.clone();
        assert!(state.evaluate(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default()).is_empty());
    }

    #[test]
    fn stale_fault_is_ignored_but_new_empty_epoch_clears_meter() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        let mut state = LiveTriggerState::default();
        let valid = snapshot_with_hops(AudioBand::Full, &[0.9], 10, 480, None);
        assert_eq!(state.evaluate(&valid, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 1);

        let mut stale_fault = empty_batch_snapshot(9);
        stale_fault.hop_batches[0].invalidate(AudioHopError::InvalidInput);
        let mut meters = FireMeterCapture::default();
        state.evaluate(&stale_fault, &setup, &layers, DT, &mut meters);
        let key = fire_meter_key_for_clip_trigger(layers[0].layer_id.as_str(), 0);
        assert!(meters.get(key).unwrap() > 0.5);

        let fresh_empty = empty_batch_snapshot(11);
        let mut meters = FireMeterCapture::default();
        state.evaluate(&fresh_empty, &setup, &layers, DT, &mut meters);
        assert_eq!(meters.get(key), Some(0.0));
    }

    #[test]
    fn missing_or_inactive_batch_clears_meter_without_using_latest_snapshot() {
        let send_a = AudioSend::new("A");
        let send_b = AudioSend::new("B");
        let send_b_id = send_b.id.clone();
        let mut setup = AudioSetup::default();
        setup.sends.extend([send_a, send_b]);
        let mut layer = Layer::new("B".to_string(), LayerType::Video, 0);
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id: send_b_id,
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        });
        cfg.enabled = true;
        cfg.shape.attack_ms = 0.0;
        cfg.shape.release_ms = 0.0;
        layer.clip_triggers.push(cfg);
        let layers = vec![layer];
        let mut state = LiveTriggerState::default();

        let mut missing = snapshot_with_hops(AudioBand::Full, &[0.9], 30, 480, None);
        missing.sends.push(manifold_core::SendFeatures::default());
        let mut meters = FireMeterCapture::default();
        state.evaluate(&missing, &setup, &layers, DT, &mut meters);
        let key = fire_meter_key_for_clip_trigger(layers[0].layer_id.as_str(), 0);
        assert_eq!(meters.get(key), Some(0.0));

        let mut inactive = empty_batch_snapshot(0);
        inactive.sends.push(manifold_core::SendFeatures::default());
        inactive.hop_batches.push(AudioHopBatch::default());
        let mut meters = FireMeterCapture::default();
        state.evaluate(&inactive, &setup, &layers, DT, &mut meters);
        assert_eq!(meters.get(key), Some(0.0));
    }

    #[test]
    fn clear_does_not_replay_retained_hops_on_resume() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        let snapshot = snapshot_with_hops(AudioBand::Full, &[0.9], 10, 480, None);
        let mut state = LiveTriggerState::default();
        state.evaluate_meter_only(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default());
        state.clear();
        assert!(state.evaluate(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default()).is_empty());

        let fresh = snapshot_with_hops(AudioBand::Full, &[0.9], 10, 960, None);
        assert_eq!(state.evaluate(&fresh, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 1);
    }

    #[test]
    fn timestamped_fires_sort_chronologically_and_keep_equal_order() {
        let send_a = AudioSend::new("A");
        let send_a_id = send_a.id.clone();
        let send_b = AudioSend::new("B");
        let send_b_id = send_b.id.clone();
        let mut setup = AudioSetup::default();
        setup.sends.extend([send_a, send_b]);
        let mut layers = Vec::new();
        for (name, send_id) in [("A", send_a_id), ("B", send_b_id)] {
            let mut layer = Layer::new(name.to_string(), LayerType::Video, 0);
            let mut cfg = LayerClipTrigger::new(AudioModSource {
                send_id,
                feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
            });
            cfg.enabled = true;
        cfg.shape.attack_ms = 0.0;
            cfg.shape.release_ms = 0.0;
            layer.clip_triggers.push(cfg);
            layers.push(layer);
        }

        let mut snapshot = snapshot_with_hops(AudioBand::Full, &[0.9], 20, 480, Some(2.0));
        let mut second = snapshot_with_hops(AudioBand::Full, &[0.9], 21, 480, Some(1.0));
        snapshot.sends.push(manifold_core::SendFeatures::default());
        snapshot.hop_batches.push(second.hop_batches.remove(0));
        let mut state = LiveTriggerState::default();
        let fires = state.evaluate(&snapshot, &setup, &layers, DT, &mut FireMeterCapture::default());
        assert_eq!(fires[0].target_layer, layers[1].layer_id);
        assert_eq!(fires[1].target_layer, layers[0].layer_id);

        let mut equal = snapshot_with_hops(AudioBand::Full, &[0.9], 22, 960, Some(3.0));
        let mut equal_second = snapshot_with_hops(AudioBand::Full, &[0.9], 23, 960, Some(3.0));
        equal.sends.push(manifold_core::SendFeatures::default());
        equal.hop_batches.push(equal_second.hop_batches.remove(0));
        let mut state = LiveTriggerState::default();
        let fires = state.evaluate(&equal, &setup, &layers, DT, &mut FireMeterCapture::default());
        assert_eq!(fires[0].target_layer, layers[0].layer_id);
        assert_eq!(fires[1].target_layer, layers[1].layer_id);
    }

    #[test]
    fn two_impulses_80ms_apart_both_fire_with_default_shape_release() {
        // BUG-242: with the DEFAULT shape (release_ms = 120, untouched),
        // two onsets landing ~80ms apart — well inside the release tail —
        // must both fire. After the fix the edge reads the
        // sensitivity-scaled RAW signal, which drops straight back to 0 the
        // tick after each onset (mirroring the upstream transient
        // detector's own decaying-impulse-per-onset shape), clearing the
        // re-arm floor immediately.
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        let mut setup = AudioSetup::default();
        setup.sends.push(send);

        let mut layer = Layer::new("Kick".to_string(), LayerType::Video, 0);
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id,
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        });
        cfg.enabled = true;
        // shape stays at AudioModShape::default() — sensitivity 1.0, attack
        // 5ms, release 120ms — the out-of-the-box configuration BUG-242 was
        // measured on.
        layer.clip_triggers.push(cfg);
        let layers = vec![layer];

        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.9);
        let cold = snapshot_with_transient(AudioBand::Full, 0.0);

        assert_eq!(
            state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(),
            1,
            "first onset must fire"
        );

        // ~80ms of quiet at 60fps (5 * 16.67ms ~= 83ms) — inside the 120ms
        // release tail, exactly the BUG-242 scenario.
        for _ in 0..5 {
            state.evaluate(&cold, &setup, &layers, DT, &mut FireMeterCapture::default());
        }

        assert_eq!(
            state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(),
            1,
            "second onset ~80ms later, inside the shape's 120ms release tail, must still fire (BUG-242)"
        );
    }

    #[test]
    fn does_not_fire_below_the_fixed_edge() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Low, 1.0);
        let mut state = LiveTriggerState::default();
        let weak = snapshot_with_transient(AudioBand::Low, 0.3);
        assert!(state.evaluate(&weak, &setup, &layers, DT, &mut FireMeterCapture::default()).is_empty());
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
        assert!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).is_empty());
    }

    #[test]
    fn disabled_config_never_fires() {
        let (setup, mut layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        layers[0].clip_triggers[0].enabled = false;
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.99);
        assert!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).is_empty());
    }

    #[test]
    fn fire_carries_the_owning_layer_as_target() {
        let (setup, layers) = setup_and_layer("Snare", AudioBand::Mid, 1.0);
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Mid, 0.99);
        let fires = state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default());
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].target_layer, layers[0].layer_id);
    }

    #[test]
    fn clear_re_arms_so_first_onset_fires_again() {
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.99);
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 1);
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 0); // disarmed
        state.clear();
        assert_eq!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).len(), 1); // re-armed
    }

    #[test]
    fn evaluate_meter_only_pushes_the_level_but_never_advances_the_edge() {
        // BUG-109 section 7.1 item 2: the stopped-tick walk must push the same
        // shaped signal the edge reads, without ever deciding a fire.
        let (setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.9);

        let mut meters = FireMeterCapture::default();
        state.evaluate_meter_only(&hot, &setup, &layers, DT, &mut meters);
        let key = fire_meter_key_for_clip_trigger(layers[0].layer_id.as_str(), 0u64);
        assert!(
            meters.get(key).unwrap() >= 0.5,
            "meter-only walk must push the conditioned level even though nothing fires"
        );

        // Repeated meter-only calls on a hot signal never advance the edge —
        // proven indirectly: a REAL evaluate() call right after still sees a
        // fresh rising edge and fires, exactly as if evaluate_meter_only had
        // never run (had it advanced the edge, this would now be disarmed).
        state.evaluate_meter_only(&hot, &setup, &layers, DT, &mut FireMeterCapture::default());
        state.evaluate_meter_only(&hot, &setup, &layers, DT, &mut FireMeterCapture::default());
        assert_eq!(
            state
                .evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default())
                .len(),
            1,
            "the edge must still be armed after any number of meter-only calls"
        );
    }

    #[test]
    fn skips_a_config_whose_send_id_no_longer_resolves() {
        let (mut setup, layers) = setup_and_layer("Kick", AudioBand::Full, 1.0);
        setup.sends.clear(); // the config's send_id now resolves to nothing
        let mut state = LiveTriggerState::default();
        let hot = snapshot_with_transient(AudioBand::Full, 0.99);
        assert!(state.evaluate(&hot, &setup, &layers, DT, &mut FireMeterCapture::default()).is_empty());
    }
}
