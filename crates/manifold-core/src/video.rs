use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}
