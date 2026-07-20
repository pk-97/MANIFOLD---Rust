//! OSC timecode sync controller.
//! Mechanical translation of Unity OscSyncController.cs.
//!
//! Bridges LiveMTC (Ableton) timecode with MANIFOLD's playback transport via OSC.
//! One-way sync: Ableton controls MANIFOLD (timecode position + transport).
//!
//! Transport is derived from timecode activity:
//! - Timecode advancing → auto-play
//! - Timecode stops arriving (timeout) → auto-pause
//!
//! All transport and position writes go through SyncArbiter for structural
//! enforcement — the arbiter rejects calls when OSC is not the authority.
//!
//! OSC addresses are configurable to match LiveMTC Bridge configuration.
//! Note: BPM sync uses Ableton Link, not OSC (LiveMTC does not send BPM).

use parking_lot::Mutex;
use std::sync::Arc;

use manifold_core::Seconds;
use manifold_core::types::{ClockAuthority, PlaybackState};

use crate::osc_receiver::OscReceiver;
use crate::sync::{SyncArbiter, SyncArbiterTarget, SyncTarget};
use crate::sync_source::SyncSource;

/// Latest OSC timecode message captured by the subscription callback
/// (which runs on the content thread inside `OscReceiver::update()`'s
/// dispatch loop, but cannot hold `&mut OscSyncController` — see
/// `enable_osc`). Fixed-size buffer: SMPTE timecode is at most 4 floats
/// (H M S F), so no heap allocation on write (callback) or drain
/// (`drain_pending_osc_timecode`, called every content-thread frame).
#[derive(Clone, Copy)]
struct PendingOscTimecode {
    values: [f32; 4],
    len: u8,
}

/// OSC timecode sync controller.
/// Port of Unity OscSyncController.cs.
pub struct OscSyncController {
    // ── Serialised configuration ──────────────────────────────────────
    // Port of [SerializeField] fields.
    /// OSC address for timecode (4 floats: H M S F).
    /// Port of `timecodeAddress`.
    pub timecode_address: String,

    /// SMPTE frame rate (29.97 for 30 DF, 30 for 30 NDF, 24, 25).
    /// Port of `timecodeFrameRate`.
    pub timecode_frame_rate: f32,

    /// Drop-frame timecode (29.97 DF). Accounts for skipped frame numbers.
    /// Port of `dropFrame`.
    pub drop_frame: bool,

    /// Only seek when timecode drift exceeds this many seconds.
    /// Port of `seekThreshold`.
    pub seek_threshold: f32,

    /// Offset in seconds added to incoming timecode.
    /// Port of `timecodeOffset`.
    pub timecode_offset: f32,

    /// Auto-play when timecode starts arriving, auto-pause when it stops.
    /// Port of `followTransport`.
    pub follow_transport: bool,

    /// Seconds without timecode before auto-pausing.
    /// Port of `transportTimeout`.
    pub transport_timeout: f32,

    /// Port of `showDebugLogs`.
    pub show_debug_logs: bool,

    // ── Public properties (read by host / UI) ─────────────────────────
    // Port of public { get; private set; } properties.
    /// Port of `IsOscEnabled`.
    pub is_osc_enabled: bool,
    /// Port of `IsReceivingTimecode`.
    pub is_receiving_timecode: bool,
    /// Port of `CurrentTimecodeSeconds`.
    pub current_timecode_seconds: Seconds,
    /// Port of `CurrentTimecodeDisplay`.
    pub current_timecode_display: String,

    // ── Dirty-check cache for timecode display ────────────────────────
    // Port of `cachedTcH/M/S/F`. Avoids string alloc per OSC message.
    cached_tc_h: i32,
    cached_tc_m: i32,
    cached_tc_s: i32,
    cached_tc_f: i32,

    // ── State tracking ────────────────────────────────────────────────
    // Port of `wasReceiving`, `lastTimecodeReceivedTime`.
    was_receiving: bool,
    last_timecode_received_time: Seconds,

    // ── Pending values (set by OSC callbacks, consumed in update()) ───
    // Port of `pendingTimecodeSeconds`, `hasNewTimecode`.
    pending_timecode_seconds: Seconds,
    has_new_timecode: bool,

