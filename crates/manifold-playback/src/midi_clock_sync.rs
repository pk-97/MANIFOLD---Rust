//! MIDI Clock/SPP sync controller.
//!
//! One-way sync: the DAW drives MANIFOLD's transport and beat by MIDI clock.
//! The midir callback stamps every clock message and sends it through a
//! channel; the content thread rebuilds the position by MIDI 1.0 rules and
//! filters the tick times into a beat at any instant
//! (FRAME_EXACT_TRANSPORT_DESIGN D20, `tick_filter`).

pub mod tick_filter;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use manifold_core::types::ClockAuthority;
use manifold_core::{Beats, Seconds};

use crate::sync::{SyncArbiter, SyncArbiterTarget, SyncTarget};
use crate::sync_source::SyncSource;
use tick_filter::{ClockKind, ClockSample, ClockTracker};

/// Ticks between two content ticks outside this range are a song-position
/// jump or a restart: a seek.
const SANE_TICK_DELTA: std::ops::RangeInclusive<i64> = 0..=384;

/// One clock message and when it arrived (D20).
#[derive(Debug, Clone, Copy)]
struct ClockEvent {
    kind: ClockKind,
    at: Instant,
}

/// midir's packet timestamp (microseconds) as an `Instant`. The smallest
/// delivery lag seen is taken as zero, so the mapping needs no knowledge of
/// the timestamp's base; a stall then shows as late delivery, not late
/// ticks. A missing stamp (0) is the callback's own `Instant::now()`.
struct StampMap {
    origin: Instant,
    min_offset_us: Option<i128>,
}

/// A stamp older than this against the best mapping means the base moved.
const STAMP_REBASE_US: i128 = 5_000_000;

impl StampMap {
    fn new() -> Self {
        Self { origin: Instant::now(), min_offset_us: None }
    }

    fn map(&mut self, stamp_us: u64, now: Instant) -> Instant {
        if stamp_us == 0 {
            return now;
        }
        let now_us = now.saturating_duration_since(self.origin).as_micros() as i128;
        let offset = now_us - stamp_us as i128;
        let min = match self.min_offset_us {
            Some(min) if offset - min <= STAMP_REBASE_US => min.min(offset),
            _ => offset,
        };
        self.min_offset_us = Some(min);
        let at_us = (stamp_us as i128 + min).clamp(0, now_us);
        self.origin + Duration::from_micros(at_us as u64)
    }
}

struct ClockSender {
    tx: mpsc::Sender<ClockEvent>,
    stamps: StampMap,
}

fn clock_kind(message: &[u8]) -> Option<ClockKind> {
    match message.first()? {
        0xF8 => Some(ClockKind::Tick),
        0xFA => Some(ClockKind::Start),
        0xFB => Some(ClockKind::Continue),
        0xFC => Some(ClockKind::Stop),
        0xF2 if message.len() >= 3 => {
            Some(ClockKind::SongPosition((u16::from(message[2]) << 7) | u16::from(message[1])))
        }
        _ => None,
    }
}

// ── MidiClockReceiver ─────────────────────────────────────────────────────────

/// Receives MIDI clock, Song Position Pointer, Start, Continue and Stop via
/// midir and sends each, stamped, to the content thread. The OS MIDI thread
/// never blocks.
struct MidiClockReceiver {
    events: Option<mpsc::Receiver<ClockEvent>>,
    connection: Option<midir::MidiInputConnection<ClockSender>>,
}

impl MidiClockReceiver {
    fn new() -> Self {
        Self { events: None, connection: None }
    }

