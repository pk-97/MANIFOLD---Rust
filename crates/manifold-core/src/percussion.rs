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

impl PercussionImportState {
    /// Post-deserialization validation.
    /// Port of Unity PercussionImportState.EnsureValid (lines 109-160).
    pub fn ensure_valid(&mut self) {
        // Validate clip placements — remove any with empty clip IDs
        self.clip_placements.retain(|p| !p.clip_id.is_empty());

        // Clamp energy envelope values to [0, 1]
        if let Some(ref mut envelope) = self.energy_envelope {
            for val in envelope.iter_mut() {
                *val = val.clamp(0.0, 1.0);
            }
        }

        // Validate stem paths — remove empty entries
        if let Some(ref mut stems) = self.stem_paths {
            stems.retain(|s| !s.is_empty());
            if stems.is_empty() {
                self.stem_paths = None;
            }
        }

        // Validate relative stem paths similarly
        if let Some(ref mut rel_stems) = self.relative_stem_paths {
            rel_stems.retain(|s| !s.is_empty());
            if rel_stems.is_empty() {
                self.relative_stem_paths = None;
            }
        }

        // Clamp audio_start_beat to >= 0
        self.audio_start_beat = self.audio_start_beat.max(0.0);
    }
}
