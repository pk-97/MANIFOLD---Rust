//! Ableton Link sync controller.
//! Mechanical translation of Unity LinkSyncController.cs.
//!
//! One-way sync: Ableton controls MANIFOLD transport + tempo when Link
//! is selected as clock authority.

use crate::sync::{SyncArbiter, SyncArbiterTarget};
use crate::sync_source::SyncSource;
use manifold_core::Beats;
use manifold_core::types::ClockAuthority;
use std::time::{Duration, Instant};

/// Link sync controller state.
/// Port of Unity LinkSyncController.cs fields.
pub struct LinkSyncController {
    is_link_enabled: bool,
    pub num_peers: i32,
    pub current_beat: Beats,
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
    #[cfg(any(test, feature = "testkit"))]
    simulated: Option<SimulatedLink>,
}

/// A Link session tests drive in place of the network: Link's beat runs at
/// `tempo` from `beat` at `at`. `update` reads it exactly where it would read
/// the network session, so the transport logic under test is the real one.
#[cfg(any(test, feature = "testkit"))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimulatedLink {
    pub at: Instant,
    pub beat: f64,
    pub tempo: f64,
    pub peers: i32,
    pub playing: bool,
}

#[cfg(any(test, feature = "testkit"))]
impl SimulatedLink {
    pub fn beat_at(&self, t: Instant) -> f64 {
        let secs = match t.checked_duration_since(self.at) {
            Some(d) => d.as_secs_f64(),
            None => -(self.at - t).as_secs_f64(),
        };
        self.beat + secs * self.tempo / 60.0
    }

    /// The same session with a new tempo from `at`, keeping the beat continuous.
    pub fn with_tempo(&self, at: Instant, tempo: f64) -> Self {
        Self { at, beat: self.beat_at(at), tempo, ..*self }
    }
}

impl LinkSyncController {
    pub fn new() -> Self {
        Self {
            is_link_enabled: false,
            num_peers: 0,
            current_beat: Beats::ZERO,
            current_phase: 0.0,
            link_tempo: 120.0,
            link_is_playing: false,
            enable_start_stop_sync: true,
            quantum: 4.0,
            last_link_is_playing: false,
            last_num_peers: 0,
            link: None,
            session_state: rusty_link::SessionState::new(),
            #[cfg(any(test, feature = "testkit"))]
            simulated: None,
        }
    }

    /// Enable Link on a simulated session instead of the network.
    #[cfg(any(test, feature = "testkit"))]
    pub fn simulate(&mut self, session: SimulatedLink) {
        self.is_link_enabled = true;
        self.simulated = Some(session);
    }

    pub fn is_link_enabled(&self) -> bool {
        self.is_link_enabled
    }
    pub fn has_active_peers(&self) -> bool {
        self.is_link_enabled && self.num_peers > 0
    }

    /// Enable Link with initial BPM.
    /// Port of C# LinkSyncController.EnableLink.
    pub fn enable_link(&mut self, initial_bpm: f64) {
        if self.is_link_enabled {
            return;
        }
        let link = rusty_link::AblLink::new(initial_bpm);
        link.enable(true);
        if self.enable_start_stop_sync {
            link.enable_start_stop_sync(true);
        }
        self.link = Some(link);
        self.is_link_enabled = true;
        self.last_link_is_playing = false;
        self.last_num_peers = 0;
        log::info!(
            "[LinkSync] Enabled (BPM: {:.1}, quantum: {})",
            initial_bpm,
            self.quantum
        );
    }

    pub fn disable_link(&mut self) {
        if !self.is_link_enabled {
            return;
        }
        if let Some(ref link) = self.link {
            link.enable(false);
        }
        self.link = None;
        self.is_link_enabled = false;
        self.num_peers = 0;
        self.link_is_playing = false;
        log::info!("[LinkSync] Disabled");
    }

    /// Poll Link state each frame. Beat and phase are Link's at `sample_at`,
    /// the deadline of the frame this tick shows, not when the tick woke: a
    /// late wake must not read as Link running ahead of the grid (D15).
    pub fn update(
        &mut self,
        arbiter: &mut SyncArbiter,
        arb_target: &mut dyn SyncArbiterTarget,
        authority: ClockAuthority,
        sample_at: Instant,
    ) {
        if !self.is_link_enabled {
            return;
        }

        if let Some(ref link) = self.link {
            link.capture_app_session_state(&mut self.session_state);
            let time = link_micros_at(link.clock_micros(), Instant::now(), sample_at);
            self.current_beat = Beats(self.session_state.beat_at_time(time, self.quantum));
            self.current_phase = self.session_state.phase_at_time(time, self.quantum);
            self.link_tempo = self.session_state.tempo();
            let new_num_peers = link.num_peers() as i32;
            let new_is_playing = self.session_state.is_playing();

            // Log peer changes
            if new_num_peers != self.last_num_peers {
                log::info!(
                    "[LinkSync] Peers: {} → {}",
                    self.last_num_peers,
                    new_num_peers
                );
                self.last_num_peers = new_num_peers;
            }
            self.num_peers = new_num_peers;
            self.link_is_playing = new_is_playing;
        }
        #[cfg(any(test, feature = "testkit"))]
        if let Some(sim) = self.simulated {
            self.current_beat = Beats(sim.beat_at(sample_at));
            self.current_phase = self.current_beat.0.rem_euclid(self.quantum);
            self.link_tempo = sim.tempo;
            self.num_peers = sim.peers;
            self.link_is_playing = sim.playing;
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
    fn is_enabled(&self) -> bool {
        self.is_link_enabled
    }
    fn display_name(&self) -> &str {
        "Link"
    }
    fn enable(&mut self) {
        self.enable_link(120.0);
    }
    fn disable(&mut self) {
        self.disable_link();
    }
}

impl Default for LinkSyncController {
    fn default() -> Self {
        Self::new()
    }
}

/// Link's clock at `at`, from Link's clock read at `now`. Link's clock and
/// `Instant` are both the host's monotonic clock, so the gap between the two
/// instants carries over unchanged; `at` may be before or after `now`.
fn link_micros_at(link_now: i64, now: Instant, at: Instant) -> i64 {
    let micros = |d: Duration| i64::try_from(d.as_micros()).unwrap_or(i64::MAX);
    match now.checked_duration_since(at) {
        Some(back) => link_now.saturating_sub(micros(back)),
        None => link_now.saturating_add(micros(at - now)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample instant, not the read instant, decides Link's time: a read
    /// 0.8 frame after the deadline gives the deadline's time, and so does
    /// one before it.
    #[test]
    fn link_is_read_at_the_given_instant() {
        let deadline = Instant::now();
        let link_at_deadline = 5_000_000_000i64;
        for late_us in [0u64, 1, 13_333, 16_000, 250_000] {
            let now = deadline + Duration::from_micros(late_us);
            let link_now = link_at_deadline + late_us as i64;
            assert_eq!(link_micros_at(link_now, now, deadline), link_at_deadline, "woke {late_us} us late");
        }
        let early = deadline - Duration::from_micros(6_667);
        assert_eq!(link_micros_at(link_at_deadline - 6_667, early, deadline), link_at_deadline, "woke early");
    }
}