    /// Open a midir connection to the port at `source_index`.
    fn start(&mut self, source_index: i32) {
        let midi_in = match midir::MidiInput::new("manifold-clock") {
            Ok(m) => m,
            Err(e) => {
                log::warn!("[MidiClockSync] Failed to create MidiInput: {}", e);
                return;
            }
        };

        let ports = midi_in.ports();
        let port = match ports.get(source_index as usize) {
            Some(p) => p,
            None => {
                log::warn!(
                    "[MidiClockSync] Port index {} not available ({} port(s) found)",
                    source_index,
                    ports.len()
                );
                return;
            }
        };

        let (tx, rx) = mpsc::channel();
        let sender = ClockSender { tx, stamps: StampMap::new() };

        let connection = match midi_in.connect(
            port,
            "manifold-clock-conn",
            move |timestamp_us: u64, message: &[u8], sender: &mut ClockSender| {
                if message.is_empty() {
                    return;
                }
                let now = Instant::now();
                // Rig log (C0): every message with midir's stamp and ours.
                crate::rig_log::emit(crate::rig_log::RigRecord::ClkByte {
                    at_ns: crate::rig_log::now_ns(),
                    midir_us: timestamp_us,
                    status: message[0],
                    d1: message.get(1).copied().unwrap_or(0),
                    d2: message.get(2).copied().unwrap_or(0),
                });
                if let Some(kind) = clock_kind(message) {
                    let at = sender.stamps.map(timestamp_us, now);
                    let _ = sender.tx.send(ClockEvent { kind, at });
                }
            },
            sender,
        ) {
            Ok(c) => c,
            Err(e) => {
                log::warn!(
                    "[MidiClockSync] Failed to connect to port {}: {}",
                    source_index,
                    e
                );
                return;
            }
        };

        self.events = Some(rx);
        self.connection = Some(connection);
        log::info!(
            "[MidiClockSync] midir clock receiver connected (port {})",
            source_index
        );
    }

    /// Close the midir connection.
    fn stop(&mut self) {
        self.connection = None;
        self.events = None;
        log::info!("[MidiClockSync] midir clock receiver disconnected");
    }

    /// Number of available MIDI input ports.
    fn source_count() -> usize {
        match midir::MidiInput::new("manifold-clock-scan") {
            Ok(m) => m.ports().len(),
            Err(_) => 0,
        }
    }

    /// Display name of the MIDI port at `index`.
    fn source_name(index: i32) -> String {
        let midi_in = match midir::MidiInput::new("manifold-clock-scan") {
            Ok(m) => m,
            Err(_) => return format!("Port {}", index),
        };
        let ports = midi_in.ports();
        match ports.get(index as usize) {
            Some(p) => midi_in
                .port_name(p)
                .unwrap_or_else(|_| format!("Port {}", index)),
            None => format!("Port {}", index),
        }
    }
}

// ── MidiClockSyncController ───────────────────────────────────────────────────

/// MIDI Clock sync controller.
pub struct MidiClockSyncController {
    is_midi_clock_enabled: bool,
    is_receiving_clock: bool,
    current_position_display: String,
    hard_seek_count: i32,
    last_hard_seek_delta_seconds: f32,
    selected_source_index: i32,
    /// Cached display name of the selected MIDI source. Updated on
    /// enable/disable/change_source — avoids per-frame CoreMIDI enumeration.
    cached_source_name: String,

    clock_signal_timeout: f32,

    last_is_playing: bool,
    last_position_sixteenths: i32,
    /// The landed tick the last position sync saw, for the jump check.
    last_synced_tick: Option<i64>,
    last_clock_activity_time: Seconds,
    cached_display_sixteenths: i32,

    /// Event times are seconds since this instant.
    origin: Instant,
    tracker: ClockTracker,

    receiver: Option<MidiClockReceiver>,
}

impl MidiClockSyncController {
    pub fn new() -> Self {
        Self {
            is_midi_clock_enabled: false,
            is_receiving_clock: false,
            current_position_display: "---".into(),
            hard_seek_count: 0,
            last_hard_seek_delta_seconds: 0.0,
            selected_source_index: -1,
            cached_source_name: "None".into(),

            clock_signal_timeout: 0.5,

            last_is_playing: false,
            last_position_sixteenths: -1,
            last_synced_tick: None,
            last_clock_activity_time: Seconds(-999.0),
            cached_display_sixteenths: -1,

            origin: Instant::now(),
            tracker: ClockTracker::new(),

            receiver: None,
        }
    }

    // ── Public properties ────────────────────────────────────────────

