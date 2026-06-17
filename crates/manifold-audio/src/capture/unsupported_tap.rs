//! Output-tap capture stub for platforms without an implementation yet.
//!
//! The seam is real even where the backend isn't: this module satisfies the same
//! interface [`process_tap`](super) does on macOS, so `capture::open` and the
//! directory's tap capabilities compile and behave identically everywhere — they
//! just report "unsupported." Each target platform has a native equivalent to
//! fill in here (see `docs/AUDIO_INFRASTRUCTURE.md` §11.4 for the mapping):
//!
//! * **Windows** — WASAPI loopback for system audio; per-process loopback via
//!   `ActivateAudioInterfaceAsync` + `AUDIOCLIENT_ACTIVATION_PARAMS`
//!   (process-loopback, Windows 10 2004+) for app audio.
//! * **Linux** — PipeWire monitor sources for system audio; per-node stream
//!   capture for app audio.
//!
//! When one is implemented, give it the same three entry points and the rest of
//! the app — runtime resolution, UI menu, persistence — works unchanged.

use super::CaptureBackend;
use crate::directory::TapHandle;

/// No tap support on this platform.
pub fn is_supported() -> bool {
    false
}

pub fn open_system_audio() -> Result<Box<dyn CaptureBackend>, String> {
    Err("system-audio tap capture is not supported on this platform".to_string())
}

pub fn open_apps(_handles: &[TapHandle]) -> Result<Box<dyn CaptureBackend>, String> {
    Err("per-application tap capture is not supported on this platform".to_string())
}
