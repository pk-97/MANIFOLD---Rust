use crate::units::{Beats, Seconds};
use serde::{Deserialize, Serialize};

/// Placement data for an imported percussion clip.
/// Port of Unity ImportedPercussionClipPlacement (Project.cs lines 398-492).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedPercussionClipPlacement {
    pub clip_id: String,
    #[serde(default)]
    pub source_time_seconds: Seconds,
    #[serde(default)]
    pub start_beat_offset: Beats,
    #[serde(default)]
    pub quantize_to_grid: bool,
    #[serde(default)]
    pub quantize_step_beats: Beats,
    #[serde(default)]
    pub alignment_offset_beats: Beats,
    #[serde(default)]
    pub alignment_slope_beats_per_second: f32,
    #[serde(default)]
    pub alignment_pivot_seconds: Seconds,
}

impl ImportedPercussionClipPlacement {
    /// Port of Unity ImportedPercussionClipPlacement.SetAlignmentState() (Project.cs lines 447-452).
    pub fn set_alignment_state(
        &mut self,
        offset_beats: Beats,
        slope_beats_per_second: f32,
        pivot_seconds: Seconds,
    ) {
        self.alignment_offset_beats = if offset_beats.0.is_finite() {
            offset_beats
        } else {
            Beats::ZERO
        };
        self.alignment_slope_beats_per_second = if slope_beats_per_second.is_finite() {
            slope_beats_per_second
        } else {
            0.0
        };
        self.alignment_pivot_seconds = if pivot_seconds.0.is_finite() {
            pivot_seconds.max(Seconds::ZERO)
        } else {
            Seconds::ZERO
        };
    }

    /// Port of Unity ImportedPercussionClipPlacement.IsValid() (Project.cs lines 468-490).
    pub fn is_valid(&self) -> bool {
        if self.clip_id.trim().is_empty() {
            return false;
        }
        if !self.source_time_seconds.0.is_finite() || self.source_time_seconds < Seconds::ZERO {
            return false;
        }
        if !self.start_beat_offset.0.is_finite() || self.start_beat_offset < Beats::ZERO {
            return false;
        }
        if !self.quantize_step_beats.0.is_finite() || self.quantize_step_beats < Beats::ZERO {
            return false;
        }
        if !self.alignment_offset_beats.0.is_finite() {
            return false;
        }
        if !self.alignment_slope_beats_per_second.is_finite() {
            return false;
        }
        if !self.alignment_pivot_seconds.0.is_finite()
            || self.alignment_pivot_seconds < Seconds::ZERO
        {
            return false;
        }
        true
    }
}

/// State for imported percussion analysis.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PercussionImportState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_path: Option<String>,
    #[serde(default)]
    pub audio_start_beat: Beats,
    #[serde(default)]
    pub clip_placements: Vec<ImportedPercussionClipPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_envelope: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_audio_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_stem_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
        self.audio_start_beat = self.audio_start_beat.max(Beats::ZERO);
    }
}
