use manifold_core::ClipId;
use std::collections::HashMap;

use manifold_core::recording::{RecordedClipProvenance, RecordedTempoChange, RecordingProvenance};
use manifold_core::tempo::TempoMap;
use manifold_core::types::TempoPointSource;

/// Handles tempo recording: tracks external tempo changes from Link/MIDI Clock,
/// records tempo points into the TempoMap, manages recording session lifecycle,
/// and captures provenance data for recorded clips.
/// Composed by ContentThread — owns no lifecycle.
/// Port of Unity TempoRecorder.cs.
pub struct TempoRecorder {
    last_recorded_beat: f32,
    last_recorded_bpm: f32,
    session_active: bool,

    clip_starts: HashMap<ClipId, RecordingClipStartInfo>,
}

/// Constants matching Unity TempoRecorder.cs.
impl TempoRecorder {
    pub const BPM_THRESHOLD: f32 = 0.05;
    const MIN_BEAT_SPACING: f32 = 0.125;
}

struct RecordingClipStartInfo {
    clip_id: ClipId,
    video_clip_id: String,
    layer_index: i32,
    midi_note: i32,
    start_time_seconds: f32,
    start_beat: f32,
    start_absolute_tick: i32,
    start_bpm: f32,
    start_tempo_source: TempoPointSource,
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
            clip_starts: HashMap::new(),
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

    pub fn clear_clip_starts(&mut self) {
        self.clip_starts.clear();
    }

    // =================================================================
    // SESSION STATE
    // =================================================================

    /// Called each frame from the update loop. Transitions recording session
    /// active/inactive based on transport state.
    /// Port of Unity TempoRecorder.cs UpdateSessionState lines 51-65.
    pub fn update_session_state(
        &mut self,
        should_be_recording: bool,
        provenance: &mut RecordingProvenance,
        tempo_map: &mut TempoMap,
        default_bpm: f32,
        get_source_at_beat: &dyn Fn(f32) -> TempoPointSource,
    ) {
        if should_be_recording && !self.session_active {
            self.session_active = true;
            self.reset_tracking();
            return;
        }

        if !should_be_recording && self.session_active {
            self.session_active = false;
            self.capture_tempo_lane_snapshot(
                provenance,
                tempo_map,
                default_bpm,
                get_source_at_beat,
            );
            self.reset_tracking();
        }
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

        tempo_map.add_or_replace_point_with_time(current_beat, bpm, source, 0.001, current_time);
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
            beat: current_beat,
            bpm,
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
        provenance.set_recorded_project_bpm(bpm, source, overwrite);
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

            let bpm_at_zero = tempo_map.get_bpm_at_beat(0.0, default_bpm);
            provenance.set_recorded_project_bpm(bpm_at_zero, source_at_zero, true);
        }
    }

    // =================================================================
    // CLIP PROVENANCE
    // =================================================================

    /// Track the start of a live clip for provenance recording.
    /// Port of Unity TempoRecorder.cs TrackClipStart lines 163-189.
    pub fn track_clip_start(
        &mut self,
        provenance: &mut RecordingProvenance,
        clip_id: &str,
        video_clip_id: &str,
        layer_index: i32,
        midi_note: i32,
        start_beat: f32,
        start_absolute_tick: i32,
        start_time: f32,
        start_bpm: f32,
        start_source: TempoPointSource,
        ticks_per_beat: i32,
    ) {
        Self::capture_project_bpm(provenance, start_bpm, start_source, false);

        let resolved_start_tick = if start_absolute_tick >= 0 {
            start_absolute_tick
        } else {
            (start_beat * ticks_per_beat as f32).round() as i32
        };

        self.clip_starts.insert(
            ClipId::new(clip_id),
            RecordingClipStartInfo {
                clip_id: ClipId::new(clip_id),
                video_clip_id: video_clip_id.to_string(),
                layer_index,
                midi_note,
                start_time_seconds: start_time,
                start_beat,
                start_absolute_tick: resolved_start_tick,
                start_bpm,
                start_tempo_source: start_source,
            },
        );
    }

    /// Finalize a live clip's provenance entry on NoteOff/commit.
    /// Port of Unity TempoRecorder.cs FinalizeClip lines 195-237.
    pub fn finalize_clip(
        &mut self,
        provenance: &mut RecordingProvenance,
        live_clip_id: &str,
        recorded_clip: Option<&manifold_core::clip::TimelineClip>,
        end_beat: f32,
        end_absolute_tick: i32,
        midi_note: i32,
        end_time: f32,
        end_bpm: f32,
        end_source: TempoPointSource,
        ticks_per_beat: i32,
    ) {
        let start = match self.clip_starts.remove(live_clip_id) {
            Some(s) => s,
            None => return,
        };

        let resolved_end_tick = if end_absolute_tick >= 0 {
            end_absolute_tick
        } else {
            (end_beat * ticks_per_beat as f32).round() as i32
        };
        let resolved_midi_note = if midi_note >= 0 { midi_note } else { start.midi_note };

        let (saved_clip_id, saved_video_id, saved_layer) = match recorded_clip {
            Some(clip) => (
                clip.id.clone(),
                clip.video_clip_id.clone(),
                clip.layer_index,
            ),
            None => (
                start.clip_id.clone(),
                start.video_clip_id.clone(),
                start.layer_index,
            ),
        };

        let entry = RecordedClipProvenance {
            clip_id: saved_clip_id,
            video_clip_id: saved_video_id,
            layer_index: saved_layer,
            layer_id: None,
            midi_note: resolved_midi_note,
            start_time_seconds: start.start_time_seconds,
            end_time_seconds: end_time,
            start_beat: start.start_beat,
            end_beat,
            start_absolute_tick: start.start_absolute_tick,
            end_absolute_tick: resolved_end_tick,
            start_bpm: start.start_bpm,
            end_bpm,
            start_tempo_source: start.start_tempo_source,
            end_tempo_source: end_source,
        };

        provenance.add_recorded_clip(entry);
    }

    /// Remove a clip start tracking entry (e.g. on cancellation).
    /// Port of Unity TempoRecorder.cs RemoveClipStart lines 241-245.
    pub fn remove_clip_start(&mut self, clip_id: &str) {
        if clip_id.is_empty() {
            return;
        }
        self.clip_starts.remove(clip_id);
    }
}
