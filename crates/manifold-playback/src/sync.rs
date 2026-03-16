use manifold_core::types::ClockAuthority;
use manifold_core::project::Project;

/// Target interface for SyncArbiter to call into.
pub trait SyncArbiterTarget {
    fn current_project(&self) -> Option<&Project>;
    fn external_time_sync(&self) -> bool;
    fn set_external_time_sync(&mut self, value: bool);
    fn play(&mut self);
    fn pause(&mut self, clear_recording: bool);
    fn nudge_time(&mut self, time: f32);
    fn seek(&mut self, time: f32);
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