    /// Shared slot the OSC subscription callback writes into (see
    /// `enable_osc`). Drained once per frame by `drain_pending_osc_timecode`,
    /// BEFORE `update()`, to feed `on_timecode_received`. This is the
    /// approved shared-state bridge for the receiver→controller boundary
    /// (mirrors `OscParamRouter`'s pending-write slot) — its footprint stays
    /// local to this one address.
    pending_osc_message: Arc<Mutex<Option<PendingOscTimecode>>>,
}

impl OscSyncController {
    /// Construct with Unity's default field values.
    /// Port of Unity serialised field defaults, with one deliberate
    /// deviation: Unity's default was the bare string `"time"` (no leading
    /// `/`), which Unity's OSC library apparently accepted unvalidated. The
    /// Rust port's receiver decodes via `rosc`, which enforces OSC's actual
    /// address syntax (leading `/`) and rejects the WHOLE packet — not just
    /// this address — if it's missing. A bare `"time"` default would make
    /// this receive path silently unfixable by any wiring: no real UDP
    /// packet using valid OSC syntax could ever decode to it. `"/time"` is
    /// the OSC-valid form of the same address; this field isn't project-
    /// persisted (constructed fresh in app.rs each boot), so correcting the
    /// default has no serialization/back-compat surface.
    pub fn new() -> Self {
        Self {
            timecode_address: "/time".to_string(),
            timecode_frame_rate: 29.97,
            drop_frame: true,
            seek_threshold: 0.05,
            timecode_offset: 0.0,
            follow_transport: true,
            transport_timeout: 0.5,
            show_debug_logs: false,

            is_osc_enabled: false,
            is_receiving_timecode: false,
            current_timecode_seconds: Seconds::ZERO,
            current_timecode_display: "--:--:--:--".to_string(),

            cached_tc_h: -1,
            cached_tc_m: -1,
            cached_tc_s: -1,
            cached_tc_f: -1,

            was_receiving: false,
            // BUG-087 fix: far-past sentinel so `is_receiving_timecode` cannot
            // read a false positive in the first `transport_timeout` window of
            // a session. With `Seconds::ZERO`, `(now - last) < transport_timeout`
            // is true at boot (now ≈ 0) before any timecode has arrived — which
            // could even trip a spurious follow-transport PLAY. A far-past
            // default keeps the delta huge until a real frame lands.
            last_timecode_received_time: Seconds(f64::NEG_INFINITY),

            pending_timecode_seconds: Seconds(-1.0),
            has_new_timecode: false,

            pending_osc_message: Arc::new(Mutex::new(None)),
        }
    }

    // =================================================================
    // Lifecycle — port of Unity Awake / EnableOsc / DisableOsc
    // =================================================================

    /// Enable OSC sync.
    /// Port of Unity OscSyncController.EnableOsc().
    ///
    /// `receiver`: shared OscReceiver to subscribe on.
    /// Returns false if prerequisites are missing (no receiver).
    pub fn enable_osc(&mut self, receiver: &mut OscReceiver) -> bool {
        if self.is_osc_enabled {
            return true;
        }

        if !receiver.is_listening() {
            receiver.start_listening();
        }

        if !self.timecode_address.is_empty() {
            // Subscribe the timecode address. OscSyncController.OnTimecodeReceived
            // runs on the main thread in Unity (OscReceiver marshals to main thread
            // via Update()). In the Rust port the callback below runs on the content
            // thread too (inside OscReceiver::update()'s dispatch loop), but it
            // cannot hold `&mut OscSyncController` — OscCallback is `Fn + Send +
            // Sync`, and the receiver only ever hands out `&self`. So the callback
            // writes into `pending_osc_message` instead; the host drains it via
            // `drain_pending_osc_timecode()` once per frame, BEFORE calling
            // `update()`, which is what actually calls on_timecode_received().
            //
            // Plain `subscribe` (not `subscribe_keyed`) is used deliberately:
            // subscribe_keyed's `unsubscribe_keyed` uses swap_remove, which
            // invalidates other callbacks' keys for the same address (a known bug,
            // out of scope here). `unsubscribe_all` in disable_osc is exact for our
            // single-subscriber use of this address.
            let slot = Arc::clone(&self.pending_osc_message);
            receiver.subscribe(
                &self.timecode_address,
                Box::new(move |_addr, values| {
                    let len = values.len().min(4);
                    let mut buf = [0f32; 4];
                    buf[..len].copy_from_slice(&values[..len]);
                    *slot.lock() = Some(PendingOscTimecode {
                        values: buf,
                        len: len as u8,
                    });
                }),
            );
            log::info!(
                "[OscSync] Enabled — TC: {}, FollowTransport: {} (port {})",
                self.timecode_address,
                self.follow_transport,
                receiver.listen_port()
            );
        }

        self.was_receiving = false;
        self.is_osc_enabled = true;
        // ExternalTimeSync is managed per-frame based on is_receiving_timecode (not just enabled)
        true
    }