    pub fn is_midi_clock_enabled(&self) -> bool {
        self.is_midi_clock_enabled
    }
    pub fn is_receiving_clock(&self) -> bool {
        self.is_receiving_clock
    }
    pub fn is_clock_transport_playing(&self) -> bool {
        self.is_receiving_clock && self.last_is_playing
    }
    pub fn current_position_display(&self) -> &str {
        &self.current_position_display
    }
    /// The filter's tempo; 120 before any estimate.
    pub fn current_clock_bpm(&self) -> f32 {
        self.tracker.bpm().unwrap_or(120.0) as f32
    }

    /// The last landed tick's beat, for display and the rig log. The beat
    /// the follower reads is `sample_at`.
    pub fn current_clock_beat(&self) -> f32 {
        self.tracker.landed_tick().map_or(0.0, |k| k as f32 / 24.0)
    }

    /// The clock's beat and tempo at `r` (D7, D20); none while stopped,
    /// before the first tick after a seed point, or when stale.
    pub fn sample_at(&self, r: Instant) -> Option<ClockSample> {
        self.tracker.sample_at(self.seconds(r))
    }

    fn seconds(&self, at: Instant) -> f64 {
        at.saturating_duration_since(self.origin).as_secs_f64()
    }

    pub fn hard_seek_count(&self) -> i32 {
        self.hard_seek_count
    }
    pub fn last_hard_seek_delta_seconds(&self) -> f32 {
        self.last_hard_seek_delta_seconds
    }
    pub fn selected_source_index(&self) -> i32 {
        self.selected_source_index
    }

    /// Number of available MIDI input ports.
    pub fn available_source_count() -> usize {
        MidiClockReceiver::source_count()
    }

    /// Display name of the MIDI port at `index`.
    pub fn available_source_name(index: i32) -> String {
        MidiClockReceiver::source_name(index)
    }

    /// List all available MIDI input port names.
    pub fn available_source_names() -> Vec<String> {
        let count = MidiClockReceiver::source_count();
        (0..count)
            .map(|i| MidiClockReceiver::source_name(i as i32))
            .collect()
    }

    /// Display name of the currently selected MIDI source.
    pub fn selected_source_name(&self) -> &str {
        &self.cached_source_name
    }

    // ── Lifecycle ────────────────────────────────────────────────────

    /// Enable MIDI Clock on the given source index; a negative index takes
    /// the first source.
    pub fn enable_midi_clock(&mut self, source_index: i32) {
        if self.is_midi_clock_enabled {
            return;
        }

        let source_index = if source_index < 0 {
            let count = MidiClockReceiver::source_count();
            if count > 0 {
                0
            } else {
                log::error!("[MidiClockSync] No MIDI sources available.");
                return;
            }
        } else {
            source_index
        };

        self.selected_source_index = source_index;
        self.cached_source_name = MidiClockReceiver::source_name(source_index);

        let mut receiver = MidiClockReceiver::new();
        receiver.start(source_index);
        self.receiver = Some(receiver);

        self.reset_state();
        self.is_midi_clock_enabled = true;

        log::info!(
            "[MidiClockSync] Enabled — source: {}",
            self.cached_source_name
        );
    }

    fn reset_state(&mut self) {
        self.last_is_playing = false;
        self.last_position_sixteenths = -1;
        self.last_synced_tick = None;
        self.last_clock_activity_time = Seconds(-999.0);
        self.hard_seek_count = 0;
        self.last_hard_seek_delta_seconds = 0.0;
        self.tracker = ClockTracker::new();
    }

    /// Switch to a different MIDI source. Restarts the receiver.
    pub fn change_source(&mut self, source_index: i32) {
        if self.is_midi_clock_enabled {
            self.disable_midi_clock();
            self.enable_midi_clock(source_index);
        } else {
            self.selected_source_index = source_index;
            self.cached_source_name = if source_index >= 0 {
                MidiClockReceiver::source_name(source_index)
            } else {
                "None".into()
            };
        }
    }

    /// Disable MIDI Clock and release the receiver.
    pub fn disable_midi_clock(&mut self) {
        if !self.is_midi_clock_enabled {
            return;
        }

        self.is_midi_clock_enabled = false;
        self.is_receiving_clock = false;
        self.cached_source_name = "None".into();
        self.current_position_display = "---".into();
        self.reset_state();

        if let Some(mut receiver) = self.receiver.take() {
            receiver.stop();
        }

        log::info!("[MidiClockSync] Disabled");
    }

