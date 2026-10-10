//! Offline audio-reactive export driver — P2 of
//! `docs/OFFLINE_AUDIO_REACTIVE_EXPORT_DESIGN.md`.
//!
//! [`OfflineAudioModDriver`] is a sibling consumer of
//! [`manifold_audio::analysis::StreamingSendAnalyzer`], NOT a mode of
//! [`crate::audio_mod_runtime::AudioModRuntime`] (design: "`AudioModRuntime`
//! itself is NOT reused offline — it drags CoreAudio directory subscriptions,
//! hot-plug listeners, and capture lifecycle"). It feeds the export-rendered
//! mono audio ([`ExportAudio`], from the P1 mixdown seam) through one analyzer
//! per analyzed send, per frame, before the engine ticks — so audio-bound
//! parameters, param triggers, and live clip triggers all move in rendered
//! video exactly as the design's D1 intends.
//!
//! ## Mirrors the live path (never reuses it)
//!
//! Everything here is a deliberate, cited mirror of
//! `audio_mod_runtime.rs::AudioModRuntime::update` (audio_mod_runtime.rs
//! ~201-432):
//! - **Which sends are analyzed** — `Project::analysis_consumed_sends()`
//!   (audio_mod_runtime.rs:240-241), not `sends.len()`. A send with an enabled
//!   audio mod OR a layer with an enabled clip trigger sourcing it qualifies.
//! - **Per-send analyzer config** — `set_crossovers`/`set_scope`/
//!   `set_pitch_tracking`/`set_floor_db` (audio_mod_runtime.rs:342-346), same
//!   values (`Project::sends_with_pitch_mods()` for the pitch gate). D5: scope
//!   is always off offline (meters are a live-UX concern).
//! - **The snapshot write** — `snap.sends.clear()`,
//!   `resize(send_count, SendFeatures::default())`, then one write per
//!   analyzed send by its position in `AudioSetup::sends`
//!   (audio_mod_runtime.rs:421-431). `send_count` is every send in the
//!   project, not just the analyzed ones — matching the live snapshot shape.
//!
//! ## What's different (by design, not by shortcut)
//!
//! The live runtime rebuilds its per-tick mono mix from a draining capture
//! ring buffer and a set of currently-playing layer taps — inherently
//! streaming, stateful state carried tick to tick. Offline has the entire
//! rendered range as one static buffer up front ([`ExportAudio`]), so D2's
//! source mapping (capture vs layers vs both) is resolved ONCE at
//! construction into a fixed per-send buffer (or a shared reference to the
//! master mix), and each frame is a pure slice-by-index into it (D6: no
//! per-frame allocation, D1: no cumulative cursor — see
//! [`frame_sample_bounds`]).
//!
//! Each analyzed send also runs the trained kick detector inline on the same samples,
//! just before its analyzer, so a fire lands on the hop that contains it (the live path
//! runs it on a worker thread; docs/KICK_REALTIME_DESIGN.md section 1b).

use manifold_audio::analysis::StreamingSendAnalyzer;
use manifold_audio::detectors::kick_detector;
use manifold_audio::kick::{KickDetector, KickFire};
use manifold_core::audio_features::{
    AudioFeatureHop, AudioHopError, AudioHopStamp, new_audio_analysis_epoch,
};
use manifold_core::AudioSendId;
use manifold_core::Seconds;
use manifold_core::SendFeatures;
use manifold_core::audio_setup::AudioSend;
use manifold_core::audio_visual::AudioVisualRegistry;
use manifold_core::id::LayerId;
use manifold_core::project::Project;
use manifold_playback::audio_mixdown::ExportAudio;
use manifold_playback::engine::PlaybackEngine;

/// Where one analyzed send's samples come from, resolved once at
/// construction (design D2).
enum SendSource {
    /// Capture-fed, layer-free: read straight from
    /// [`OfflineAudioModDriver::master_mono`] every frame — avoids cloning the
    /// (possibly large) master mix once per capture-fed send.
    Master,
    /// Layer-fed, or capture-fed-and-layer-fed (summed once here): the send's
    /// own buffer, built once at init (D6).
    Own(Vec<f32>),
}

/// One send this driver actually analyzes: its snapshot slot, its source, and
/// its analyzer.
struct AnalyzedSend {
    /// Index into `AudioSetup::sends` — also the index into
    /// `AudioFeatureSnapshot::sends`, since the snapshot is positional
    /// (mirrors the live write at audio_mod_runtime.rs:427-430).
    snapshot_index: usize,
    send_id: AudioSendId,
    source: SendSource,
    analyzer: StreamingSendAnalyzer,
    epoch: u64,
    /// `None` when the kick model was refused: kick then stays 0.
    kick: Option<KickDetector>,
    kick_fires: Vec<KickFire>,
}

/// Runs a send's kick detector on `samples` and queues its fires; call just before the
/// analyzer is pushed the same samples.
fn detect_kicks(
    kick: &mut Option<KickDetector>,
    fires: &mut Vec<KickFire>,
    analyzer: &mut StreamingSendAnalyzer,
    samples: &[f32],
) {
    let Some(kick) = kick.as_mut() else { return };
    fires.clear();
    kick.push(samples, fires);
    analyzer.queue_kick_fires(fires.iter().map(|f| f.sample));
}

/// Sample-index bounds `[start, end)` for frame `frame_idx`, at `rate` Hz,
/// `fps` frames/sec. Computed directly from the frame index every call — NOT
/// from a running cursor — so per-frame rounding can never compound into
/// drift over a long export (design D1). `frame_idx as f64 * rate` stays
/// exact in f64 for any export this app will render (frame_idx and rate are
/// both far under 2^26, so the product is far under 2^53).
fn frame_sample_bounds(frame_idx: u32, rate: u32, fps: f64) -> (usize, usize) {
    let rate = rate as f64;
    let start = (frame_idx as f64 * rate / fps).floor() as usize;
    let end = ((frame_idx as f64 + 1.0) * rate / fps).floor() as usize;
    (start, end)
}

