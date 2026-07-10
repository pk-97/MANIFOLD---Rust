//! F1 — OSC/SMPTE timecode receive path wiring (CORE_ENGINE_MAP finding).
//!
//! Before this fix, `enable_osc` never subscribed on the `OscReceiver` (a
//! literal TODO), so `on_timecode_received` had zero real callers and
//! `is_receiving_timecode` could never become true. These tests drive the
//! REAL receive path — a synthetic OSC packet sent over a real UDP socket to
//! a real, listening `OscReceiver` — rather than calling
//! `on_timecode_received` directly, so they prove the wiring, not just the
//! parse logic.
//!
//! Per-frame loop mirrors `content_thread::tick_sync_controllers`:
//!   osc_receiver.update()             // drain UDP -> dispatch to subscribers
//!   osc_sync.drain_pending_osc_timecode(now)   // subscription slot -> on_timecode_received
//!   osc_sync.update(now, ...)         // transport-follow + position sync
//!
//! These tests also caught a second, independent bug the wiring fix alone
//! didn't solve: the default `timecode_address` was `"time"` (mechanically
//! ported from Unity's default), which is not a syntactically valid OSC
//! address (must start with `/`). `rosc`'s decoder — unlike whatever Unity's
//! OSC library did — rejects the WHOLE packet, not just the address match,
//! for a malformed address. So even with the subscribe/drain wiring fixed,
//! no real UDP timecode packet could ever have decoded. Fixed alongside the
//! wiring by changing the default to `"/time"` (see `osc_sync.rs::new()`).

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use manifold_core::Seconds;
use manifold_core::project::Project;
use manifold_core::types::{ClockAuthority, PlaybackState};

use manifold_playback::osc_receiver::OscReceiver;
use manifold_playback::osc_sync::OscSyncController;
use manifold_playback::sync::{SyncArbiter, SyncArbiterTarget, SyncTarget};

/// `OscSyncController::new()` defaults `last_timecode_received_time` to
/// `Seconds::ZERO` (not a sentinel). If a test's `now` also starts near
/// zero, `(now - last_timecode_received_time) < transport_timeout` reads
/// true on the very first tick regardless of whether any real message has
/// arrived — a false "receiving" positive that would make these tests pass
/// without exercising the wiring at all. Offsetting `now` well past
/// `transport_timeout` (default 0.5s) avoids it without touching production
/// code; the same false-positive window exists at real app startup too
/// (t=0), logged separately (BUG-087) since it's a distinct bug from F1's
/// wiring gap.
const NOW_BASELINE_SECS: f64 = 10.0;

/// Minimal scripted `SyncTarget`: read-only playback state the controller
/// consults each `update()` call.
struct FakeSyncTarget {
    state: PlaybackState,
    time: Seconds,
}

impl SyncTarget for FakeSyncTarget {
    fn current_state(&self) -> PlaybackState {
        self.state
    }
    fn current_time(&self) -> Seconds {
        self.time
    }
    fn is_playing(&self) -> bool {
        self.state == PlaybackState::Playing
    }
    fn timeline_beat_to_time(&self, beat: manifold_core::Beats) -> Seconds {
        Seconds((beat.as_f32() * 0.5) as f64) // 120 BPM fallback, unused by these tests
    }
    fn current_project(&self) -> Option<&Project> {
        None
    }
}

/// Minimal scripted `SyncArbiterTarget`: records what the arbiter forwards
/// (play/pause/seek/nudge) so tests can assert on the controller's output
/// without a real `PlaybackEngine`.
#[derive(Default)]
struct FakeArbTarget {
    external_time_sync: bool,
    played: bool,
    paused: bool,
    seeked_to: Option<Seconds>,
    nudged_to: Option<Seconds>,
}

impl SyncArbiterTarget for FakeArbTarget {
    fn current_project(&self) -> Option<&Project> {
        None
    }
    fn external_time_sync(&self) -> bool {
        self.external_time_sync
    }
    fn set_external_time_sync(&mut self, value: bool) {
        self.external_time_sync = value;
    }
    fn play(&mut self) {
        self.played = true;
    }
    fn pause(&mut self, _clear_recording: bool) {
        self.paused = true;
    }
    fn nudge_time(&mut self, time: Seconds) {
        self.nudged_to = Some(time);
    }
    fn seek(&mut self, time: Seconds) {
        self.seeked_to = Some(time);
    }
}

/// Encode an OSC message with the given address and float args, per
/// `osc_receiver.rs`'s decode side (`rosc::decoder::decode_udp`).
fn encode_osc_floats(address: &str, values: &[f32]) -> Vec<u8> {
    let msg = rosc::OscMessage {
        addr: address.to_string(),
        args: values.iter().map(|v| rosc::OscType::Float(*v)).collect(),
    };
    rosc::encoder::encode(&rosc::OscPacket::Message(msg)).expect("encode OSC message")
}

/// Send a UDP packet to the receiver's listen port from an ephemeral socket
/// on localhost — exercises the real bind→decode→queue→dispatch path in
/// `osc_receiver.rs`, not a direct in-process call.
fn send_udp(port: i32, bytes: &[u8]) {
    let sock = UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral send socket");
    sock.send_to(bytes, ("127.0.0.1", port as u16))
        .expect("send OSC packet");
}

