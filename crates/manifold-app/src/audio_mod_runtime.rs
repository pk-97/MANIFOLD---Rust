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

use manifold_audio::analysis::{
    AudioFeatureWorker, ColumnReader, CrossoverBank, FeatureReader, GainBank, SendSpec,
    SpectrogramTap,
};
use manifold_audio::capture::{AudioCaptureConfig, AudioCaptureDevice};
use manifold_core::audio_setup::{AudioDeviceRef, AudioSetup};
use manifold_core::id::AudioSendId;
use manifold_core::project::Project;
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
    _device: AudioCaptureDevice,
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
    /// Hot-plug subscription guard — unregisters the listener on drop.
    _hotplug_sub: manifold_audio::directory::Subscription,
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
}

impl Default for AudioModRuntime {
    fn default() -> Self {
        use std::sync::atomic::Ordering;
        let directory = manifold_audio::directory::system_directory();
        let devices_dirty = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = devices_dirty.clone();
        // The callback fires on an arbitrary HAL thread — only flip an atomic.
        let sub = directory.subscribe(Box::new(move || flag.store(true, Ordering::Relaxed)));
        Self {
            capture: None,
            last_version: 0,
            directory,
            devices_dirty,
            _hotplug_sub: sub,
            mic_access_requested: false,
            spec_send: None,
            spec_dirty: false,
            crossovers: Arc::new(CrossoverBank::new(
                manifold_core::audio_setup::DEFAULT_LOW_HZ,
                manifold_core::audio_setup::DEFAULT_MID_HZ,
            )),
            tapped_index: None,
        }
    }
}

impl AudioModRuntime {
    /// Reconcile the capture lifecycle (when the project changed, or a device
    /// hot-plugged) and feed the engine the latest feature snapshot. Call once
    /// per tick, before `engine.tick`.
    pub fn update(&mut self, engine: &mut PlaybackEngine, data_version: u64) {
        let hotplugged = self
            .devices_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        if data_version != self.last_version || hotplugged || self.spec_dirty {
            if hotplugged {
                // A device appeared/vanished/changed — drop capture so reconcile
                // rebuilds against the current hardware (the stored ref is
                // unchanged, so signature alone wouldn't trigger a rebuild).
                self.capture = None;
            }
            self.reconcile(engine.project());
            self.last_version = data_version;
            self.spec_dirty = false;
        }

        // Resolve the scope's send id → worker index against the live project and
        // write the tap (cheap, every tick — survives rebuilds).
        let tap_index = self
            .spec_send
            .as_ref()
            .and_then(|id| engine.project().and_then(|p| p.audio_setup.send_index(id)));
        if let Some(cap) = &self.capture {
            cap.tap.set_selected(tap_index);
        }
        self.tapped_index = tap_index;

        // Sync the Low/Mid/High crossovers from the project every tick (cheap
        // atomic stores). The worker reads this bank live, so a band-divider drag
        // retunes the analysis bands with no capture restart — same model as gain.
        if let Some(p) = engine.project() {
            self.crossovers
                .set(p.audio_setup.low_hz, p.audio_setup.mid_hz);
        }

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

    /// Bin count of the current column stream, or 0 if capture is inactive.
    pub fn spectrogram_num_bins(&self) -> usize {
        self.capture.as_ref().map_or(0, |c| c.columns.num_bins())
    }

    /// Worker index of the scope-tapped send, or `None` when nothing is tapped.
    /// The content thread uses this to pull the tapped send's features for the
    /// scope's per-band meters.
    pub fn tapped_send_index(&self) -> Option<usize> {
        self.tapped_index
    }

    /// Drain all complete VQT columns produced since the last call, oldest →
    /// newest. No-op when capture is inactive.
    pub fn drain_spectrogram_columns(&mut self, f: impl FnMut(&[f32])) {
        if let Some(cap) = &mut self.capture {
            cap.columns.drain_columns(f);
        }
    }

    /// The scope's analysed frequency range `(fmin, fmax)` Hz — for the
    /// frequency axis and band-divider overlays. `None` when capture is inactive.
    pub fn spectrogram_freq_range(&self) -> Option<(f32, f32)> {
        let cap = self.capture.as_ref()?;
        let cfg = manifold_spectral::SpectrogramConfig::default();
        Some((cfg.fmin, cfg.effective_fmax(cap.sample_rate)))
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

        // Resolve the stored device reference (UID, name fallback) to the
        // current openable device name. A configured-but-absent device leaves
        // capture dark (remappable policy); `None` = system default input.
        let device_name: Option<String> = match &project.audio_setup.device {
            Some(dev_ref) => match self.directory.resolve(dev_ref.uid_opt(), Some(&dev_ref.name)) {
                Some(info) => Some(info.name),
                None => {
                    log::warn!(
                        "[AudioMod] Saved audio device '{}' not present; audio \
                         modulation idle until it returns or is re-pointed",
                        dev_ref.name
                    );
                    return;
                }
            },
            None => None,
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

        let mut device = match AudioCaptureDevice::new(AudioCaptureConfig {
            device_name: device_name.clone(),
        }) {
            Ok(d) => d,
            Err(e) => {
                log::warn!(
                    "[AudioMod] Capture device unavailable ({e}); audio modulation idle \
                     until the input is re-pointed"
                );
                return;
            }
        };
        let sample_rate = device.sample_rate();
        let channels = device.channels();
        let Some(consumer) = device.take_consumer() else {
            log::error!("[AudioMod] Capture device returned no consumer");
            return;
        };
        if let Err(e) = device.start() {
            log::warn!("[AudioMod] Failed to start capture: {e}");
            return;
        }

        let tap = Arc::new(SpectrogramTap::default());
        let (worker, reader, columns) = AudioFeatureWorker::spawn(
            consumer,
            sample_rate,
            channels,
            specs,
            gains.clone(),
            self.crossovers.clone(),
            tap.clone(),
        );
        log::info!(
            "[AudioMod] Capture started: device={device_name:?}, {send_count} sends, \
             {sample_rate}Hz {channels}ch"
        );
        self.capture = Some(AudioModCapture {
            _device: device,
            _worker: worker,
            reader,
            signature: desired,
            gains,
            tap,
            columns,
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
