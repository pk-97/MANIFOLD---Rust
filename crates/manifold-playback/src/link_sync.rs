//! Ableton Link sync controller.
//! Mechanical translation of Unity LinkSyncController.cs.
//!
//! One-way sync: Ableton controls MANIFOLD transport + tempo when Link
//! is selected as clock authority.
//!
//! STUB: Full implementation requires `ableton-link` crate or native FFI.
//! This file provides the correct interface and state tracking so the rest
//! of the transport system can compile and wire correctly.

use crate::sync_source::SyncSource;

/// Link sync controller state.
/// Port of Unity LinkSyncController.cs fields.
pub struct LinkSyncController {
    is_link_enabled: bool,
    pub num_peers: i32,
    pub current_beat: f64,
    pub current_phase: f64,
    pub link_tempo: f64,
    pub link_is_playing: bool,
    enable_start_stop_sync: bool,
    quantum: f64,

    // Private state
    last_link_is_playing: bool,
    last_num_peers: i32,
}

impl LinkSyncController {
    pub fn new() -> Self {
        Self {
            is_link_enabled: false,
            num_peers: 0,
            current_beat: 0.0,
            current_phase: 0.0,
            link_tempo: 120.0,
            link_is_playing: false,
            enable_start_stop_sync: true,
            quantum: 4.0,
            last_link_is_playing: false,
            last_num_peers: 0,
        }
    }

    pub fn is_link_enabled(&self) -> bool { self.is_link_enabled }
    pub fn has_active_peers(&self) -> bool { self.is_link_enabled && self.num_peers > 0 }

    /// Enable Link. STUB: logs that native plugin is not available.
    pub fn enable_link(&mut self, _initial_bpm: f64) {
        if self.is_link_enabled { return; }
        // TODO: Initialize AbletonLink native plugin
        // link = AbletonLink::init(initial_bpm);
        // link.set_quantum(self.quantum);
        // if self.enable_start_stop_sync { link.enable_start_stop_sync(true); }
        log::info!("[LinkSync] Enable requested (native plugin not available in Rust port)");
        self.is_link_enabled = true;
        self.last_link_is_playing = false;
        self.last_num_peers = 0;
    }

    pub fn disable_link(&mut self) {
        if !self.is_link_enabled { return; }
        self.is_link_enabled = false;
        self.num_peers = 0;
        self.link_is_playing = false;
        // TODO: AbletonLink::shutdown()
        log::info!("[LinkSync] Disabled");
    }

    /// Poll Link state each frame. STUB: no-op until native plugin available.
    /// When implemented, this will:
    /// 1. link.update_state()
    /// 2. Copy beat/phase/tempo/numPeers/isPlaying to public properties
    /// 3. Log peer changes
    /// 4. Call sync_transport_from_link() to enforce Link's playing state
    pub fn update(&mut self) {
        if !self.is_link_enabled { return; }
        // TODO: Poll native Link plugin
        // self.current_beat = link.beat;
        // self.current_phase = link.phase;
        // self.link_tempo = link.tempo;
        // self.num_peers = link.num_peers;
        // self.link_is_playing = link.is_playing;
        self.last_link_is_playing = self.link_is_playing;
    }
}

impl SyncSource for LinkSyncController {
    fn is_enabled(&self) -> bool { self.is_link_enabled }
    fn display_name(&self) -> &str { "Link" }
    fn enable(&mut self) { self.enable_link(120.0); }
    fn disable(&mut self) { self.disable_link(); }
}

impl Default for LinkSyncController {
    fn default() -> Self { Self::new() }
}