    /// Disable OSC sync and clear all state.
    /// Port of Unity OscSyncController.DisableOsc().
    pub fn disable_osc(&mut self, receiver: Option<&mut OscReceiver>) {
        if !self.is_osc_enabled {
            return;
        }

        if let Some(rcv) = receiver
            && !self.timecode_address.is_empty()
        {
            rcv.unsubscribe_all(&self.timecode_address);
        }

        self.is_osc_enabled = false;
        self.is_receiving_timecode = false;
        self.was_receiving = false;
        self.current_timecode_display = "--:--:--:--".to_string();
        self.has_new_timecode = false;
        *self.pending_osc_message.lock() = None;

        // syncArbiter?.ClearExternalTimeSync() — caller must forward this.
        log::info!("[OscSync] Disabled");
    }

    pub fn toggle_osc(&mut self, receiver: &mut OscReceiver) {
        if self.is_osc_enabled {
            self.disable_osc(Some(receiver));
        } else {
            self.enable_osc(receiver);
        }
    }

    // =================================================================
    // OSC Callback — port of Unity OnTimecodeReceived()
    // =================================================================

    /// Drain the latest OSC timecode message captured by the subscription
    /// callback installed in `enable_osc` (if any arrived since the last
    /// drain) and feed it into `on_timecode_received`.
    ///
    /// Call once per frame from the host update loop, AFTER
    /// `OscReceiver::update()` has dispatched this frame's UDP messages to
    /// subscribers, and BEFORE calling `update()` below — `update()` reads
    /// `has_new_timecode`/`last_timecode_received_time`, which this sets.
    ///
    /// Zero-allocation: the slot holds a fixed-size buffer, and `take()`
    /// moves it out of the `Option` without cloning. A no-op when no new
    /// message arrived this frame (the common case at 60Hz vs. the OSC
    /// bridge's own send rate).
    pub fn drain_pending_osc_timecode(&mut self, now: Seconds) {
        let msg = self.pending_osc_message.lock().take();
        if let Some(m) = msg {
            // Address is unused by on_timecode_received (values alone
            // determine the parse path) — "" is fine, avoids a clone of
            // timecode_address just to satisfy the parameter.
            self.on_timecode_received("", &m.values[..m.len as usize], now);
        }
    }

    /// Process an incoming OSC timecode message.
    /// In Unity this fires on the main thread (marshalled by OscReceiver.Update()).
    /// In Rust the host calls this after draining the OscReceiver queue.
    ///
    /// Port of Unity OscSyncController.OnTimecodeReceived(string address, float[] values).
    ///
    /// `now` = current wall-clock time in seconds (replaces Unity's `Time.time`).
    pub fn on_timecode_received(&mut self, _address: &str, values: &[f32], now: Seconds) {
        if values.len() >= 4 {
            let hours = values[0] as i32;
            let minutes = values[1] as i32;
            let seconds = values[2] as i32;
            let frames = values[3] as i32;

            self.pending_timecode_seconds = Seconds(
                (self.timecode_to_seconds(hours, minutes, seconds, frames) + self.timecode_offset)
                    as f64,
            );

            if hours != self.cached_tc_h
                || minutes != self.cached_tc_m
                || seconds != self.cached_tc_s
                || frames != self.cached_tc_f
            {
                self.cached_tc_h = hours;
                self.cached_tc_m = minutes;
                self.cached_tc_s = seconds;
                self.cached_tc_f = frames;
                self.current_timecode_display =
                    format!("{:02}:{:02}:{:02}:{:02}", hours, minutes, seconds, frames);
            }
        } else if !values.is_empty() {
            self.pending_timecode_seconds = Seconds((values[0] + self.timecode_offset) as f64);
            let total_sec = self.pending_timecode_seconds.0 as i32;
            let h = total_sec / 3600;
            let m = (total_sec % 3600) / 60;
            let s = total_sec % 60;
            let f = ((self.pending_timecode_seconds.0 as f32 - total_sec as f32)
                * self.timecode_frame_rate) as i32;

            if h != self.cached_tc_h
                || m != self.cached_tc_m
                || s != self.cached_tc_s
                || f != self.cached_tc_f
            {
                self.cached_tc_h = h;
                self.cached_tc_m = m;
                self.cached_tc_s = s;
                self.cached_tc_f = f;
                self.current_timecode_display = format!("{:02}:{:02}:{:02}:{:02}", h, m, s, f);
            }
        } else {
            return;
        }

        self.has_new_timecode = true;
        self.last_timecode_received_time = now;
    }

