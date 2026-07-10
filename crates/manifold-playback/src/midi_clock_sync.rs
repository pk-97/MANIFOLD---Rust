//! MIDI Clock/SPP sync controller.
//! Mechanical translation of Unity MidiClockSyncController.cs.
//!
//! One-way sync: DAW controls MANIFOLD position + transport via MIDI Clock.
//! Position is received in sixteenth notes (beat-native).
//!
//! MidiClockReceiver replaces Unity's MidiClock native CoreMIDI plugin (MidiClock.cs).
//! midir provides raw MIDI byte access — same CoreMIDI backend on macOS, ALSA on Linux.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use manifold_core::types::ClockAuthority;
use manifold_core::{Beats, Seconds};

use crate::sync::{SyncArbiter, SyncArbiterTarget, SyncTarget};
use crate::sync_source::SyncSource;

// ── MidiClockState (lock-free, packed into AtomicU64) ─────────────────────────

/// Lock-free MIDI clock state shared between midir callback and main thread.
/// Packed into a single u64 for atomic read-modify-write without locks.
///
/// Layout (64 bits):
///   bits  0..31  position_sixteenths (i32, stored as u32)
///   bits 32..47  clock_tick (i16, stored as u16, only uses 0-5)
///   bit  48      is_playing
///   bit  49      has_received_clock
struct AtomicClockState(AtomicU64);

/// Unpacked snapshot of MIDI clock state.
#[derive(Clone, Copy, Default)]
struct MidiClockState {
    position_sixteenths: i32,
    clock_tick: i32,
    is_playing: bool,
    has_received_clock: bool,
}

impl MidiClockState {
    fn pack(self) -> u64 {
        let pos = self.position_sixteenths as u32 as u64;
        let tick = (self.clock_tick as u16 as u64) << 32;
        let playing = (self.is_playing as u64) << 48;
        let received = (self.has_received_clock as u64) << 49;
        pos | tick | playing | received
    }

    fn unpack(bits: u64) -> Self {
        Self {
            position_sixteenths: bits as u32 as i32,
            clock_tick: ((bits >> 32) & 0xFFFF) as i32,
            is_playing: (bits >> 48) & 1 != 0,
            has_received_clock: (bits >> 49) & 1 != 0,
        }
    }
}

impl AtomicClockState {
    fn new() -> Self {
        Self(AtomicU64::new(MidiClockState::default().pack()))
    }

    /// Atomically read the current state.
    fn load(&self) -> MidiClockState {
        MidiClockState::unpack(self.0.load(Ordering::Acquire))
    }

    /// Atomically update the state via a closure (CAS loop).
    /// The closure receives the current state and returns the new state.
    fn update(&self, f: impl Fn(MidiClockState) -> MidiClockState) {
        let _ = self
            .0
            .fetch_update(Ordering::Release, Ordering::Acquire, |bits| {
                Some(f(MidiClockState::unpack(bits)).pack())
            });
    }
}

// ── MidiClockReceiver ─────────────────────────────────────────────────────────

/// Receives MIDI clock/SPP/start/stop via midir.
/// Replaces Unity's MidiClock.cs native CoreMIDI plugin.
///
/// Architecture divergence [D-30]: Unity's MidiClock plugin internally maintains
/// PositionSixteenths + ClockTick from 0xF8 ticks and 0xF2 SPP. midir delivers
/// raw bytes — we reconstruct the same fields in the callback.
/// See docs/KNOWN_DIVERGENCES.md.
///
/// LOCK-FREE: State is packed into AtomicU64, updated via CAS in the midir
/// callback. No Mutex — the OS MIDI thread never blocks.
struct MidiClockReceiver {
    state: Arc<AtomicClockState>,
    connection: Option<midir::MidiInputConnection<Arc<AtomicClockState>>>,
}

impl MidiClockReceiver {
    fn new() -> Self {
        Self {
            state: Arc::new(AtomicClockState::new()),
            connection: None,
        }
    }

    /// Open a midir connection to the port at `source_index`.
    /// Equivalent to MidiClock_Start(ptr, sourceIndex) in Unity.
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

        let state_arc = Arc::clone(&self.state);

