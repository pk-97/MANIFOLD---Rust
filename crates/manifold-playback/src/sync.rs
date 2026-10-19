use manifold_core::project::Project;
use manifold_core::types::{ClockAuthority, PlaybackState};
use manifold_core::{Beats, Seconds};

/// Read-only view of playback state for sync controllers.
/// Sync controllers hold this instead of PlaybackController to enforce
/// structural separation: reads go through SyncTarget, writes go through SyncArbiter.
/// Port of C# ISyncTarget.cs lines 8-16.
pub trait SyncTarget {
    fn current_state(&self) -> PlaybackState;
    /// The map time of the beat on screen: what a source compares its own
    /// position against. Under an external authority the grid time drifts
    /// from the map with the source's tempo; the beat does not (D8).
    fn position_time(&self) -> Seconds;
    fn is_playing(&self) -> bool;
    fn timeline_beat_to_time(&self, beat: Beats) -> Seconds;
    fn current_project(&self) -> Option<&Project>;
}

/// Write surface for SyncArbiter to forward gated commands.
/// Implemented by PlaybackController. Separated from SyncTarget so that
/// sync controllers cannot access mutation methods directly.
/// Port of C# ISyncArbiterTarget.cs lines 20-30.
pub trait SyncArbiterTarget {
    fn current_project(&self) -> Option<&Project>;
    fn play(&mut self);
    fn pause(&mut self, clear_recording: bool);
    fn seek(&mut self, time: Seconds);
}

/// Snapshot of read-only playback state for use when the engine is also
/// borrowed mutably as SyncArbiterTarget. Captures values once per frame,
/// then passed to sync controllers that need both read and write access.
pub struct SyncTargetSnapshot {
    state: PlaybackState,
    time: Seconds,
    bpm: f32,
}

impl SyncTargetSnapshot {
    /// Capture a snapshot from any SyncTarget implementor.
    pub fn from_engine(target: &dyn SyncTarget) -> Self {
        let bpm = target.current_project().map_or(120.0, |p| p.settings.bpm.0);
        Self {
            state: target.current_state(),
            time: target.position_time(),
            bpm,
        }
    }
}

impl SyncTarget for SyncTargetSnapshot {
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
        // Fallback: use BPM for beat→time conversion (no tempo map in snapshot).
        let beat_f = beat.as_f32();
        if self.bpm > 0.0 {
            Seconds((beat_f * 60.0 / self.bpm) as f64)
        } else {
            Seconds((beat_f * 0.5) as f64)
        }
    }
    fn current_project(&self) -> Option<&Project> {
        None
    }
}

/// Structural gatekeeper for sync source authority.
/// Port of C# SyncArbiter.
pub struct SyncArbiter {
    pub suppress_next_transport: bool,
    pub manifold_owns_playback: bool,
    /// Wall-clock time when `manifold_owns_playback` was last set.
    /// Prevents premature clearing during the OSC→DAW→MIDI round trip.
    owns_set_time: Seconds,
    /// Wall-clock time of the last user-initiated seek (ruler scrub, click, etc.).
    /// During the cooldown window, MIDI Clock position sync and beat derivation
    /// are suppressed so Ableton has time to receive the OSC seek and update
    /// its MIDI Clock output. Without this, MIDI Clock would drag the playhead
    /// back to the pre-seek position during the round-trip latency.
    last_user_seek_time: Seconds,
}

/// Grace period (seconds) after setting manifold_owns before it can be cleared.
/// Covers OSC send → Ableton processes → MIDI Clock reflects new state.
const OWNERSHIP_GRACE_PERIOD: f32 = 0.5;

/// Cooldown (seconds) after a user-initiated seek during which MIDI Clock
/// position sync is suppressed. Covers the OSC → Ableton → MIDI Clock round trip.
const SEEK_COOLDOWN: f64 = 0.3;

/// Rig log (C0): a transport change that passed the gate, with its source.
fn log_transport(source: ClockAuthority, kind: crate::rig_log::TransportKind, value: f64) {
    crate::rig_log::emit(crate::rig_log::RigRecord::Transport {
        at_ns: crate::rig_log::now_ns(),
        source: crate::rig_log::TransportSource::from_authority(source),
        kind,
        value,
    });
}

