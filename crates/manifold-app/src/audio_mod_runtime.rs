//! Audio-modulation capture runtime.
//!
//! Owns the always-on audio capture device + downmix worker for audio
//! modulation, and feeds the playback engine a fresh per-send feature snapshot
//! each tick. Lives on the content thread (which owns the engine and, through
//! it, the project). Step 3 of `docs/AUDIO_MODULATION_DESIGN.md`.
//!
//! ## One analyzer per send
//!
//! Analysis runs **here**, on the content thread, one [`StreamingSendAnalyzer`]
//! per send. The capture worker only downmixes the device to per-send mono; the
//! content thread sums that with the send's audio-layer taps (resampled to a
//! common rate) and pushes the mix into the send's single analyzer. So a send
//! can be fed by the capture device, by audio layers, or by **both at once** —
//! one analysis drives features, the spectrogram scope, and the per-band meters
//! ("what you hear is what modulates"). See `docs/AUDIO_LAYER_DESIGN.md` section 3R.
//!
//! Lifecycle is **gated and self-healing**: the capture worker runs only while
//! the project has at least one capture-fed send and either an active audio
//! modulation or the Audio Setup scope open. The device and worker rebuild when
//! the capture-relevant config changes (device or any send's channels) — a
//! relabel alone does not restart capture. A missing device leaves capture dark
//! until the user re-points it (the remappable device policy).

mod input;

use input::InputBatch;
use std::sync::Arc;

use ahash::AHashMap;

use manifold_audio::analysis::{
    AudioFeatureWorker, GainBank, LinearResampler, MonoReader, StreamingSendAnalyzer,
};
use manifold_audio::capture::{self, CaptureBackend, CaptureSource};
use manifold_audio::detectors::{DetectorEvent, DetectorWorker, EventKind, kick_detector};
use manifold_core::{AudioSend, LayerId, Seconds, SendFeatures};
use manifold_core::audio_features::{AudioFeatureHop, AudioHopBatch, AudioHopError, AudioHopStamp,
    AudioInputDiscontinuity, AudioInputProblem, AudioInputSource, new_audio_analysis_epoch};
use manifold_core::audio_setup::{AudioDeviceRef, AudioSetup, AudioSourceKind};
use manifold_core::id::AudioSendId;
use manifold_core::project::Project;
use manifold_playback::audio_layer_playback::AudioLayerPlayback;
use manifold_playback::engine::PlaybackEngine;

/// Longest an update waits for the detector workers, across all sends (docs/KICK_REALTIME_DESIGN.md
/// section 1b). A worker that misses it delivers its fires one update late.
const DETECTOR_WAIT: std::time::Duration = std::time::Duration::from_millis(2);

/// The capture-relevant fingerprint of an [`AudioSetup`]. Changes here force a
/// device/worker rebuild; label-only or capture-flag-only edits compare equal and
/// don't. The device is keyed by its stable [`AudioDeviceRef`] (UID), so renaming
/// the OS device doesn't churn capture — it re-resolves to the same hardware.
#[derive(Clone, PartialEq, Default)]
struct CaptureSignature {
    device: Option<AudioDeviceRef>,
    /// Channels per send in send order — the order is significant because it is
    /// the worker's frame index.
    sends: Vec<Vec<u16>>,
}

/// Changes to actual visualizer inputs invalidate history; labels and gain
/// edits do not. Compared only when the project changes.
#[derive(PartialEq)]
struct VisualRoutingSignature {
    device: Option<AudioDeviceRef>,
    sends: Vec<(AudioSendId, Vec<u16>, Vec<manifold_core::LayerId>)>,
}

impl CaptureSignature {
    fn from_setup(setup: &AudioSetup) -> Self {
        Self {
            device: setup.device.clone(),
            sends: setup.sends.iter().map(|s| s.channels.clone()).collect(),
        }
    }
}

/// A live capture: the device captures, the worker downmixes each send to mono.
/// Both are kept alive by ownership here; dropping the struct stops capture and
/// joins the worker.
struct AudioModCapture {
    _backend: Box<dyn CaptureBackend>,
    _worker: AudioFeatureWorker,
    /// Read end of the worker's per-send mono streams.
    mono: MonoReader,
    signature: CaptureSignature,
    /// Live per-send gain shared with the worker. A gain-only edit writes here
    /// in place — no capture restart (gain isn't in [`CaptureSignature`]).
    gains: Arc<GainBank>,
}

/// One send's content-thread analysis: the single [`StreamingSendAnalyzer`] that
/// sees the send's whole input (capture mono + layer taps, summed), plus a
/// resampler that aligns layer taps to the analyzer's rate when a capture send
/// also pulls in layers at a different rate.
struct SendAnalyzer {
    /// The rate the analyzer (and its features / scope columns) is built for.
    rate: u32,
    epoch: u64,
    analyzer: StreamingSendAnalyzer,
    received_frames: u64,
    /// Layer-tap → analyzer-rate resampler, built lazily; `(from_rate, state)`.
    resampler: Option<(u32, LinearResampler)>,
    capture_generation: Option<u64>,
    channels: Vec<u16>,
    layers: Vec<LayerId>,
    /// The send's trained event detectors (kick) on their own thread, fed the same mono
    /// from the same origin as `analyzer`. `None` when the model was refused or the thread
    /// could not start: kick then stays 0.
    detectors: Option<DetectorWorker>,
    /// This update's mixed mono, held between [`Self::stage`] and analysis.
    staged: Vec<f32>,
}

impl SendAnalyzer {
    fn new(rate: u32, low_hz: f32, mid_hz: f32, send: &AudioSend, capture_generation: Option<u64>) -> Self {
        Self {
            rate,
            epoch: new_audio_analysis_epoch(),
            analyzer: StreamingSendAnalyzer::new(rate, low_hz, mid_hz),
            received_frames: 0,
            resampler: None,
            capture_generation,
            channels: send.channels.clone(),
            layers: send.layers().to_vec(),
            detectors: kick_detector(rate).and_then(|kick| {
                DetectorWorker::spawn(vec![Box::new(kick)])
                    .inspect_err(|e| log::error!("[AudioMod] detector worker did not start: {e}"))
                    .ok()
            }),
            staged: Vec::new(),
        }
    }