        let connection = match midi_in.connect(
            port,
            "manifold-clock-conn",
            move |_timestamp_us: u64, message: &[u8], state_arc: &mut Arc<AtomicClockState>| {
                if message.is_empty() {
                    return;
                }

                let status = message[0];

                match status {
                    // Timing Clock — 24 per quarter note, 6 per sixteenth.
                    0xF8 => {
                        state_arc.update(|mut s| {
                            s.clock_tick += 1;
                            if s.clock_tick >= 6 {
                                s.clock_tick = 0;
                                s.position_sixteenths += 1;
                            }
                            s.has_received_clock = true;
                            s
                        });
                    }
                    // Start — reset position to 0.
                    0xFA => {
                        state_arc.update(|mut s| {
                            s.is_playing = true;
                            s.position_sixteenths = 0;
                            s.clock_tick = 0;
                            s.has_received_clock = true;
                            s
                        });
                    }
                    // Continue — resume without resetting position.
                    0xFB => {
                        state_arc.update(|mut s| {
                            s.is_playing = true;
                            s.has_received_clock = true;
                            s
                        });
                    }
                    // Stop.
                    0xFC => {
                        state_arc.update(|mut s| {
                            s.is_playing = false;
                            s.has_received_clock = true;
                            s
                        });
                    }
                    // Song Position Pointer — 2 data bytes: LSB, MSB.
                    0xF2 if message.len() >= 3 => {
                        let lsb = message[1] as i32;
                        let msb = message[2] as i32;
                        let pos = (msb << 7) | lsb;
                        state_arc.update(|mut s| {
                            s.position_sixteenths = pos;
                            s.clock_tick = 0;
                            s.has_received_clock = true;
                            s
                        });
                    }
                    _ => {}
                }
            },
            state_arc,
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

        self.connection = Some(connection);
        log::info!(
            "[MidiClockSync] midir clock receiver connected (port {})",
            source_index
        );
    }

    /// Close the midir connection.
    /// Equivalent to MidiClock_Stop / MidiClock_Destroy in Unity.
    fn stop(&mut self) {
        self.connection = None;
        log::info!("[MidiClockSync] midir clock receiver disconnected");
    }

    /// Snapshot current state — lock-free atomic read.
    /// Equivalent to MidiClock_Update() P/Invoke in Unity.
    /// Returns (position_sixteenths, clock_tick, is_playing, has_received_clock).
    fn update_state(&self) -> (i32, i32, bool, bool) {
        let s = self.state.load();
        (
            s.position_sixteenths,
            s.clock_tick,
            s.is_playing,
            s.has_received_clock,
        )
    }

    /// Number of available MIDI input ports.
    /// Equivalent to MidiClock.GetSourceCount() in Unity.
    fn source_count() -> usize {
        match midir::MidiInput::new("manifold-clock-scan") {
            Ok(m) => m.ports().len(),
            Err(_) => 0,
        }
    }

    /// Display name of the MIDI port at `index`.
    /// Equivalent to MidiClock.GetSourceName(index) in Unity.
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
/// Port of Unity MidiClockSyncController.cs.
pub struct MidiClockSyncController {
    is_midi_clock_enabled: bool,
    is_receiving_clock: bool,
    current_position_sixteenths: i32,
    current_position_display: String,
    current_clock_bpm: f32,
    hard_seek_count: i32,
    last_hard_seek_delta_seconds: f32,
    selected_source_index: i32,
    /// Cached display name of the selected MIDI source. Updated on
    /// enable/disable/change_source — avoids per-frame CoreMIDI enumeration.
    cached_source_name: String,

    // Config
    clock_signal_timeout: f32,
    derive_bpm_from_clock: bool,
    min_ticks_per_bpm_estimate: i32,
    bpm_ema_alpha: f32,

    // Private state — equivalent to Unity's lastObserved* fields
    last_is_playing: bool,
    last_position_sixteenths: i32,
    last_observed_sixteenths: i32,
    last_observed_clock_tick: i32,
    last_observed_playing: bool,
    last_clock_activity_time: Seconds,
    cached_display_sixteenths: i32,

