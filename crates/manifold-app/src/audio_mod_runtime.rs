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
//! ("what you hear is what modulates"). See `docs/AUDIO_LAYER_DESIGN.md` §3R.
//!
//! Lifecycle is **gated and self-healing**: the capture worker runs only while
//! the project has at least one capture-fed send and either an active audio
//! modulation or the Audio Setup scope open. The device and worker rebuild when
//! the capture-relevant config changes (device or any send's channels) — a
//! relabel alone does not restart capture. A missing device leaves capture dark
//! until the user re-points it (the remappable device policy).

use std::sync::Arc;

use ahash::AHashMap;

use manifold_audio::analysis::{
    AudioFeatureWorker, GainBank, LinearResampler, MonoReader, StreamingSendAnalyzer,
};
use manifold_audio::capture::{self, CaptureBackend, CaptureSource};
use manifold_core::SendFeatures;
use manifold_core::audio_setup::{AudioDeviceRef, AudioSetup, AudioSourceKind};
use manifold_core::id::AudioSendId;
use manifold_core::project::Project;
use manifold_playback::audio_layer_playback::AudioLayerPlayback;
use manifold_playback::engine::PlaybackEngine;

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
    analyzer: StreamingSendAnalyzer,
    /// Layer-tap → analyzer-rate resampler, built lazily; `(from_rate, state)`.
    resampler: Option<(u32, LinearResampler)>,
}

impl SendAnalyzer {
    fn new(rate: u32, low_hz: f32, mid_hz: f32) -> Self {
        Self {
            rate,
            analyzer: StreamingSendAnalyzer::new(rate, low_hz, mid_hz),
            resampler: None,
        }
    }
}