    /// Holds this update's mixed mono for analysis and hands it to the detector worker, so
    /// the worker runs while the other sends are mixed.
    fn stage(&mut self, mono: &[f32]) {
        self.staged.clear();
        self.staged.extend_from_slice(mono);
        if let Some(worker) = self.detectors.as_mut() {
            worker.submit(mono);
        }
    }

    /// Queues the worker's kick fires into the analyzer, waiting at most until `deadline`.
    /// Fires that miss it are queued on a later update and land on the first hop analysed
    /// after their sample (one frame late).
    fn collect_events(&mut self, deadline: std::time::Instant, scratch: &mut Vec<DetectorEvent>) {
        let Some(worker) = self.detectors.as_mut() else { return };
        scratch.clear();
        worker.collect(deadline, scratch);
        self.analyzer
            .queue_kick_fires(scratch.iter().filter(|e| e.kind == EventKind::Kick).map(|e| e.sample));
    }

    fn analyze_hops(
        &mut self,
        mono: &[f32],
        batch: &mut AudioHopBatch,
        mut source_time: impl FnMut(usize) -> Option<std::time::Instant>,
        mut spectrum: impl FnMut(&[f32]),
    ) -> Result<(), AudioHopError> {
        batch.begin(self.epoch);
        if let Some(error) = batch.failure() { return Err(error); }
        let first_frame = self.received_frames;
        let Some(end_frame) = first_frame.checked_add(mono.len() as u64) else {
            batch.invalidate(AudioHopError::InvalidInput);
            return Err(AudioHopError::InvalidInput);
        };
        self.received_frames = end_frame;
        let dt = Seconds(self.analyzer.hop() as f64 / f64::from(self.rate));
        let epoch = self.epoch;
        let sample_rate = self.rate;
        self.analyzer.push_with_hops(mono, |hop, column| {
            let Some(offset) = hop.end_sample.checked_sub(first_frame)
                .and_then(|offset| usize::try_from(offset).ok())
                .filter(|offset| *offset <= mono.len()) else {
                batch.invalidate(AudioHopError::InvalidInput);
                return;
            };
            let result = batch.push(AudioFeatureHop {
                stamp: AudioHopStamp {
                    epoch, end_sample: hop.end_sample, sample_rate,
                    // Mixed capture/layer samples have no transport anchor yet.
                    source_time: source_time(offset),
                    timeline_time: None,
                },
                dt,
                features: hop.features,
            });
            if result.is_ok() { spectrum(column); }
        });
        batch.failure().map_or(Ok(()), Err)
    }
}

/// Content-thread-owned runtime that reconciles capture against the project and
/// feeds the engine its feature snapshot.
pub struct AudioModRuntime {
    /// Per-send waveform and spectrum histories fed from the same mixed mono
    /// stream as the modulation analyzer.
    visuals: manifold_core::audio_visual::AudioVisualRegistry,
    visual_routing: Option<VisualRoutingSignature>,
    capture: Option<AudioModCapture>,
    /// Last project data-version reconciled against — reconcile only runs when
    /// the project changed, not every frame.
    last_version: u64,
    /// Device directory, used to resolve the stored device ref to an openable
    /// name and to hold the hot-plug subscription.
    directory: Box<dyn manifold_audio::directory::AudioDeviceDirectory>,
    /// Set by the hot-plug listener (on a HAL thread) when the device set or
    /// default device changes; drained each tick to force a re-resolve.
    devices_dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set by the process-list listener when an audio-producing app launches or
    /// quits; drained each tick. Acted on **only** when the current source is an
    /// app tap, so device / system-audio captures don't churn on unrelated apps.
    processes_dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Explicit seek/transport boundaries clear history; ordinary clock nudges
    /// and steady live capture while stopped preserve it.
    last_transport_epoch: u64,
    /// Hot-plug subscription guard — unregisters the device listener on drop.
    _hotplug_sub: manifold_audio::directory::Subscription,
    /// Process-list subscription guard — unregisters the listener on drop.
    _process_sub: manifold_audio::directory::Subscription,
    /// Whether we've already triggered the one-time mic permission prompt.
    mic_access_requested: bool,
    /// Send the Audio Setup scope is showing, if open. The tapped send's analyzer
    /// buffers scope columns; the per-band meters read its snapshot slot.
    spec_send: Option<AudioSendId>,
    /// Set when `spec_send` changes, forcing a reconcile next tick — selecting a
    /// send opens the scope, which can start capture even with no audio mods yet
    /// (calibration precedes assignment).
    spec_dirty: bool,
    /// Per-send analyzers, keyed by send id. Each is the single source of features
    /// + scope columns for its send, fed the send's whole (mixed) input.
    analyzers: AHashMap<AudioSendId, SendAnalyzer>,
    /// D7 activation set (AUDIO_OBJECT_TRACKING P4): sends with at least one
    /// enabled Pitch/Presence mod. Recomputed only on a data-version change;
    /// switches each analyzer's ridge tracker on/off per tick (byte-identical
    /// analysis when off, so unbound projects pay nothing).
    pitch_sends: ahash::AHashSet<AudioSendId>,
    /// D4 activation set (AUDIO_SENDS_UX_DESIGN section 3.2): sends with at least one
    /// enabled audio mod or enabled trigger route — `Project::analysis_consumed_sends`.
    /// Recomputed only on a data-version change, mirroring `pitch_sends`. The
    /// per-send loop skips any send outside this set that also isn't the
    /// scope-tapped send: no mono push, no analyzer entry. Makes analysis cost
    /// proportional to what's actually bound, not to `sends.len()`.
    consumed: ahash::AHashSet<AudioSendId>,
    /// Sends read by enabled waveform/spectrum graph sources. These are kept
    /// separate so visual history storage does not grow for modulation-only
    /// sends.
    visual_consumed: ahash::AHashSet<AudioSendId>,
    /// Snapshot index (project send order) of the scope-tapped send, for the
    /// per-band meters. Resolved each tick.
    tapped_index: Option<usize>,
    // ── Reusable scratch (no per-tick allocation once warmed) ──
    /// Stamped capture mono, interleaved by send, drained once per update.
    capture_batch: InputBatch,
    capture_generation: u64,
    input_update: u64,
    layer_batches: AHashMap<LayerId, InputBatch>,
    /// Summed layer taps for one send, before resampling.
    layer_mix: Vec<f32>,
    /// One send's final mixed mono (capture + layers), pushed to its analyzer.
    mono_mix: Vec<f32>,
    /// Resampled layer mono (layer rate → analyzer rate) for one send.
    resampled: Vec<f32>,
    /// Send indices staged this update, analysed after their detectors are collected.
    staged_sends: Vec<usize>,
    /// Detector events collected for one send.
    detector_events: Vec<DetectorEvent>,
    /// Cached at construction from `MANIFOLD_AUDIO_TRACE` — the P1 gate
    /// instrument (`docs/AUDIO_SENDS_UX_DESIGN.md` section 4 Phase 1). Checked once,
    /// not per tick, so the trace stays zero-cost when unset.
    trace: bool,
}

