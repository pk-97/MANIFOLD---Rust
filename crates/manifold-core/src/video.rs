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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

    /// Supported video file extensions.
    /// Unity VideoLibrary.cs line 25.
    pub const SUPPORTED_EXTENSIONS: &'static [&'static str] = &[".mp4", ".mov", ".webm", ".avi"];

    /// Add a clip to the library, updating the lookup.
    /// Unity VideoLibrary.cs AddClip lines 56-69.
    pub fn add_clip(&mut self, clip: VideoClip) {
        if self.clip_lookup.contains_key(&clip.id) {
            return; // Already exists
        }
        let idx = self.clips.len();
        self.clip_lookup.insert(clip.id.clone(), idx);
        self.clips.push(clip);
    }

    /// Remove a clip from the library by ID.
    /// Unity VideoLibrary.cs RemoveClip lines 74-87.
    pub fn remove_clip(&mut self, id: &str) -> Option<VideoClip> {
        if let Some(&idx) = self.clip_lookup.get(id) {
            if idx < self.clips.len() {
                let clip = self.clips.remove(idx);
                self.rebuild_lookup();
                return Some(clip);
            }
        }
        None
    }

    /// Clear all clips and lookup.
    /// Unity VideoLibrary.cs Clear lines 126-130.
    pub fn clear(&mut self) {
        self.clips.clear();
        self.clip_lookup.clear();
    }

    /// Find a clip by file path.
    /// Unity VideoLibrary.cs FindClipByPath lines 259-269.
    pub fn find_clip_by_path(&self, path: &str) -> Option<&VideoClip> {
        self.clips.iter().find(|c| c.file_path == path)
    }

    /// Remove clips whose files no longer exist.
    /// Unity VideoLibrary.cs RemoveMissingClips lines 292-308.
    pub fn remove_missing_clips(&mut self) -> usize {
        let before = self.clips.len();
        self.clips.retain(|clip| {
            clip.file_path.is_empty() || Path::new(&clip.file_path).exists()
        });
        let removed = before - self.clips.len();
        if removed > 0 {
            self.rebuild_lookup();
        }
        removed
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
