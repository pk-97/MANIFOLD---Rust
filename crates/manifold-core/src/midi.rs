use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::types::ClipDurationMode;

/// A single MIDI note → clip mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiNoteMapping {
    pub midi_note: i32,
    #[serde(default)]
    pub video_clip_ids: Vec<String>,
    #[serde(default)]
    pub target_layer_index: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mode: Option<ClipDurationMode>,
}

/// MIDI note → clip mappings for the project.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MidiMappingConfig {
    #[serde(default)]
    pub mappings: Vec<MidiNoteMapping>,

    #[serde(skip)]
    mapping_dict: HashMap<i32, usize>,
}

impl MidiMappingConfig {
    pub fn rebuild_dictionary(&mut self) {
        self.mapping_dict.clear();
        for (i, mapping) in self.mappings.iter().enumerate() {
            self.mapping_dict.insert(mapping.midi_note, i);
        }
    }

    pub fn get_mapping_for_note(&self, note: i32) -> Option<&MidiNoteMapping> {
        self.mapping_dict.get(&note).and_then(|&i| self.mappings.get(i))
    }

    /// Remove mappings referencing clip IDs not in the valid set.
    pub fn purge_orphaned_clip_ids(&mut self, valid_ids: &std::collections::HashSet<String>) -> usize {
        let mut removed = 0;
        for mapping in &mut self.mappings {
            let before = mapping.video_clip_ids.len();
            mapping.video_clip_ids.retain(|id| valid_ids.contains(id));
            removed += before - mapping.video_clip_ids.len();
        }
        // Remove empty mappings
        self.mappings.retain(|m| !m.video_clip_ids.is_empty());
        self.rebuild_dictionary();
        removed
    }
}