/// Content-thread-owned runtime that reconciles capture against the project and
/// feeds the engine its feature snapshot.
pub struct AudioModRuntime {
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
    /// D4 activation set (AUDIO_SENDS_UX_DESIGN §3.2): sends with at least one
    /// enabled audio mod or enabled trigger route — `Project::analysis_consumed_sends`.
    /// Recomputed only on a data-version change, mirroring `pitch_sends`. The
    /// per-send loop skips any send outside this set that also isn't the
    /// scope-tapped send: no mono push, no analyzer entry. Makes analysis cost
    /// proportional to what's actually bound, not to `sends.len()`.
    consumed: ahash::AHashSet<AudioSendId>,
    /// Snapshot index (project send order) of the scope-tapped send, for the
    /// per-band meters. Resolved each tick.
    tapped_index: Option<usize>,
    // ── Reusable scratch (no per-tick allocation once warmed) ──
    /// Per-send capture mono, drained from the worker each tick (index = send).
    capture_mono: Vec<Vec<f32>>,
    /// Summed layer taps for one send, before resampling.
    layer_mix: Vec<f32>,
    /// One send's final mixed mono (capture + layers), pushed to its analyzer.
    mono_mix: Vec<f32>,
    /// Resampled layer mono (layer rate → analyzer rate) for one send.
    resampled: Vec<f32>,
    /// Cached at construction from `MANIFOLD_AUDIO_TRACE` — the P1 gate
    /// instrument (`docs/AUDIO_SENDS_UX_DESIGN.md` §4 Phase 1). Checked once,
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
            capture: None,
            last_version: 0,
            directory,
            devices_dirty,
            processes_dirty,
            _hotplug_sub: sub,
            _process_sub: process_sub,
            mic_access_requested: false,
            spec_send: None,
            spec_dirty: false,
            analyzers: AHashMap::new(),
            pitch_sends: ahash::AHashSet::new(),
            consumed: ahash::AHashSet::new(),
            tapped_index: None,
            capture_mono: Vec::new(),
            layer_mix: Vec::new(),
            mono_mix: Vec::new(),
            resampled: Vec::new(),
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
                // The device or the tapped app appeared/vanished/changed — drop
                // capture so reconcile rebuilds against the current state (the
                // stored ref is unchanged, so the signature alone wouldn't fire).
                self.capture = None;
            }
            self.reconcile(engine.project());
            // D4 activation set (AUDIO_SENDS_UX_DESIGN §3.2): recomputed only here,
            // never per tick.
            self.consumed =
                engine.project().map(|p| p.analysis_consumed_sends()).unwrap_or_default();
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
            }
            self.pitch_sends =
                engine.project().map(|p| p.sends_with_pitch_mods()).unwrap_or_default();
            self.last_version = data_version;
            self.spec_dirty = false;
        }

        // Analysis runs when something reads it: an active param modulation, an
        // active live trigger (fires clips even with the scope closed), or the
        // Audio Setup scope is open for calibration.
        let (active, send_count) = engine.project().map_or((false, 0), |p| {
            let needs = p.has_active_audio_mods()
                || self.spec_send.is_some()
                || p.has_active_clip_triggers();
            (needs, p.audio_setup.sends.len())
        });

        // The rate the capture worker delivers mono at (also the analyzer rate for
        // any capture-fed send).
        let device_rate = self.capture.as_ref().map(|c| c.mono.sample_rate());

        // Drain the worker's per-send mono for this tick. Taken out so `self` can
        // be borrowed field-wise below.
        let mut capture_mono = std::mem::take(&mut self.capture_mono);
        for v in capture_mono.iter_mut() {
            v.clear();
        }
        if let Some(cap) = self.capture.as_mut() {
            let n = cap.mono.send_count();
            if capture_mono.len() < n {
                capture_mono.resize_with(n, Vec::new);
            }
            cap.mono.drain(&mut capture_mono);
        }

        // ── Per-send analysis: one analyzer per send, fed its whole input ──
        let mut analyzers = std::mem::take(&mut self.analyzers);
        let mut mono_mix = std::mem::take(&mut self.mono_mix);
        let mut layer_mix = std::mem::take(&mut self.layer_mix);
        let mut resampled = std::mem::take(&mut self.resampled);
        let mut features: Vec<(usize, SendFeatures)> = Vec::new();
        let mut tapped_index = None;

        if active && let Some(project) = engine.project() {
            let (low_hz, mid_hz) = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
            for (i, send) in project.audio_setup.sends.iter().enumerate() {
                let is_tapped = self.spec_send.as_ref() == Some(&send.id);
                if is_tapped {
                    tapped_index = Some(i);
                }
                // D4 gate (AUDIO_SENDS_UX_DESIGN §3.2): a send outside the
                // consumed set and not the scope-tapped send costs nothing —
                // no mono push, no analyzer entry. One hash lookup per send.
                if !is_tapped && !self.consumed.contains(&send.id) {
                    continue;
                }
                let has_cap = send.has_capture() && device_rate.is_some();
                let layers = send.layers();
                // Layer tap rate (uniform — every layer routes through one kira
                // mixer), if any layer's tap has reported a rate yet.
                let layer_rate = if layers.is_empty() {
                    None
                } else {
                    layer_playback
                        .as_deref()
                        .and_then(|pb| layers.iter().find_map(|l| pb.layer_tap_sample_rate(l)))
                };
                // Analyzer rate: the device rate when capture feeds the send, else
                // the layer rate. No input this tick → leave the slot at default.
                let canonical = if has_cap {
                    device_rate.unwrap()
                } else if let Some(lr) = layer_rate {
                    lr
                } else {
                    continue;
                };

                let entry = match analyzers.entry(send.id.clone()) {
                    std::collections::hash_map::Entry::Occupied(e) => {
                        let slot = e.into_mut();
                        if slot.rate != canonical {
                            *slot = SendAnalyzer::new(canonical, low_hz, mid_hz);
                        }
                        slot
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(SendAnalyzer::new(canonical, low_hz, mid_hz))
                    }
                };
                entry.analyzer.set_crossovers(low_hz, mid_hz);
                entry.analyzer.set_scope(is_tapped);
                entry.analyzer.set_pitch_tracking(self.pitch_sends.contains(&send.id));
                // Pre-analysis squelch: applied live, identical for scope + features.
                entry.analyzer.set_floor_db(send.floor_db);

                // Build the send's mixed mono for this tick.
                mono_mix.clear();
                if has_cap && let Some(cap_in) = capture_mono.get(i) {
                    mono_mix.extend_from_slice(cap_in);
                }
                if !layers.is_empty() && let Some(pb) = layer_playback.as_deref_mut() {
                    // Sum every feeding layer's post-fader tap.
                    layer_mix.clear();
                    for (li, layer_id) in layers.iter().enumerate() {
                        if li == 0 {
                            pb.drain_layer_tap(layer_id, |chunk| layer_mix.extend_from_slice(chunk));
                        } else {
                            let mut idx = 0usize;
                            pb.drain_layer_tap(layer_id, |chunk| {
                                for &s in chunk {
                                    if idx < layer_mix.len() {
                                        layer_mix[idx] += s;
                                    } else {
                                        layer_mix.push(s);
                                    }
                                    idx += 1;
                                }
                            });
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

                entry.analyzer.push(&mono_mix);
                features.push((i, entry.analyzer.latest()));
            }

            // P1 gate instrument (AUDIO_SENDS_UX_DESIGN §4 Phase 1): only runs
            // when `trace` was cached true at construction — zero cost otherwise.
            if self.trace {
                let ids: Vec<&str> = features
                    .iter()
                    .filter_map(|(i, _)| project.audio_setup.sends.get(*i).map(|s| s.id.as_str()))
                    .collect();
                eprintln!("[AudioMod] analyzed {} send(s): {ids:?}", features.len());
            }
        }

        self.analyzers = analyzers;
        self.mono_mix = mono_mix;
        self.layer_mix = layer_mix;
        self.resampled = resampled;
        self.capture_mono = capture_mono;
        self.tapped_index = tapped_index;

        // Feed the engine. Reuse the snapshot's Vec capacity → no per-frame
        // allocation once warmed. An empty `sends` disables the audio phase.
        let snap = engine.audio_snapshot_mut();
        snap.sends.clear();
        snap.sends
            .resize(send_count, manifold_core::SendFeatures::default());
        for (i, f) in features {
            if let Some(slot) = snap.sends.get_mut(i) {
                *slot = f;
            }
        }
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
            AudioSourceKind::InputDevice => {
                match self.directory.resolve(dev_ref.uid_opt(), Some(&dev_ref.name)) {
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
                        Some((CaptureSource::Apps { handles: vec![app.handle] }, label))
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
            || project.has_active_clip_triggers();
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

        let (worker, mono) =
            AudioFeatureWorker::spawn(consumer, sample_rate, channels, send_channels, gains.clone());
        log::info!(
            "[AudioMod] Capture started: source={source_label}, {send_count} sends, \
             {sample_rate}Hz {channels}ch"
        );
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