impl SyncArbiter {
    pub fn new() -> Self {
        Self {
            suppress_next_transport: false,
            manifold_owns_playback: false,
            owns_set_time: Seconds(-999.0),
            last_user_seek_time: Seconds(-999.0),
        }
    }

    /// Set manifold_owns with a wall-clock timestamp for grace period tracking.
    pub fn set_manifold_owns_at(&mut self, now: Seconds) {
        self.manifold_owns_playback = true;
        self.owns_set_time = now;
    }

    /// Clear ownership only if the grace period has elapsed.
    /// Prevents MIDI Clock from clearing manifold_owns before the
    /// OSC→DAW→MIDI round trip completes.
    pub fn clear_ownership_if_expired(&mut self, now: Seconds) {
        if (now - self.owns_set_time).0 >= OWNERSHIP_GRACE_PERIOD as f64 {
            self.manifold_owns_playback = false;
        }
    }

    pub fn clear_ownership(&mut self) {
        self.manifold_owns_playback = false;
    }

    /// Record that a user-initiated seek just happened. Starts a brief cooldown
    /// during which MIDI Clock position sync is suppressed (ruler scrub, click-seek, etc.).
    pub fn set_user_seek_time(&mut self, now: Seconds) {
        self.last_user_seek_time = now;
    }

    /// Whether the seek cooldown is active (MIDI Clock position sync should be suppressed).
    pub fn is_seek_cooldown_active(&self, now: Seconds) -> bool {
        (now - self.last_user_seek_time).0 < SEEK_COOLDOWN
    }

    pub fn play(
        &mut self,
        source: ClockAuthority,
        authority: ClockAuthority,
        target: &mut dyn SyncArbiterTarget,
    ) -> bool {
        if source != authority {
            return false;
        }
        self.suppress_next_transport = true;
        log_transport(source, crate::rig_log::TransportKind::Play, f64::NAN);
        target.play();
        true
    }

    pub fn pause(
        &mut self,
        source: ClockAuthority,
        authority: ClockAuthority,
        target: &mut dyn SyncArbiterTarget,
        clear_recording: bool,
    ) -> bool {
        if source != authority {
            return false;
        }
        self.suppress_next_transport = true;
        log_transport(source, crate::rig_log::TransportKind::Pause, f64::NAN);
        target.pause(clear_recording);
        true
    }

    pub fn seek(
        &mut self,
        source: ClockAuthority,
        authority: ClockAuthority,
        target: &mut dyn SyncArbiterTarget,
        time: Seconds,
    ) -> bool {
        if source != authority {
            return false;
        }
        // NOTE: Unity's Seek() does NOT set SuppressNextTransport.
        // Only Play() and Pause() suppress echo. Seeks during playback are
        // detected by OscPositionSender via beat-delta comparison instead.
        log_transport(source, crate::rig_log::TransportKind::Seek, time.0);
        target.seek(time);
        true
    }
}