    /// Drain this tick's clock events in order, then forward transport and
    /// position to MANIFOLD.
    ///
    /// `suppress_clock_plane`: caller-computed gate (ABLETON_TRANSPORT_SYNC_DESIGN
    /// D5): seek-cooldown (M4L path) OR an unacknowledged AbletonOSC transport
    /// command in flight. While true, MIDI Clock must not drive the engine —
    /// position OR transport — because everything Ableton emits is known-stale
    /// until the command plane confirms.
    pub fn update(
        &mut self,
        now: Seconds,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        sync_target: &dyn SyncTarget,
        authority: ClockAuthority,
        suppress_clock_plane: bool,
    ) {
        if !self.is_midi_clock_enabled {
            return;
        }
        if let Some(events) = self.receiver.as_ref().and_then(|r| r.events.as_ref()) {
            // The map's tempo at the armed position seeds a filter with no
            // estimate yet (D20).
            let armed = Beats(self.tracker.next_tick() as f64 / 24.0);
            let beat_seconds = (sync_target.timeline_beat_to_time(armed + Beats(1.0)) - sync_target.timeline_beat_to_time(armed)).0;
            let fallback_bpm = if beat_seconds > 0.0 { 60.0 / beat_seconds } else { 120.0 };
            while let Ok(event) = events.try_recv() {
                let at = event.at.saturating_duration_since(self.origin).as_secs_f64();
                self.tracker.event(event.kind, at, fallback_bpm);
                self.last_clock_activity_time = now;
            }
        }
        self.apply(now, arbiter, arb_target, sync_target, authority, suppress_clock_plane);
    }

    fn apply(
        &mut self,
        now: Seconds,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        sync_target: &dyn SyncTarget,
        authority: ClockAuthority,
        suppress_clock_plane: bool,
    ) {
        let is_playing = self.tracker.is_playing();
        let was_receiving = self.is_receiving_clock;
        let has_recent_clock_activity =
            (now - self.last_clock_activity_time).0 as f32 <= self.clock_signal_timeout;
        self.is_receiving_clock = has_recent_clock_activity;

        if has_recent_clock_activity && !was_receiving {
            log::info!(
                "[MidiClockSync] SYNC ESTABLISHED — receiving clock from {}",
                self.selected_source_name()
            );
        } else if !has_recent_clock_activity && was_receiving {
            log::warn!(
                "[MidiClockSync] SYNC LOST — no clock signal for {:.1}s",
                self.clock_signal_timeout
            );
        }

        // Whether MANIFOLD initiated this play session.
        let manifold_owns = arbiter.manifold_owns_playback;

        // Transport sync — gated by arbiter authority check. Also held while
        // the clock plane is suppressed: during an in-flight play-from-cursor,
        // Ableton's clock still says "stopped" — relaying that as a pause is
        // the "doesn't respond" flap.
        if !manifold_owns && !suppress_clock_plane {
            let playing = has_recent_clock_activity && is_playing;
            if playing {
                if !sync_target.is_playing() {
                    arbiter.play(ClockAuthority::MidiClock, authority, arb_target);
                    if playing != self.last_is_playing {
                        log::info!("[MidiClockSync] Transport: PLAY");
                    }
                }
            } else if sync_target.is_playing() {
                arbiter.pause(ClockAuthority::MidiClock, authority, arb_target, false);
                if playing != self.last_is_playing {
                    log::info!("[MidiClockSync] Transport: PAUSE");
                }
            }
            self.last_is_playing = playing;
        }

        // Clear MANIFOLD ownership when CLK shows stopped, after the grace
        // period of the OSC→DAW→MIDI round trip.
        if manifold_owns && (!has_recent_clock_activity || !is_playing) {
            arbiter.clear_ownership_if_expired(now);
        }

        // Position sync — manifold_owns gates transport only. Suppressed while
        // the clock plane is gated: the clock is reporting a stale position.
        if has_recent_clock_activity && !suppress_clock_plane {
            let ticks = if is_playing {
                self.tracker.landed_tick().unwrap_or(self.tracker.next_tick())
            } else {
                self.tracker.next_tick()
            };
            let sixteenths = i32::try_from(ticks.div_euclid(6)).unwrap_or(i32::MAX);
            self.update_position_display(sixteenths);
            self.sync_position_to_playback(ticks, arbiter, arb_target, sync_target, authority);
        }
    }