/// Sum every present, non-empty layer tap referenced by `layer_ids` into one
/// buffer. Layers with no entry in `per_layer_mono` (or an empty one) are
/// skipped, not treated as an error — the honest-silence rule (D2) applies at
/// the per-layer granularity, not just the per-send one: a send with three
/// layers where one was never tapped still analyzes the other two. Returns
/// `None` (not an empty `Vec`) when nothing contributed, so the caller can
/// distinguish "no layer input" from "silent layer input".
fn sum_layer_taps(
    layer_ids: &[LayerId],
    per_layer_mono: &ahash::AHashMap<LayerId, Vec<f32>>,
) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for lid in layer_ids {
        let Some(buf) = per_layer_mono.get(lid) else {
            continue;
        };
        if buf.is_empty() {
            continue;
        }
        match sum.as_mut() {
            None => sum = Some(buf.clone()),
            Some(acc) => {
                if buf.len() > acc.len() {
                    acc.resize(buf.len(), 0.0);
                }
                for (a, b) in acc.iter_mut().zip(buf.iter()) {
                    *a += b;
                }
            }
        }
    }
    sum
}

/// Add `add` into `base` in place, extending `base` with zeros if `add` is
/// longer (element-wise sum, zero-padding the shorter side).
fn add_in_place(base: &mut Vec<f32>, add: &[f32]) {
    if add.len() > base.len() {
        base.resize(add.len(), 0.0);
    }
    for (a, b) in base.iter_mut().zip(add.iter()) {
        *a += b;
    }
}

/// Feeds export-rendered audio ([`ExportAudio`], from the P1 mixdown seam)
/// through one [`StreamingSendAnalyzer`] per analyzed send, per export frame —
/// the offline counterpart to `AudioModRuntime::update`. See the module docs
/// for what's mirrored and what's deliberately different.
pub struct OfflineAudioModDriver<'a> {
    /// The full export mix, mono — read directly by every send whose source
    /// is [`SendSource::Master`] (see that variant's docs).
    master_mono: &'a [f32],
    sends: Vec<AnalyzedSend>,
    visuals: AudioVisualRegistry,
    /// Total sends in the project (analyzed or not) — the snapshot's length,
    /// matching the live write's `send_count` (audio_mod_runtime.rs:267).
    send_count: usize,
    sample_rate: u32,
    pre_roll_samples: usize,
    fps: f64,
    export_origin: Seconds,
    last_frame: Option<u32>,
    failure: Option<AudioHopError>,
}

impl<'a> OfflineAudioModDriver<'a> {
    /// Build the driver for one export: resolve every consumed send's D2
    /// source mapping against `audio`, construct + pre-roll its analyzer, and
    /// log the mapping (D2: "This substitution is LOGGED per send ... never
    /// silent"). Returns `None` when `Project::analysis_consumed_sends()` is
    /// empty — nothing in the project reads audio, so there's nothing for the
    /// export loop to drive.
    pub fn new(
        project: &Project,
        audio: &'a ExportAudio,
        fps: f64,
        export_origin: Seconds,
    ) -> Option<Self> {
        if !fps.is_finite() || fps <= 0.0 {
            log::error!("[OfflineAudioMod] invalid export FPS {fps:?}");
            return None;
        }
        if audio.sample_rate == 0 {
            log::error!("[OfflineAudioMod] invalid export sample rate 0");
            return None;
        }
        if !export_origin.0.is_finite() {
            log::error!(
                "[OfflineAudioMod] invalid export timeline origin {:?}",
                export_origin.0
            );
            return None;
        }
        let mut consumed = project.analysis_consumed_sends();
        let visual_consumed = crate::audio_visualization::visualizer_consumed_sends(project);
        consumed.extend(visual_consumed.iter().cloned());
        if consumed.is_empty() {
            log::info!(
                "[OfflineAudioMod] no send has an enabled audio mod or clip trigger — \
                 offline audio-mod is inactive for this export"
            );
            return None;
        }

        let pitch_sends = project.sends_with_pitch_mods();
        let (low_hz, mid_hz) = (project.audio_setup.low_hz, project.audio_setup.mid_hz);

        let mut analyzed = Vec::with_capacity(consumed.len());
        let mut visuals = AudioVisualRegistry::new();
        visuals.set_first_send(project.audio_setup.sends.first().map(|send| &send.id));
        for (i, send) in project.audio_setup.sends.iter().enumerate() {
            if !consumed.contains(&send.id) {
                continue;
            }

            let has_cap = send.has_capture();
            let layer_sum = sum_layer_taps(send.layers(), &audio.per_layer_mono);

            let source = match (has_cap, layer_sum) {
                (true, Some(sum)) => {
                    log::info!(
                        "[OfflineAudioMod] send '{}' ({}): capture -> full export mix + \
                         layers {:?}",
                        send.label,
                        send.id,
                        send.layers()
                            .iter()
                            .map(LayerId::to_string)
                            .collect::<Vec<_>>(),
                    );
                    let mut combined = audio.master_mono.clone();
                    add_in_place(&mut combined, &sum);
                    SendSource::Own(combined)
                }
                (true, None) => {
                    log::info!(
                        "[OfflineAudioMod] send '{}' ({}): capture -> full export mix",
                        send.label,
                        send.id,
                    );
                    SendSource::Master
                }
                (false, Some(sum)) => {
                    log::info!(
                        "[OfflineAudioMod] send '{}' ({}): layers {:?}",
                        send.label,
                        send.id,
                        send.layers()
                            .iter()
                            .map(LayerId::to_string)
                            .collect::<Vec<_>>(),
                    );
                    SendSource::Own(sum)
                }
                (false, None) => {
                    log::info!(
                        "[OfflineAudioMod] send '{}' ({}): no audio in range \
                         (no capture, no reachable layer tap) — features stay default",
                        send.label,
                        send.id,
                    );
                    continue;
                }
            };

            let analyzed_send =
                build_analyzed_send(i, source, send, audio, low_hz, mid_hz, &pitch_sends);
            if visual_consumed.contains(&send.id) {
                visuals.ensure(
                    &send.id,
                    audio.sample_rate,
                    analyzed_send.analyzer.num_bins(),
                    analyzed_send.analyzer.hop(),
                );
            }
            analyzed.push(analyzed_send);
        }

        Some(Self {
            master_mono: &audio.master_mono,
            sends: analyzed,
            visuals,
            send_count: project.audio_setup.sends.len(),
            sample_rate: audio.sample_rate,
            pre_roll_samples: audio.pre_roll_samples,
            fps,
            export_origin,
            last_frame: None,
            failure: None,
        })
    }

