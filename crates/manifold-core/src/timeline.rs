use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::clip::TimelineClip;
use crate::layer::Layer;

/// The timeline containing all layers and clips.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Timeline {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub layers: Vec<Layer>,
    #[serde(default)]
    pub export_in_beat: f32,
    #[serde(default)]
    pub export_out_beat: f32,
    #[serde(default)]
    pub export_range_enabled: bool,

    /// Runtime clip lookup cache: clip_id → (layer_index, clip_index).
    #[serde(skip)]
    clip_lookup: HashMap<String, (usize, usize)>,
    #[serde(skip)]
    clip_lookup_dirty: bool,
}

impl Timeline {
    /// Rebuild the O(1) clip lookup cache.
    pub fn rebuild_clip_lookup(&mut self) {
        self.clip_lookup.clear();
        for (li, layer) in self.layers.iter().enumerate() {
            for (ci, clip) in layer.clips.iter().enumerate() {
                self.clip_lookup.insert(clip.id.clone(), (li, ci));
            }
        }
        self.clip_lookup_dirty = false;
    }

    pub fn mark_clip_lookup_dirty(&mut self) {
        self.clip_lookup_dirty = true;
    }

    fn ensure_lookup(&mut self) {
        if self.clip_lookup_dirty || self.clip_lookup.is_empty() {
            self.rebuild_clip_lookup();
        }
    }

    /// Resolve clip location: (layer_index, clip_index). Self-heals on miss.
    fn resolve_clip_location(&mut self, clip_id: &str) -> Option<(usize, usize)> {
        self.ensure_lookup();
        if let Some(&(li, ci)) = self.clip_lookup.get(clip_id) {
            // Validate cache hit
            if self.layers.get(li)
                .and_then(|l| l.clips.get(ci))
                .is_some_and(|c| c.id == clip_id)
            {
                return Some((li, ci));
            }
            // Cache stale — rebuild and retry
            self.rebuild_clip_lookup();
            if let Some(&(li, ci)) = self.clip_lookup.get(clip_id) {
                return Some((li, ci));
            }
        }
        None
    }

    /// O(1) clip lookup by ID with self-healing on miss.
    pub fn find_clip_by_id(&mut self, clip_id: &str) -> Option<&TimelineClip> {
        let (li, ci) = self.resolve_clip_location(clip_id)?;
        self.layers.get(li).and_then(|l| l.clips.get(ci))
    }

    /// O(1) mutable clip lookup.
    pub fn find_clip_by_id_mut(&mut self, clip_id: &str) -> Option<&mut TimelineClip> {
        if let Some((li, ci)) = self.resolve_clip_location(clip_id) {
            return self.layers.get_mut(li).and_then(|l| l.clips.get_mut(ci));
        }
        // Fallback: linear scan
        for layer in &mut self.layers {
            for clip in &mut layer.clips {
                if clip.id == clip_id {
                    return Some(clip);
                }
            }
        }
        None
    }

    /// Register a single clip in the lookup (incremental add).
    pub fn register_clip_in_lookup(&mut self, clip_id: &str, layer_index: usize, clip_index: usize) {
        self.clip_lookup.insert(clip_id.to_string(), (layer_index, clip_index));
    }

    /// Get total duration in beats (max clip EndBeat across all layers).
    pub fn duration_beats(&self) -> f32 {
        let mut max_beat = 0.0f32;
        for layer in &self.layers {
            for clip in &layer.clips {
                max_beat = max_beat.max(clip.end_beat());
            }
        }
        max_beat
    }

    /// Total clip count across all layers.
    pub fn total_clip_count(&self) -> usize {
        self.layers.iter().map(|l| l.clips.len()).sum()
    }

    /// Get active clips at a given beat (respecting mute/solo).
    pub fn get_active_clips_at_beat(&mut self, beat: f32) -> Vec<(usize, usize)> {
        let has_solo = self.layers.iter().any(|l| l.is_solo);
        let mut results = Vec::new();

        for (li, layer) in self.layers.iter_mut().enumerate() {
            // Solo/mute logic
            if has_solo && !layer.is_solo {
                continue;
            }
            if layer.is_muted {
                continue;
            }

            let mut active_indices = Vec::new();
            layer.collect_active_clips_at_beat(beat, &mut active_indices);
            for ci in active_indices {
                if !layer.clips[ci].is_muted {
                    results.push((li, ci));
                }
            }
        }

        results
    }
}
