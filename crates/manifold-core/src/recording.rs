use serde::{Deserialize, Serialize};
use crate::types::TempoPointSource;
use crate::tempo::TempoPoint;

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