    /// Push frame `frame_idx`'s sample window into every analyzed send and
    /// write the resulting `SendFeatures` into `engine`'s audio snapshot —
    /// call this immediately before `engine.tick(..)` for that frame (mirrors
    /// `AudioModRuntime::update`'s write, audio_mod_runtime.rs:421-431).
    ///
    /// The window is `[floor(f*rate/fps), floor((f+1)*rate/fps))`, offset by
    /// the pre-roll and clamped to the buffer length — see
    /// [`frame_sample_bounds`] for why this can't drift.
    pub fn feed_frame(
        &mut self,
        frame_idx: u32,
        engine: &mut PlaybackEngine,
    ) -> Result<(), AudioHopError> {
        if let Some(error) = self.failure {
            self.fault_snapshot(engine, error);
            return Err(error);
        }
        if self.last_frame == Some(frame_idx) {
            return Ok(());
        }
        if self.last_frame.is_none() && frame_idx != 0 {
            let error = AudioHopError::InvalidInput;
            self.failure = Some(error);
            self.fault_snapshot(engine, error);
            return Err(error);
        }
        if let Some(previous) = self.last_frame
            && frame_idx != previous.saturating_add(1)
        {
            let error = AudioHopError::InvalidInput;
            self.failure = Some(error);
            self.fault_snapshot(engine, error);
            return Err(error);
        }

        let (start, end) = frame_sample_bounds(frame_idx, self.sample_rate, self.fps);
        let pre = self.pre_roll_samples;
        let master = self.master_mono;
        let sample_rate = self.sample_rate;
        let sample_rate_f64 = sample_rate as f64;
        let export_origin = self.export_origin;
        let pre_roll_f64 = pre as f64;
        let mut feed_error = None;

        {
            let snap = engine.audio_snapshot_mut();
            snap.input_discontinuities.clear();
            snap.sends.clear();
            snap.sends.resize(self.send_count, SendFeatures::default());
            snap.hop_batches.resize_with(self.send_count, Default::default);
            for (index, batch) in snap.hop_batches.iter_mut().enumerate() {
                if self
                    .sends
                    .iter()
                    .any(|entry| entry.snapshot_index == index)
                {
                    let epoch = batch.epoch();
                    batch.begin(epoch);
                } else {
                    batch.begin(0);
                }
            }

            for entry in self.sends.iter_mut() {
                let buf: &[f32] = match &entry.source {
                    SendSource::Master => master,
                    SendSource::Own(v) => v.as_slice(),
                };
                let lo = pre.saturating_add(start).min(buf.len());
                let hi = pre.saturating_add(end).min(buf.len());
                let frame = &buf[lo..hi];
                let send_id = &entry.send_id;
                let visual = self.visuals.get(Some(send_id)).is_some();
                if visual {
                    self.visuals.feed_waveform(send_id, frame);
                }
                let batch = &mut snap.hop_batches[entry.snapshot_index];
                batch.begin(entry.epoch);
                let visuals = &mut self.visuals;
                let epoch = entry.epoch;
                let hop_size = entry.analyzer.hop();
                detect_kicks(&mut entry.kick, &mut entry.kick_fires, &mut entry.analyzer, frame);
                let mut push_error = None;
                entry.analyzer.push_with_hops(frame, |analyzed, column| {
                    if visual {
                        visuals.feed_spectrum(send_id, column);
                    }
                    let stamp = AudioHopStamp {
                        epoch,
                        end_sample: analyzed.end_sample,
                        sample_rate,
                        source_time: None,
                        timeline_time: Some(Seconds(
                            export_origin.0
                                + (analyzed.end_sample as f64 - pre_roll_f64) / sample_rate_f64,
                        )),
                    };
                    let hop = AudioFeatureHop {
                        stamp,
                        dt: Seconds(hop_size as f64 / sample_rate_f64),
                        features: analyzed.features,
                    };
                    if push_error.is_none() {
                        push_error = batch.push(hop).err();
                    }
                });
                if push_error.is_some() {
                    feed_error = push_error;
                    break;
                }
                if let Some(slot) = snap.sends.get_mut(entry.snapshot_index) {
                    *slot = entry.analyzer.latest();
                }
            }
        }
        if let Some(error) = feed_error {
            self.failure = Some(error);
            self.fault_snapshot(engine, error);
            return Err(error);
        }
        self.last_frame = Some(frame_idx);
        Ok(())
    }

    fn fault_snapshot(&mut self, engine: &mut PlaybackEngine, error: AudioHopError) {
        self.visuals.clear();
        let snap = engine.audio_snapshot_mut();
        snap.input_discontinuities.clear();
        snap.sends.clear();
        snap.sends.resize(self.send_count, SendFeatures::default());
        snap.hop_batches.resize_with(self.send_count, Default::default);
        for batch in &mut snap.hop_batches {
            batch.invalidate(error);
        }
    }

    pub fn visuals(&self) -> &AudioVisualRegistry {
        &self.visuals
    }
}