    // =================================================================
    // Update — port of Unity Update()
    // =================================================================

    /// Process pending timecode and transport detection.
    /// Call once per frame from the host update loop.
    ///
    /// `now`          — current time in seconds (replaces `Time.time`)
    /// `sync_target`  — read-only playback state
    /// `arbiter`      — gated write surface
    /// `arb_target`   — mutable playback target (forwarded by arbiter)
    /// `authority`    — current project's ClockAuthority
    ///
    /// Port of Unity OscSyncController.Update().
    pub fn update(
        &mut self,
        now: Seconds,
        sync_target: &dyn SyncTarget,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        authority: ClockAuthority,
    ) {
        if !self.is_osc_enabled {
            return;
        }

        // Determine if timecode is actively being received.
        let receiving = (now - self.last_timecode_received_time).0 < self.transport_timeout as f64;
        self.is_receiving_timecode = receiving;

        // Only suppress local deltaTime when OSC is the selected authority and
        // timecode is actively arriving — gated by arbiter.
        arbiter.set_external_time_sync(ClockAuthority::Osc, authority, arb_target, receiving);

        // Transport detection: timecode arriving = play, timecode stopped = pause.
        if self.follow_transport {
            if receiving && !self.was_receiving {
                // Timecode just started arriving → play.
                if sync_target.current_state() != PlaybackState::Playing {
                    arbiter.play(ClockAuthority::Osc, authority, arb_target);
                    log::info!("[OscSync] Transport: PLAY (timecode started)");
                }
            } else if !receiving && self.was_receiving {
                // Timecode stopped arriving → pause.
                if sync_target.current_state() == PlaybackState::Playing {
                    arbiter.pause(ClockAuthority::Osc, authority, arb_target, false);
                    log::info!("[OscSync] Transport: PAUSE (timecode timeout)");
                }
            }
        }
        self.was_receiving = receiving;

        // Process timecode — position writes gated by arbiter.
        if self.has_new_timecode {
            self.has_new_timecode = false;
            self.current_timecode_seconds = self.pending_timecode_seconds;
            self.sync_timecode_to_playback(sync_target, arbiter, arb_target, authority);
        }
    }

    // =================================================================
    // Sync methods — port of Unity SyncTimecodeToPlayback()
    // =================================================================

