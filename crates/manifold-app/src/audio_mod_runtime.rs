//! Audio-modulation capture runtime.
//!
//! Owns the always-on audio capture device + feature worker for audio
//! modulation, and feeds the playback engine a fresh per-send feature snapshot
//! each tick. Lives on the content thread (which owns the engine and, through
//! it, the project). Step 3 of `docs/AUDIO_MODULATION_DESIGN.md`.
//!
//! Lifecycle is **gated and self-healing**: capture runs only while the project
//! has at least one enabled audio modulation and at least one send. The device
//! and worker rebuild when the capture-relevant config changes (device or any
//! send's channels) — a relabel alone does not restart capture. A missing
//! device leaves capture dark until the user re-points it (the remappable
//! device policy), rather than failing the tick.

use std::sync::Arc;

use ahash::AHashMap;

use manifold_audio::analysis::{
    AudioFeatureWorker, ColumnReader, CrossoverBank, FeatureReader, GainBank, ScalarReader,
    SendSpec, SpectrogramTap, StreamingSendAnalyzer,
};
use manifold_audio::capture::{self, CaptureBackend, CaptureSource};
use manifold_core::audio_setup::{AudioDeviceRef, AudioSetup, AudioSourceKind};
use manifold_core::id::AudioSendId;
use manifold_core::project::Project;
use manifold_playback::audio_layer_playback::AudioLayerPlayback;
use manifold_playback::engine::PlaybackEngine;

/// The capture-relevant fingerprint of an [`AudioSetup`]. Changes here force a
/// device/worker rebuild; label-only edits compare equal and don't. The device
/// is keyed by its stable [`AudioDeviceRef`] (UID), so renaming the OS device
/// doesn't churn capture — it re-resolves to the same hardware.
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