/// Drive one frame of the drain loop this test mirrors from
/// `tick_sync_controllers`, then run the controller's `update()`.
#[allow(clippy::too_many_arguments)]
fn tick(
    receiver: &mut OscReceiver,
    osc_sync: &mut OscSyncController,
    now: Seconds,
    target: &FakeSyncTarget,
    arbiter: &mut SyncArbiter,
    arb_target: &mut FakeArbTarget,
) {
    receiver.update();
    osc_sync.drain_pending_osc_timecode(now);
    osc_sync.update(now, target, arbiter, arb_target, ClockAuthority::Osc);
}

/// Poll `receiver.update()` + drain until the controller reports it is
/// receiving timecode, or fail after a short real-time budget. UDP is
/// real network I/O even on loopback, so a fixed retry loop (not a single
/// call) is needed to avoid flakiness.
fn wait_for_receiving(
    receiver: &mut OscReceiver,
    osc_sync: &mut OscSyncController,
    target: &FakeSyncTarget,
    arbiter: &mut SyncArbiter,
    arb_target: &mut FakeArbTarget,
    start: Instant,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let now = Seconds(NOW_BASELINE_SECS + start.elapsed().as_secs_f64());
        tick(receiver, osc_sync, now, target, arbiter, arb_target);
        if osc_sync.is_receiving_timecode {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// The core F1 proof: a synthetic SMPTE timecode packet sent over a real UDP
/// socket to a bound, listening `OscReceiver` reaches `on_timecode_received`
/// and drives the controller — is_receiving_timecode flips true, and (paused
/// + drift beyond seek_threshold) the arbiter target receives a seek to the
/// decoded timecode.
#[test]
fn osc_timecode_packet_drives_seek_when_paused() {
    let mut receiver = OscReceiver::new();
    receiver.set_port(19801);
    let mut osc_sync = OscSyncController::new();
    osc_sync.timecode_address = "/time".to_string();
    assert!(
        osc_sync.enable_osc(&mut receiver),
        "enable_osc must succeed with a receiver present"
    );
    assert!(osc_sync.is_osc_enabled);

    let target = FakeSyncTarget {
        state: PlaybackState::Paused,
        time: Seconds(0.0),
    };
    let mut arbiter = SyncArbiter::new();
    let mut arb_target = FakeArbTarget::default();

    // 00:00:10:00 non-drop-frame @ 30fps -> 10.0s. drop_frame defaults true,
    // so disable it for an exact expected value.
    osc_sync.drop_frame = false;
    osc_sync.timecode_frame_rate = 30.0;
    let packet = encode_osc_floats("/time", &[0.0, 0.0, 10.0, 0.0]);

    let start = Instant::now();
    send_udp(receiver.listen_port(), &packet);

    let received = wait_for_receiving(
        &mut receiver,
        &mut osc_sync,
        &target,
        &mut arbiter,
        &mut arb_target,
        start,
    );
    assert!(
        received,
        "OscSyncController never observed timecode after a real UDP packet — \
         the receive path (subscribe -> pending slot -> drain -> \
         on_timecode_received) is not wired"
    );

    assert!(
        (osc_sync.current_timecode_seconds.0 - 10.0).abs() < 0.05,
        "decoded timecode seconds wrong: {}",
        osc_sync.current_timecode_seconds.0
    );

    // Paused + drift (10.0s) exceeds seek_threshold (0.05s default) ->
    // sync_timecode_to_playback must have issued a seek to ~10.0s.
    let seeked = arb_target
        .seeked_to
        .expect("paused controller receiving timecode beyond seek_threshold must seek");
    assert!(
        (seeked.0 - 10.0).abs() < 0.05,
        "seek target wrong: {}",
        seeked.0
    );

    // Cleanup: unsubscribe + stop the background thread.
    osc_sync.disable_osc(Some(&mut receiver));
    assert!(!osc_sync.is_osc_enabled);
}

/// Transport-follow: timecode arriving while stopped/paused must trigger
/// PLAY (CORE_ENGINE_MAP §7's ported-from-Unity semantics — pinned here as
/// the wiring contract, not a claim about a live Ableton lock holding on
/// stage).
#[test]
fn osc_timecode_arrival_triggers_play() {
    let mut receiver = OscReceiver::new();
    receiver.set_port(19802);
    let mut osc_sync = OscSyncController::new();
    osc_sync.timecode_address = "/time".to_string();
    osc_sync.follow_transport = true;
    assert!(osc_sync.enable_osc(&mut receiver));

    let target = FakeSyncTarget {
        state: PlaybackState::Stopped,
        time: Seconds(0.0),
    };
    let mut arbiter = SyncArbiter::new();
    let mut arb_target = FakeArbTarget::default();

    let start = Instant::now();
    let packet = encode_osc_floats("/time", &[0.0, 0.0, 1.0, 0.0]);
    send_udp(receiver.listen_port(), &packet);

    let received = wait_for_receiving(
        &mut receiver,
        &mut osc_sync,
        &target,
        &mut arbiter,
        &mut arb_target,
        start,
    );
    assert!(received, "timecode never observed — wiring broken");
    assert!(
        arb_target.played,
        "timecode starting to arrive while stopped must trigger PLAY \
         (follow_transport wiring)"
    );

    osc_sync.disable_osc(Some(&mut receiver));
}
