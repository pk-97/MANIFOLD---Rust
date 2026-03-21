//! Ableton Link sync controller.
//! Mechanical translation of Unity LinkSyncController.cs.
//!
//! One-way sync: Ableton controls MANIFOLD transport + tempo when Link
//! is selected as clock authority.

use manifold_core::types::ClockAuthority;
use crate::sync::{SyncArbiter, SyncArbiterTarget};
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

    // rusty_link handles
    link: Option<rusty_link::AblLink>,
    session_state: rusty_link::SessionState,
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
            link: None,
            session_state: rusty_link::SessionState::new(),
        }
    }

    pub fn is_link_enabled(&self) -> bool { self.is_link_enabled }
    pub fn has_active_peers(&self) -> bool { self.is_link_enabled && self.num_peers > 0 }

    /// Enable Link with initial BPM.
    /// Port of C# LinkSyncController.EnableLink.
    pub fn enable_link(&mut self, initial_bpm: f64) {
        if self.is_link_enabled { return; }
        let link = rusty_link::AblLink::new(initial_bpm);
        link.enable(true);
        if self.enable_start_stop_sync {
            link.enable_start_stop_sync(true);
        }
        self.link = Some(link);
        self.is_link_enabled = true;
        self.last_link_is_playing = false;
        self.last_num_peers = 0;
        log::info!("[LinkSync] Enabled (BPM: {:.1}, quantum: {})", initial_bpm, self.quantum);
    }

    pub fn disable_link(&mut self) {
        if !self.is_link_enabled { return; }
        if let Some(ref link) = self.link {
            link.enable(false);
        }
        self.link = None;
        self.is_link_enabled = false;
        self.num_peers = 0;
        self.link_is_playing = false;
        log::info!("[LinkSync] Disabled");
    }

    /// Poll Link state each frame.
    /// Port of C# LinkSyncController.Update (lines 165-190).
    pub fn update(
        &mut self,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        authority: ClockAuthority,
    ) {
        if !self.is_link_enabled { return; }

        if let Some(ref link) = self.link {
            link.capture_app_session_state(&mut self.session_state);
            let time = link.clock_micros();
            self.current_beat = self.session_state.beat_at_time(time, self.quantum);
            self.current_phase = self.session_state.phase_at_time(time, self.quantum);
            self.link_tempo = self.session_state.tempo();
            let new_num_peers = link.num_peers() as i32;
            let new_is_playing = self.session_state.is_playing();

            // Log peer changes
            if new_num_peers != self.last_num_peers {
                log::info!("[LinkSync] Peers: {} → {}", self.last_num_peers, new_num_peers);
                self.last_num_peers = new_num_peers;
            }
            self.num_peers = new_num_peers;
            self.link_is_playing = new_is_playing;
        }

        // Sync transport from Link state
        self.sync_transport_from_link(arbiter, arb_target, authority);

        self.last_link_is_playing = self.link_is_playing;
    }

    /// Sync MANIFOLD transport state from Link's playing state.
    /// Port of C# LinkSyncController.SyncTransportFromLink (lines 165-190).
    /// When Link starts playing, MANIFOLD follows. When Link stops, MANIFOLD follows.
    fn sync_transport_from_link(
        &mut self,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        authority: ClockAuthority,
    ) {
        if !self.enable_start_stop_sync {
            return;
        }

        let was_playing = self.last_link_is_playing;
        let is_playing = self.link_is_playing;

        if is_playing && !was_playing {
            arbiter.play(ClockAuthority::Link, authority, arb_target);
            log::info!("[LinkSync] Link started playing — MANIFOLD play");
        } else if !is_playing && was_playing {
            arbiter.pause(ClockAuthority::Link, authority, arb_target, false);
            log::info!("[LinkSync] Link stopped playing — MANIFOLD pause");
        }
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