/// A live capture: the device captures, the worker drains and analyzes. Both
/// are kept alive by ownership here; dropping the struct stops capture and
/// joins the worker.
struct AudioModCapture {
    _backend: Box<dyn CaptureBackend>,
    _worker: AudioFeatureWorker,
    reader: FeatureReader,
    signature: CaptureSignature,
    /// Live per-send gain shared with the worker. A gain-only edit writes here
    /// in place — no capture restart (gain isn't in [`CaptureSignature`]).
    gains: Arc<GainBank>,
    /// Spectrogram tap control — which send the worker produces VQT columns for.
    tap: Arc<SpectrogramTap>,
    /// Read end of the worker's VQT column stream.
    columns: ColumnReader,
    /// Read end of the worker's per-column overlay scalars (centroid + onset).
    scalars: ScalarReader,
    /// Capture sample rate, for the scope's frequency-axis range.
    sample_rate: f32,
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
    /// Send the Audio Setup scope is showing, if open. Resolved to a worker
    /// index against the live project each tick and written to the tap, so it
    /// survives capture rebuilds (which mint a fresh tap).
    spec_send: Option<AudioSendId>,
    /// Set when `spec_send` changes, forcing a reconcile next tick — selecting a
    /// send opens the scope, which can start capture even with no audio mods yet
    /// (calibration precedes assignment).
    spec_dirty: bool,
    /// Live Low/Mid/High crossovers shared with the worker. Global to all sends
    /// and persistent across capture rebuilds (like the tap), synced from the
    /// project each tick so a band-divider drag retunes analysis without a
    /// capture restart.
    crossovers: Arc<CrossoverBank>,
    /// Worker index of the scope-tapped send, resolved each tick. Lets the
    /// content thread pull that send's features for the scope's per-band meters.
    tapped_index: Option<usize>,
    /// Realtime per-send analyzers for layer-fed sends (audio layers). Each
    /// drains its layer's post-fader kira tap and reduces it to [`SendFeatures`]
    /// with the same DSP as a live capture send — so the modulation tracks what's
    /// actually heard (warp, gain, mute all baked in). Keyed by the send's id;
    /// the stored `u32` is the sample rate the analyzer was built for, so a rate
    /// change rebuilds it. When a layer-fed send is the scope's tapped send, its
    /// analyzer also buffers spectrogram columns (so it draws like a live input).
    /// See `docs/AUDIO_LAYER_DESIGN.md` §3R.
    layer_analyzers: AHashMap<AudioSendId, (u32, StreamingSendAnalyzer)>,
    /// The scope's tapped send when it is layer-fed (its columns come from the
    /// inline analyzer, not the capture worker). `None` = capture send / nothing.
    tapped_layer_send: Option<AudioSendId>,
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
            crossovers: Arc::new(CrossoverBank::new(
                manifold_core::audio_setup::DEFAULT_LOW_HZ,
                manifold_core::audio_setup::DEFAULT_MID_HZ,
            )),
            tapped_index: None,
            layer_analyzers: AHashMap::new(),
            tapped_layer_send: None,
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
        layer_playback: Option<&mut AudioLayerPlayback>,
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
            // Drop analyzers for sends that are no longer layer-fed (runs only on
            // a project change, never per tick — keeps the map bounded without
            // allocating on the hot path).
            if let Some(project) = engine.project() {
                self.layer_analyzers.retain(|id, _| {
                    project
                        .audio_setup
                        .sends
                        .iter()
                        .any(|s| &s.id == id && s.layer_source().is_some())
                });
            }
            self.last_version = data_version;
            self.spec_dirty = false;
        }

        // Sync the Low/Mid/High crossovers from the project every tick (cheap
        // atomic stores). The capture worker AND every per-layer worker read this
        // shared bank live, so a band-divider drag retunes all analyses with no
        // restart — same model as gain.
        if let Some(p) = engine.project() {
            self.crossovers
                .set(p.audio_setup.low_hz, p.audio_setup.mid_hz);
        }

        // ── Layer-fed sends: feed each layer's post-fader tap into its own
        // AudioFeatureWorker (a one-channel mono "device") — the *same* analysis a
        // capture send runs. So a layer-fed send produces features + spectrogram
        // columns + overlay scalars identically to a live input: scope, meters,
        // and modulation all work, because it IS an input. Spawn lazily once the
        // layer has a track and the tap has reported its rate; respawn if the
        // feeding layer changes. Samples are post-fader, so warp/gain/mute are
        // already baked in. See §3R.
        //
        // Features collected here, applied below under the snapshot borrow. The
        // snapshot is indexed by `AudioSetup::sends` order; layer-fed sends are
        // silent slots in the capture worker, so we overwrite them.
        let mut layer_features: Vec<(usize, manifold_core::SendFeatures)> = Vec::new();
        let mut send_count = 0usize;
        let mut tapped_layer_send: Option<AudioSendId> = None;
        if let (Some(project), Some(playback)) = (engine.project(), layer_playback) {
            send_count = project.audio_setup.sends.len();
            let (low_hz, mid_hz) = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
            for (i, send) in project.audio_setup.sends.iter().enumerate() {
                let Some(layer_id) = send.layer_source() else {
                    continue;
                };
                // The tap reports the mixer's output rate via its first init; until
                // then no audio has flowed. Build (or rebuild on a rate change) the
                // analyzer for that rate.
                let Some(sample_rate) = playback.layer_tap_sample_rate(layer_id) else {
                    continue;
                };
                let analyzer = match self.layer_analyzers.entry(send.id.clone()) {
                    std::collections::hash_map::Entry::Occupied(e) => {
                        let slot = e.into_mut();
                        if slot.0 != sample_rate {
                            *slot = (sample_rate, StreamingSendAnalyzer::new(sample_rate, low_hz, mid_hz));
                        }
                        &mut slot.1
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        &mut e
                            .insert((sample_rate, StreamingSendAnalyzer::new(sample_rate, low_hz, mid_hz)))
                            .1
                    }
                };
                // Live-retune the bands, mark this analyzer as the scope source if
                // it's the tapped send (so it buffers columns), then feed it the
                // post-fader samples. Silence in → features decay, so a muted/
                // paused layer reads as no modulation and a dark scope.
                let is_tapped = self.spec_send.as_ref() == Some(&send.id);
                if is_tapped {
                    tapped_layer_send = Some(send.id.clone());
                }
                analyzer.set_crossovers(low_hz, mid_hz);
                analyzer.set_scope(is_tapped);
                playback.drain_layer_tap(layer_id, |chunk| analyzer.push(chunk));
                layer_features.push((i, analyzer.latest()));
            }
        } else if let Some(project) = engine.project() {
            send_count = project.audio_setup.sends.len();
        }

        // Resolve the scope's send → snapshot index (project order) for the
        // per-band meters. The capture worker draws columns only when the tapped
        // send is a capture send; a layer-fed send's columns come from its inline
        // analyzer, so mute the capture tap to avoid producing columns nobody
        // drains.
        let tap_index = self
            .spec_send
            .as_ref()
            .and_then(|id| engine.project().and_then(|p| p.audio_setup.send_index(id)));
        if let Some(cap) = &self.capture {
            cap.tap
                .set_selected(if tapped_layer_send.is_some() { None } else { tap_index });
        }
        self.tapped_index = tap_index;
        self.tapped_layer_send = tapped_layer_send;

        // Feed the engine. Reuse the snapshot's Vec capacity → no per-frame
        // allocation once warmed. An empty `sends` disables the audio phase.
        let snap = engine.audio_snapshot_mut();
        snap.sends.clear();
        if let Some(cap) = &mut self.capture
            && let Some(frame) = cap.reader.latest()
        {
            for i in 0..frame.count() {
                if let Some(f) = frame.send(i) {
                    snap.sends.push(f);
                }
            }
        }
        // Ensure a slot per project send so layer-fed sends have a home even when
        // capture is dark (no worker frame), keeping the snapshot index aligned
        // with `AudioSetup::sends`.
        if snap.sends.len() < send_count {
            snap.sends
                .resize(send_count, manifold_core::SendFeatures::default());
        }
        // Overwrite layer-fed slots with their inline-analyzer features.
        for (i, f) in layer_features {
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

    /// The analyzer feeding the scope when the tapped send is layer-fed (its
    /// columns come from the inline analyzer, not the capture worker).
    fn tapped_layer_analyzer(&self) -> Option<&StreamingSendAnalyzer> {
        let id = self.tapped_layer_send.as_ref()?;
        self.layer_analyzers.get(id).map(|(_, a)| a)
    }

    /// Bin count of the current column stream, or 0 if nothing is tapped. Reads
    /// the layer analyzer when the tapped send is layer-fed, else the capture
    /// worker.
    pub fn spectrogram_num_bins(&self) -> usize {
        if let Some(a) = self.tapped_layer_analyzer() {
            return a.num_bins();
        }
        self.capture.as_ref().map_or(0, |c| c.columns.num_bins())
    }

    /// Snapshot index (project send order) of the scope-tapped send, or `None`
    /// when nothing is tapped. The content thread uses this to pull the tapped
    /// send's features for the scope's per-band meters. Correct for both capture
    /// and layer-fed sends, since the snapshot is indexed by project send order.
    pub fn tapped_send_index(&self) -> Option<usize> {
        self.tapped_index
    }

    /// Drain all complete VQT columns produced since the last call, oldest →
    /// newest. Reads the layer analyzer for a layer-fed tapped send, else the
    /// capture worker. No-op when nothing is tapped.
    pub fn drain_spectrogram_columns(&mut self, f: impl FnMut(&[f32])) {
        if let Some(id) = self.tapped_layer_send.clone() {
            if let Some((_, a)) = self.layer_analyzers.get_mut(&id) {
                a.drain_scope_columns(f);
            }
            return;
        }
        if let Some(cap) = &mut self.capture {
            cap.columns.drain_columns(f);
        }
    }

    /// Drain the per-column overlay scalars `([centroid_full, centroid_low,
    /// centroid_mid, centroid_high], [onset_low, onset_mid, onset_high])` produced
    /// since the last call, oldest → newest and in lockstep with
    /// [`Self::drain_spectrogram_columns`]. Layer analyzer for a layer-fed tapped
    /// send, else the capture worker.
    pub fn drain_spectrogram_scalars(&mut self, f: impl FnMut([f32; 4], [f32; 3])) {
        if let Some(id) = self.tapped_layer_send.clone() {
            if let Some((_, a)) = self.layer_analyzers.get_mut(&id) {
                a.drain_scope_scalars(f);
            }
            return;
        }
        if let Some(cap) = &mut self.capture {
            cap.scalars.drain(f);
        }
    }

    /// The scope's analysed frequency range `(fmin, fmax)` Hz — for the
    /// frequency axis and band-divider overlays. Layer analyzer for a layer-fed
    /// tapped send, else the capture worker. `None` when nothing is tapped.
    pub fn spectrogram_freq_range(&self) -> Option<(f32, f32)> {
        if let Some(a) = self.tapped_layer_analyzer() {
            return Some(a.freq_range());
        }
        let cap = self.capture.as_ref()?;
        let cfg = manifold_spectral::SpectrogramConfig::default();
        Some((cfg.fmin, cfg.effective_fmax(cap.sample_rate)))
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

    /// Start, stop, or rebuild capture to match the project's audio setup.
    fn reconcile(&mut self, project: Option<&Project>) {
        let Some(project) = project else {
            self.capture = None;
            return;
        };

        // Capture runs when there's an active audio mod OR the Audio Setup scope
        // is open on a send (calibration before assignment), and at least one
        // send exists.
        let gate = (project.has_active_audio_mods() || self.spec_send.is_some())
            && !project.audio_setup.sends.is_empty();
        if !gate {
            if self.capture.is_some() {
                log::info!("[AudioMod] Stopping capture — no active audio modulations");
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
        let specs: Vec<SendSpec> = project
            .audio_setup
            .sends
            .iter()
            .map(|s| SendSpec {
                channels: s.channels.clone(),
            })
            .collect();
        let send_count = specs.len();
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

        let tap = Arc::new(SpectrogramTap::default());
        let (worker, reader, columns, scalars) = AudioFeatureWorker::spawn(
            consumer,
            sample_rate,
            channels,
            specs,
            gains.clone(),
            self.crossovers.clone(),
            tap.clone(),
        );
        log::info!(
            "[AudioMod] Capture started: source={source_label}, {send_count} sends, \
             {sample_rate}Hz {channels}ch"
        );
        self.capture = Some(AudioModCapture {
            _backend: backend,
            _worker: worker,
            reader,
            signature: desired,
            gains,
            tap,
            columns,
            scalars,
            sample_rate: sample_rate as f32,
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
