use manifold_core::types::{ClockAuthority, PlaybackState};
use manifold_core::project::Project;

/// Read-only view of playback state for sync controllers.
/// Sync controllers hold this instead of PlaybackController to enforce
/// structural separation: reads go through SyncTarget, writes go through SyncArbiter.
/// Port of C# ISyncTarget.cs lines 8-16.
pub trait SyncTarget {
    fn current_state(&self) -> PlaybackState;
    fn current_time(&self) -> f32;
    fn is_playing(&self) -> bool;
    fn timeline_beat_to_time(&self, beat: f32) -> f32;
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
    fn nudge_time(&mut self, time: f32);
    fn seek(&mut self, time: f32);
}

/// Snapshot of read-only playback state for use when the engine is also
/// borrowed mutably as SyncArbiterTarget. Captures values once per frame,
/// then passed to sync controllers that need both read and write access.
pub struct SyncTargetSnapshot {
    state: PlaybackState,
    time: f32,
    bpm: f32,
}

impl SyncTargetSnapshot {
    /// Capture a snapshot from any SyncTarget implementor.
    pub fn from_engine(target: &dyn SyncTarget) -> Self {
        let bpm = target.current_project()
            .map_or(120.0, |p| p.settings.bpm);
        Self {
            state: target.current_state(),
            time: target.current_time(),
            bpm,
        }
    }
}

impl SyncTarget for SyncTargetSnapshot {
    fn current_state(&self) -> PlaybackState { self.state }
    fn current_time(&self) -> f32 { self.time }
    fn is_playing(&self) -> bool { self.state == PlaybackState::Playing }
    fn timeline_beat_to_time(&self, beat: f32) -> f32 {
        // Fallback: use BPM for beat→time conversion (no tempo map in snapshot).
        if self.bpm > 0.0 { beat * 60.0 / self.bpm } else { beat * 0.5 }
    }
    fn current_project(&self) -> Option<&Project> { None }
}

/// Structural gatekeeper for sync source authority.
/// Port of C# SyncArbiter.
pub struct SyncArbiter {
    pub suppress_next_transport: bool,
    pub manifold_owns_playback: bool,
}

impl SyncArbiter {
    pub fn new() -> Self {
        Self {
            suppress_next_transport: false,
            manifold_owns_playback: false,
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

    pub fn clear_ownership(&mut self) {
        self.manifold_owns_playback = false;
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

    pub fn nudge_time(&self, source: ClockAuthority, authority: ClockAuthority, target: &mut dyn SyncArbiterTarget, time: f32) -> bool {
        if source != authority { return false; }
        target.nudge_time(time);
        true
    }

    pub fn seek(&mut self, source: ClockAuthority, authority: ClockAuthority, target: &mut dyn SyncArbiterTarget, time: f32) -> bool {
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