impl Default for AudioModRuntime {
    fn default() -> Self {
        use std::sync::atomic::Ordering;
        let directory = manifold_audio::directory::system_directory();
        let devices_dirty = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = devices_dirty.clone();
        // The callbacks fire on arbitrary HAL threads — only flip an atomic.
        let sub = directory.subscribe(Box::new(move || flag.store(true, Ordering::Relaxed)));
        let processes_dirty = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pflag = processes_dirty.clone();
        let process_sub =
            directory.subscribe_processes(Box::new(move || pflag.store(true, Ordering::Relaxed)));
        Self {
            visuals: manifold_core::audio_visual::AudioVisualRegistry::new(),
            visual_routing: None,
            capture: None,
            last_version: u64::MAX,
            directory,
            devices_dirty,
            processes_dirty,
            last_transport_epoch: u64::MAX,
            _hotplug_sub: sub,
            _process_sub: process_sub,
            mic_access_requested: false,
            spec_send: None,
            spec_dirty: false,
            analyzers: AHashMap::new(),
            pitch_sends: ahash::AHashSet::new(),
            consumed: ahash::AHashSet::new(),
            visual_consumed: ahash::AHashSet::new(),
            tapped_index: None,
            capture_batch: InputBatch::default(),
            capture_generation: 0,
            input_update: 0,
            layer_batches: AHashMap::new(),
            layer_mix: Vec::new(),
            mono_mix: Vec::new(),
            resampled: Vec::new(),
            staged_sends: Vec::new(),
            detector_events: Vec::with_capacity(256),
            trace: std::env::var_os("MANIFOLD_AUDIO_TRACE").is_some(),
        }
    }
}

