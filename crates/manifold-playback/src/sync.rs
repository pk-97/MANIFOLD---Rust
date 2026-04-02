use manifold_core::types::{ClockAuthority, PlaybackState};
use manifold_core::project::Project;
use manifold_core::{Beats, Seconds};

/// Read-only view of playback state for sync controllers.
/// Sync controllers hold this instead of PlaybackController to enforce
/// structural separation: reads go through SyncTarget, writes go through SyncArbiter.
/// Port of C# ISyncTarget.cs lines 8-16.
pub trait SyncTarget {
    fn current_state(&self) -> PlaybackState;
    fn current_time(&self) -> Seconds;
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
    fn external_time_sync(&self) -> bool;
    fn set_external_time_sync(&mut self, value: bool);
    fn play(&mut self);
    fn pause(&mut self, clear_recording: bool);
    fn nudge_time(&mut self, time: Seconds);
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
        let bpm = target.current_project()
            .map_or(120.0, |p| p.settings.bpm.0);
        Self {
            state: target.current_state(),
            time: target.current_time(),
            bpm,
        }
    }
}

impl SyncTarget for SyncTargetSnapshot {
    fn current_state(&self) -> PlaybackState { self.state }
    fn current_time(&self) -> Seconds { self.time }
    fn is_playing(&self) -> bool { self.state == PlaybackState::Playing }
    fn timeline_beat_to_time(&self, beat: Beats) -> Seconds {
        // Fallback: use BPM for beat→time conversion (no tempo map in snapshot).
        let beat_f = beat.as_f32();
        if self.bpm > 0.0 {
            Seconds((beat_f * 60.0 / self.bpm) as f64)
        } else {
            Seconds((beat_f * 0.5) as f64)
        }
    }
    fn current_project(&self) -> Option<&Project> { None }
}

/// Structural gatekeeper for sync source authority.
/// Port of C# SyncArbiter.
pub struct SyncArbiter {
    pub suppress_next_transport: bool,
    pub manifold_owns_playback: bool,
    /// Whether a user-initiated seek is pending confirmation from MIDI Clock.
    /// While true, MIDI Clock position sync and beat derivation are suppressed.
    /// Cleared deterministically when MIDI Clock's position converges with the
    /// engine's position (no timers — clears the instant MIDI Clock catches up).
    pub pending_seek: bool,
}

/// Convergence threshold (seconds). When the delta between MIDI Clock's
/// position and the engine's position falls below this, the pending seek
/// is considered confirmed and MIDI Clock resumes driving position.
/// ~6 MIDI Clock ticks at 120 BPM — tight enough to be musically meaningful,
/// loose enough to account for MIDI Clock quantization (24 PPQN).
const SEEK_CONVERGE_THRESHOLD: f64 = 0.1;

impl SyncArbiter {
    pub fn new() -> Self {
        Self {
            suppress_next_transport: false,
            manifold_owns_playback: false,
            pending_seek: false,
        }
    }

    pub fn current_authority(project: Option<&Project>) -> ClockAuthority {
        project
            .map(|p| p.settings.clock_authority)
            .unwrap_or(ClockAuthority::Internal)
    }

    pub fn set_manifold_owns(&mut self) {
        self.manifold_owns_playback = true;
    }

    /// Set manifold_owns for transport echo suppression.
    pub fn set_manifold_owns_at(&mut self, _now: Seconds) {
        self.manifold_owns_playback = true;
    }

    /// Clear ownership — called when MIDI Clock confirms the expected
    /// transport state (deterministic, not timer-based).
    pub fn clear_ownership(&mut self) {
        self.manifold_owns_playback = false;
    }

    /// Mark a user-initiated seek as pending. MIDI Clock position sync
    /// is suppressed until `check_seek_convergence` clears it.
    pub fn set_pending_seek(&mut self) {
        self.pending_seek = true;
    }

    /// Check if MIDI Clock has converged to the engine's position after
    /// a pending seek. Returns true if the seek was cleared (convergence
    /// detected), meaning MIDI Clock can resume driving position.
    pub fn check_seek_convergence(&mut self, delta: Seconds) -> bool {
        if !self.pending_seek { return true; }
        if delta.0.abs() < SEEK_CONVERGE_THRESHOLD {
            self.pending_seek = false;
            true
        } else {
            false
        }
    }

    pub fn play(&mut self, source: ClockAuthority, authority: ClockAuthority, target: &mut dyn SyncArbiterTarget) -> bool {
        if source != authority { return false; }
        self.suppress_next_transport = true;
        target.play();
        true
    }

    pub fn pause(&mut self, source: ClockAuthority, authority: ClockAuthority, target: &mut dyn SyncArbiterTarget, clear_recording: bool) -> bool {
        if source != authority { return false; }
        self.suppress_next_transport = true;
        target.pause(clear_recording);
        true
    }

    pub fn nudge_time(&self, source: ClockAuthority, authority: ClockAuthority, target: &mut dyn SyncArbiterTarget, time: Seconds) -> bool {
        if source != authority { return false; }
        target.nudge_time(time);
        true
    }

    pub fn seek(&mut self, source: ClockAuthority, authority: ClockAuthority, target: &mut dyn SyncArbiterTarget, time: Seconds) -> bool {
        if source != authority { return false; }
        // NOTE: Unity's Seek() does NOT set SuppressNextTransport.
        // Only Play() and Pause() suppress echo. Seeks during playback are
        // detected by OscPositionSender via beat-delta comparison instead.
        target.seek(time);
        true
    }

    pub fn set_external_time_sync(&self, source: ClockAuthority, authority: ClockAuthority, target: &mut dyn SyncArbiterTarget, value: bool) -> bool {
        if source != authority { return false; }
        target.set_external_time_sync(value);
        true
    }

    pub fn clear_external_time_sync(&self, target: &mut dyn SyncArbiterTarget) {
        target.set_external_time_sync(false);
    }
}

impl Default for SyncArbiter {
    fn default() -> Self {
        Self::new()
    }
}
