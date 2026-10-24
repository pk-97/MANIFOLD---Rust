use manifold_core::recording::{RecordedTempoChange, RecordingProvenance};
use manifold_core::tempo::TempoMap;
use manifold_core::types::TempoPointSource;
use manifold_core::units::{Beats, Bpm};

/// Handles tempo recording: tracks external tempo changes from Link/MIDI Clock,
/// records tempo points into the TempoMap, manages recording session lifecycle,
/// and captures provenance data for recorded clips.
/// Composed by ContentThread — owns no lifecycle.
/// Port of Unity TempoRecorder.cs.
pub struct TempoRecorder {
    last_recorded_beat: f32,
    last_recorded_bpm: f32,
    session_active: bool,
}

/// Constants matching Unity TempoRecorder.cs.
impl TempoRecorder {
    pub const BPM_THRESHOLD: f32 = 0.05;
    const MIN_BEAT_SPACING: f32 = 0.125;
}

// =================================================================
// LIFECYCLE
// =================================================================

impl Default for TempoRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl TempoRecorder {
    pub fn new() -> Self {
        Self {
            last_recorded_beat: f32::NEG_INFINITY,
            last_recorded_bpm: -1.0,
            session_active: false,
        }
    }

    pub fn is_session_active(&self) -> bool {
        self.session_active
    }

    pub fn reset(&mut self) {
        self.session_active = false;
        self.reset_tracking();
    }

    pub fn reset_tracking(&mut self) {
        self.last_recorded_beat = f32::NEG_INFINITY;
        self.last_recorded_bpm = -1.0;
    }

    // =================================================================
    // SESSION STATE
    // =================================================================

    /// Arm a recording session. Writes nothing to the project.
    pub fn start_session(&mut self) {
        self.session_active = true;
        self.reset_tracking();
    }

    /// End the recording session if active (called from Stop/Pause).
    /// Port of Unity TempoRecorder.cs EndSessionIfActive lines 71-77.
    pub fn end_session_if_active(
        &mut self,
        provenance: &mut RecordingProvenance,
        tempo_map: &mut TempoMap,
        default_bpm: f32,
        get_source_at_beat: &dyn Fn(f32) -> TempoPointSource,
    ) {
        if !self.session_active {
            return;
        }
        self.session_active = false;
        self.capture_tempo_lane_snapshot(provenance, tempo_map, default_bpm, get_source_at_beat);
    }

    // =================================================================
    // TEMPO POINT RECORDING
    // =================================================================

    /// Record a tempo point into the TempoMap if BPM changed sufficiently
    /// and enough beats have elapsed since the last recorded point.
    /// Port of Unity TempoRecorder.cs TryRecordTempoPoint lines 85-108.
    pub fn try_record_tempo_point(
        &mut self,
        tempo_map: &mut TempoMap,
        current_beat: f32,
        current_time: f32,
        bpm: f32,
        source: TempoPointSource,
    ) -> bool {
        // Seek/jump while armed: restart spacing history.
        if !self.last_recorded_beat.is_infinite()
            && current_beat < self.last_recorded_beat - Self::MIN_BEAT_SPACING
        {
            self.reset_tracking();
        }

        let bpm_changed = self.last_recorded_bpm <= 0.0
            || (bpm - self.last_recorded_bpm).abs() >= Self::BPM_THRESHOLD;
        let beat_advanced = self.last_recorded_beat.is_infinite()
            || (current_beat - self.last_recorded_beat).abs() >= Self::MIN_BEAT_SPACING;

        if !bpm_changed || !beat_advanced {
            return false;
        }

        tempo_map.add_or_replace_point_with_time(
            Beats::from_f32(current_beat),
            Bpm(bpm),
            source,
            0.001,
            manifold_core::units::Seconds::from_f32(current_time),
        );
        self.last_recorded_beat = current_beat;
        self.last_recorded_bpm = bpm;
        true
    }

    /// Append a tempo change to RecordingProvenance and capture the project BPM.
    /// Port of Unity TempoRecorder.cs AppendTempoChange lines 114-123.
    pub fn append_tempo_change(
        &mut self,
        provenance: &mut RecordingProvenance,
        current_time: f32,
        current_beat: f32,
        bpm: f32,
        source: TempoPointSource,
    ) {
        Self::capture_project_bpm(provenance, bpm, source, false);
        provenance.add_tempo_change(RecordedTempoChange {
            time_seconds: current_time,
            beat: Beats::from_f32(current_beat),
            bpm: Bpm(bpm),
            source,
        });
    }

    /// Store the current BPM as the recorded project BPM in provenance.
    /// Port of Unity TempoRecorder.cs CaptureProjectBpm lines 129-134.
    pub fn capture_project_bpm(
        provenance: &mut RecordingProvenance,
        bpm: f32,
        source: TempoPointSource,
        overwrite: bool,
    ) {
        provenance.set_recorded_project_bpm(Bpm(bpm), source, overwrite);
    }

    /// Snapshot the TempoMap into RecordingProvenance as the recorded tempo lane.
    /// Port of Unity TempoRecorder.cs CaptureTempoLaneSnapshot lines 140-157.
    pub fn capture_tempo_lane_snapshot(
        &self,
        provenance: &mut RecordingProvenance,
        tempo_map: &mut TempoMap,
        default_bpm: f32,
        get_source_at_beat: &dyn Fn(f32) -> TempoPointSource,
    ) {
        provenance.capture_recorded_tempo_lane(tempo_map, true);

        if tempo_map.point_count() > 0 {
            let mut source_at_zero = get_source_at_beat(0.0);
            if source_at_zero == TempoPointSource::Unknown {
                source_at_zero = TempoPointSource::Recorded;
            }

            let bpm_at_zero = tempo_map.get_bpm_at_beat(Beats::ZERO, Bpm(default_bpm));
            provenance.set_recorded_project_bpm(bpm_at_zero, source_at_zero, true);
        }
    }
}