    // ── Position sync ────────────────────────────────────────────────

    /// Seek MANIFOLD to the clock's position on a jump: a Song Position
    /// Pointer or restart (a tick delta outside the sane range), an error of
    /// 2 s or more while playing, or any position change while stopped.
    /// Inside 2 s the follower trims the beat from this tick's sample (D6).
    fn sync_position_to_playback(
        &mut self,
        ticks: i64,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        sync_target: &dyn SyncTarget,
        authority: ClockAuthority,
    ) {
        if sync_target.current_project().is_none() {
            return;
        }

        let current_sixteenths = i32::try_from(ticks.div_euclid(6)).unwrap_or(i32::MAX);
        let clock_time = sync_target.timeline_beat_to_time(Beats(ticks as f64 / 24.0));
        let is_transport_jump = self
            .last_synced_tick
            .is_some_and(|last| !SANE_TICK_DELTA.contains(&(ticks - last)));
        self.last_synced_tick = Some(ticks);

        let delta = (clock_time - sync_target.position_time()).abs();

        if sync_target.is_playing() {
            if is_transport_jump || delta >= Seconds(2.0) {
                self.hard_seek_count += 1;
                self.last_hard_seek_delta_seconds = delta.as_f32();
                arbiter.seek(ClockAuthority::MidiClock, authority, arb_target, clock_time);
            }
        } else if current_sixteenths != self.last_position_sixteenths {
            // Not playing: seek on position change.
            self.hard_seek_count += 1;
            self.last_hard_seek_delta_seconds = delta.as_f32();
            arbiter.seek(ClockAuthority::MidiClock, authority, arb_target, clock_time);
        }

        self.last_position_sixteenths = current_sixteenths;
    }

    // ── Display ──────────────────────────────────────────────────────

    /// Update position display. Dirty-checked to avoid string alloc per frame.
    /// Format: bars.beats.sub (1-based).
    fn update_position_display(&mut self, sixteenths: i32) {
        if sixteenths == self.cached_display_sixteenths {
            return;
        }
        self.cached_display_sixteenths = sixteenths;

        let bars = sixteenths / 16 + 1;
        let beats = (sixteenths % 16) / 4 + 1;
        let sub = (sixteenths % 4) + 1;
        self.current_position_display = format!("{}.{}.{}", bars, beats, sub);
    }
}

impl SyncSource for MidiClockSyncController {
    fn is_enabled(&self) -> bool {
        self.is_midi_clock_enabled
    }
    fn display_name(&self) -> &str {
        "CLK"
    }
    fn enable(&mut self) {
        self.enable_midi_clock(self.selected_source_index);
    }
    fn disable(&mut self) {
        self.disable_midi_clock();
    }
}

