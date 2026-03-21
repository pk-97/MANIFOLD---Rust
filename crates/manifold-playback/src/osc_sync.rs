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

use manifold_core::types::{ClockAuthority, PlaybackState};

use crate::sync::{SyncArbiter, SyncArbiterTarget, SyncTarget};
use crate::sync_source::SyncSource;
use crate::osc_receiver::OscReceiver;

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
    pub current_timecode_seconds: f32,
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
    last_timecode_received_time: f32,

    // ── Pending values (set by OSC callbacks, consumed in update()) ───
    // Port of `pendingTimecodeSeconds`, `hasNewTimecode`.
    pending_timecode_seconds: f32,
    has_new_timecode: bool,
}

impl OscSyncController {
    /// Construct with Unity's default field values.
    /// Port of Unity serialised field defaults.
    pub fn new() -> Self {
        Self {
            timecode_address: "time".to_string(),
            timecode_frame_rate: 29.97,
            drop_frame: true,
            seek_threshold: 0.05,
            timecode_offset: 0.0,
            follow_transport: true,
            transport_timeout: 0.5,
            show_debug_logs: false,

            is_osc_enabled: false,
            is_receiving_timecode: false,
            current_timecode_seconds: 0.0,
            current_timecode_display: "--:--:--:--".to_string(),

            cached_tc_h: -1,
            cached_tc_m: -1,
            cached_tc_s: -1,
            cached_tc_f: -1,

            was_receiving: false,
            last_timecode_received_time: 0.0,

            pending_timecode_seconds: -1.0,
            has_new_timecode: false,
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
        if self.is_osc_enabled { return true; }

        if !receiver.is_listening() {
            receiver.start_listening();
        }

        if !self.timecode_address.is_empty() {
            // Subscribe the timecode address.
            // NOTE: OscSyncController.OnTimecodeReceived runs on the main thread in Unity
            // (OscReceiver marshals to main thread via Update()). In the Rust port the host
            // calls osc_sync.on_timecode_received() after draining the OscReceiver's queue —
            // exact same semantics. The actual subscription key is stored so Disable can remove it.
            //
            // TODO: when native OSC is live, subscribe via:
            //   receiver.subscribe_keyed(&self.timecode_address, Box::new(move |addr, values| { ... }));
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
        if !self.is_osc_enabled { return; }

        if let Some(rcv) = receiver
            && !self.timecode_address.is_empty() {
                rcv.unsubscribe_all(&self.timecode_address);
            }

        self.is_osc_enabled = false;
        self.is_receiving_timecode = false;
        self.was_receiving = false;
        self.current_timecode_display = "--:--:--:--".to_string();
        self.has_new_timecode = false;

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

    /// Process an incoming OSC timecode message.
    /// In Unity this fires on the main thread (marshalled by OscReceiver.Update()).
    /// In Rust the host calls this after draining the OscReceiver queue.
    ///
    /// Port of Unity OscSyncController.OnTimecodeReceived(string address, float[] values).
    ///
    /// `now` = current wall-clock time in seconds (replaces Unity's `Time.time`).
    pub fn on_timecode_received(&mut self, _address: &str, values: &[f32], now: f32) {
        if values.len() >= 4 {
            let hours   = values[0] as i32;
            let minutes = values[1] as i32;
            let seconds = values[2] as i32;
            let frames  = values[3] as i32;

            self.pending_timecode_seconds =
                self.timecode_to_seconds(hours, minutes, seconds, frames) + self.timecode_offset;

            if hours != self.cached_tc_h
                || minutes != self.cached_tc_m
                || seconds != self.cached_tc_s
                || frames  != self.cached_tc_f
            {
                self.cached_tc_h = hours;
                self.cached_tc_m = minutes;
                self.cached_tc_s = seconds;
                self.cached_tc_f = frames;
                self.current_timecode_display =
                    format!("{:02}:{:02}:{:02}:{:02}", hours, minutes, seconds, frames);
            }
        } else if !values.is_empty() {
            self.pending_timecode_seconds = values[0] + self.timecode_offset;
            let total_sec = self.pending_timecode_seconds as i32;
            let h = total_sec / 3600;
            let m = (total_sec % 3600) / 60;
            let s = total_sec % 60;
            let f = ((self.pending_timecode_seconds - total_sec as f32) * self.timecode_frame_rate) as i32;

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
        now: f32,
        sync_target: &dyn SyncTarget,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        authority: ClockAuthority,
    ) {
        if !self.is_osc_enabled { return; }

        // Determine if timecode is actively being received.
        let receiving = (now - self.last_timecode_received_time) < self.transport_timeout;
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
        let osc_time   = self.current_timecode_seconds;
        let current_time = sync_target.current_time();
        let delta = (osc_time - current_time).abs();

        if delta < 0.001 { return; } // identical

        if sync_target.is_playing() {
            if delta < 0.5 {
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
                        current_time, osc_time, delta, self.current_timecode_display
                    );
                }
            }
        } else {
            // Not playing: only Seek when drift exceeds threshold (avoid churn while paused).
            if delta > self.seek_threshold {
                arbiter.seek(ClockAuthority::Osc, authority, arb_target, osc_time);

                if self.show_debug_logs {
                    log::debug!(
                        "[OscSync] Seek: {:.2} → {:.2} (delta={:.3}s) [{}]",
                        current_time, osc_time, delta, self.current_timecode_display
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
            let total_frames = 108000 * hours + 1800 * minutes + 30 * seconds + frames - dropped_frames;
            total_frames as f32 / 29.97
        } else {
            hours as f32 * 3600.0 + minutes as f32 * 60.0 + seconds as f32 + frames as f32 / self.timecode_frame_rate
        }
    }
}

// =================================================================
// ISyncSource implementation
// =================================================================

impl SyncSource for OscSyncController {
    fn is_enabled(&self) -> bool { self.is_osc_enabled }
    fn display_name(&self) -> &str { "OSC" }

    /// Enable without providing a receiver — caller must use enable_osc() directly
    /// when a receiver reference is available. This default fallback is a no-op stub
    /// that logs a warning, matching Unity's pattern where the receiver is always
    /// available because it's a scene component.
    fn enable(&mut self) {
        log::warn!("[OscSync] SyncSource::enable() called without OscReceiver — use enable_osc(receiver) directly");
    }

    fn disable(&mut self) {
        self.disable_osc(None);
    }
}

impl Default for OscSyncController {
    fn default() -> Self { Self::new() }
}
