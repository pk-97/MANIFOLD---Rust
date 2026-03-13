use serde::{Deserialize, Serialize};

/// Placement data for an imported percussion clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedPercussionClipPlacement {
    pub clip_id: String,
    #[serde(default)]
    pub source_time_seconds: f32,
    #[serde(default)]
    pub start_beat_offset: f32,
    #[serde(default)]
    pub quantize_to_grid: bool,
    #[serde(default)]
    pub quantize_step_beats: f32,
    #[serde(default)]
    pub alignment_offset_beats: f32,
    #[serde(default)]
    pub alignment_slope_beats_per_second: f32,
    #[serde(default)]
    pub alignment_pivot_seconds: f32,
}

/// State for imported percussion analysis.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PercussionImportState {
    #[serde(default)]
    pub audio_path: Option<String>,
    #[serde(default)]
    pub audio_start_beat: f32,
    #[serde(default)]
    pub clip_placements: Vec<ImportedPercussionClipPlacement>,
    #[serde(default)]
    pub energy_envelope: Option<Vec<f32>>,
    #[serde(default)]
    pub stem_paths: Option<Vec<String>>,
    #[serde(default)]
    pub relative_audio_path: Option<String>,
    #[serde(default)]
    pub relative_stem_paths: Option<Vec<String>>,
    #[serde(default)]
    pub audio_hash: Option<String>,
}
