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
        let _ = self.0.fetch_update(Ordering::Release, Ordering::Acquire, |bits| {
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
                log::warn!("[MidiClockSync] Failed to connect to port {}: {}", source_index, e);
                return;
            }
        };

        self.connection = Some(connection);
        log::info!("[MidiClockSync] midir clock receiver connected (port {})", source_index);
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
        (s.position_sixteenths, s.clock_tick, s.is_playing, s.has_received_clock)
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
            Some(p) => midi_in.port_name(p).unwrap_or_else(|_| format!("Port {}", index)),
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
    last_clock_activity_time: f32,
    cached_display_sixteenths: i32,

    // BPM estimation
    last_tempo_abs_tick: i32,
    last_tempo_sample_time: f32,
    tempo_accum_time: f32,
    tempo_accum_ticks: i32,

    // Transport time integrator
    transport_time_integrator_initialized: bool,
    last_transport_absolute_tick: i32,
    integrated_clock_time_seconds: f32,

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

            clock_signal_timeout: 0.5,
            derive_bpm_from_clock: true,
            min_ticks_per_bpm_estimate: 96,
            bpm_ema_alpha: 0.30,

            last_is_playing: false,
            last_position_sixteenths: -1,
            last_observed_sixteenths: 0,
            last_observed_clock_tick: 0,
            last_observed_playing: false,
            last_clock_activity_time: -999.0,
            cached_display_sixteenths: -1,

            last_tempo_abs_tick: -1,
            last_tempo_sample_time: -1.0,
            tempo_accum_time: 0.0,
            tempo_accum_ticks: 0,

            transport_time_integrator_initialized: false,
            last_transport_absolute_tick: -1,
            integrated_clock_time_seconds: 0.0,

            receiver: None,
        }
    }

    // ── Public properties ────────────────────────────────────────────

    pub fn is_midi_clock_enabled(&self) -> bool { self.is_midi_clock_enabled }
    pub fn is_receiving_clock(&self) -> bool { self.is_receiving_clock }
    pub fn is_clock_transport_playing(&self) -> bool { self.is_receiving_clock && self.last_is_playing }
    pub fn current_position_display(&self) -> &str { &self.current_position_display }
    pub fn current_clock_bpm(&self) -> f32 { self.current_clock_bpm }

    /// Current MIDI clock position as a beat value.
    /// Computed as (position_sixteenths + clock_tick / 6.0) / 4.0.
    /// Port of C# PlaybackController.Update() beat derivation formula.
    pub fn current_clock_beat(&self) -> f32 {
        (self.last_observed_sixteenths as f32 + self.last_observed_clock_tick as f32 / 6.0) / 4.0
    }

    pub fn hard_seek_count(&self) -> i32 { self.hard_seek_count }
    pub fn last_hard_seek_delta_seconds(&self) -> f32 { self.last_hard_seek_delta_seconds }
    pub fn selected_source_index(&self) -> i32 { self.selected_source_index }

    /// Display name of the currently selected MIDI source.
    /// Port of C# MidiClockSyncController.SelectedSourceName property.
    pub fn selected_source_name(&self) -> String {
        if self.selected_source_index >= 0 {
            MidiClockReceiver::source_name(self.selected_source_index)
        } else {
            "None".into()
        }
    }

    // ── Lifecycle ────────────────────────────────────────────────────

    /// Enable MIDI Clock on the given source index.
    /// Port of C# MidiClockSyncController.EnableMidiClock (lines 97-151).
    pub fn enable_midi_clock(&mut self, source_index: i32) {
        if self.is_midi_clock_enabled { return; }

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
        self.last_clock_activity_time = if has_received_clock { 0.0 } else { -999.0 };

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
        self.integrated_clock_time_seconds = 0.0;
        self.is_midi_clock_enabled = true;

        let source_name = MidiClockReceiver::source_name(source_index);
        log::info!("[MidiClockSync] Enabled — source: {}", source_name);
    }

    /// Switch to a different MIDI source. Restarts the receiver.
    /// Port of C# MidiClockSyncController.ChangeSource (lines 154-165).
    pub fn change_source(&mut self, source_index: i32) {
        if self.is_midi_clock_enabled {
            self.disable_midi_clock();
            self.enable_midi_clock(source_index);
        } else {
            self.selected_source_index = source_index;
        }
    }

    /// Disable MIDI Clock and release the receiver.
    /// Port of C# MidiClockSyncController.DisableMidiClock (lines 167-189).
    pub fn disable_midi_clock(&mut self) {
        if !self.is_midi_clock_enabled { return; }

        self.is_midi_clock_enabled = false;
        self.is_receiving_clock = false;
        self.current_position_display = "---".into();
        self.last_is_playing = false;
        self.last_clock_activity_time = -999.0;
        self.hard_seek_count = 0;
        self.last_hard_seek_delta_seconds = 0.0;
        self.reset_bpm_estimator(0.0, 0);
        self.transport_time_integrator_initialized = false;
        self.last_transport_absolute_tick = -1;
        self.integrated_clock_time_seconds = 0.0;

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
    pub fn update(
        &mut self,
        now: f32,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        sync_target: &dyn SyncTarget,
        authority: ClockAuthority,
    ) {
        if !self.is_midi_clock_enabled || self.receiver.is_none() { return; }

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
        let has_recent_clock_activity = (now - self.last_clock_activity_time) <= self.clock_signal_timeout;
        self.is_receiving_clock = has_recent_clock_activity;

        // Whether MANIFOLD initiated this play session (port of C# line 245).
        let manifold_owns = arbiter.manifold_owns_playback;

        // BPM estimation from clock ticks (port of C# line 247).
        self.update_bpm_from_clock(now, pos_sixteenths, clock_tick, has_recent_clock_activity, is_playing);

        // Suppress local deltaTime when CLK is active authority and playing (port of C# lines 251-253).
        arbiter.set_external_time_sync(
            ClockAuthority::MidiClock,
            authority,
            arb_target,
            has_recent_clock_activity && is_playing && !manifold_owns,
        );

        // Transport sync — gated by arbiter authority check (port of C# lines 256-279).
        if !manifold_owns {
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
        if manifold_owns && (!has_recent_clock_activity || !is_playing) {
            arbiter.clear_ownership();
        }

        // Position sync — gated by arbiter authority check (port of C# lines 290-295).
        if has_recent_clock_activity && !manifold_owns {
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
        self.integrated_clock_time_seconds = 0.0;
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

        if dt <= 0.0 { return; }
        if d_ticks < 0 {
            // Song-position jump or source reset; restart estimator window.
            self.tempo_accum_ticks = 0;
            self.tempo_accum_time = 0.0;
            return;
        }

        self.tempo_accum_ticks += d_ticks;
        self.tempo_accum_time += dt;

        let tick_window = self.min_ticks_per_bpm_estimate.max(1);
        if self.tempo_accum_ticks < tick_window || self.tempo_accum_time <= 0.0 { return; }

        let raw_bpm = (self.tempo_accum_ticks as f32 * 60.0) / (24.0 * self.tempo_accum_time);
        self.tempo_accum_ticks = 0;
        self.tempo_accum_time = 0.0;

        if !(20.0..=300.0).contains(&raw_bpm) { return; }

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
        if sync_target.current_project().is_none() { return; }

        let current_sixteenths = pos_sixteenths;
        let absolute_tick = pos_sixteenths * 6 + clock_tick;

        // Include sub-sixteenth tick for 24 PPQN precision (port of C# lines 375-377).
        // 6 ticks per sixteenth, 4 sixteenths per beat.
        let clock_beat = (pos_sixteenths as f32 + clock_tick as f32 / 6.0) / 4.0;
        let clock_time = sync_target.timeline_beat_to_time(clock_beat);
        let mut is_transport_jump = false;

        if !self.transport_time_integrator_initialized {
            // First call: anchor integrator (port of C# lines 380-383).
            self.integrated_clock_time_seconds = clock_time.max(0.0);
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
            if !is_transport_jump && delta < 0.5 {
                // NudgeTime for smooth per-frame tracking (port of C# line 407).
                arbiter.nudge_time(ClockAuthority::MidiClock, authority, arb_target, clock_time);
            } else {
                // Large jump via full Seek (port of C# lines 412-419).
                self.hard_seek_count += 1;
                self.last_hard_seek_delta_seconds = delta;
                arbiter.seek(ClockAuthority::MidiClock, authority, arb_target, clock_time);
            }
        } else {
            // Not playing: Seek on position change (port of C# lines 425-432).
            if current_sixteenths != self.last_position_sixteenths {
                self.hard_seek_count += 1;
                self.last_hard_seek_delta_seconds = delta;
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
        if sixteenths == self.cached_display_sixteenths { return; }
        self.cached_display_sixteenths = sixteenths;

        let bars = sixteenths / 16 + 1;
        let beats = (sixteenths % 16) / 4 + 1;
        let sub = (sixteenths % 4) + 1;
        self.current_position_display = format!("{}.{}.{}", bars, beats, sub);
    }
}

impl SyncSource for MidiClockSyncController {
    fn is_enabled(&self) -> bool { self.is_midi_clock_enabled }
    fn display_name(&self) -> &str { "CLK" }
    fn enable(&mut self) { self.enable_midi_clock(self.selected_source_index); }
    fn disable(&mut self) { self.disable_midi_clock(); }
}

impl Default for MidiClockSyncController {
    fn default() -> Self { Self::new() }
}