    // BPM estimation
    last_tempo_abs_tick: i32,
    last_tempo_sample_time: f32,
    tempo_accum_time: f32,
    tempo_accum_ticks: i32,

    // Transport time integrator
    transport_time_integrator_initialized: bool,
    last_transport_absolute_tick: i32,
    integrated_clock_time_seconds: Seconds,

    // midir receiver (replaces Unity's MidiClock native plugin instance)
    receiver: Option<MidiClockReceiver>,
}

impl MidiClockSyncController {
    pub fn new() -> Self {
        Self {
            is_midi_clock_enabled: false,
            is_receiving_clock: false,
            current_position_sixteenths: 0,
            current_position_display: "---".into(),
            current_clock_bpm: 120.0,
            hard_seek_count: 0,
            last_hard_seek_delta_seconds: 0.0,
            selected_source_index: -1,
            cached_source_name: "None".into(),

            clock_signal_timeout: 0.5,
            derive_bpm_from_clock: true,
            min_ticks_per_bpm_estimate: 96,
            bpm_ema_alpha: 0.30,

            last_is_playing: false,
            last_position_sixteenths: -1,
            last_observed_sixteenths: 0,
            last_observed_clock_tick: 0,
            last_observed_playing: false,
            last_clock_activity_time: Seconds(-999.0),
            cached_display_sixteenths: -1,

            last_tempo_abs_tick: -1,
            last_tempo_sample_time: -1.0,
            tempo_accum_time: 0.0,
            tempo_accum_ticks: 0,

            transport_time_integrator_initialized: false,
            last_transport_absolute_tick: -1,
            integrated_clock_time_seconds: Seconds::ZERO,

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
    pub fn current_clock_bpm(&self) -> f32 {
        self.current_clock_bpm
    }

    /// Current MIDI clock position as a beat value.
    /// Computed as (position_sixteenths + clock_tick / 6.0) / 4.0.
    /// Port of C# PlaybackController.Update() beat derivation formula.
    pub fn current_clock_beat(&self) -> f32 {
        (self.last_observed_sixteenths as f32 + self.last_observed_clock_tick as f32 / 6.0) / 4.0
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
    /// Port of C# MidiClockSyncController.SelectedSourceName property.
    pub fn selected_source_name(&self) -> &str {
        &self.cached_source_name
    }

    // ── Lifecycle ────────────────────────────────────────────────────

    /// Enable MIDI Clock on the given source index.
    /// Port of C# MidiClockSyncController.EnableMidiClock (lines 97-151).
    pub fn enable_midi_clock(&mut self, source_index: i32) {
        if self.is_midi_clock_enabled {
            return;
        }

        // Auto-select first source if none specified (port of C# lines 110-119).
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

        // Prime observed state (port of C# lines 131-135).
        // After start(), we immediately snapshot what the receiver has (all zeros initially).
        let (pos_sixteenths, clock_tick, is_playing, has_received_clock) =
            self.receiver.as_ref().unwrap().update_state();
        self.last_observed_sixteenths = pos_sixteenths;
        self.last_observed_clock_tick = clock_tick;
        self.last_observed_playing = is_playing;
        self.last_clock_activity_time = if has_received_clock {
            Seconds::ZERO
        } else {
            Seconds(-999.0)
        };

        self.last_is_playing = false;
        self.last_position_sixteenths = -1;
        self.current_clock_bpm = 120.0;
        self.hard_seek_count = 0;
        self.last_hard_seek_delta_seconds = 0.0;
        let initial_abs_tick = pos_sixteenths * 6 + clock_tick;
        self.reset_bpm_estimator(0.0, initial_abs_tick);
        // Transport time integrator will be initialized on first SyncPositionToPlayback call.
        self.transport_time_integrator_initialized = false;
        self.last_transport_absolute_tick = -1;
        self.integrated_clock_time_seconds = Seconds::ZERO;
        self.is_midi_clock_enabled = true;

        log::info!(
            "[MidiClockSync] Enabled — source: {}",
            self.cached_source_name
        );
    }

    /// Switch to a different MIDI source. Restarts the receiver.
    /// Port of C# MidiClockSyncController.ChangeSource (lines 154-165).
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
    /// Port of C# MidiClockSyncController.DisableMidiClock (lines 167-189).
    pub fn disable_midi_clock(&mut self) {
        if !self.is_midi_clock_enabled {
            return;
        }

        self.is_midi_clock_enabled = false;
        self.is_receiving_clock = false;
        self.cached_source_name = "None".into();
        self.current_position_display = "---".into();
        self.last_is_playing = false;
        self.last_clock_activity_time = Seconds(-999.0);
        self.hard_seek_count = 0;
        self.last_hard_seek_delta_seconds = 0.0;
        self.reset_bpm_estimator(0.0, 0);
        self.transport_time_integrator_initialized = false;
        self.last_transport_absolute_tick = -1;
        self.integrated_clock_time_seconds = Seconds::ZERO;

        // Drop connection — equivalent to MidiClock.Shutdown() in Unity.
        if let Some(mut receiver) = self.receiver.take() {
            receiver.stop();
        }

        log::info!("[MidiClockSync] Disabled");
    }

    /// Poll MIDI clock state each frame and forward transport/position to MANIFOLD.
    /// Port of C# MidiClockSyncController.Update (lines 215-296).
    ///
    /// Unity's Update() takes no parameters — it holds refs to syncTarget and syncArbiter
    /// as fields. Rust passes them as arguments to avoid unsafe shared mutable state.
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
        if !self.is_midi_clock_enabled || self.receiver.is_none() {
            return;
        }

        // Poll native state. Equivalent to clock.UpdateState() + reading clock.* in Unity.
        let (pos_sixteenths, clock_tick, is_playing, has_received_clock) =
            self.receiver.as_ref().unwrap().update_state();

        // Detect state change (port of C# lines 222-237).
        // Unity's stateChanged drives lastClockActivityTime; we replicate that logic.
        let state_changed = pos_sixteenths != self.last_observed_sixteenths
            || clock_tick != self.last_observed_clock_tick
            || is_playing != self.last_observed_playing;

        if state_changed {
            self.last_observed_sixteenths = pos_sixteenths;
            self.last_observed_clock_tick = clock_tick;
            self.last_observed_playing = is_playing;

            if has_received_clock {
                self.last_clock_activity_time = now;
            }
        }

        // Activity timeout check (port of C# lines 239-240).
        let was_receiving = self.is_receiving_clock;
        let has_recent_clock_activity =
            (now - self.last_clock_activity_time).0 as f32 <= self.clock_signal_timeout;
        self.is_receiving_clock = has_recent_clock_activity;

        // Log sync state transitions for live monitoring.
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

        // Whether MANIFOLD initiated this play session (port of C# line 245).
        let manifold_owns = arbiter.manifold_owns_playback;

        // BPM estimation from clock ticks (port of C# line 247).
        self.update_bpm_from_clock(
            now.as_f32(),
            pos_sixteenths,
            clock_tick,
            has_recent_clock_activity,
            is_playing,
        );

        // Suppress local deltaTime when CLK is active authority and playing.
        // Not gated on manifold_owns — MIDI Clock always drives timing when active.
        // Gated on the clock-plane suppress — during scrubs and in-flight
        // AbletonOSC commands, the engine advances internally until Ableton
        // confirms the new state.
        arbiter.set_external_time_sync(
            ClockAuthority::MidiClock,
            authority,
            arb_target,
            has_recent_clock_activity && is_playing && !suppress_clock_plane,
        );

        // Transport sync — gated by arbiter authority check (port of C# lines 256-279).
        // Also held while the clock plane is suppressed: during an in-flight
        // play-from-cursor, Ableton's clock still says "stopped" — relaying
        // that as a pause is the "doesn't respond" flap (F1's cousin).
        if !manifold_owns && !suppress_clock_plane {
            let playing = has_recent_clock_activity && is_playing;
            if playing {
                if !sync_target.is_playing() {
                    arbiter.play(ClockAuthority::MidiClock, authority, arb_target);
                    if playing != self.last_is_playing {
                        log::info!("[MidiClockSync] Transport: PLAY");
                    }
                }
            } else {
                if sync_target.is_playing() {
                    arbiter.pause(ClockAuthority::MidiClock, authority, arb_target, false);
                    if playing != self.last_is_playing {
                        log::info!("[MidiClockSync] Transport: PAUSE");
                    }
                }
            }
            self.last_is_playing = playing;
        }

        // Clear MANIFOLD ownership when CLK shows stopped (port of C# lines 283-287).
        // Use grace-period-aware clear to avoid premature clearing during the
        // OSC→DAW→MIDI round trip (Ableton needs time to process and reflect state).
        if manifold_owns && (!has_recent_clock_activity || !is_playing) {
            arbiter.clear_ownership_if_expired(now);
        }

        // Position sync — MIDI Clock always drives position when active.
        // manifold_owns only gates transport (play/stop), not position.
        // Suppressed while the clock plane is gated (user scrub not yet
        // processed by Ableton, or an AbletonOSC command awaiting its ack —
        // either way the clock is reporting a stale position).
        if has_recent_clock_activity && !suppress_clock_plane {
            self.current_position_sixteenths = pos_sixteenths;
            self.update_position_display(pos_sixteenths);
            self.sync_position_to_playback(
                pos_sixteenths,
                clock_tick,
                arbiter,
                arb_target,
                sync_target,
                authority,
            );
        }
    }

    /// Reset transport time integrator.
    /// Port of C# MidiClockSyncController.ResetTransportTimeIntegrator (lines 357-362).
    pub fn reset_transport_time_integrator(&mut self) {
        self.transport_time_integrator_initialized = false;
        self.last_transport_absolute_tick = -1;
        self.integrated_clock_time_seconds = Seconds::ZERO;
    }

    // ── BPM estimation ───────────────────────────────────────────────

    fn reset_bpm_estimator(&mut self, now: f32, absolute_tick: i32) {
        self.last_tempo_abs_tick = absolute_tick;
        self.last_tempo_sample_time = now;
        self.tempo_accum_ticks = 0;
        self.tempo_accum_time = 0.0;
    }

    /// Estimate BPM from clock tick rate using exponential moving average.
    /// 24 PPQN: 6 ticks per sixteenth note, 24 per quarter note.
    /// Port of C# MidiClockSyncController.UpdateBpmFromClock (lines 306-355).
    fn update_bpm_from_clock(
        &mut self,
        now: f32,
        pos_sixteenths: i32,
        clock_tick: i32,
        has_recent_clock_activity: bool,
        is_playing: bool,
    ) {
        // absolute_tick = PositionSixteenths * 6 + ClockTick (port of C# line 314).
        let absolute_tick = pos_sixteenths * 6 + clock_tick;

        if !self.derive_bpm_from_clock {
            self.reset_bpm_estimator(now, absolute_tick);
            return;
        }

        if !is_playing || !has_recent_clock_activity {
            self.reset_bpm_estimator(now, absolute_tick);
            return;
        }
        if self.last_tempo_sample_time < 0.0 || self.last_tempo_abs_tick < 0 {
            self.reset_bpm_estimator(now, absolute_tick);
            return;
        }

        let dt = now - self.last_tempo_sample_time;
        let d_ticks = absolute_tick - self.last_tempo_abs_tick;
        self.last_tempo_sample_time = now;
        self.last_tempo_abs_tick = absolute_tick;

        if dt <= 0.0 {
            return;
        }
        if d_ticks < 0 {
            // Song-position jump or source reset; restart estimator window.
            self.tempo_accum_ticks = 0;
            self.tempo_accum_time = 0.0;
            return;
        }

        self.tempo_accum_ticks += d_ticks;
        self.tempo_accum_time += dt;

        let tick_window = self.min_ticks_per_bpm_estimate.max(1);
        if self.tempo_accum_ticks < tick_window || self.tempo_accum_time <= 0.0 {
            return;
        }

        let raw_bpm = (self.tempo_accum_ticks as f32 * 60.0) / (24.0 * self.tempo_accum_time);
        self.tempo_accum_ticks = 0;
        self.tempo_accum_time = 0.0;

        if !(20.0..=300.0).contains(&raw_bpm) {
            return;
        }

        // Port of C# line 354: Mathf.Lerp clamps t to [0,1].
        let alpha = self.bpm_ema_alpha.clamp(0.0, 1.0);
        self.current_clock_bpm = if self.current_clock_bpm <= 0.0 {
            raw_bpm
        } else {
            // Mathf.Lerp(a, b, t) = a + (b - a) * clamp01(t)
            self.current_clock_bpm + (raw_bpm - self.current_clock_bpm) * alpha
        };
    }

    // ── Position sync ────────────────────────────────────────────────

    /// Sync MANIFOLD playback position to MIDI clock position.
    /// Port of C# MidiClockSyncController.SyncPositionToPlayback (lines 368-436).
    fn sync_position_to_playback(
        &mut self,
        pos_sixteenths: i32,
        clock_tick: i32,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        sync_target: &dyn SyncTarget,
        authority: ClockAuthority,
    ) {
        // Port of C# line 370: guard on project and arbiter.
        if sync_target.current_project().is_none() {
            return;
        }

        let current_sixteenths = pos_sixteenths;
        let absolute_tick = pos_sixteenths * 6 + clock_tick;

        // Include sub-sixteenth tick for 24 PPQN precision (port of C# lines 375-377).
        // 6 ticks per sixteenth, 4 sixteenths per beat.
        let clock_beat = (pos_sixteenths as f32 + clock_tick as f32 / 6.0) / 4.0;
        let clock_time = sync_target.timeline_beat_to_time(Beats::from_f32(clock_beat));
        let mut is_transport_jump = false;

        if !self.transport_time_integrator_initialized {
            // First call: anchor integrator (port of C# lines 380-383).
            self.integrated_clock_time_seconds = clock_time.max(Seconds::ZERO);
            self.last_transport_absolute_tick = absolute_tick;
            self.transport_time_integrator_initialized = true;
        } else {
            let delta_ticks = absolute_tick - self.last_transport_absolute_tick;
            if !(0..=384).contains(&delta_ticks) {
                // Song-position jump / restart: re-anchor (port of C# lines 387-391).
                is_transport_jump = true;
            }

            // Keep diagnostics coherent (port of C# lines 395-397).
            self.integrated_clock_time_seconds = clock_time;
            self.last_transport_absolute_tick = absolute_tick;
        }

        let current_time = sync_target.current_time();
        let delta = (clock_time - current_time).abs();

        if sync_target.is_playing() {
            if !is_transport_jump && delta < Seconds(2.0) {
                // NudgeTime for smooth per-frame tracking (port of C# line 407).
                // Threshold widened from 0.5s to 2.0s to absorb tempo ramps
                // without triggering hard seeks. SPP jumps and transport
                // restarts are already caught by is_transport_jump (tick-delta
                // range check), so this threshold only needs to catch genuine
                // position errors — not continuous drift from tempo automation.
                arbiter.nudge_time(ClockAuthority::MidiClock, authority, arb_target, clock_time);
            } else {
                // Large jump via full Seek (port of C# lines 412-419).
                self.hard_seek_count += 1;
                self.last_hard_seek_delta_seconds = delta.as_f32();
                arbiter.seek(ClockAuthority::MidiClock, authority, arb_target, clock_time);
            }
        } else {
            // Not playing: Seek on position change (port of C# lines 425-432).
            if current_sixteenths != self.last_position_sixteenths {
                self.hard_seek_count += 1;
                self.last_hard_seek_delta_seconds = delta.as_f32();
                arbiter.seek(ClockAuthority::MidiClock, authority, arb_target, clock_time);
            }
        }

        self.last_position_sixteenths = current_sixteenths;
    }

    // ── Display ──────────────────────────────────────────────────────

    /// Update position display. Dirty-checked to avoid string alloc per frame.
    /// Format: bars.beats.sub (1-based).
    /// Port of C# MidiClockSyncController.UpdatePositionDisplay (lines 445-455).
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

    // ── MidiClockState pack/unpack round trip ───────────────────────────

    /// Bits 0..31 hold `position_sixteenths` as a raw i32 bit pattern — the
    /// cast chain (`as u32 as u64` / `as u32 as i32`) is bit-preserving, so
    /// every i32 value round-trips exactly.
    #[test]
    fn midi_clock_state_position_roundtrips_full_i32_range() {
        for &pos in &[
            i32::MIN,
            i32::MIN + 1,
            -1_000_000,
            -1,
            0,
            1,
            42,
            1_000_000,
            i32::MAX - 1,
            i32::MAX,
        ] {
            let state = MidiClockState {
                position_sixteenths: pos,
                clock_tick: 0,
                is_playing: false,
                has_received_clock: false,
            };
            let round = MidiClockState::unpack(state.pack());
            assert_eq!(round.position_sixteenths, pos);
        }
    }

    /// `clock_tick` only ever carries 0..=5 in production (6 MIDI-clock
    /// ticks per sixteenth note) — exhaustive over that domain crossed with
    /// the is_playing/has_received_clock flag combinations.
    #[test]
    fn midi_clock_state_tick_and_flags_roundtrip() {
        for tick in 0..=5 {
            for &playing in &[true, false] {
                for &received in &[true, false] {
                    let state = MidiClockState {
                        position_sixteenths: 42,
                        clock_tick: tick,
                        is_playing: playing,
                        has_received_clock: received,
                    };
                    let round = MidiClockState::unpack(state.pack());
                    assert_eq!(round.position_sixteenths, 42);
                    assert_eq!(round.clock_tick, tick);
                    assert_eq!(round.is_playing, playing);
                    assert_eq!(round.has_received_clock, received);
                }
            }
        }
    }

    #[test]
    fn atomic_clock_state_load_update_roundtrip() {
        let atomic = AtomicClockState::new();
        atomic.update(|_| MidiClockState {
            position_sixteenths: 777,
            clock_tick: 3,
            is_playing: true,
            has_received_clock: true,
        });
        let loaded = atomic.load();
        assert_eq!(loaded.position_sixteenths, 777);
        assert_eq!(loaded.clock_tick, 3);
        assert!(loaded.is_playing);
        assert!(loaded.has_received_clock);
    }

    // ── BPM estimator ────────────────────────────────────────────────────

    /// §11: BPM estimated over ≥96 ticks (24 PPQN ⇒ 4 beats), EMA α=0.30.
    /// The raw-BPM formula (`ticks*60 / (24*seconds)`) is the standard
    /// MIDI-clock tempo derivation (24 clocks per quarter note) — an
    /// external standard, not something derived here, so convergence to the
    /// injected rate is safe to pin.
    #[test]
    fn bpm_estimator_converges_toward_injected_tempo() {
        let target_bpm = 150.0_f32; // deliberately non-120
        let seconds_per_tick = 60.0 / (target_bpm * 24.0);
        let mut ctrl = MidiClockSyncController::new();

        let mut now = 0.0_f32;
        // First call only anchors the estimator (guarded by last_tempo_sample_time < 0).
        ctrl.update_bpm_from_clock(now, 0, 0, true, true);

        for t in 1..=3000_i64 {
            now += seconds_per_tick;
            let pos = (t / 6) as i32;
            let tick = (t % 6) as i32;
            ctrl.update_bpm_from_clock(now, pos, tick, true, true);
        }

        assert!(
            (ctrl.current_clock_bpm() - target_bpm).abs() < 0.5,
            "expected convergence to {target_bpm} BPM, got {}",
            ctrl.current_clock_bpm()
        );
    }

    /// A backward tick jump (SPP rewind / MIDI Start resetting position to
    /// 0) must not corrupt the running estimate — `update_bpm_from_clock`
    /// returns before touching `current_clock_bpm` on a negative delta
    /// ("Song-position jump or source reset; restart estimator window").
    #[test]
    fn bpm_estimator_survives_backward_tick_jump() {
        let target_bpm = 140.0_f32;
        let seconds_per_tick = 60.0 / (target_bpm * 24.0);
        let mut ctrl = MidiClockSyncController::new();
        let mut now = 0.0_f32;
        ctrl.update_bpm_from_clock(now, 0, 0, true, true);
        for t in 1..=200_i64 {
            now += seconds_per_tick;
            ctrl.update_bpm_from_clock(now, (t / 6) as i32, (t % 6) as i32, true, true);
        }
        let bpm_before = ctrl.current_clock_bpm();

        // Backward jump: position resets to 0 (e.g. MIDI Start).
        now += seconds_per_tick;
        ctrl.update_bpm_from_clock(now, 0, 0, true, true);

        assert_eq!(
            ctrl.current_clock_bpm(),
            bpm_before,
            "a backward tick delta must not perturb the estimate mid-window"
        );
    }

    // ── Nudge-vs-seek position sync ─────────────────────────────────────

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
        fn current_time(&self) -> Seconds {
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
        external_time_sync: bool,
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
        fn play(&mut self) {}
        fn pause(&mut self, _clear_recording: bool) {}
        fn nudge_time(&mut self, _time: Seconds) {
            self.nudge_count += 1;
        }
        fn seek(&mut self, _time: Seconds) {
            self.seek_count += 1;
        }
    }

    /// §11 CLK nudge/seek split = 2.0s: while playing, a small position
    /// error nudges rather than hard-seeking.
    #[test]
    fn clk_position_sync_nudges_when_playing_within_threshold() {
        let bpm = 150.0;
        // beat 25 @ 150 BPM = 10.0s exactly -> matches current_time -> delta 0.
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
            100,
            0,
            &mut arbiter,
            &mut arb_target,
            &sync_target,
            ClockAuthority::MidiClock,
        );

        assert_eq!(arb_target.nudge_count, 1);
        assert_eq!(arb_target.seek_count, 0);
    }

    /// Delta >= 2.0s forces a full seek instead of a nudge.
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

        // beat 25 @ 150 BPM = 10.0s away from current_time(0.0) -> delta > 2.0s.
        ctrl.sync_position_to_playback(
            100,
            0,
            &mut arbiter,
            &mut arb_target,
            &sync_target,
            ClockAuthority::MidiClock,
        );

        assert_eq!(arb_target.seek_count, 1);
        assert_eq!(arb_target.nudge_count, 0);
    }