impl AudioModRuntime {
    /// Reconcile the capture lifecycle (when the project changed, or a device
    /// hot-plugged) and feed the engine the latest feature snapshot. Call once
    /// per tick, before `engine.tick`. `layer_playback` (when present) carries the
    /// per-layer post-fader taps that feed layer-fed sends.
    pub fn update(
        &mut self,
        engine: &mut PlaybackEngine,
        data_version: u64,
        mut layer_playback: Option<&mut AudioLayerPlayback>,
    ) {
        let mut discontinuities = std::mem::take(&mut engine.audio_snapshot_mut().input_discontinuities);
        discontinuities.clear();
        self.input_update = self.input_update.wrapping_add(1);
        let transport_epoch = engine.transport_epoch();
        if transport_epoch != self.last_transport_epoch {
            self.visuals.clear();
            // A seek/export boundary starts a new analysis identity too. In
            // particular, returning from offline analysis must not resume an
            // older live epoch that evaluators would correctly reject as stale.
            self.analyzers.clear();
            self.last_transport_epoch = transport_epoch;
        }
        let hotplugged = self
            .devices_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        let processes_changed = self
            .processes_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        // The current source's kind decides which hot-plug signals are relevant.
        // `None` (system default input) counts as a hardware-device source.
        let source_kind = engine
            .project()
            .and_then(|p| p.audio_setup.device.as_ref().map(|d| d.kind));

        // A device hot-plug matters only to a hardware-device source. A tap
        // doesn't resolve against the device list — and critically, creating our
        // own private aggregate device fires this very notification, so acting on
        // it for a tap would tear the tap down and rebuild it every tick in a
        // feedback loop. Ignore device churn while a tap is the source.
        let device_rebuild =
            hotplugged && matches!(source_kind, None | Some(AudioSourceKind::InputDevice));
        // A process-list change matters only to an app tap (re-resolve its live
        // process handle), so an unrelated app starting/stopping audio — or our
        // aggregate churn — never rebuilds a device or system-audio capture.
        let app_rebuild = processes_changed && matches!(source_kind, Some(AudioSourceKind::App));
        if data_version != self.last_version || device_rebuild || app_rebuild || self.spec_dirty {
            if device_rebuild || app_rebuild {
                self.visuals.clear();
            }
            if device_rebuild || app_rebuild {
                // The device or the tapped app appeared/vanished/changed — drop
                // capture so reconcile rebuilds against the current state (the
                // stored ref is unchanged, so the signature alone wouldn't fire).
                self.capture = None;
            }
            let next_visual_consumed = engine
                .project()
                .map(crate::audio_visualization::visualizer_consumed_sends)
                .unwrap_or_default();
            let visual_routing = engine.project().map(|project| VisualRoutingSignature {
                device: project.audio_setup.device.clone(),
                sends: project.audio_setup.sends.iter()
                    .filter(|send| next_visual_consumed.contains(&send.id))
                    .map(|send| (send.id.clone(), send.channels.clone(), send.layers().to_vec()))
                    .collect(),
            });
            if visual_routing != self.visual_routing {
                self.visuals.clear();
                self.visual_routing = visual_routing;
            }
            self.visual_consumed = next_visual_consumed;
            if let Some(project) = engine.project() {
                self.visuals
                    .set_first_send(project.audio_setup.sends.first().map(|send| &send.id));
            } else {
                self.visuals.set_first_send(None);
            }
            self.reconcile(engine.project());
            // D4 activation set (AUDIO_SENDS_UX_DESIGN section 3.2): recomputed only here,
            // never per tick.
            self.consumed = engine
                .project()
                .map(|p| {
                    let mut consumed = p.analysis_consumed_sends();
                    consumed.extend(self.visual_consumed.iter().cloned());
                    consumed
                })
                .unwrap_or_default();
            // Drop analyzers for sends that no longer exist, or that are no longer
            // consumed and aren't the scope-tapped send (runs only on a project
            // change, never per tick — keeps the map bounded without allocating on
            // the hot path).
            if let Some(project) = engine.project() {
                let consumed = &self.consumed;
                let spec_send = &self.spec_send;
                self.analyzers.retain(|id, _| {
                    project.audio_setup.sends.iter().any(|s| &s.id == id)
                        && (consumed.contains(id) || spec_send.as_ref() == Some(id))
                });
                let visual_ids: Vec<_> = project
                    .audio_setup
                    .sends
                    .iter()
                    .filter(|send| self.visual_consumed.contains(&send.id))
                    .map(|send| send.id.clone())
                    .collect();
                self.visuals.remove_unlisted(&visual_ids);
                self.layer_batches.retain(|id, _| {
                    project.audio_setup.sends.iter().any(|send| send.layers().contains(id))
                });
            }
            self.pitch_sends = engine
                .project()
                .map(|p| p.sends_with_pitch_mods())
                .unwrap_or_default();
            self.last_version = data_version;
            self.spec_dirty = false;
        }

        // Analysis runs when something reads it: an active param modulation, an
        // active live trigger (fires clips even with the scope closed), or the
        // Audio Setup scope is open for calibration.
        let (active, send_count) = engine.project().map_or((false, 0), |p| {
            let needs = p.has_active_audio_mods()
                || self.spec_send.is_some()
                || p.has_active_clip_triggers()
                || !self.visual_consumed.is_empty();
            (needs, p.audio_setup.sends.len())
        });
        if !active {
            self.visuals.clear();
        }

        // The rate the capture worker delivers mono at (also the analyzer rate for
        // any capture-fed send).
        let device_rate = self.capture.as_ref().map(|c| c.mono.sample_rate());

        self.capture_batch.begin(self.input_update);
        if let Some(cap) = self.capture.as_mut() {
            let channels = cap.mono.send_count().max(1);
            let batch = &mut self.capture_batch;
            cap.mono.drain_stamped(|read, samples| {
                batch.consume(read, samples, channels, |problem| {
                    discontinuities.push(AudioInputDiscontinuity { source: AudioInputSource::Capture, problem });
                });
            });
        }

        // Drain each physical layer tap once, then share its immutable batch
        // across every consuming send. Draining inside the send loop starved
        // the second send when both selected the same layer.
        if active && let (Some(project), Some(pb)) = (engine.project(), layer_playback.as_deref_mut()) {
            for send in &project.audio_setup.sends {
                if self.spec_send.as_ref() != Some(&send.id) && !self.consumed.contains(&send.id) { continue; }
                for layer_id in send.layers() {
                    if !self.layer_batches.contains_key(layer_id) {
                        self.layer_batches.insert(layer_id.clone(), InputBatch::default());
                    }
                    let batch = self.layer_batches.get_mut(layer_id).expect("inserted above");
                    batch.drain_once(self.input_update, |batch| {
                        if pb.layer_tap_sample_rate(layer_id).is_none() {
                            batch.unavailable(|problem| {
                                discontinuities.push(AudioInputDiscontinuity { source: AudioInputSource::Layer(layer_id.clone()), problem });
                            });
                            return;
                        }
                        pb.drain_layer_tap_stamped(layer_id, |read, samples| {
                            batch.consume(read, samples, 1, |problem| {
                                discontinuities.push(AudioInputDiscontinuity { source: AudioInputSource::Layer(layer_id.clone()), problem });
                            });
                        });
                    });
                }
            }
        }

        // ── Per-send analysis: one analyzer per send, fed its whole input ──
        let mut analyzers = std::mem::take(&mut self.analyzers);
        let mut mono_mix = std::mem::take(&mut self.mono_mix);
        let mut layer_mix = std::mem::take(&mut self.layer_mix);
        let mut resampled = std::mem::take(&mut self.resampled);
        let mut staged_sends = std::mem::take(&mut self.staged_sends);
        staged_sends.clear();
        let mut detector_events = std::mem::take(&mut self.detector_events);
        let mut features = std::mem::take(&mut engine.audio_snapshot_mut().sends);
        features.clear();
        features.resize(send_count, SendFeatures::default());
        let mut hop_batches = std::mem::take(&mut engine.audio_snapshot_mut().hop_batches);
        hop_batches.resize_with(send_count, AudioHopBatch::default);
        for batch in &mut hop_batches {
            batch.begin(if active { batch.epoch() } else { 0 });
        }
        let mut tapped_index = None;

        if active && let Some(project) = engine.project() {
            let (low_hz, mid_hz) = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
            for (i, send) in project.audio_setup.sends.iter().enumerate() {
                let is_tapped = self.spec_send.as_ref() == Some(&send.id);
                if is_tapped {
                    tapped_index = Some(i);
                }
                // D4 gate (AUDIO_SENDS_UX_DESIGN section 3.2): a send outside the
                // consumed set and not the scope-tapped send costs nothing —
                // no mono push, no analyzer entry. One hash lookup per send.
                if !is_tapped && !self.consumed.contains(&send.id) {
                    hop_batches[i].begin(0);
                    continue;
                }
                let has_cap = send.has_capture() && device_rate.is_some();
                let layers = send.layers();
                // Layer tap rate (uniform — every layer routes through one kira
                // mixer), if any layer's tap has reported a rate yet.
                let layer_rate = if layers.is_empty() {
                    None
                } else {
                    layers.iter().find_map(|id| {
                        self.layer_batches.get(id)
                            .filter(|batch| batch.drained_update == self.input_update)
                            .and_then(InputBatch::sample_rate)
                            .or_else(|| layer_playback.as_deref().and_then(|pb| pb.layer_tap_sample_rate(id)))
                    })
                };
                // Analyzer rate: the device rate when capture feeds the send, else
                // the layer rate. No input this tick → leave the slot at default.
                let canonical = if has_cap {
                    device_rate.unwrap()
                } else if let Some(lr) = layer_rate {
                    lr
                } else {
                    hop_batches[i].begin(0);
                    continue;
                };

                let capture_generation = has_cap.then_some(self.capture_generation);
                let entry = match analyzers.entry(send.id.clone()) {
                    std::collections::hash_map::Entry::Occupied(e) => {
                        let slot = e.into_mut();
                        if slot.rate != canonical || slot.capture_generation != capture_generation
                            || slot.channels != send.channels || slot.layers != layers
                        {
                            discontinuities.push(AudioInputDiscontinuity {
                                source: AudioInputSource::Send(send.id.clone()),
                                problem: AudioInputProblem::SourceChanged,
                            });
                            *slot = SendAnalyzer::new(canonical, low_hz, mid_hz, send, capture_generation);
                            if let Some(history) = self.visuals.get_mut(Some(&send.id)) { history.clear(); }
                        }
                        slot
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(SendAnalyzer::new(canonical, low_hz, mid_hz, send, capture_generation))
                    }
                };
                let visualized = self.visual_consumed.contains(&send.id);
                if visualized {
                    self.visuals.ensure(
                        &send.id,
                        canonical,
                        entry.analyzer.num_bins(),
                        entry.analyzer.hop(),
                    );
                }
                entry.analyzer.set_crossovers(low_hz, mid_hz);
                entry.analyzer.set_scope(is_tapped);
                entry
                    .analyzer
                    .set_pitch_tracking(self.pitch_sends.contains(&send.id));
                // Pre-analysis squelch: applied live, identical for scope + features.
                entry.analyzer.set_floor_db(send.floor_db);

                // Build the send's mixed mono only from uninterrupted batches.
                mono_mix.clear();
                let mut interrupted = has_cap && self.capture_batch.interrupted;
                if has_cap && let Some(cap) = self.capture.as_ref() {
                    for frame in self.capture_batch.samples.chunks_exact(cap.mono.send_count().max(1)) {
                        if let Some(value) = frame.get(i) { mono_mix.push(*value); }
                    }
                }
                if !layers.is_empty() {
                    layer_mix.clear();
                    for layer_id in layers {
                        if let Some(batch) = self.layer_batches.get(layer_id).filter(|batch| batch.drained_update == self.input_update) {
                            interrupted |= batch.interrupted;
                            for (index, &sample) in batch.samples.iter().enumerate() {
                                if index < layer_mix.len() { layer_mix[index] += sample; }
                                else { layer_mix.push(sample); }
                            }
                        }
                    }
                    // Align the layer mono to the analyzer rate when capture set a
                    // different one (mismatched device vs kira rate); identity when
                    // they match, so the common 48k↔48k case is a direct sum.
                    let layer_samples: &[f32] = if has_cap && layer_rate != Some(canonical) {
                        let from = layer_rate.unwrap_or(canonical);
                        if entry.resampler.as_ref().is_none_or(|(f, _)| *f != from) {
                            entry.resampler = Some((from, LinearResampler::new(from, canonical)));
                        }
                        resampled.clear();
                        if let Some((_, r)) = entry.resampler.as_mut() {
                            r.process(&layer_mix, &mut resampled);
                        }
                        &resampled
                    } else {
                        &layer_mix
                    };
                    // Sum into the mix (element-wise, zero-extending the shorter).
                    for (k, &s) in layer_samples.iter().enumerate() {
                        if k < mono_mix.len() {
                            mono_mix[k] += s;
                        } else {
                            mono_mix.push(s);
                        }
                    }
                }

                if interrupted {
                    *entry = SendAnalyzer::new(canonical, low_hz, mid_hz, send, capture_generation);
                    hop_batches[i].begin(entry.epoch);
                    if let Some(history) = self.visuals.get_mut(Some(&send.id)) { history.clear(); }
                    continue;
                }

                if visualized && mono_mix.is_empty() && !has_cap {
                    if let Some(history) = self.visuals.get_mut(Some(&send.id)) {
                        history.clear();
                    }
                } else if visualized {
                    self.visuals.feed_waveform(&send.id, &mono_mix);
                }
                entry.stage(&mono_mix);
                staged_sends.push(i);
            }

            // The detector workers ran while the sends were mixed. Collect them under one
            // shared deadline, queue their fires, then analyse each staged send.
            let deadline = std::time::Instant::now() + DETECTOR_WAIT;
            for &i in &staged_sends {
                let send = &project.audio_setup.sends[i];
                let Some(entry) = analyzers.get_mut(&send.id) else { continue };
                entry.collect_events(deadline, &mut detector_events);
                let visualized = self.visual_consumed.contains(&send.id);
                let visuals = &mut self.visuals;
                let capture_batch = &self.capture_batch;
                // The existing mixed-source drain is not time aligned. Only
                // capture-only sends have a single truthful clock today.
                let capture_only = entry.capture_generation.is_some() && send.layers().is_empty();
                let mono = std::mem::take(&mut entry.staged);
                let result = entry.analyze_hops(&mono, &mut hop_batches[i], |offset| {
                    if capture_only { capture_batch.source_time_at(offset) } else { None }
                }, |column| {
                    if visualized { visuals.feed_spectrum(&send.id, column); }
                });
                entry.staged = mono;
                if let Err(error) = result {
                    discontinuities.push(AudioInputDiscontinuity {
                        source: AudioInputSource::Send(send.id.clone()),
                        problem: match error {
                            AudioHopError::CapacityExceeded => AudioInputProblem::AnalysisOverflow,
                            AudioHopError::InvalidInput => AudioInputProblem::InvalidInput,
                        },
                    });
                    // Publish the failed interval, then start a fresh analyzer
                    // epoch next update. Never join its prefix/tail or replay it.
                    *entry = SendAnalyzer::new(entry.rate, low_hz, mid_hz, send, entry.capture_generation);
                    if let Some(history) = visuals.get_mut(Some(&send.id)) { history.clear(); }
                    continue;
                }
                features[i] = entry.analyzer.latest();
            }

            // P1 gate instrument (AUDIO_SENDS_UX_DESIGN section 4 Phase 1): only runs
            // when `trace` was cached true at construction — zero cost otherwise.
            if self.trace {
                let ids: Vec<&str> = hop_batches
                    .iter()
                    .enumerate()
                    .filter(|(_, batch)| batch.epoch() != 0)
                    .filter_map(|(i, _)| project.audio_setup.sends.get(i).map(|s| s.id.as_str()))
                    .collect();
                eprintln!("[AudioMod] analyzed {} send(s): {ids:?}", ids.len());
            }
        }

        self.analyzers = analyzers;
        self.mono_mix = mono_mix;
        self.layer_mix = layer_mix;
        self.resampled = resampled;
        self.staged_sends = staged_sends;
        self.detector_events = detector_events;
        self.tapped_index = tapped_index;

        // Feed the engine. Reuse the snapshot's Vec capacity → no per-frame
        // allocation once warmed. An empty `sends` disables the audio phase.
        for discontinuity in &discontinuities {
            log::warn!("[AudioMod] input discontinuity: {:?}: {:?}", discontinuity.source, discontinuity.problem);
        }
        let snap = engine.audio_snapshot_mut();
        snap.input_discontinuities = discontinuities;
        snap.sends = features;
        snap.hop_batches = hop_batches;
    }