impl Default for SyncArbiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every call so the gate tests can assert exactly what passed
    /// through. Mirrors the shape of `PlaybackEngine`'s `SyncArbiterTarget`
    /// impl without any of the playback machinery.
    #[derive(Default)]
    struct FakeArbTarget {
        played: bool,
        paused: bool,
        sought: Option<Seconds>,
    }

    impl SyncArbiterTarget for FakeArbTarget {
        fn current_project(&self) -> Option<&Project> {
            None
        }
        fn play(&mut self) {
            self.played = true;
        }
        fn pause(&mut self, _clear_recording: bool) {
            self.paused = true;
        }
        fn seek(&mut self, time: Seconds) {
            self.sought = Some(time);
        }
    }

    const ALL_AUTHORITIES: [ClockAuthority; 4] = [
        ClockAuthority::Internal,
        ClockAuthority::Link,
        ClockAuthority::MidiClock,
        ClockAuthority::Osc,
    ];

    // ── section 7 gate matrix: "every sync controller passes (source, authority)
    // and the call is dropped unless they match." Exhaustive over the 4×4
    // grid for each of the three gated operations. ──────────────────────────

    #[test]
    fn arbiter_gate_matrix_play() {
        for &source in &ALL_AUTHORITIES {
            for &authority in &ALL_AUTHORITIES {
                let mut arbiter = SyncArbiter::new();
                let mut target = FakeArbTarget::default();
                let passed = arbiter.play(source, authority, &mut target);
                let should_pass = source == authority;
                assert_eq!(passed, should_pass, "play({source:?}, {authority:?})");
                assert_eq!(target.played, should_pass);
                assert_eq!(arbiter.suppress_next_transport, should_pass);
            }
        }
    }

    #[test]
    fn arbiter_gate_matrix_pause() {
        for &source in &ALL_AUTHORITIES {
            for &authority in &ALL_AUTHORITIES {
                let mut arbiter = SyncArbiter::new();
                let mut target = FakeArbTarget::default();
                let passed = arbiter.pause(source, authority, &mut target, false);
                let should_pass = source == authority;
                assert_eq!(passed, should_pass, "pause({source:?}, {authority:?})");
                assert_eq!(target.paused, should_pass);
            }
        }
    }

    #[test]
    fn arbiter_gate_matrix_seek() {
        for &source in &ALL_AUTHORITIES {
            for &authority in &ALL_AUTHORITIES {
                let mut arbiter = SyncArbiter::new();
                let mut target = FakeArbTarget::default();
                let passed = arbiter.seek(source, authority, &mut target, Seconds(7.0));
                let should_pass = source == authority;
                assert_eq!(passed, should_pass, "seek({source:?}, {authority:?})");
                assert_eq!(target.sought, should_pass.then_some(Seconds(7.0)));
            }
        }
    }

    // ── section 11 thresholds ──────────────────────────────────────────────────

    /// `OWNERSHIP_GRACE_PERIOD` = 0.5s: `manifold_owns_playback` cannot be
    /// cleared before the grace period elapses, even if asked — covers the
    /// OSC→DAW→MIDI round trip.
    #[test]
    fn ownership_grace_period_blocks_clear_before_elapsed() {
        let mut arbiter = SyncArbiter::new();
        arbiter.set_manifold_owns_at(Seconds(10.0));
        arbiter.clear_ownership_if_expired(Seconds(10.0 + 0.5 - 0.001));
        assert!(
            arbiter.manifold_owns_playback,
            "ownership must survive up to (but not including) the 0.5s grace period"
        );
    }

    #[test]
    fn ownership_grace_period_allows_clear_at_boundary() {
        let mut arbiter = SyncArbiter::new();
        arbiter.set_manifold_owns_at(Seconds(10.0));
        arbiter.clear_ownership_if_expired(Seconds(10.5));
        assert!(
            !arbiter.manifold_owns_playback,
            "ownership must clear at exactly the 0.5s grace period (>= semantics)"
        );
    }

    /// `SEEK_COOLDOWN` = 0.3s: CLK position sync is suppressed for 0.3s
    /// after a user-initiated seek (ruler scrub, click-seek).
    #[test]
    fn seek_cooldown_active_before_threshold() {
        let mut arbiter = SyncArbiter::new();
        arbiter.set_user_seek_time(Seconds(5.0));
        assert!(arbiter.is_seek_cooldown_active(Seconds(5.0 + 0.3 - 0.001)));
    }

    #[test]
    fn seek_cooldown_inactive_at_threshold() {
        let mut arbiter = SyncArbiter::new();
        arbiter.set_user_seek_time(Seconds(5.0));
        // 0.3 isn't exactly representable in f64, so `5.0 + 0.3` computed via
        // a decimal literal (5.3) can round a hair under the true sum,
        // flipping the strict `<` comparison. A small margin past the
        // threshold avoids that rounding trap while still proving the same
        // "expired" edge as `5.0 + SEEK_COOLDOWN` intends.
        assert!(!arbiter.is_seek_cooldown_active(Seconds(5.0 + 0.3 + 0.001)));
    }
}
