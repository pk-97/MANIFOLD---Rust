use manifold_core::project::Project;
use manifold_core::types::{ClockAuthority, PlaybackState};
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
        let bpm = target.current_project().map_or(120.0, |p| p.settings.bpm.0);
        Self {
            state: target.current_state(),
            time: target.current_time(),
            bpm,
        }
    }
}

impl SyncTarget for SyncTargetSnapshot {
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
    /// Pending seek: when Manifold sends a seek via SYNC (OSC → Ableton),
    /// hold the local playhead at the target beat until CLK confirms it.
    /// This prevents the playhead from snapping back to the old position
    /// during the OSC→Ableton→CLK round trip (~10-50ms).
    pending_seek_beat: Option<f32>,
    pending_seek_time: Seconds,
}

/// Grace period (seconds) after setting manifold_owns before it can be cleared.
/// Covers OSC send → Ableton processes → MIDI Clock reflects new state.
const OWNERSHIP_GRACE_PERIOD: f32 = 0.5;
/// Maximum time to hold a pending seek before giving up (CLK didn't confirm).
const PENDING_SEEK_TIMEOUT: f64 = 0.5;
/// Beat proximity threshold — CLK is "close enough" to the seek target.
const PENDING_SEEK_TOLERANCE_BEATS: f32 = 2.0;

impl SyncArbiter {
    pub fn new() -> Self {
        Self {
            suppress_next_transport: false,
            manifold_owns_playback: false,
            owns_set_time: Seconds(-999.0),
            pending_seek_beat: None,
            pending_seek_time: Seconds(-999.0),
        }
    }

    pub fn current_authority(project: Option<&Project>) -> ClockAuthority {
        project
            .map(|p| p.settings.clock_authority)
            .unwrap_or(ClockAuthority::Internal)
    }

    pub fn set_manifold_owns(&mut self) {
        self.manifold_owns_playback = true;
        // Record wall-clock time so clear_ownership can enforce the grace period.
        // Uses owns_set_time field; caller must have called update_time() this frame.
    }

    /// Set manifold_owns for transport echo suppression.
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

    /// Record that Manifold just sent a seek to Ableton via SYNC.
    /// CLK will hold the local playhead at `beat` until CLK confirms
    /// the position (within tolerance) or the timeout expires.
    pub fn set_pending_seek(&mut self, beat: f32, now: Seconds) {
        self.pending_seek_beat = Some(beat);
        self.pending_seek_time = now;
    }

    /// Check if CLK should skip nudge_time because a pending seek hasn't
    /// been confirmed yet. Returns true if CLK's reported beat is still
    /// far from the seek target (Ableton hasn't caught up).
    /// Returns false (resume normal tracking) if:
    /// - No pending seek
    /// - CLK beat is close to the target (confirmed)
    /// - Timeout expired (give up waiting)
    pub fn should_hold_for_pending_seek(&mut self, clk_beat: f32, now: Seconds) -> bool {
        let target = match self.pending_seek_beat {
            Some(b) => b,
            None => return false,
        };

        // Timeout — give up, resume CLK tracking
        if (now - self.pending_seek_time).0 >= PENDING_SEEK_TIMEOUT {
            self.pending_seek_beat = None;
            return false;
        }

        // CLK caught up — confirmed, resume tracking
        if (clk_beat - target).abs() < PENDING_SEEK_TOLERANCE_BEATS {
            self.pending_seek_beat = None;
            return false;
        }

        // Still waiting — hold position
        true
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
        target.pause(clear_recording);
        true
    }

    pub fn nudge_time(
        &self,
        source: ClockAuthority,
        authority: ClockAuthority,
        target: &mut dyn SyncArbiterTarget,
        time: Seconds,
    ) -> bool {
        if source != authority {
            return false;
        }
        target.nudge_time(time);
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
        target.seek(time);
        true
    }

    pub fn set_external_time_sync(
        &self,
        source: ClockAuthority,
        authority: ClockAuthority,
        target: &mut dyn SyncArbiterTarget,
        value: bool,
    ) -> bool {
        if source != authority {
            return false;
        }
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