    /// Visual histories fed by the live analyzer. Readers should use the
    /// immutable registry on the content/UI snapshot path.
    pub fn visuals(&self) -> &manifold_core::audio_visual::AudioVisualRegistry {
        &self.visuals
    }

    /// Set which send the Audio Setup scope is showing (`None` = panel closed /
    /// nothing selected). Forces a reconcile so capture can start for
    /// calibration even before any audio mod is assigned.
    pub fn set_spectrogram_send(&mut self, send: Option<AudioSendId>) {
        if self.spec_send != send {
            self.spec_send = send;
            self.spec_dirty = true;
        }
    }

    /// The analyzer feeding the scope (the tapped send's), if any.
    fn tapped_analyzer(&self) -> Option<&StreamingSendAnalyzer> {
        let id = self.spec_send.as_ref()?;
        self.analyzers.get(id).map(|e| &e.analyzer)
    }

    /// Bin count of the scope-tapped send's column stream, or 0 if nothing is
    /// tapped / it has no analyzer yet.
    pub fn spectrogram_num_bins(&self) -> usize {
        self.tapped_analyzer().map_or(0, |a| a.num_bins())
    }

    /// Snapshot index (project send order) of the scope-tapped send, or `None`
    /// when nothing is tapped. The content thread uses this to pull the tapped
    /// send's features for the scope's per-band meters.
    pub fn tapped_send_index(&self) -> Option<usize> {
        self.tapped_index
    }