    /// §11 CLK tick-delta sanity = 0..=384: a transport restart (position
    /// snaps back to 0, e.g. MIDI Start) is a large *negative* tick delta —
    /// outside the sane range — and must force a hard seek even when the
    /// resulting time delta alone would look nudge-sized.
    #[test]
    fn clk_position_sync_transport_restart_forces_seek_despite_small_delta() {
        let bpm = 150.0;
        let mut arb_target = FakeArbTarget::default();
        let mut arbiter = SyncArbiter::new();
        let mut ctrl = MidiClockSyncController::new();

        // Anchor at beat 25 (absolute_tick 600), matching current_time exactly.
        let sync_target_anchor = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(10.0),
            bpm,
            project: Some(Project::default()),
        };
        ctrl.sync_position_to_playback(
            100,
            0,
            &mut arbiter,
            &mut arb_target,
            &sync_target_anchor,
            ClockAuthority::MidiClock,
        );
        assert_eq!(arb_target.nudge_count, 1);
        assert_eq!(
            arb_target.seek_count, 0,
            "anchor call should nudge, not seek"
        );

        // Restart: position resets to 0 (absolute_tick 0, delta -600 —
        // outside 0..=384). MANIFOLD's own current_time is still near the
        // pre-restart value (0.02s): a small time delta, but the tick-jump
        // guard must win over the 2.0s nudge threshold.
        let sync_target_restart = FakeSyncTarget {
            state: PlaybackState::Playing,
            time: Seconds(0.02),
            bpm,
            project: Some(Project::default()),
        };
        ctrl.sync_position_to_playback(
            0,
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
        assert_eq!(
            arb_target.nudge_count, 1,
            "nudge count must not have grown on the restart call"
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
            5,
            0,
            &mut arbiter,
            &mut arb_target,
            &sync_target,
            ClockAuthority::MidiClock,
        );
        assert_eq!(arb_target.seek_count, 1, "first observed position must seek");

        // Same sixteenths again -> no new seek.
        ctrl.sync_position_to_playback(
            5,
            0,
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
