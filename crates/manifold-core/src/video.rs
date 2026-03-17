use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// A single video file reference in the library.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoClip {
    pub id: String,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub relative_file_path: Option<String>,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub duration: f32,
    #[serde(default)]
    pub resolution_width: i32,
    #[serde(default)]
    pub resolution_height: i32,
    #[serde(default)]
    pub file_size: i64,
    #[serde(default)]
    pub last_modified_ticks: i64,
}

/// The project's video clip library.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VideoLibrary {
    #[serde(default)]
    pub clips: Vec<VideoClip>,

    /// Runtime lookup cache (not serialized).
    #[serde(skip)]
    clip_lookup: HashMap<String, usize>,
}

impl VideoLibrary {
    pub fn rebuild_lookup(&mut self) {
        self.clip_lookup.clear();
        for (i, clip) in self.clips.iter().enumerate() {
            self.clip_lookup.insert(clip.id.clone(), i);
        }
    }

    pub fn find_clip_by_id(&self, id: &str) -> Option<&VideoClip> {
        self.clip_lookup.get(id).and_then(|&i| self.clips.get(i))
    }

    pub fn clip_count(&self) -> usize {
        self.clips.len()
    }

    /// Check if a clip exists in the library by ID.
    /// Port of C# VideoLibrary.HasClip().
    pub fn has_clip(&self, id: &str) -> bool {
        self.clip_lookup.contains_key(id)
    }

    /// Validate all clips in the library. Returns paths of missing files.
    /// Port of C# VideoLibrary.ValidateClips() (lines 274-287).
    pub fn validate_clips(&self) -> ValidationResult {
        let mut missing_files = Vec::new();
        for clip in &self.clips {
            if !clip.file_path.is_empty() && !Path::new(&clip.file_path).exists() {
                missing_files.push(clip.file_path.clone());
            }
        }
        ValidationResult { missing_files }
    }
}

/// Result of VideoLibrary.validate_clips().
/// Port of C# VideoLibrary.ValidationResult.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub missing_files: Vec<String>,
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        self.missing_files.is_empty()
    }
}