    /// Drain all complete VQT columns the tapped send produced since the last
    /// call, oldest → newest. No-op when nothing is tapped.
    pub fn drain_spectrogram_columns(&mut self, f: impl FnMut(&[f32])) {
        if let Some(id) = self.spec_send.clone()
            && let Some(entry) = self.analyzers.get_mut(&id)
        {
            entry.analyzer.drain_scope_columns(f);
        }
    }

    /// Drain the tapped send's per-column overlay records (one
    /// [`manifold_spectral::ScopeColumn`] each — centroid traces + onset tick
    /// lanes), oldest → newest, in lockstep with
    /// [`Self::drain_spectrogram_columns`]. No-op when nothing is tapped.
    pub fn drain_spectrogram_scalars(&mut self, f: impl FnMut(manifold_spectral::ScopeColumn)) {
        if let Some(id) = self.spec_send.clone()
            && let Some(entry) = self.analyzers.get_mut(&id)
        {
            entry.analyzer.drain_scope_scalars(f);
        }
    }

    /// The tapped send's analysed frequency range `(fmin, fmax)` Hz — for the
    /// frequency axis and band-divider overlays. `None` when nothing is tapped.
    pub fn spectrogram_freq_range(&self) -> Option<(f32, f32)> {
        self.tapped_analyzer().map(|a| a.freq_range())
    }

    /// Resolve the project's chosen input to a ready-to-open [`CaptureSource`]
    /// plus a human label for logging. `None` means the configured source is
    /// currently unavailable (device absent, app not running, tap unsupported) —
    /// capture should stay dark, the remappable policy. A `None` device ref maps
    /// to the system default input.
    fn resolve_source(&self, setup: &AudioSetup) -> Option<(CaptureSource, String)> {
        let Some(dev_ref) = &setup.device else {
            return Some((CaptureSource::DefaultInput, "System Default".to_string()));
        };
        match dev_ref.kind {
            // No capture at all — sends are fed by layers only, so the device
            // stays dark. (The worker runs from layer taps regardless; this just
            // never opens a capture backend.)
            AudioSourceKind::None => None,
            AudioSourceKind::InputDevice => {
                match self
                    .directory
                    .resolve(dev_ref.uid_opt(), Some(&dev_ref.name))
                {
                    Some(info) => {
                        let label = info.name.clone();
                        Some((CaptureSource::Device { name: info.name }, label))
                    }
                    None => {
                        log::warn!(
                            "[AudioMod] Saved audio device '{}' not present; audio \
                             modulation idle until it returns or is re-pointed",
                            dev_ref.name
                        );
                        None
                    }
                }
            }
            AudioSourceKind::SystemAudio => {
                if self.directory.tap_capabilities().system_audio {
                    Some((CaptureSource::SystemAudio, "System Audio".to_string()))
                } else {
                    log::warn!(
                        "[AudioMod] System-audio tap not supported on this OS (needs \
                         macOS 14.4+); audio modulation idle"
                    );
                    None
                }
            }
            AudioSourceKind::App => {
                let bundle_id = dev_ref.uid_opt().unwrap_or("");
                match self.directory.resolve_app(bundle_id) {
                    Some(app) => {
                        let label = format!("app:{}", app.name);
                        Some((
                            CaptureSource::Apps {
                                handles: vec![app.handle],
                            },
                            label,
                        ))
                    }
                    None => {
                        log::warn!(
                            "[AudioMod] App '{}' not running (or output tap unsupported); \
                             audio modulation idle until it returns",
                            dev_ref.name
                        );
                        None
                    }
                }
            }
        }
    }