    fn sync_timecode_to_playback(
        &self,
        sync_target: &dyn SyncTarget,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        authority: ClockAuthority,
    ) {
        let osc_time = self.current_timecode_seconds;
        let current_time = sync_target.current_time();
        let delta = (osc_time - current_time).abs();

        if delta.0 < 0.001 {
            return;
        } // identical

        if sync_target.is_playing() {
            if delta.0 < 0.5 {
                // Normal sync: set time directly. No threshold — apply every OSC frame
                // so drift never accumulates. ExternalTimeSync prevents deltaTime from
                // fighting this, so the playhead advances purely from OSC timecode.
                arbiter.nudge_time(ClockAuthority::Osc, authority, arb_target, osc_time);
            } else {
                // Large jump during playback: full Seek (rebuilds clip state).
                arbiter.seek(ClockAuthority::Osc, authority, arb_target, osc_time);

                if self.show_debug_logs {
                    log::debug!(
                        "[OscSync] Seek: {:.2} → {:.2} (delta={:.3}s) [{}]",
                        current_time,
                        osc_time,
                        delta,
                        self.current_timecode_display
                    );
                }
            }
        } else {
            // Not playing: only Seek when drift exceeds threshold (avoid churn while paused).
            if delta.0 > self.seek_threshold as f64 {
                arbiter.seek(ClockAuthority::Osc, authority, arb_target, osc_time);

                if self.show_debug_logs {
                    log::debug!(
                        "[OscSync] Seek: {:.2} → {:.2} (delta={:.3}s) [{}]",
                        current_time,
                        osc_time,
                        delta,
                        self.current_timecode_display
                    );
                }
            }
        }
    }

    // =================================================================
    // Timecode conversion — port of Unity TimecodeToSeconds()
    // =================================================================

    /// Convert SMPTE timecode components to linear seconds.
    /// Port of Unity OscSyncController.TimecodeToSeconds().
    fn timecode_to_seconds(&self, hours: i32, minutes: i32, seconds: i32, frames: i32) -> f32 {
        if self.drop_frame {
            // SMPTE 12M drop-frame: convert displayed TC to linear frame count.
            // Frames 0,1 are skipped at each minute except 0,10,20,30,40,50.
            let total_minutes = 60 * hours + minutes;
            let dropped_frames = 2 * (total_minutes - total_minutes / 10);
            let total_frames =
                108000 * hours + 1800 * minutes + 30 * seconds + frames - dropped_frames;
            // BUG-091 fix: true NTSC drop-frame rate is 30000/1001
            // (≈29.970029970…), not the literal 29.97. Computed in f64 to keep
            // the divisor exact, then narrowed. At 00:01:00:02 this now yields
            // exactly 60.06s.
            (total_frames as f64 * 1001.0 / 30000.0) as f32
        } else {
            hours as f32 * 3600.0
                + minutes as f32 * 60.0
                + seconds as f32
                + frames as f32 / self.timecode_frame_rate
        }
    }
}

// =================================================================
// ISyncSource implementation
// =================================================================

impl SyncSource for OscSyncController {
    fn is_enabled(&self) -> bool {
        self.is_osc_enabled
    }
    fn display_name(&self) -> &str {
        "OSC"
    }

    /// Enable without providing a receiver — caller must use enable_osc() directly
    /// when a receiver reference is available. This default fallback is a no-op stub
    /// that logs a warning, matching Unity's pattern where the receiver is always
    /// available because it's a scene component.
    fn enable(&mut self) {
        log::warn!(
            "[OscSync] SyncSource::enable() called without OscReceiver — use enable_osc(receiver) directly"
        );
    }

    fn disable(&mut self) {
        self.disable_osc(None);
    }
}

