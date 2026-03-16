//! MIDI Clock/SPP sync controller.
//! Mechanical translation of Unity MidiClockSyncController.cs.
//!
//! One-way sync: DAW controls MANIFOLD position + transport via MIDI Clock.
//! Position is received in sixteenth notes (beat-native).
//!
//! STUB: Full implementation requires `midir` crate for MIDI I/O.
//! This file provides the correct interface and state tracking.

use crate::sync_source::SyncSource;

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

    // Private state
    last_is_playing: bool,
    last_position_sixteenths: i32,
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
            last_clock_activity_time: -999.0,
            cached_display_sixteenths: -1,

            last_tempo_abs_tick: -1,
            last_tempo_sample_time: -1.0,
            tempo_accum_time: 0.0,
            tempo_accum_ticks: 0,

            transport_time_integrator_initialized: false,
            last_transport_absolute_tick: -1,
            integrated_clock_time_seconds: 0.0,
        }
    }

    // ── Public properties ────────────────────────────────────────────

    pub fn is_midi_clock_enabled(&self) -> bool { self.is_midi_clock_enabled }
    pub fn is_receiving_clock(&self) -> bool { self.is_receiving_clock }
    pub fn is_clock_transport_playing(&self) -> bool { self.is_receiving_clock && self.last_is_playing }
    pub fn current_position_display(&self) -> &str { &self.current_position_display }
    pub fn current_clock_bpm(&self) -> f32 { self.current_clock_bpm }
    pub fn selected_source_index(&self) -> i32 { self.selected_source_index }
    pub fn selected_source_name(&self) -> &str { "None" } // TODO: from midir

    // ── Lifecycle ────────────────────────────────────────────────────

    /// Enable MIDI Clock. STUB: logs that native plugin is not available.
    pub fn enable_midi_clock(&mut self, source_index: i32) {
        if self.is_midi_clock_enabled { return; }

        self.selected_source_index = if source_index < 0 { 0 } else { source_index };

        // TODO: MidiClock::init(source_index) via midir
        log::info!("[MidiClockSync] Enable requested (MIDI I/O not available in Rust port)");

        self.last_is_playing = false;
        self.last_position_sixteenths = -1;
        self.current_clock_bpm = 120.0;
        self.hard_seek_count = 0;
        self.last_hard_seek_delta_seconds = 0.0;
        self.reset_bpm_estimator(0.0, 0);
        self.transport_time_integrator_initialized = false;
        self.is_midi_clock_enabled = true;
    }

    /// Switch to a different MIDI source. Restarts the native plugin.
    pub fn change_source(&mut self, source_index: i32) {
        if self.is_midi_clock_enabled {
            self.disable_midi_clock();
            self.enable_midi_clock(source_index);
        } else {
            self.selected_source_index = source_index;
        }
    }

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

        // TODO: MidiClock::shutdown()
        log::info!("[MidiClockSync] Disabled");
    }

    /// Poll MIDI clock state each frame. STUB: no-op until midir available.
    pub fn update(&mut self, _now: f32) {
        if !self.is_midi_clock_enabled { return; }
        // TODO: Poll MidiClock native plugin via midir
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
    #[allow(dead_code)]
    fn update_bpm_from_clock(&mut self, now: f32, absolute_tick: i32, is_playing: bool, has_activity: bool) {
        if !self.derive_bpm_from_clock {
            self.reset_bpm_estimator(now, absolute_tick);
            return;
        }
        if !is_playing || !has_activity {
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
            // Song-position jump or source reset
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

        if raw_bpm < 20.0 || raw_bpm > 300.0 { return; }

        let alpha = self.bpm_ema_alpha.clamp(0.0, 1.0);
        self.current_clock_bpm = if self.current_clock_bpm <= 0.0 {
            raw_bpm
        } else {
            self.current_clock_bpm + alpha * (raw_bpm - self.current_clock_bpm)
        };
    }

    /// Update position display. Dirty-checked to avoid string alloc per frame.
    /// Format: bars.beats.sub (1-based).
    #[allow(dead_code)]
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