    /// Start, stop, or rebuild the capture worker to match the project's audio
    /// setup.
    fn reconcile(&mut self, project: Option<&Project>) {
        let Some(project) = project else {
            self.capture = None;
            return;
        };

        // The capture worker runs when at least one send is capture-fed AND
        // something reads analysis: an active audio mod, an active live trigger, or
        // the Audio Setup scope open (calibration). A project whose sends are all
        // layer-fed needs no device.
        let any_capture = project.audio_setup.sends.iter().any(|s| s.has_capture());
        let needs_analysis = project.has_active_audio_mods()
            || self.spec_send.is_some()
            || project.has_active_clip_triggers()
            || !self.visual_consumed.is_empty();
        let gate = any_capture && needs_analysis && !project.audio_setup.sends.is_empty();
        if !gate {
            if self.capture.is_some() {
                log::info!("[AudioMod] Stopping capture — no capture-fed send needs the device");
                self.capture = None;
            }
            return;
        }

        let desired = CaptureSignature::from_setup(&project.audio_setup);
        if let Some(cap) = self.capture.as_ref()
            && cap.signature == desired
        {
            // No structural change. Gain isn't in the signature, so sync it live
            // here — a gain edit lands without restarting the capture stream.
            sync_gains(&cap.gains, &project.audio_setup);
            return;
        }

        // (Re)build. Drop any existing capture first so we don't hold two
        // streams on one device during the swap.
        self.capture = None;

        // Trigger the one-time mic permission prompt if undecided, and warn
        // clearly if it's blocked — otherwise built-in-mic capture is silently
        // zero. Status is app-global, so this is harmless for virtual devices.
        match manifold_audio::permission::status() {
            manifold_audio::permission::MicPermission::NotDetermined
                if !self.mic_access_requested =>
            {
                manifold_audio::permission::request_microphone_access();
                self.mic_access_requested = true;
            }
            manifold_audio::permission::MicPermission::Denied => {
                log::warn!(
                    "[AudioMod] Microphone access is denied — any send routed to the \
                     built-in mic will be silent. Grant access in System Settings → \
                     Privacy & Security → Microphone."
                );
            }
            _ => {}
        }

        // Resolve the stored source reference to a ready-to-open `CaptureSource`.
        // A configured-but-absent source (device unplugged, app not running, tap
        // unsupported) leaves capture dark — the remappable policy — rather than
        // failing the tick. `None` = system default input.
        let Some((source, source_label)) = self.resolve_source(&project.audio_setup) else {
            return;
        };
        // Worker downmixes every send in order (layer-only sends produce mono the
        // content thread ignores) so the worker send index matches project order.
        let send_channels: Vec<Vec<u16>> = project
            .audio_setup
            .sends
            .iter()
            .map(|s| s.channels.clone())
            .collect();
        let send_count = send_channels.len();
        // Initial per-send linear gains, in send order — the worker reads these
        // live through the shared bank.
        let initial_gains: Vec<f32> = project
            .audio_setup
            .sends
            .iter()
            .map(|s| s.gain_linear())
            .collect();
        let gains = Arc::new(GainBank::new(&initial_gains));

        let mut backend = match capture::open(source) {
            Ok(b) => b,
            Err(e) => {
                log::warn!(
                    "[AudioMod] Capture source unavailable ({e}); audio modulation idle \
                     until the input is re-pointed"
                );
                return;
            }
        };
        let sample_rate = backend.sample_rate();
        let channels = backend.channels();
        let Some(consumer) = backend.take_consumer() else {
            log::error!("[AudioMod] Capture backend returned no consumer");
            return;
        };
        if let Err(e) = backend.start() {
            log::warn!("[AudioMod] Failed to start capture: {e}");
            return;
        }

        let (worker, mono) = AudioFeatureWorker::spawn(
            consumer,
            sample_rate,
            channels,
            send_channels,
            gains.clone(),
        );
        log::info!(
            "[AudioMod] Capture started: source={source_label}, {send_count} sends, \
             {sample_rate}Hz {channels}ch"
        );
        self.capture_generation = self.capture_generation.wrapping_add(1);
        self.capture_batch = InputBatch::default();
        self.capture = Some(AudioModCapture {
            _backend: backend,
            _worker: worker,
            mono,
            signature: desired,
            gains,
        });
    }
}

/// Write each send's current linear gain into the shared [`GainBank`], in send
/// order (the worker's send index). Cheap, lock-free; called every reconcile
/// that finds no structural change, so a gain edit takes effect without a
/// capture restart. A send count mismatch can't happen here — a send add/remove
/// changes [`CaptureSignature`] and forces a rebuild before this runs.
fn sync_gains(gains: &GainBank, setup: &AudioSetup) {
    for (i, send) in setup.sends.iter().enumerate() {
        gains.set_linear(i, send.gain_linear());
    }
}

#[cfg(test)]
mod hop_tests {
    use super::*;