impl Default for MidiClockSyncController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::project::Project;
    use manifold_core::types::PlaybackState;

    use tick_filter::{period_of_bpm, TickFilter};

    // ── D20: position rules ──────────────────────────────────────────────

    const P120: f64 = 60.0 / (24.0 * 120.0);

    /// Feed `n` ticks at `period` from `start`, from tick time `t`.
    fn ticks(tracker: &mut ClockTracker, t: f64, n: usize, period: f64) -> f64 {
        let mut at = t;
        for _ in 0..n {
            tracker.event(ClockKind::Tick, at, 120.0);
            at += period;
        }
        at
    }

    /// MIDI 1.0: the first tick after Start is position 0, not 1/24.
    #[test]
    fn first_clock_after_start_is_the_start_position() {
        let mut tracker = ClockTracker::new();
        tracker.event(ClockKind::Start, 0.0, 120.0);
        assert_eq!(tracker.sample_at(0.0), None, "no beat before the first tick");
        tracker.event(ClockKind::Tick, 0.001, 120.0);
        assert_eq!(tracker.landed_tick(), Some(0));
        let s = tracker.sample_at(0.001).expect("a beat after the first tick");
        assert!(s.beat.abs() < 1e-12, "beat {}", s.beat);
        ticks(&mut tracker, 0.001 + P120, 23, P120);
        assert_eq!(tracker.landed_tick(), Some(23));
    }

    #[test]
    fn ticks_while_stopped_do_not_move_the_position() {
        let mut tracker = ClockTracker::new();
        tracker.event(ClockKind::Start, 0.0, 120.0);
        let t = ticks(&mut tracker, 0.0, 48, P120);
        tracker.event(ClockKind::Stop, t, 120.0);
        let t = ticks(&mut tracker, t, 100, P120);
        assert_eq!(tracker.landed_tick(), Some(47));
        assert_eq!(tracker.next_tick(), 48);
        assert_eq!(tracker.sample_at(t), None, "no sample while stopped");
        tracker.event(ClockKind::Continue, t, 120.0);
        tracker.event(ClockKind::Tick, t + 0.001, 120.0);
        assert_eq!(tracker.landed_tick(), Some(48), "Continue lands on the current position");
    }

    #[test]
    fn song_position_then_continue_arms_the_position() {
        let mut tracker = ClockTracker::new();
        tracker.event(ClockKind::Start, 0.0, 120.0);
        let t = ticks(&mut tracker, 0.0, 30, P120);
        tracker.event(ClockKind::Stop, t, 120.0);
        tracker.event(ClockKind::SongPosition(64), t + 0.1, 120.0);
        assert_eq!(tracker.next_tick(), 384);
        tracker.event(ClockKind::Continue, t + 0.2, 120.0);
        tracker.event(ClockKind::Tick, t + 0.21, 120.0);
        assert_eq!(tracker.landed_tick(), Some(384));
        let s = tracker.sample_at(t + 0.21).unwrap();
        assert!((s.beat - 16.0).abs() < 1e-9, "beat {}", s.beat);
    }

    // ── D20: the filter ──────────────────────────────────────────────────

    /// A single 4 ms late tick moves the phase by a fraction of it, and the
    /// filter is back within 0.1 ms after a beat.
    #[test]
    fn filter_ignores_a_single_late_tick() {
        let mut filter = TickFilter::seeded(0, 0.0, P120);
        for k in 1..=480 {
            filter.tick(k, k as f64 * P120);
        }
        filter.tick(481, 481.0 * P120 + 0.004);
        assert!((filter.time_of(481.0) - 481.0 * P120).abs() < 0.0005, "one late tick moved the phase {}", filter.time_of(481.0) - 481.0 * P120);
        for k in 482..=600 {
            filter.tick(k, k as f64 * P120);
        }
        assert!((filter.time_of(600.0) - 600.0 * P120).abs() < 0.0001);
        assert!((filter.bpm() - 120.0).abs() < 0.05, "bpm {}", filter.bpm());
    }

    /// Two growing misses are not a tempo change; the third refits the period
    /// to the new tempo at once.
    #[test]
    fn refit_needs_three_growing_misses() {
        let mut filter = TickFilter::seeded(0, 0.0, P120);
        for k in 1..=240 {
            filter.tick(k, k as f64 * P120);
        }
        // 120 -> 140 at tick 240: arrivals come early by a growing margin.
        let p140 = period_of_bpm(140.0);
        let base = 240.0 * P120;
        let mut refit_at = None;
        for n in 1..=12 {
            let before = filter.period();
            filter.tick(240 + n, base + n as f64 * p140);
            if refit_at.is_none() && (filter.period() - p140).abs() < 0.02 * p140 && (before - p140).abs() > 0.05 * p140 {
                refit_at = Some(n);
            }
        }
        let refit_at = refit_at.expect("the tempo change was refit");
        assert!(refit_at >= 3, "refit after {refit_at} ticks, before three misses");
        assert!(refit_at <= 8, "refit only after {refit_at} ticks");
    }

    /// Stalls of 150-600 ms released as a burst stamped on arrival: the
    /// period stays inside the clamp and near the truth, the position never
    /// diverges.
    #[test]
    fn stall_then_burst_does_not_diverge() {
        for stall in [0.15, 0.2, 0.3, 0.6] {
            let mut tracker = ClockTracker::new();
            tracker.event(ClockKind::Start, 0.0, 120.0);
            let held_from = 10.0;
            let release = held_from + stall;
            let mut k = 0usize;
            loop {
                let true_at = k as f64 * P120;
                if true_at > 20.0 {
                    break;
                }
                let at = if true_at >= held_from && true_at < release { release } else { true_at };
                tracker.event(ClockKind::Tick, at, 120.0);
                let bpm = tracker.bpm().unwrap();
                assert!((20.0..=300.0).contains(&bpm), "stall {stall}: bpm {bpm}");
                if true_at >= release {
                    let s = tracker.sample_at(true_at).unwrap();
                    assert!((s.beat - k as f64 / 24.0).abs() * 0.5 < 0.020, "stall {stall}: tick {k} off by {} ms", (s.beat - k as f64 / 24.0) * 500.0);
                }
                k += 1;
            }
            let bpm = tracker.bpm().unwrap();
            assert!((bpm - 120.0).abs() < 1.0, "stall {stall}: bpm {bpm} at the end");
        }
    }

    /// A Start (position back to 0) never reaches the filter as a backward
    /// tick: it seeds the phase again and keeps the tempo estimate.
    #[test]
    fn bpm_estimator_survives_backward_tick_jump() {
        let p140 = period_of_bpm(140.0);
        let mut tracker = ClockTracker::new();
        tracker.event(ClockKind::Start, 0.0, 120.0);
        let t = ticks(&mut tracker, 0.0, 2400, p140);
        let bpm_before = tracker.bpm().unwrap();
        assert!((bpm_before - 140.0).abs() < 0.05, "bpm {bpm_before}");
        tracker.event(ClockKind::Start, t, 120.0);
        tracker.event(ClockKind::Tick, t + 0.001, 120.0);
        assert_eq!(tracker.landed_tick(), Some(0));
        assert_eq!(tracker.bpm(), Some(bpm_before), "a restart keeps the estimate");
    }

    /// The stamp map takes the fastest delivery as zero lag: a later message
    /// delivered late keeps its own stamp, never the delivery time.
    #[test]
    fn stamps_map_to_delivery_minus_the_fastest_lag() {
        let mut map = StampMap::new();
        let origin = map.origin;
        let at = |us: u64| origin + Duration::from_micros(us);
        // Stamp base 1_000_000 µs ahead of the origin; first delivery 300 µs late.
        let first = map.map(1_000_000 + 10_000, at(10_300));
        assert_eq!(first, at(10_300));
        // A faster delivery tightens the mapping.
        let second = map.map(1_000_000 + 20_000, at(20_100));
        assert_eq!(second, at(20_100));
        // Delivered 200 ms late (a stall): mapped to its own stamp.
        let stalled = map.map(1_000_000 + 30_000, at(230_000));
        assert_eq!(stalled, at(30_100));
        assert_eq!(map.map(0, at(500_000)), at(500_000), "no stamp: the delivery time");
    }

    // ── Seek-or-trim position sync ─────────────────────────────────────

    struct FakeSyncTarget {
        state: PlaybackState,
        time: Seconds,
        bpm: f32,
        project: Option<Project>,
    }

    impl SyncTarget for FakeSyncTarget {
        fn current_state(&self) -> PlaybackState {
            self.state
        }
        fn position_time(&self) -> Seconds {
            self.time
        }
        fn is_playing(&self) -> bool {
            self.state == PlaybackState::Playing
        }
        fn timeline_beat_to_time(&self, beat: Beats) -> Seconds {
            Seconds(beat.as_f32() as f64 * 60.0 / self.bpm as f64)
        }
        fn current_project(&self) -> Option<&Project> {
            self.project.as_ref()
        }
    }

    #[derive(Default)]
    struct FakeArbTarget {
        seek_count: u32,
    }

    impl SyncArbiterTarget for FakeArbTarget {
        fn current_project(&self) -> Option<&Project> {
            None
        }
        fn play(&mut self) {}
        fn pause(&mut self, _clear_recording: bool) {}
        fn seek(&mut self, _time: Seconds) {
            self.seek_count += 1;
        }
    }

    /// section 11 CLK seek threshold = 2.0s: while playing, a small position
    /// error is left to the follower's trim, never seeked.
    #[test]
    fn clk_position_sync_leaves_small_errors_to_the_follower() {
        let bpm = 150.0;
        // beat 25 @ 150 BPM = 10.0s exactly -> matches position_time -> delta 0.
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(10.0),
            bpm,
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();
        let mut ctrl = MidiClockSyncController::new();

        ctrl.sync_position_to_playback(
            600,
            &mut arbiter,
            &mut arb_target,
            &sync_target,
            ClockAuthority::MidiClock,
        );

        assert_eq!(arb_target.seek_count, 0);
    }

    /// Delta >= 2.0s forces a full seek.
    #[test]
    fn clk_position_sync_hard_seeks_when_playing_beyond_threshold() {
        let bpm = 150.0;
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(0.0),
            bpm,
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();
        let mut ctrl = MidiClockSyncController::new();

        // beat 25 @ 150 BPM = 10.0s away from position_time(0.0) -> delta > 2.0s.
        ctrl.sync_position_to_playback(
            600,
            &mut arbiter,
            &mut arb_target,
            &sync_target,
            ClockAuthority::MidiClock,
        );

        assert_eq!(arb_target.seek_count, 1);
    }

    /// section 11 CLK tick-delta sanity = 0..=384: a transport restart (position
    /// snaps back to 0, e.g. MIDI Start) is a large *negative* tick delta —
    /// outside the sane range — and must force a hard seek even when the
    /// resulting time delta alone would stay under the seek threshold.
    #[test]
    fn clk_position_sync_transport_restart_forces_seek_despite_small_delta() {
        let bpm = 150.0;
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();
        let mut ctrl = MidiClockSyncController::new();

        // Anchor at beat 25 (absolute_tick 600), matching position_time exactly.
        let sync_target_anchor = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(10.0),
            bpm,
            project: Some(Project::default()),
        };
        ctrl.sync_position_to_playback(
            600,
            &mut arbiter,
            &mut arb_target,
            &sync_target_anchor,
            ClockAuthority::MidiClock,
        );
        assert_eq!(arb_target.seek_count, 0, "anchor call should not seek");

        // Restart: position resets to 0 (absolute_tick 0, delta -600 —
        // outside 0..=384). MANIFOLD's own position_time is still near the
        // pre-restart value (0.02s): a small time delta, but the tick-jump
        // guard must win over the 2.0s threshold.
        let sync_target_restart = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(0.02),
            bpm,
            project: Some(Project::default()),
        };
        ctrl.sync_position_to_playback(
            0,
            &mut arbiter,
            &mut arb_target,
            &sync_target_restart,
            ClockAuthority::MidiClock,
        );

        assert_eq!(
            arb_target.seek_count, 1,
            "transport restart must hard-seek even though |Δtime| < 2.0s"
        );
    }

    /// While stopped, position sync only seeks on an actual sixteenth
    /// change (avoids re-seek churn every poll while paused).
    #[test]
    fn clk_position_sync_stopped_seeks_only_on_position_change() {
        let bpm = 150.0;
        let sync_target = FakeSyncTarget {
            state: PlaybackState::Stopped,
            time: Seconds(0.0),
            bpm,
            project: Some(Project::default()),
        };
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();
        let mut ctrl = MidiClockSyncController::new();

        ctrl.sync_position_to_playback(
            30,
            &mut arbiter,
            &mut arb_target,
            &sync_target,
            ClockAuthority::MidiClock,
        );
        assert_eq!(arb_target.seek_count, 1, "first observed position must seek");

        // Same sixteenths again -> no new seek.
        ctrl.sync_position_to_playback(
            30,
            &mut arbiter,
            &mut arb_target,
            &sync_target,
            ClockAuthority::MidiClock,
        );
        assert_eq!(
            arb_target.seek_count, 1,
            "an unchanged sixteenth position must not re-trigger a seek"
        );
    }
}
