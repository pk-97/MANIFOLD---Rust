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

use manifold_audio::analysis::{AudioFeatureWorker, FeatureReader, SendSpec};
use manifold_audio::capture::{AudioCaptureConfig, AudioCaptureDevice};
use manifold_core::audio_setup::{AudioDeviceRef, AudioSetup};
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
        if data_version != self.last_version || hotplugged {
            if hotplugged {
                // A device appeared/vanished/changed — drop capture so reconcile
                // rebuilds against the current hardware (the stored ref is
                // unchanged, so signature alone wouldn't trigger a rebuild).
                self.capture = None;
            }
            self.reconcile(engine.project());
            self.last_version = data_version;
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

    /// Start, stop, or rebuild capture to match the project's audio setup.
    fn reconcile(&mut self, project: Option<&Project>) {
        let Some(project) = project else {
            self.capture = None;
            return;
        };

        let gate =
            project.has_active_audio_mods() && !project.audio_setup.sends.is_empty();
        if !gate {
            if self.capture.is_some() {
                log::info!("[AudioMod] Stopping capture — no active audio modulations");
                self.capture = None;
            }
            return;
        }

        let desired = CaptureSignature::from_setup(&project.audio_setup);
        if self.capture.as_ref().is_some_and(|c| c.signature == desired) {
            return; // already capturing with this exact config
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

        let (worker, reader) =
            AudioFeatureWorker::spawn(consumer, sample_rate, channels, specs);
        log::info!(
            "[AudioMod] Capture started: device={device_name:?}, {send_count} sends, \
             {sample_rate}Hz {channels}ch"
        );
        self.capture = Some(AudioModCapture {
            _device: device,
            _worker: worker,
            reader,
            signature: desired,
        });
    }
}
