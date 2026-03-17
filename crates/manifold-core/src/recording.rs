use serde::{Deserialize, Serialize};
use crate::types::TempoPointSource;
use crate::tempo::{TempoMap, TempoPoint};
use crate::math::BeatQuantizer;

/// Provenance data for a single recorded clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedClipProvenance {
    pub clip_id: String,
    #[serde(default)]
    pub video_clip_id: String,
    #[serde(default)]
    pub layer_index: i32,
    #[serde(default)]
    pub midi_note: i32,
    #[serde(default)]
    pub start_time_seconds: f32,
    #[serde(default)]
    pub end_time_seconds: f32,
    #[serde(default)]
    pub start_beat: f32,
    #[serde(default)]
    pub end_beat: f32,
    #[serde(default)]
    pub start_absolute_tick: i32,
    #[serde(default)]
    pub end_absolute_tick: i32,
    #[serde(default)]
    pub start_bpm: f32,
    #[serde(default)]
    pub end_bpm: f32,
    #[serde(default)]
    pub start_tempo_source: TempoPointSource,
    #[serde(default)]
    pub end_tempo_source: TempoPointSource,
}

/// A recorded tempo change event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTempoChange {
    pub time_seconds: f32,
    pub beat: f32,
    pub bpm: f32,
    #[serde(default)]
    pub source: TempoPointSource,
}

/// Full recording provenance for the project.
/// Port of Unity RecordingProvenance.cs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecordingProvenance {
    #[serde(default)]
    pub recorded_clips: Vec<RecordedClipProvenance>,
    #[serde(default)]
    pub tempo_changes: Vec<RecordedTempoChange>,
    #[serde(default)]
    pub recorded_tempo_lane: Vec<TempoPoint>,
    #[serde(default)]
    pub has_recorded_project_bpm: bool,
    #[serde(default)]
    pub recorded_project_bpm: f32,
    #[serde(default)]
    pub recorded_project_bpm_source: TempoPointSource,
}

impl RecordingProvenance {
    /// Post-deserialization validation.
    /// Unity RecordingProvenance.cs EnsureValid lines 142-166.
    pub fn ensure_valid(&mut self) {
        // Sort tempo lane by beat
        self.recorded_tempo_lane.sort_by(|a, b| {
            a.beat.partial_cmp(&b.beat).unwrap_or(std::cmp::Ordering::Equal)
        });

        if self.has_recorded_project_bpm {
            self.recorded_project_bpm = BeatQuantizer::quantize_bpm(self.recorded_project_bpm);
        } else if self.recorded_project_bpm <= 0.0 {
            self.recorded_project_bpm = 120.0;
        }
    }

    /// Clear all provenance data.
    /// Unity RecordingProvenance.cs Clear lines 168-174.
    pub fn clear(&mut self) {
        self.recorded_clips.clear();
        self.tempo_changes.clear();
        self.recorded_tempo_lane.clear();
        self.clear_recorded_project_bpm();
    }

    /// Add a recorded clip provenance entry.
    /// Unity RecordingProvenance.cs AddRecordedClip lines 176-181.
    pub fn add_recorded_clip(&mut self, clip: RecordedClipProvenance) {
        self.ensure_valid();
        self.recorded_clips.push(clip);
    }

    /// Add a tempo change event.
    /// Unity RecordingProvenance.cs AddTempoChange lines 183-188.
    pub fn add_tempo_change(&mut self, change: RecordedTempoChange) {
        self.ensure_valid();
        self.tempo_changes.push(change);
    }

    /// Try to get the recorded tempo lane.
    /// Unity RecordingProvenance.cs TryGetRecordedTempoLane lines 190-201.
    pub fn try_get_recorded_tempo_lane(&self) -> Option<&[TempoPoint]> {
        if self.recorded_tempo_lane.is_empty() {
            None
        } else {
            Some(&self.recorded_tempo_lane)
        }
    }

    /// Whether a recorded tempo lane exists.
    pub fn has_recorded_tempo_lane(&self) -> bool {
        !self.recorded_tempo_lane.is_empty()
    }