/// Construct one send's analyzer, configure it identically to the live path
/// (audio_mod_runtime.rs:342-346), and push its pre-roll (design D3).
fn build_analyzed_send(
    snapshot_index: usize,
    source: SendSource,
    send: &AudioSend,
    audio: &ExportAudio,
    low_hz: f32,
    mid_hz: f32,
    pitch_sends: &ahash::AHashSet<manifold_core::id::AudioSendId>,
) -> AnalyzedSend {
    let mut analyzer = StreamingSendAnalyzer::new(audio.sample_rate, low_hz, mid_hz);
    let epoch = new_audio_analysis_epoch();
    // audio_mod_runtime.rs:342-346 — set_crossovers is redundant with `new`'s
    // own crossover args here (nothing retunes them offline mid-export), kept
    // for parity with the live call sequence and so a future per-frame
    // crossover feature (none exists today) finds the call already in place.
    analyzer.set_crossovers(low_hz, mid_hz);
    // D5: scope/spectrogram is never driven offline.
    analyzer.set_scope(false);
    analyzer.set_pitch_tracking(pitch_sends.contains(&send.id));
    analyzer.set_floor_db(send.floor_db);

    let buf: &[f32] = match &source {
        SendSource::Master => &audio.master_mono,
        SendSource::Own(v) => v.as_slice(),
    };
    // D3: settle envelopes/decays before frame 0 with up to 1s of pre-roll.
    let preroll_end = audio.pre_roll_samples.min(buf.len());
    let mut kick = kick_detector(audio.sample_rate);
    let mut kick_fires = Vec::with_capacity(256);
    detect_kicks(&mut kick, &mut kick_fires, &mut analyzer, &buf[..preroll_end]);
    analyzer.push(&buf[..preroll_end]);

    AnalyzedSend {
        snapshot_index,
        send_id: send.id.clone(),
        source,
        analyzer,
        epoch,
        kick,
        kick_fires,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahash::AHashMap;
    use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
    use manifold_core::effects::PresetInstance;
    use manifold_core::AudioSend;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
    use manifold_core::audio_features::{AudioFeatureHop, AudioHopBatch, AudioHopError, AudioHopStamp};
    use manifold_core::audio_trigger::{FireMeterCapture, LayerClipTrigger};
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;
    use manifold_playback::live_trigger::LiveTriggerState;

    /// Push a send named `label` onto `project`, plus a layer carrying one
    /// enabled `LayerClipTrigger` sourcing it — the simplest way to make a
    /// send qualify for `analysis_consumed_sends()` without a
    /// `PresetInstance`/audio-mod fixture (P2: consumption is layer-owned
    /// now — `AudioSend::triggers` is drained legacy storage and is never
    /// read by `analysis_consumed_sends()` again). Returns a mutable
    /// reference to the pushed send so callers can still set `channels`/
    /// `source` before running the driver.
    fn consumed_send<'p>(project: &'p mut Project, label: &str) -> &'p mut AudioSend {
        let send = AudioSend::new(label);
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);

        let mut layer = Layer::new(
            format!("{label} consumer"),
            LayerType::Video,
            project.timeline.layers.len() as i32,
        );
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id,
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        });
        cfg.enabled = true;
        layer.clip_triggers.push(cfg);
        project.timeline.layers.push(layer);

        project.audio_setup.sends.last_mut().unwrap()
    }

    fn empty_export_audio(
        sample_rate: u32,
        master_mono: Vec<f32>,
        pre_roll_samples: usize,
    ) -> ExportAudio {
        ExportAudio {
            sample_rate,
            left: Vec::new(),
            right: Vec::new(),
            master_mono,
            per_layer_mono: AHashMap::new(),
            pre_roll_samples,
            audible_in_range: true,
        }
    }

    // ─── frame_sample_bounds — D1 no-drift property ───

    #[test]
    fn frame_bounds_are_contiguous_and_exact_for_integer_ratio() {
        // 48000/60 == 800 exactly.
        let mut prev_end = 0usize;
        for f in 0..10_000u32 {
            let (s, e) = frame_sample_bounds(f, 48_000, 60.0);
            assert_eq!(
                s, prev_end,
                "frame {f} start must equal the previous frame's end"
            );
            assert_eq!(
                e - s,
                800,
                "frame {f} length must be exactly 800 at 48kHz/60fps"
            );
            prev_end = e;
        }
        assert_eq!(prev_end, 10_000 * 800);
    }

    #[test]
    fn frame_bounds_are_contiguous_and_bounded_for_fractional_ratio() {
        // 44100/24 == 1837.5 -- a genuinely fractional per-frame boundary.
        let (rate, fps) = (44_100u32, 24.0);
        let mut prev_end = 0usize;
        for f in 0..10_000u32 {
            let (s, e) = frame_sample_bounds(f, rate, fps);
            assert_eq!(
                s, prev_end,
                "frame {f} start must equal the previous frame's end (no gap/overlap => no drift)"
            );
            let len = e - s;
            assert!(
                len == 1837 || len == 1838,
                "frame {f} length {len} not in {{1837,1838}}"
            );
            prev_end = e;
        }
        let expected_final = ((10_000u64 * rate as u64) as f64 / fps).floor() as usize;
        assert_eq!(
            prev_end, expected_final,
            "final boundary must equal floor(N*rate/fps) exactly"
        );
    }

    // ─── inactive project ───

    #[test]
    fn new_returns_none_when_no_send_is_consumed() {
        let project = Project::default();
        let audio = empty_export_audio(48_000, vec![0.0; 48_000], 0);
        assert!(OfflineAudioModDriver::new(&project, &audio, 60.0, Seconds::ZERO).is_none());
    }

    #[test]
    fn visualizer_only_send_drives_offline_histories_without_modulation() {
        let rate = 48_000u32;
        let layer_id = LayerId::new("visual-layer");
        let send = AudioSend::new("Visual");
        let send_id = send.id.clone();
        let mut project = Project::default();
        project.audio_setup.sends.push(send);
        project.audio_setup.sends[0].channels.clear();
        project.audio_setup.sends[0].source.layers.push(layer_id.clone());

        let mut params = std::collections::BTreeMap::new();
        params.insert(
            "send".into(),
            SerializedParamValue::String { value: send_id.to_string() },
        );
        let source = EffectGraphNode {
            id: 1,
            node_id: Default::default(),
            type_id: "node.audio_waveform".into(),
            handle: None,
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        };
        let mut visualizer = PresetInstance::new(manifold_core::PresetTypeId::new("TestVisualizer"));
        visualizer.graph = Some(EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![source],
            wires: Vec::new(),
        });
        project.settings.master_effects.push(visualizer);

        let master = vec![0.0; rate as usize * 2];
        let layer = sine_master_mono(rate, 0, 0.0, 2.0);
        let mut audio = empty_export_audio(rate, master, 0);
        audio.per_layer_mono.insert(layer_id, layer);
        let mut driver = OfflineAudioModDriver::new(&project, &audio, 60.0, Seconds::ZERO)
            .expect("visualizer source must activate offline analysis");
        let mut engine = PlaybackEngine::new(Vec::new());
        for frame in 0..120 {
            driver.feed_frame(frame, &mut engine).unwrap();
        }

        let history = driver.visuals().get(None).expect("first configured visual send");
        let mut waveform = [0.0; 64];
        history.waveform_into(&mut waveform, 100.0, false);
        assert!(waveform.iter().any(|sample| sample.abs() > 0.1));
        let bins = history.spectrum_bins();
        let mut spectrum = vec![0.0; 16 * bins];
        history.spectrum_into(&mut spectrum, 16, bins, 0.1);
        assert!(spectrum.iter().any(|magnitude| *magnitude > 0.0));
    }

    // ─── sine fixture: silence before onset, clear signal after ───

    fn sine_master_mono(
        rate: u32,
        pre_roll_samples: usize,
        silent_seconds_in_range: f32,
        total_seconds: f32,
    ) -> Vec<f32> {
        let total_len = (rate as f32 * total_seconds) as usize;
        let onset_at = pre_roll_samples + (rate as f32 * silent_seconds_in_range) as usize;
        let mut buf = vec![0.0f32; total_len];
        for (i, s) in buf.iter_mut().enumerate().skip(onset_at) {
            let t = (i - onset_at) as f32 / rate as f32;
            *s = (2.0 * std::f32::consts::PI * 220.0 * t).sin();
        }
        buf
    }

    fn burst_waveform(rate: u32, pre_roll_samples: usize, seconds: usize) -> Vec<f32> {
        let range_len = rate as usize * seconds;
        let mut buf = vec![0.0; pre_roll_samples + range_len];
        let period = rate as usize / 5;
        let burst_len = rate as usize / 80;
        for (index, sample) in buf.iter_mut().enumerate().skip(pre_roll_samples) {
            let local = index - pre_roll_samples;
            if local % period < burst_len {
                let t = local as f32 / rate as f32;
                *sample = (std::f32::consts::TAU * 440.0 * t).sin()
                    + 0.35 * (std::f32::consts::TAU * 1_200.0 * t).sin();
            }
        }
        buf
    }

    fn offline_hop_trace(fps: u32) -> Vec<AudioFeatureHop> {
        let rate = 48_000u32;
        let pre_roll = 1_237usize;
        let audio = empty_export_audio(rate, burst_waveform(rate, pre_roll, 2), pre_roll);
        let mut project = Project::default();
        consumed_send(&mut project, "Trace").channels = vec![0];
        let mut driver = OfflineAudioModDriver::new(
            &project,
            &audio,
            fps as f64,
            Seconds(17.25),
        )
        .unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        let mut trace = Vec::new();
        for frame in 0..fps * 2 {
            driver.feed_frame(frame, &mut engine).unwrap();
            trace.extend(engine.audio_snapshot().hop_batches[0].hops().iter().copied());
        }
        trace
    }

    #[test]
    fn offline_hop_trace_is_partition_invariant_with_origin_and_preroll() {
        let baseline = offline_hop_trace(60);
        assert!(!baseline.is_empty(), "waveform must produce analyzed hops");
        for hop in &baseline {
            assert_eq!(hop.stamp.sample_rate, 48_000);
            assert_eq!(hop.stamp.timeline_time,
                Some(Seconds(17.25 + (hop.stamp.end_sample as f64 - 1237.0) / 48_000.0)));
            assert_eq!(hop.stamp.epoch, baseline[0].stamp.epoch);
        }
        for fps in [1, 24, 30, 60] {
            let trace = offline_hop_trace(fps);
            assert_eq!(trace.len(), baseline.len(), "hop count differs at {fps} FPS");
            for (index, (actual, expected)) in trace.iter().zip(&baseline).enumerate() {
                assert_eq!(actual.stamp.end_sample, expected.stamp.end_sample, "hop {index}");
                assert_eq!(actual.features, expected.features, "features at hop {index}");
                assert_eq!(actual.dt, expected.dt, "dt at hop {index}");
                assert_eq!(actual.stamp.timeline_time, expected.stamp.timeline_time, "time at hop {index}");
            }
        }
    }

    fn trigger_endpoint_trace(fps: u32) -> Vec<u64> {
        let rate = 48_000u32;
        let pre_roll = 1_237usize;
        let audio = empty_export_audio(rate, burst_waveform(rate, pre_roll, 2), pre_roll);
        let mut project = Project::default();
        consumed_send(&mut project, "Trigger").channels = vec![0];
        project.timeline.layers[0].clip_triggers[0].enabled = true;
        project.timeline.layers[0].clip_triggers[0].source.feature.band = AudioBand::Full;
        project.timeline.layers[0].clip_triggers[0].shape.attack_ms = 0.0;
        project.timeline.layers[0].clip_triggers[0].shape.release_ms = 0.0;
        let mut driver = OfflineAudioModDriver::new(
            &project,
            &audio,
            fps as f64,
            Seconds(17.25),
        )
        .unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        let mut state = LiveTriggerState::default();
        let mut endpoints = Vec::new();
        for frame in 0..fps * 2 {
            driver.feed_frame(frame, &mut engine).unwrap();
            let fires = state.evaluate(
                engine.audio_snapshot(),
                &project.audio_setup,
                &project.timeline.layers,
                Seconds(1.0 / fps as f64),
                &mut FireMeterCapture::default(),
            );
            endpoints.extend(
                fires
                    .into_iter()
                    .filter_map(|fire| fire.audio_stamp.map(|stamp| stamp.end_sample)),
            );
        }
        endpoints
    }

    #[test]
    fn live_trigger_fire_endpoints_are_partition_invariant() {
        let baseline = trigger_endpoint_trace(60);
        assert!(!baseline.is_empty(), "burst waveform must fire a live trigger");
        for fps in [1, 24, 30, 60] {
            assert_eq!(trigger_endpoint_trace(fps), baseline, "fires differ at {fps} FPS");
        }
    }

    #[test]
    fn frame_sequence_rejects_first_skip_and_latches_duplicate_order_errors() {
        let rate = 48_000u32;
        let audio = empty_export_audio(rate, burst_waveform(rate, 0, 2), 0);
        let mut project = Project::default();
        consumed_send(&mut project, "Sequence").channels = vec![0];

        let mut first_skip = OfflineAudioModDriver::new(&project, &audio, 60.0, Seconds::ZERO).unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        assert_eq!(first_skip.feed_frame(1, &mut engine), Err(AudioHopError::InvalidInput));
        assert_eq!(first_skip.feed_frame(0, &mut engine), Err(AudioHopError::InvalidInput));

        let mut order = OfflineAudioModDriver::new(&project, &audio, 60.0, Seconds::ZERO).unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        order.feed_frame(0, &mut engine).unwrap();
        let before = engine.audio_snapshot().hop_batches[0].hops().to_vec();
        order.feed_frame(0, &mut engine).unwrap();
        assert_eq!(engine.audio_snapshot().hop_batches[0].hops(), before.as_slice());
        assert_eq!(order.feed_frame(2, &mut engine), Err(AudioHopError::InvalidInput));
        assert_eq!(order.feed_frame(1, &mut engine), Err(AudioHopError::InvalidInput));
    }

    #[test]
    fn unavailable_send_clears_previous_active_batch() {
        let rate = 48_000u32;
        let audio = empty_export_audio(rate, burst_waveform(rate, 0, 1), 0);
        let mut project = Project::default();
        consumed_send(&mut project, "Active").channels = vec![0];
        project.audio_setup.sends.push(AudioSend::new("Unavailable"));
        let mut driver = OfflineAudioModDriver::new(&project, &audio, 60.0, Seconds::ZERO).unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        let mut stale = AudioHopBatch::default();
        stale.begin(99);
        stale.push(AudioFeatureHop {
            stamp: AudioHopStamp { epoch: 99, end_sample: 512, sample_rate: rate, source_time: None, timeline_time: None },
            dt: Seconds(512.0 / rate as f64),
            features: SendFeatures::default(),
        }).unwrap();
        engine.audio_snapshot_mut().hop_batches = vec![AudioHopBatch::default(), stale];
        driver.feed_frame(0, &mut engine).unwrap();
        let unavailable = &engine.audio_snapshot().hop_batches[1];
        assert_eq!(unavailable.epoch(), 0);
        assert!(unavailable.hops().is_empty());
        assert_eq!(unavailable.failure(), None);
    }

    #[test]
    fn capacity_failure_invalidates_all_batch_outputs_and_visuals() {
        let rate = 48_000;
        let audio = empty_export_audio(rate, burst_waveform(rate, 0, 1), 0);
        let mut project = Project::default();
        consumed_send(&mut project, "Accepted prefix").channels = vec![0];
        consumed_send(&mut project, "Overflow").channels = vec![0];
        let mut driver = OfflineAudioModDriver::new(&project, &audio, 1.0, Seconds::ZERO).unwrap();
        let send_id = project.audio_setup.sends[0].id.clone();
        driver.visuals.ensure(&send_id, rate, driver.sends[0].analyzer.num_bins(), driver.sends[0].analyzer.hop());
        let mut engine = PlaybackEngine::new(Vec::new());
        engine.audio_snapshot_mut().hop_batches = vec![AudioHopBatch::default(), AudioHopBatch::with_capacity(1)];
        assert_eq!(driver.feed_frame(0, &mut engine), Err(AudioHopError::CapacityExceeded));
        assert!(engine.audio_snapshot().hop_batches.iter().all(|batch| {
            batch.hops().is_empty() && batch.failure() == Some(AudioHopError::CapacityExceeded)
        }));
        assert!(engine.audio_snapshot().sends.iter().all(|features| *features == SendFeatures::default()));
        assert_eq!(driver.feed_frame(0, &mut engine), Err(AudioHopError::CapacityExceeded));
        let history = driver.visuals().get(Some(&send_id)).unwrap();
        let mut waveform = [1.0; 8];
        history.waveform_into(&mut waveform, 10.0, false);
        assert!(waveform.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn sine_fixture_full_band_amplitude_silent_before_onset_and_nonzero_after() {
        let rate = 48_000u32;
        let pre_roll = rate as usize; // 1s
        // Silence continues 1s into the main range too, so frame 0 (range
        // start) is still silent; sine starts 1s into the range. Total
        // buffer is 5s (pre-roll 1s + 4s range) so a 170-frame walk at
        // 60fps (< 240 frames == the full 4s range) stays well inside the
        // buffer, never touching the clamped-to-empty tail.
        let master = sine_master_mono(rate, pre_roll, 1.0, 5.0);

        let audio = empty_export_audio(rate, master, pre_roll);
        let mut project = Project::default();
        consumed_send(&mut project, "Kick").channels = vec![0, 1]; // capture-fed -> Master source

        let fps = 60.0;
        let mut driver = OfflineAudioModDriver::new(&project, &audio, fps, Seconds::ZERO)
            .expect("a send with an enabled clip trigger must be consumed");
        let mut engine = PlaybackEngine::new(Vec::new());

        // Frame 0 == range start == still inside the silent second.
        driver.feed_frame(0, &mut engine).unwrap();
        let silent = engine.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;

        // Drive forward well past the onset (range frame 60 == t=2.0s ==
        // sine start; frame 170 == t=3.83s, comfortably into steady signal)
        // so the analyzer's window is full of signal, not straddling the
        // transition.
        let mut loud = silent;
        for f in 1..=170u32 {
            driver.feed_frame(f, &mut engine).unwrap();
            loud = engine.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;
        }

        assert!(
            silent < 0.05,
            "expected near-zero amplitude before onset, got {silent}"
        );
        assert!(
            loud > silent + 0.2,
            "expected clearly higher amplitude after onset ({loud} vs {silent})"
        );
    }

    // ─── determinism (D4) ───

    #[test]
    fn two_runs_over_the_same_inputs_are_bit_identical() {
        let rate = 48_000u32;
        let pre_roll = rate as usize;
        let master = sine_master_mono(rate, pre_roll, 0.5, 3.0);
        let audio = empty_export_audio(rate, master, pre_roll);

        let mut project = Project::default();
        consumed_send(&mut project, "Kick");
        let fps = 30.0;

        let run = || {
            let mut driver = OfflineAudioModDriver::new(&project, &audio, fps, Seconds::ZERO).unwrap();
            let mut engine = PlaybackEngine::new(Vec::new());
            let mut out = Vec::new();
            for f in 0..120u32 {
                driver.feed_frame(f, &mut engine).unwrap();
                out.push(engine.audio_snapshot().sends[0]);
            }
            out
        };

        let a = run();
        let b = run();
        assert_eq!(a.len(), b.len());
        for (i, (fa, fb)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(fa, fb, "frame {i} diverged between two identical runs");
        }
    }

    #[test]
    fn offline_features_match_at_shared_sample_boundaries_across_frame_rates() {
        // Exercise the actual export driver, including non-hop-aligned pre-roll.
        // The latest snapshot is compared at matching AUDIO window ends, not at
        // frame starts: feed_frame intentionally analyzes that frame's interval.
        // This proves source analysis only; modulation still evaluates per tick.
        let rate = 44_100u32;
        let pre_roll = 4_413usize;
        let mono: Vec<_> = (0..pre_roll + rate as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let burst = if i % 7_001 < 1_103 { 0.8 } else { 0.05 };
                burst * ((std::f32::consts::TAU * 180.0 * t).sin()
                    + 0.25 * (std::f32::consts::TAU * 1_800.0 * t).sin())
            })
            .collect();
        let audio = empty_export_audio(rate, mono, pre_roll);
        let mut project = Project::default();
        consumed_send(&mut project, "Physics control source").channels = vec![0];

        let run = |fps: u32| {
            let mut driver = OfflineAudioModDriver::new(&project, &audio, fps as f64, Seconds::ZERO).unwrap();
            let mut engine = PlaybackEngine::new(Vec::new());
            let mut shared = Vec::new();
            for frame in 0..fps {
                driver.feed_frame(frame, &mut engine).unwrap();
                if (frame + 1) % (fps / 6) == 0 {
                    shared.push(engine.audio_snapshot().sends[0]);
                }
            }
            shared
        };
        let expected = run(60);
        assert!(expected.iter().any(|f| f.bands[0].amplitude > 0.1));
        assert!(expected.windows(2).any(|pair| pair[0] != pair[1]));
        assert_eq!(run(24), expected, "24 FPS changed the audio analysis");
        assert_eq!(run(30), expected, "30 FPS changed the audio analysis");
    }

    #[test]
    fn offline_control_observations_preserve_every_hop_across_frame_rates() {
        use manifold_core::audio_mod::ParameterAudioMod;
        use manifold_core::control_history::{ControlContribution, TriggerSourceStamp};
        use manifold_core::params::Param;
        use manifold_core::{Beats, PresetTypeId};
        use manifold_playback::modulation::{control_capture_error, evaluate_modulation};

        let rate = 44_100u32;
        let pre_roll = 4_413usize;
        let mono = (0..pre_roll + rate as usize).map(|i| {
            let gain = if i % 7_001 < 1_103 { 0.8 } else { 0.05 };
            gain * (std::f32::consts::TAU * 180.0 * i as f32 / rate as f32).sin()
        }).collect();
        let audio = empty_export_audio(rate, mono, pre_roll);
        let send = AudioSend::new("Control capture");
        let mut fx = PresetInstance::new(PresetTypeId::new("ControlCaptureTest"));
        fx.params.push(Param::bundled(serde_json::from_value(serde_json::json!({
            "id": "force", "name": "Force", "min": 0.0, "max": 1.0, "defaultValue": 0.0
        })).unwrap()));
        let mut modulation = ParameterAudioMod::new("force".into(), send.id.clone(),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full));
        modulation.shape.attack_ms = 70.0;
        modulation.shape.release_ms = 180.0;
        fx.audio_mods = Some(vec![modulation]);
        let mut original = Project::default();
        original.audio_setup.sends.push(send);
        original.audio_setup.sends[0].channels = vec![0];
        original.settings.master_effects.push(fx);

        let run = |fps: u32| {
            let mut project = original.clone();
            let mut driver = OfflineAudioModDriver::new(&project, &audio, fps as f64, Seconds(17.25)).unwrap();
            let mut engine = PlaybackEngine::new(Vec::new());
            let mut trace = Vec::new();
            let controls = manifold_playback::clip_controls::ClipControlFrame::default();
            let mut pulses = Vec::new();
            let mut meters = FireMeterCapture::default();
            for frame in 0..fps {
                driver.feed_frame(frame, &mut engine).unwrap();
                let current = Seconds(17.25 + frame as f64 / fps as f64);
                evaluate_modulation(&mut project, Beats(current.0 * 2.0), current,
                    Seconds(1.0 / fps as f64), engine.audio_snapshot(),
                    &controls, &mut pulses, &mut meters);
                assert!(control_capture_error(&project).is_none());
                let m = &project.settings.master_effects[0].audio_mods.as_ref().unwrap()[0];
                assert_eq!(m.control_observations.observations().len(), engine.audio_snapshot().hop_batches[0].hops().len());
                for observation in m.control_observations.observations() {
                    assert_eq!(observation.evaluation_time, Some(current));
                    let TriggerSourceStamp::Audio { stamp, .. } = observation.stamp else { panic!("expected an audio observation"); };
                    trace.push((stamp.end_sample, stamp.timeline_time.unwrap(),
                        observation.dt, observation.contribution));
                }
            }
            trace
        };
        let expected = run(60);
        assert!(expected.len() > 60, "more control updates than display frames");
        assert!(expected.iter().all(|sample| matches!(sample.3, ControlContribution::Continuous(_))));
        assert!(expected.windows(2).any(|pair| pair[0].3 != pair[1].3));
        for fps in [24, 30, 1] {
            assert_eq!(run(fps), expected, "{fps} FPS changed retained controls");
        }
    }

    // ─── D2 source mapping ───

    #[test]
    fn capture_only_send_reads_master_mono() {
        let rate = 48_000u32;
        let master = sine_master_mono(rate, 0, 0.0, 2.0);
        let audio = empty_export_audio(rate, master, 0);

        let mut project = Project::default();
        consumed_send(&mut project, "Master tap").channels = vec![0, 1]; // has_capture() == true, no layers

        let mut driver = OfflineAudioModDriver::new(&project, &audio, 60.0, Seconds::ZERO).unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        for f in 0..30u32 {
            driver.feed_frame(f, &mut engine).unwrap();
        }
        let amp = engine.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;
        assert!(
            amp > 0.1,
            "capture-fed send should read the master mix's signal, got {amp}"
        );
    }

    #[test]
    fn mixed_capture_and_layer_send_sums_both_sources() {
        let rate = 48_000u32;
        // Master mix carries the signal; the layer tap is silence. If the
        // "both" branch ignores the master and reads only the layer, this
        // send would read as silent — it must not.
        let master = sine_master_mono(rate, 0, 0.0, 2.0);
        let layer_id = LayerId::new("layer-silent");
        let mut per_layer = AHashMap::new();
        per_layer.insert(layer_id.clone(), vec![0.0f32; master.len()]);
        let mut audio = empty_export_audio(rate, master, 0);
        audio.per_layer_mono = per_layer;

        let mut project = Project::default();
        let send = consumed_send(&mut project, "Both A");
        send.channels = vec![0, 1];
        send.source.layers.push(layer_id);

        let mut driver = OfflineAudioModDriver::new(&project, &audio, 60.0, Seconds::ZERO).unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        for f in 0..30u32 {
            driver.feed_frame(f, &mut engine).unwrap();
        }
        let amp = engine.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;
        assert!(
            amp > 0.1,
            "master's signal must still contribute when a silent layer is also routed, got {amp}"
        );

        // And the reverse: silent master, signal-carrying layer.
        let rate2 = 48_000u32;
        let silent_master = vec![0.0f32; (rate2 as f32 * 2.0) as usize];
        let signal_layer = sine_master_mono(rate2, 0, 0.0, 2.0);
        let layer_id2 = LayerId::new("layer-loud");
        let mut per_layer2 = AHashMap::new();
        per_layer2.insert(layer_id2.clone(), signal_layer);
        let mut audio2 = empty_export_audio(rate2, silent_master, 0);
        audio2.per_layer_mono = per_layer2;

        let mut project2 = Project::default();
        let send2 = consumed_send(&mut project2, "Both B");
        send2.channels = vec![0, 1];
        send2.source.layers.push(layer_id2);

        let mut driver2 = OfflineAudioModDriver::new(&project2, &audio2, 60.0, Seconds::ZERO).unwrap();
        let mut engine2 = PlaybackEngine::new(Vec::new());
        for f in 0..30u32 {
            driver2.feed_frame(f, &mut engine2).unwrap();
        }
        let amp2 = engine2.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;
        assert!(
            amp2 > 0.1,
            "layer's signal must still contribute when the master is silent, got {amp2}"
        );
    }

    #[test]
    fn unrouted_consumed_send_stays_at_default_features() {
        // Consumed (via a layer's clip trigger) but neither capture nor
        // layers are wired — honest silence (D2), and the send is simply not
        // analyzed.
        let rate = 48_000u32;
        let master = sine_master_mono(rate, 0, 0.0, 2.0);
        let audio = empty_export_audio(rate, master, 0);

        let mut project = Project::default();
        // Empty channels → layer-only / unrouted, so no capture is read (a fresh
        // send now defaults to stereo capture; clear it to keep this unrouted).
        consumed_send(&mut project, "Unrouted").channels.clear();

        let mut driver = OfflineAudioModDriver::new(&project, &audio, 60.0, Seconds::ZERO)
            .expect("driver still builds - the send IS consumed, it just has no source");
        let mut engine = PlaybackEngine::new(Vec::new());
        driver.feed_frame(0, &mut engine).unwrap();
        assert_eq!(
            engine.audio_snapshot().sends[0],
            SendFeatures::default(),
            "an unrouted consumed send must stay at default features, never read the master mix"
        );
    }
}
