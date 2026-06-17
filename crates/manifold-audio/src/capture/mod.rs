//! Audio capture — the **sample path**, behind one backend-neutral trait.
//!
//! A [`CaptureBackend`] streams interleaved Float32 samples into a lock-free
//! SPSC ring buffer and reports its sample rate + channel count. The rest of the
//! app consumes only the [`AudioConsumer`] + those two numbers — never a
//! platform type — so the same downstream analysis/recording code runs whatever
//! the source is. This mirrors the metadata split in [`crate::directory`] and
//! the backend-neutral split in `manifold-gpu` (Metal now, Vulkan later).
//!
//! Two source families implement the trait:
//!
//! * **Input devices** — hardware, aggregate, and virtual inputs (BlackHole,
//!   an audio interface, the built-in mic) via `cpal`. See [`cpal_input`].
//! * **Output taps** — the system-wide mix, or one application's audio, captured
//!   without any cable or loopback driver. macOS uses CoreAudio process taps
//!   (14.4+); other platforms have native equivalents (WASAPI loopback,
//!   PipeWire monitors) and are stubbed until implemented. See [`tap`].
//!
//! The runtime resolves a persisted `AudioDeviceRef` to a [`CaptureSource`] and
//! calls [`open`]; nothing above this module knows which backend it got.

mod cpal_input;

#[cfg_attr(target_os = "macos", path = "process_tap.rs")]
#[cfg_attr(not(target_os = "macos"), path = "unsupported_tap.rs")]
mod tap;

pub use cpal_input::{AudioCaptureConfig, AudioCaptureDevice, AudioDeviceInfo};

use crate::directory::TapHandle;

/// Ring buffer consumer type for reading captured audio samples. Interleaved
/// Float32, `channels`-wide (see [`CaptureBackend::channels`]).
pub type AudioConsumer = ringbuf::HeapCons<f32>;

/// A live audio capture stream, source-agnostic.
///
/// Construction opens the source and allocates the ring buffer but does **not**
/// start the realtime callback — the owner takes the consumer with
/// [`take_consumer`](Self::take_consumer), then calls [`start`](Self::start).
/// Dropping the backend stops capture and releases all OS resources.
///
/// `Send` so the content thread can own it; not `Sync` (single owner).
pub trait CaptureBackend: Send {
    /// Capture sample rate (Hz), from the source's native format.
    fn sample_rate(&self) -> u32;
    /// Interleave width of the samples written to the ring buffer.
    fn channels(&self) -> u16;
    /// Take the ring buffer consumer. Returns `Some` exactly once; the consumer
    /// is moved to whoever drains the stream (the analysis worker).
    fn take_consumer(&mut self) -> Option<AudioConsumer>;
    /// Begin the realtime callback. Samples start flowing into the ring buffer.
    fn start(&self) -> Result<(), String>;
    /// Pause the realtime callback. Idempotent; the stream can be restarted.
    fn stop(&self);
    /// Ring buffer overflow events since creation (consumer drained too slowly).
    fn overflow_count(&self) -> u64 {
        0
    }
}

/// A fully-resolved capture source, ready to [`open`]. This is the output of
/// resolving a persisted `AudioDeviceRef` against the live system: a device UID
/// has become an openable device name, an app bundle id has become live process
/// [`TapHandle`]s. Nothing here is persisted — it is recomputed each time
/// capture (re)builds, so it always reflects the current hardware/process state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureSource {
    /// The system default input device.
    DefaultInput,
    /// A specific input device, by its current openable name.
    Device { name: String },
    /// The whole system audio output mix, tapped.
    SystemAudio,
    /// One or more application processes, tapped and mixed down. Handles are
    /// live platform process identifiers from [`crate::directory`], meaningful
    /// only to the same platform's tap backend.
    Apps { handles: Vec<TapHandle> },
}

/// Open a capture backend for the given source. The realtime callback is not
/// started — call [`CaptureBackend::start`] after taking the consumer.
///
/// Tap sources ([`SystemAudio`](CaptureSource::SystemAudio),
/// [`Apps`](CaptureSource::Apps)) return an error on platforms / OS versions
/// without tap support; callers gate on [`crate::directory`]'s tap capabilities
/// before offering them, so this is the belt-and-suspenders guard.
pub fn open(source: CaptureSource) -> Result<Box<dyn CaptureBackend>, String> {
    match source {
        CaptureSource::DefaultInput => Ok(Box::new(AudioCaptureDevice::new(AudioCaptureConfig {
            device_name: None,
        })?)),
        CaptureSource::Device { name } => Ok(Box::new(AudioCaptureDevice::new(
            AudioCaptureConfig { device_name: Some(name) },
        )?)),
        CaptureSource::SystemAudio => tap::open_system_audio(),
        CaptureSource::Apps { handles } => tap::open_apps(&handles),
    }
}

/// Whether output-tap capture (system audio + per-app) is available on this
/// platform and OS version. The directory uses this to report tap capabilities;
/// the UI uses those to decide whether to offer the tap sources at all.
pub fn tap_supported() -> bool {
    tap::is_supported()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_supported_is_queryable() {
        // Just must not panic; the value is OS-dependent.
        let _ = tap_supported();
    }

    #[test]
    fn opening_an_empty_app_tap_is_an_error_not_a_panic() {
        // No process handles → never a valid tap, on every platform / OS version
        // (the macOS backend guards empty before touching the HAL).
        assert!(open(CaptureSource::Apps { handles: vec![] }).is_err());
    }

    #[test]
    fn capture_source_is_comparable() {
        assert_eq!(CaptureSource::SystemAudio, CaptureSource::SystemAudio);
        assert_ne!(
            CaptureSource::DefaultInput,
            CaptureSource::Device { name: "x".into() }
        );
    }
}