    /// Set the recorded tempo lane.
    /// Unity RecordingProvenance.cs SetRecordedTempoLane lines 203-225.
    pub fn set_recorded_tempo_lane(&mut self, source_lane: &[TempoPoint], overwrite: bool) {
        self.ensure_valid();
        if self.has_recorded_tempo_lane() && !overwrite {
            return;
        }

        self.recorded_tempo_lane.clear();
        for point in source_lane {
            self.recorded_tempo_lane.push(TempoPoint {
                beat: point.beat,
                bpm: point.bpm,
                source: point.source,
                recorded_at_seconds: point.recorded_at_seconds,
            });
        }

        self.recorded_tempo_lane.sort_by(|a, b| {
            a.beat.partial_cmp(&b.beat).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Capture the recorded tempo lane from a tempo map.
    /// Unity RecordingProvenance.cs CaptureRecordedTempoLane lines 227-240.
    pub fn capture_recorded_tempo_lane(&mut self, tempo_map: &mut TempoMap, overwrite: bool) {
        let points: Vec<TempoPoint> = {
            let sorted = tempo_map.get_sorted_points();
            sorted.to_vec()
        };
        self.set_recorded_tempo_lane(&points, overwrite);
    }

    /// Try to restore the recorded tempo lane to a target tempo map.
    /// Unity RecordingProvenance.cs TryRestoreRecordedTempoLane lines 242-265.
    pub fn try_restore_recorded_tempo_lane(&self, target: &mut TempoMap, fallback_bpm: f32) -> bool {
        if !self.has_recorded_tempo_lane() {
            return false;
        }

        target.clear();
        for point in &self.recorded_tempo_lane {
            target.add_or_replace_point_with_time(
                point.beat,
                point.bpm,
                point.source,
                0.001,
                point.recorded_at_seconds,
            );
        }

        let beat_zero_source = self.get_source_at_beat(&self.recorded_tempo_lane, 0.0, TempoPointSource::Recorded);
        let source = if beat_zero_source == TempoPointSource::Unknown {
            TempoPointSource::Recorded
        } else {
            beat_zero_source
        };
        target.ensure_default_at_beat_zero(fallback_bpm, source);

        true
    }

    /// Get tempo source at a given beat from a lane.
    /// Unity RecordingProvenance.cs GetSourceAtBeat lines 267-286.
    fn get_source_at_beat(&self, lane: &[TempoPoint], beat: f32, fallback: TempoPointSource) -> TempoPointSource {
        if lane.is_empty() {
            return fallback;
        }
        let mut source = fallback;
        for point in lane {
            if point.beat > beat {
                break;
            }
            source = point.source;
        }
        source
    }

    /// Check if the recorded tempo lane is equivalent to a tempo map.
    /// Unity RecordingProvenance.cs IsRecordedTempoLaneEquivalent lines 288-316.
    pub fn is_recorded_tempo_lane_equivalent(
        &self,
        tempo_map: &mut TempoMap,
        beat_epsilon: f32,
        bpm_epsilon: f32,
    ) -> bool {
        if !self.has_recorded_tempo_lane() {
            return tempo_map.points.is_empty();
        }

        let current_lane = tempo_map.get_sorted_points();
        if current_lane.len() != self.recorded_tempo_lane.len() {
            return false;
        }

        for (i, a) in current_lane.iter().enumerate() {
            let b = &self.recorded_tempo_lane[i];
            if (a.beat - b.beat).abs() > beat_epsilon {
                return false;
            }
            if (a.bpm - b.bpm).abs() > bpm_epsilon {
                return false;
            }
            if a.source != b.source {
                return false;
            }
        }

        true
    }

    /// Try to get the recorded project BPM.
    /// Unity RecordingProvenance.cs TryGetRecordedProjectBpm lines 318-329.
    pub fn try_get_recorded_project_bpm(&self) -> Option<f32> {
        if self.has_recorded_project_bpm {
            Some(self.recorded_project_bpm.clamp(20.0, 300.0))
        } else {
            None
        }
    }

    /// Set the recorded project BPM.
    /// Unity RecordingProvenance.cs SetRecordedProjectBpm lines 331-342.
    pub fn set_recorded_project_bpm(&mut self, bpm: f32, source: TempoPointSource, overwrite: bool) {
        self.ensure_valid();
        if self.has_recorded_project_bpm && !overwrite {
            return;
        }
        self.recorded_project_bpm = BeatQuantizer::quantize_bpm(bpm.clamp(20.0, 300.0));
        self.recorded_project_bpm_source = source;
        self.has_recorded_project_bpm = true;
    }

    /// Clear the recorded project BPM.
    /// Unity RecordingProvenance.cs ClearRecordedProjectBpm lines 344-349.
    pub fn clear_recorded_project_bpm(&mut self) {
        self.has_recorded_project_bpm = false;
        self.recorded_project_bpm = 120.0;
        self.recorded_project_bpm_source = TempoPointSource::Unknown;
    }

    pub fn recorded_clip_count(&self) -> usize {
        self.recorded_clips.len()
    }

    pub fn tempo_change_count(&self) -> usize {
        self.tempo_changes.len()
    }
}