    #[test]
    fn live_hops_are_partition_invariant_without_invented_transport_time() {
        let send = AudioSend::new("Live hop test");
        let rate = 48_000;
        let input: Vec<f32> = (0..rate).map(|index| {
            let envelope = if index % 6000 < 1800 { 0.6 } else { 0.0 };
            envelope * (index as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin()
        }).collect();
        let run = |chunks: &[usize]| {
            let mut analyzer = SendAnalyzer::new(rate, 250.0, 2500.0, &send, None);
            let mut batch = AudioHopBatch::default();
            let mut offset = 0;
            let mut output = Vec::new();
            let mut columns = 0;
            for size in chunks.iter().cycle() {
                let end = (offset + size).min(input.len());
                analyzer.analyze_hops(&input[offset..end], &mut batch, |_| None, |_| columns += 1).unwrap();
                for hop in batch.hops() {
                    assert_eq!(hop.stamp.epoch, analyzer.epoch);
                    assert_eq!(hop.stamp.timeline_time, None);
                    assert_eq!(hop.stamp.source_time, None);
                    assert_eq!(hop.stamp.sample_rate, rate);
                    output.push((hop.stamp.end_sample, hop.dt, hop.features));
                }
                offset = end;
                if offset == input.len() { break; }
            }
            assert_eq!(columns, output.len());
            analyzer.analyze_hops(&[], &mut batch, |_| None, |_| panic!("no input cannot produce a hop")).unwrap();
            assert!(batch.hops().is_empty());
            output
        };
        let expected = run(&[input.len()]);
        assert!(expected.len() > 10);
        assert_eq!(run(&[1, 17, 8192, 31, 1003]), expected);
    }

    #[test]
    fn capture_hop_clocks_survive_display_partitions_and_source_block_boundaries() {
        use manifold_core::audio_stream::{AudioClockAnchor, audio_stream};
        use std::time::{Duration, Instant};

        let send = AudioSend::new("Clocked capture");
        let origin = Instant::now();
        for rate in [44_100, 48_000] {
            let probe = SendAnalyzer::new(rate, 250.0, 2500.0, &send, None);
            let hop_size = probe.analyzer.hop();
            let input: Vec<f32> = (0..rate as usize).map(|index| {
                0.4 * (index as f32 * 220.0 * std::f32::consts::TAU / rate as f32).sin()
            }).collect();
            let run = |chunks: &[usize]| {
                let (mut source, mut consumer) = audio_stream(1, input.len(), 512, rate);
                // Clock each source block independently with a small backend
                // offset. A hop ending exactly on a block boundary must use
                // the same clock whether the next block has arrived or not.
                for (index, block) in input.chunks(hop_size).enumerate() {
                    let frame = (index * hop_size) as u64;
                    let delta = Duration::from_nanos(frame * 1_000_000_000 / u64::from(rate));
                    let instant = origin + delta + Duration::from_micros((index % 3) as u64);
                    source.push_interleaved_clocked(block, Some(AudioClockAnchor { instant, frame }));
                }
                let mut analyzer = SendAnalyzer::new(rate, 250.0, 2500.0, &send, None);
                let mut input_batch = InputBatch::default();
                let mut hops = AudioHopBatch::default();
                let mut scratch = vec![0.; input.len()];
                let mut remaining = input.len();
                let mut output = Vec::new();
                for (update, size) in chunks.iter().cycle().enumerate() {
                    input_batch.begin(update as u64 + 1);
                    let mut wanted = (*size).min(remaining);
                    while wanted > 0 {
                        let read = consumer.read(&mut scratch[..wanted]).unwrap();
                        let manifold_core::audio_stream::AudioStreamRead::Samples { samples, .. } = read
                            else { panic!("clocked input must be continuous"); };
                        input_batch.consume(read, &scratch[..samples], 1, |_| panic!("valid input"));
                        wanted -= samples;
                        remaining -= samples;
                    }
                    analyzer.analyze_hops(&input_batch.samples, &mut hops,
                        |offset| input_batch.source_time_at(offset), |_| {}).unwrap();
                    for hop in hops.hops() {
                        assert!(hop.stamp.source_time.is_some());
                        assert_eq!(hop.stamp.timeline_time, None);
                        output.push((hop.stamp.end_sample, hop.stamp.source_time, hop.features));
                    }
                    if remaining == 0 { break; }
                }
                output
            };
            let expected = run(&[input.len()]); // one-second display stall
            assert!(expected.len() > 10);
            assert_eq!(run(&[hop_size]), expected);
            for fps in [24, 30, 60] { assert_eq!(run(&[rate as usize / fps]), expected); }
            assert_eq!(run(&[1, 17, 4096, 1003]), expected);
        }
    }

    /// The live path end to end: staged mono → detector worker → queued fires → the Low band's
    /// kick at 1.0 on the hop covering each fire the inline detector stamps.
    #[test]
    fn worker_kick_fires_land_on_their_hops() {
        use manifold_audio::kick::{KickDetector, container::Container};
        use std::time::{Duration, Instant};

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../manifold-audio/tests/fixtures/kick/clip_dense.mkick");
        let golden = Container::parse(&std::fs::read(path).unwrap()).unwrap();
        let n = golden.shape("audio_i16").unwrap()[0];
        let input: Vec<f32> = golden.i16s("audio_i16", &[n]).unwrap().iter().map(|&v| v as f32 / 32768.0).collect();
        let mut inline = Vec::new();
        KickDetector::new(48_000).unwrap().push(&input, &mut inline);
        assert!(inline.len() > 10);

        let send = AudioSend::new("Kick");
        let mut analyzer = SendAnalyzer::new(48_000, 250.0, 2500.0, &send, None);
        assert!(analyzer.detectors.is_some());
        let hop = analyzer.analyzer.hop() as u64;
        let mut batch = AudioHopBatch::default();
        let mut events = Vec::with_capacity(256);
        let mut fired = Vec::new();
        for frame in input.chunks(800) {
            analyzer.stage(frame);
            analyzer.collect_events(Instant::now() + Duration::from_secs(5), &mut events);
            let mono = std::mem::take(&mut analyzer.staged);
            analyzer.analyze_hops(&mono, &mut batch, |_| None, |_| {}).unwrap();
            analyzer.staged = mono;
            fired.extend(batch.hops().iter().filter(|h| h.features.bands[1].kick == 1.0).map(|h| h.stamp.end_sample));
        }
        let want: Vec<u64> = inline.iter().map(|f| f.sample.div_ceil(hop) * hop).collect();
        assert_eq!(fired, want);
    }

    #[test]
    fn live_overflow_exposes_no_partial_batch_and_new_analyzer_recovers() {
        let send = AudioSend::new("Overflow");
        let mut analyzer = SendAnalyzer::new(48_000, 250.0, 2500.0, &send, None);
        let epoch = analyzer.epoch;
        let hop = analyzer.analyzer.hop();
        let mut batch = AudioHopBatch::with_capacity(1);
        assert_eq!(analyzer.analyze_hops(&vec![0.0; hop * 2], &mut batch, |_| None, |_| {}),
            Err(AudioHopError::CapacityExceeded));
        assert!(batch.hops().is_empty());
        assert_eq!(analyzer.analyze_hops(&[], &mut batch, |_| None, |_| {}),
            Err(AudioHopError::CapacityExceeded));
        analyzer = SendAnalyzer::new(48_000, 250.0, 2500.0, &send, None);
        assert_ne!(analyzer.epoch, epoch);
        analyzer.analyze_hops(&vec![0.0; hop], &mut batch, |_| None, |_| {}).unwrap();
        assert_eq!(batch.hops().len(), 1);
        assert_eq!(batch.hops()[0].stamp.end_sample, hop as u64);
    }
}