impl Default for OscSyncController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::project::Project;

    // ── Drop-frame timecode vectors ──────────────────────────────────────
    //
    // BUG-091 FIXED: the drop-frame branch now divides by the true NTSC rate
    // 30000/1001, so the absolute seconds value is standards-exact and pinned
    // directly (see `drop_frame_absolute_seconds_are_standards_exact`). The two
    // *frame-drop pattern* tests below remain valid — they assert which display
    // numbers get skipped and which minute is exempt, as a self-consistent
    // frame-duration delta, independent of the exact divisor.

    #[test]
    fn drop_frame_skips_two_frame_numbers_at_non_tenth_minute_boundary() {
        let ctrl = OscSyncController::new(); // drop_frame: true by default
        let one_frame =
            ctrl.timecode_to_seconds(0, 0, 0, 1) - ctrl.timecode_to_seconds(0, 0, 0, 0);

        // SMPTE 12M: 00:00:59:29 is immediately followed by 00:01:00:02 —
        // display numbers 00:01:00:00 and 00:01:00:01 do not exist.
        let before = ctrl.timecode_to_seconds(0, 0, 59, 29);
        let after = ctrl.timecode_to_seconds(0, 1, 0, 2);
        assert!(
            (after - before - one_frame).abs() < 1e-4,
            "the raw frame count must advance by exactly one frame across the \
             skip boundary (00:00:59:29 -> 00:01:00:02): before={before}, after={after}, \
             one_frame={one_frame}"
        );
    }

    #[test]
    fn drop_frame_does_not_skip_at_ten_minute_boundary() {
        let ctrl = OscSyncController::new();
        let one_frame =
            ctrl.timecode_to_seconds(0, 0, 0, 1) - ctrl.timecode_to_seconds(0, 0, 0, 0);

        // SMPTE 12M: every 10th minute is exempt from the 2-frame skip, so
        // 00:09:59:29 -> 00:10:00:00 is an ordinary single-frame advance
        // (unlike the minute-1 boundary above).
        let before = ctrl.timecode_to_seconds(0, 9, 59, 29);
        let after = ctrl.timecode_to_seconds(0, 10, 0, 0);
        assert!(
            (after - before - one_frame).abs() < 1e-4,
            "the tenth-minute boundary must NOT skip (00:09:59:29 -> 00:10:00:00 \
             is a plain +1 frame): before={before}, after={after}, one_frame={one_frame}"
        );
    }

    /// BUG-091 fixed: the drop-frame divisor is the exact NTSC rate
    /// 30000/1001, so the absolute value is standards-exact. SMPTE 12M:
    /// 00:01:00:02 DF is exactly 60.06s (1800 raw frames × 1001/30000).
    #[test]
    fn drop_frame_absolute_seconds_are_standards_exact() {
        let ctrl = OscSyncController::new();
        let s = ctrl.timecode_to_seconds(0, 1, 0, 2);
        assert!(
            (s - 60.06).abs() < 1e-3,
            "00:01:00:02 drop-frame must be 60.06s (standards-exact), got {s}"
        );
    }

    /// The non-drop-frame branch is exact, unambiguous arithmetic
    /// (`h*3600 + m*60 + s + f/rate`) using the controller's own
    /// `timecode_frame_rate` field directly — no external-standard
    /// approximation involved, safe to assert absolutely.
    #[test]
    fn non_drop_frame_timecode_is_linear() {
        let mut ctrl = OscSyncController::new();
        ctrl.drop_frame = false;
        ctrl.timecode_frame_rate = 25.0;
        let secs = ctrl.timecode_to_seconds(1, 2, 3, 10);
        let expected = 1.0 * 3600.0 + 2.0 * 60.0 + 3.0 + 10.0 / 25.0;
        assert!((secs - expected).abs() < 1e-4);
    }

    // ── Nudge-vs-seek + transport-follow ─────────────────────────────────

    struct FakeSyncTarget {
        state: PlaybackState,
        time: Seconds,
        project: Option<Project>,
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
            Seconds(beat.as_f32() as f64 * 0.5)
        }
        fn current_project(&self) -> Option<&Project> {
            self.project.as_ref()
        }
    }

    #[derive(Default)]
    struct FakeArbTarget {
        external_time_sync: bool,
        played: bool,
        paused: bool,
        nudge_count: u32,
        seek_count: u32,
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
        fn nudge_time(&mut self, _time: Seconds) {
            self.nudge_count += 1;
        }
        fn seek(&mut self, _time: Seconds) {
            self.seek_count += 1;
        }
    }

    /// §11 OSC nudge/seek split = 0.5s: while playing, a small position
    /// error nudges every message rather than hard-seeking.
    #[test]
    fn osc_sync_nudges_when_playing_delta_under_threshold() {
        let ctrl = OscSyncController {
            current_timecode_seconds: Seconds(5.2),
            ..OscSyncController::new()
        };
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(5.0),
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();

        ctrl.sync_timecode_to_playback(&sync_target, &mut arbiter, &mut arb_target, ClockAuthority::Osc);

        assert_eq!(arb_target.nudge_count, 1);
        assert_eq!(arb_target.seek_count, 0);
    }

    #[test]
    fn osc_sync_hard_seeks_when_playing_delta_over_threshold() {
        let ctrl = OscSyncController {
            current_timecode_seconds: Seconds(6.0),
            ..OscSyncController::new()
        };
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(5.0), // delta 1.0s >= 0.5s
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();

        ctrl.sync_timecode_to_playback(&sync_target, &mut arbiter, &mut arb_target, ClockAuthority::Osc);

        assert_eq!(arb_target.seek_count, 1);
        assert_eq!(arb_target.nudge_count, 0);
    }

    #[test]
    fn osc_sync_playing_identical_time_is_noop() {
        let ctrl = OscSyncController {
            current_timecode_seconds: Seconds(5.0),
            ..OscSyncController::new()
        };
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(5.0),
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();

        ctrl.sync_timecode_to_playback(&sync_target, &mut arbiter, &mut arb_target, ClockAuthority::Osc);

        assert_eq!(arb_target.nudge_count, 0);
        assert_eq!(arb_target.seek_count, 0);
    }

    /// §11 OSC stopped `seek_threshold` = 0.05s: while stopped, only seek
    /// when drift exceeds the threshold (avoid churn while paused).
    #[test]
    fn osc_sync_stopped_seeks_beyond_threshold() {
        let ctrl = OscSyncController {
            current_timecode_seconds: Seconds(5.06),
            ..OscSyncController::new()
        };
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Stopped,
            time: Seconds(5.0), // delta 0.06s > 0.05s threshold
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();

        ctrl.sync_timecode_to_playback(&sync_target, &mut arbiter, &mut arb_target, ClockAuthority::Osc);

        assert_eq!(arb_target.seek_count, 1);
    }

    #[test]
    fn osc_sync_stopped_ignores_small_drift() {
        let ctrl = OscSyncController {
            current_timecode_seconds: Seconds(5.02),
            ..OscSyncController::new()
        };
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Stopped,
            time: Seconds(5.0), // delta 0.02s < 0.05s threshold
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();

        ctrl.sync_timecode_to_playback(&sync_target, &mut arbiter, &mut arb_target, ClockAuthority::Osc);

        assert_eq!(arb_target.seek_count, 0);
        assert_eq!(arb_target.nudge_count, 0);
    }

    /// §7: "timecode arriving = play, 0.5s silence = pause."
    #[test]
    fn osc_update_plays_when_timecode_starts_arriving() {
        let mut ctrl = OscSyncController::new();
        ctrl.is_osc_enabled = true;
        ctrl.was_receiving = false;
        ctrl.last_timecode_received_time = Seconds(0.0);
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Stopped,
            time: Seconds(0.0),
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();

        ctrl.update(
            Seconds(0.1),
            &sync_target,
            &mut arbiter,
            &mut arb_target,
            ClockAuthority::Osc,
        );

        assert!(arb_target.played, "timecode arriving while stopped must play");
    }

    #[test]
    fn osc_update_pauses_when_timecode_stops_arriving() {
        let mut ctrl = OscSyncController::new();
        ctrl.is_osc_enabled = true;
        ctrl.was_receiving = true;
        ctrl.last_timecode_received_time = Seconds(0.0);
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(1.0),
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();

        // 1.0s of silence exceeds the 0.5s transport_timeout.
        ctrl.update(
            Seconds(1.0),
            &sync_target,
            &mut arbiter,
            &mut arb_target,
            ClockAuthority::Osc,
        );

        assert!(
            arb_target.paused,
            "timecode silence beyond transport_timeout while playing must pause"
        );
    }

    /// BUG-087 regression: a fresh controller that has NEVER received timecode
    /// must not report receiving — nor trip a spurious follow-transport PLAY —
    /// in the first `transport_timeout` window of a session. The far-past
    /// sentinel default fixes it.
    #[test]
    fn osc_update_no_false_receive_at_startup_before_any_timecode() {
        let mut ctrl = OscSyncController::new();
        ctrl.is_osc_enabled = true; // enabled, but no timecode has EVER arrived
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Stopped,
            time: Seconds(0.0),
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();

        // First frames of the session, wall clock ≈ 0.
        ctrl.update(
            Seconds(0.05),
            &sync_target,
            &mut arbiter,
            &mut arb_target,
            ClockAuthority::Osc,
        );

        assert!(
            !ctrl.is_receiving_timecode,
            "no timecode has arrived — must not report receiving at startup"
        );
        assert!(
            !arb_target.played,
            "startup false-positive must not trigger a spurious play"
        );
    }
}
