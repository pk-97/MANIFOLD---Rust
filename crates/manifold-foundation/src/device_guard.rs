//! Process-wide gate on real hardware and machine-shared resources.
//!
//! Real devices are refused by default. Only the live GUI app calls
//! [`allow_real_devices`] (in `main`, after every headless subcommand has
//! branched off). Every test binary, the headless harness, perf-soak, ui-snap
//! and every tool binary therefore runs with the gate shut and needs no setup.
//! The default is deny so that a new harness or binary cannot forget to set it.
//!
//! Every real-device opener asks the gate first:
//! - [`require`] for openers that headless code never legitimately reaches
//!   (capture, the mic permission prompt, audio output, Link, MIDI ports).
//!   It panics when the gate is shut, which fails the test loudly.
//! - [`allowed`] for openers the headless content thread reaches as part of
//!   normal work (project load starts OSC, the Ableton bridge, LED output;
//!   MIDI discovery scans). The caller takes a stand-in path when it returns
//!   false: an ephemeral loopback port, a disconnected bridge, no LED output,
//!   an empty port list.
//!
//! `crates/manifold-foundation/tests/device_guard_contract.rs` fails if any
//! opener in the workspace reaches the OS without asking the gate.
//!
//! Set `MANIFOLD_DEVICE_LOG=1` to print every check to stderr.

use std::sync::atomic::{AtomicBool, Ordering};

/// A real device or machine-shared resource an opener is about to touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealDevice {
    /// An audio input device stream (the microphone, an interface, BlackHole).
    AudioInput,
    /// A CoreAudio process tap plus its aggregate device (system or app audio).
    AudioTap,
    /// The system microphone permission prompt.
    MicPermission,
    /// The default audio output device.
    AudioOutput,
    /// A CoreMIDI client (port scans and port connections).
    Midi,
    /// An Ableton Link session (UDP multicast on the local network).
    Link,
    /// The OSC listener on the app's fixed port.
    OscListen,
    /// OSC position messages sent to a local listener.
    OscSend,
    /// The Ableton Live OSC bridge (fixed receive port, sends to Live).
    AbletonBridge,
    /// ArtNet packets to the LED rig on the local network.
    ArtNet,
}

static REAL_DEVICES_ALLOWED: AtomicBool = AtomicBool::new(false);

/// Open the gate for this process. Only the live GUI app calls this.
pub fn allow_real_devices() {
    REAL_DEVICES_ALLOWED.store(true, Ordering::SeqCst);
}

/// Whether this process may open real devices, without logging a check.
/// For gates that decide whether to try at all (the capture runtime).
pub fn real_devices_enabled() -> bool {
    REAL_DEVICES_ALLOWED.load(Ordering::SeqCst)
}

/// Ask the gate before opening `device`. False means take the stand-in path.
pub fn allowed(device: RealDevice) -> bool {
    let ok = real_devices_enabled();
    log_check(device, ok);
    ok
}

/// Ask the gate before opening `device`; panics when the gate is shut.
pub fn require(device: RealDevice) {
    if !allowed(device) {
        panic!(
            "real device {device:?} opened outside the live app. Tests and headless \
             binaries must never touch real hardware (manifold_foundation::device_guard)."
        );
    }
}

fn log_check(device: RealDevice, ok: bool) {
    if std::env::var_os("MANIFOLD_DEVICE_LOG").is_some() {
        let verdict = if ok { "allowed" } else { "denied" };
        eprintln!("[device-guard] {device:?} {verdict}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_is_shut_until_the_app_opens_it() {
        assert!(
            !real_devices_enabled(),
            "a test process starts with real devices refused"
        );
        assert!(!allowed(RealDevice::AudioInput));
        assert!(std::panic::catch_unwind(|| require(RealDevice::AudioInput)).is_err());
        allow_real_devices();
        assert!(allowed(RealDevice::AudioInput));
        require(RealDevice::AudioInput);
    }
}
